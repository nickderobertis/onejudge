//! [`CommandProvider`]: a [`Provider`] backed by an external command speaking a
//! small JSON-lines protocol (one request object in on stdin, one response object
//! out on stdout, per op). It backs the deterministic test doubles the e2e suite
//! drives and any custom provider a consumer writes. The wire contract is
//! documented in `docs/protocol.md`.
//!
//! Protocol **v8** adds the classified worker-turn outcome to every supervisor
//! request. Protocol **v7** adds caller-named `artifacts` to the judge request's `evidence`,
//! which **v6** introduced. Protocol **v6** added optional evaluator evidence to judge requests. Protocol **v5** adds `notes` to the supervisor request — the role-addressed
//! corrections delivered into the run so far, omitted when there are none, so a v4
//! double sees a byte-identical request. Protocol **v4** added the unified
//! supervisor request; v2 dropped
//! `platform`/`model` from every request: the custom command
//! owns harness/model selection itself (onejudge no longer passes them).

use std::io::Write as _;
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

use crate::error::{Error, ProviderErrorKind, Result};
use crate::provider::{
    supervise_with_reask, Assessment, AssistantTurn, EvidenceContext, JudgeKind, JudgeQuery,
    JudgeValue, JudgeVerdict, Provider, SkillRef, SupervisorOutcome, SupervisorQuery,
    SupervisorTurn, TurnOutcome, UserTurn,
};
use crate::spawn::{role_of, SharedSpawnHook, SpawnContext, SpawnedProcess, Spawner};
use crate::transcript::{Message, ToolEvent};
use crate::usage::Usage;

// --- Wire types (the JSON-lines protocol) ---------------------------------
//
// Each request frame is its own type, so under `sdk-schema` it generates the schema
// it is registered under (`frame_schemas`): `agent.onejudge-frame.<op>@8`, the
// declaration `docs/protocol.md` names. `deny_unknown_fields` and `default` change
// nothing onejudge writes; they state what a frame admits.

/// The protocol version `docs/protocol.md` calls current, and the version every
/// registered frame schema carries.
#[cfg(feature = "sdk-schema")]
pub(crate) const PROTOCOL_VERSION: u32 = 8;

#[derive(Serialize)]
#[cfg_attr(feature = "sdk-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
struct SkillPayload<'a> {
    name: &'a str,
    path: &'a str,
    instructions: &'a str,
}

#[derive(Serialize)]
#[cfg_attr(feature = "sdk-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
struct EvidencePayload<'a> {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    worktree: Option<&'a str>,
    history_files: &'a [String],
    /// Caller-named artifacts, resolved against the worktree (protocol v7).
    /// Omitted when none are named, so a v6 command sees a byte-identical request.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    artifacts: Vec<String>,
}

/// `respond`: run one skill turn.
#[derive(Serialize)]
#[cfg_attr(feature = "sdk-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
struct RespondFrame<'a> {
    skill: SkillPayload<'a>,
    messages: &'a [Message],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session: Option<&'a str>,
}

/// `user`: produce one simulated-user turn.
#[derive(Serialize)]
#[cfg_attr(feature = "sdk-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
struct UserFrame<'a> {
    persona: &'a str,
    messages: &'a [Message],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session: Option<&'a str>,
}

/// `supervisor`: decide completion, or produce the next user turn.
#[derive(Serialize)]
#[cfg_attr(feature = "sdk-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
struct SupervisorFrame<'a> {
    task: &'a str,
    persona: &'a str,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    done_when: Option<&'a str>,
    worktree: &'a str,
    history_name: &'a str,
    /// Every note delivered into this run so far (protocol v5). Omitted when
    /// none has been, so a v4 double sees a byte-identical request.
    #[serde(default, skip_serializing_if = "<[_]>::is_empty")]
    notes: &'a [crate::note::DeliveredNote],
    turn: &'a TurnOutcome,
    messages: &'a [Message],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session: Option<&'a str>,
}

/// `judge`: score a criterion against the transcript. A numeric score carries its
/// bounds; a boolean one has none.
#[derive(Serialize)]
#[cfg_attr(feature = "sdk-schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
enum JudgeFrame<'a> {
    Boolean {
        criterion: &'a str,
        messages: &'a [Message],
        #[serde(default, skip_serializing_if = "Option::is_none")]
        evidence: Option<EvidencePayload<'a>>,
    },
    Numeric {
        criterion: &'a str,
        min: f64,
        max: f64,
        messages: &'a [Message],
        #[serde(default, skip_serializing_if = "Option::is_none")]
        evidence: Option<EvidencePayload<'a>>,
    },
}

impl<'a> JudgeFrame<'a> {
    /// The frame `query` asks for.
    ///
    /// # Errors
    /// [`Error::Invalid`] for a numeric query with no scale: the protocol's
    /// `numeric` request carries `min` and `max`, and one without them is a frame
    /// no command can score.
    fn of(
        query: &JudgeQuery<'a>,
        messages: &'a [Message],
        evidence: Option<EvidencePayload<'a>>,
    ) -> Result<Self> {
        match (query.kind, query.scale) {
            (JudgeKind::Boolean, _) => Ok(Self::Boolean {
                criterion: query.criterion,
                messages,
                evidence,
            }),
            (JudgeKind::Numeric, Some((min, max))) => Ok(Self::Numeric {
                criterion: query.criterion,
                min,
                max,
                messages,
                evidence,
            }),
            (JudgeKind::Numeric, None) => Err(Error::Invalid(
                "a numeric judge query needs a scale: the protocol's `numeric` request \
                 carries `min` and `max`"
                    .into(),
            )),
        }
    }
}

/// `assess`: write a free-text judgement.
#[derive(Serialize)]
#[cfg_attr(feature = "sdk-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
struct AssessFrame<'a> {
    prompt: &'a str,
    messages: &'a [Message],
}

/// One request, discriminated by its `op`.
#[derive(Serialize)]
#[serde(tag = "op", rename_all = "lowercase")]
enum Request<'a> {
    Respond(RespondFrame<'a>),
    User(UserFrame<'a>),
    Supervisor(SupervisorFrame<'a>),
    Judge(JudgeFrame<'a>),
    Assess(AssessFrame<'a>),
}

/// Every request frame's schema, with the id it is registered under:
/// `agent.onejudge-frame.<op>@8`, the frame type's own document with its `op`
/// pinned to the operation's word.
#[cfg(feature = "sdk-schema")]
pub(crate) fn frame_schemas() -> Vec<(onemessagebus::SchemaId, schemars::Schema)> {
    use onemessagebus::SchemaId;
    const RESPOND: SchemaId =
        SchemaId::literal("agent", "onejudge-frame.respond", PROTOCOL_VERSION);
    const USER: SchemaId = SchemaId::literal("agent", "onejudge-frame.user", PROTOCOL_VERSION);
    const SUPERVISOR: SchemaId =
        SchemaId::literal("agent", "onejudge-frame.supervisor", PROTOCOL_VERSION);
    const JUDGE: SchemaId = SchemaId::literal("agent", "onejudge-frame.judge", PROTOCOL_VERSION);
    const ASSESS: SchemaId = SchemaId::literal("agent", "onejudge-frame.assess", PROTOCOL_VERSION);
    vec![
        (RESPOND, frame_schema::<RespondFrame<'static>>("respond")),
        (USER, frame_schema::<UserFrame<'static>>("user")),
        (
            SUPERVISOR,
            frame_schema::<SupervisorFrame<'static>>("supervisor"),
        ),
        (JUDGE, frame_schema::<JudgeFrame<'static>>("judge")),
        (ASSESS, frame_schema::<AssessFrame<'static>>("assess")),
    ]
}

#[cfg(feature = "sdk-schema")]
fn frame_schema<F: schemars::JsonSchema>(op: &str) -> schemars::Schema {
    let mut schema = schemars::schema_for!(F);
    if let Some(root) = schema.as_object_mut() {
        pin_op(root, op);
        // A frame discriminated further (`judge`, by `kind`) is one document per
        // shape, and each shape is a frame naming the same `op`.
        if let Some(serde_json::Value::Array(shapes)) = root.get_mut("oneOf") {
            for shape in shapes
                .iter_mut()
                .filter_map(serde_json::Value::as_object_mut)
            {
                pin_op(shape, op);
            }
        }
    }
    schema
}

/// Declare `op` as the frame's first, required member, fixed to `word`.
#[cfg(feature = "sdk-schema")]
fn pin_op(object: &mut serde_json::Map<String, serde_json::Value>, word: &str) {
    use serde_json::Value;
    let mut properties = serde_json::Map::new();
    properties.insert(
        "op".into(),
        serde_json::json!({
            "type": "string",
            "const": word,
            "description": "The operation this frame asks for."
        }),
    );
    if let Some(Value::Object(existing)) = object.get("properties") {
        properties.extend(existing.clone());
    }
    object.insert("properties".into(), Value::Object(properties));
    let mut required = vec![Value::String("op".into())];
    if let Some(Value::Array(existing)) = object.get("required") {
        required.extend(existing.iter().cloned());
    }
    object.insert("required".into(), Value::Array(required));
}

#[derive(Deserialize)]
struct RespondPayload {
    message: String,
    #[serde(default)]
    done: bool,
    #[serde(default)]
    usage: Option<Usage>,
    /// Optional normalized tool events a provider may report; absent/`null` when
    /// it surfaces none.
    #[serde(default)]
    events: Option<Vec<ToolEvent>>,
}

#[derive(Deserialize)]
struct UserPayload {
    message: String,
    #[serde(default)]
    stop: bool,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct SupervisorPayload {
    completion: bool,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct JudgePayload {
    value: JudgeValue,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct AssessmentPayload {
    text: String,
    #[serde(default)]
    usage: Option<Usage>,
}

// --- CommandProvider ------------------------------------------------------

/// A [`Provider`] backed by an external command speaking the JSON-lines protocol.
#[derive(Debug, Clone)]
pub struct CommandProvider {
    argv: Vec<String>,
    spawner: Spawner,
}

impl CommandProvider {
    /// Build a provider from an argv vector (program + args). The program is
    /// resolved on `PATH`.
    ///
    /// # Errors
    /// [`Error::Invalid`] if `argv` is empty.
    pub fn new(argv: Vec<String>) -> Result<Self> {
        if argv.is_empty() {
            return Err(Error::Invalid("provider command is empty".into()));
        }
        Ok(Self {
            argv,
            spawner: Spawner::default(),
        })
    }

    /// Offer every process this provider spawns to `hook` before it starts work,
    /// so an in-process embedder can place it in a group it owns and can later
    /// terminate. See [`SpawnHook`](crate::SpawnHook).
    #[must_use]
    pub fn with_spawn_hook(mut self, hook: SharedSpawnHook) -> Self {
        self.spawner.install(hook);
        self
    }

    /// Send one request and parse the single response object from stdout.
    fn call<T: for<'de> Deserialize<'de>>(&self, request: &Request<'_>, op: &str) -> Result<T> {
        let payload = serde_json::to_vec(request).map_err(|e| {
            Error::provider(op.to_string(), format!("could not encode request: {e}"))
        })?;

        let mut command = Command::new(&self.argv[0]);
        command
            .args(&self.argv[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = self.spawner.spawn(
            &mut command,
            &SpawnContext {
                role: role_of(op),
                op,
                program: &self.argv[0],
            },
            |e| {
                Error::provider_classified(
                    op.to_string(),
                    format!(
                        "could not run provider `{}`: {e}. Is it installed and on PATH?",
                        self.argv[0]
                    ),
                    ProviderErrorKind::Spawn,
                )
            },
        )?;

        {
            let stdin = child
                .stdin
                .as_mut()
                .ok_or_else(|| Error::provider(op.to_string(), "could not open provider stdin"))?;
            stdin
                .write_all(&payload)
                .and_then(|()| stdin.write_all(b"\n"))
                .map_err(|e| {
                    Error::provider(op.to_string(), format!("could not write request: {e}"))
                })?;
        }

        let output = child.wait_with_output().map_err(|e| {
            Error::provider(op.to_string(), format!("provider did not complete: {e}"))
        })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::provider_classified(
                op.to_string(),
                format!("provider exited with {}: {}", output.status, stderr.trim()),
                ProviderErrorKind::Protocol,
            ));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let line = stdout.trim();
        if line.is_empty() {
            return Err(Error::provider_classified(
                op.to_string(),
                "provider produced no output (expected one JSON response object)",
                ProviderErrorKind::Protocol,
            ));
        }
        serde_json::from_str(line).map_err(|e| {
            Error::provider_classified(
                op.to_string(),
                format!("provider response was not valid JSON for `{op}`: {e}; got: {line}"),
                ProviderErrorKind::Protocol,
            )
        })
    }
}

impl Provider for CommandProvider {
    fn supervises_lost_turns(&self) -> bool {
        true
    }

    fn reset_telemetry(&self) {
        self.spawner.reset();
    }

    fn spawned_processes(&self) -> Vec<SpawnedProcess> {
        self.spawner.records()
    }

    fn respond(
        &self,
        skill: &SkillRef<'_>,
        messages: &[Message],
        session: Option<&str>,
    ) -> Result<AssistantTurn> {
        let request = Request::Respond(RespondFrame {
            skill: SkillPayload {
                name: skill.name,
                path: skill.dir,
                instructions: skill.instructions,
            },
            messages,
            session,
        });
        let payload: RespondPayload = self.call(&request, "respond")?;
        Ok(AssistantTurn {
            message: payload.message,
            done: payload.done,
            usage: payload.usage,
            events: payload.events.unwrap_or_default(),
        })
    }

    fn judge_with_evidence(
        &self,
        query: &JudgeQuery<'_>,
        messages: &[Message],
        evidence: EvidenceContext<'_>,
    ) -> Result<JudgeVerdict> {
        let evidence = (evidence.worktree.is_some()
            || !evidence.history_files.is_empty()
            || !evidence.artifacts.is_empty())
        .then(|| EvidencePayload {
            worktree: evidence.worktree,
            history_files: evidence.history_files,
            artifacts: evidence.resolved_artifacts(),
        });
        let payload: JudgePayload = self.call(
            &Request::Judge(JudgeFrame::of(query, messages, evidence)?),
            "judge",
        )?;
        match (query.kind, payload.value) {
            (JudgeKind::Boolean, JudgeValue::Number(_))
            | (JudgeKind::Numeric, JudgeValue::Bool(_)) => Err(Error::provider_classified(
                "judge",
                "verdict value has the wrong type",
                ProviderErrorKind::Protocol,
            )),
            _ => Ok(JudgeVerdict {
                value: payload.value,
                reason: payload.reason,
                usage: payload.usage,
            }),
        }
    }

    fn simulate_user(
        &self,
        persona: &str,
        messages: &[Message],
        session: Option<&str>,
    ) -> Result<UserTurn> {
        let request = Request::User(UserFrame {
            persona,
            messages,
            session,
        });
        let payload: UserPayload = self.call(&request, "user")?;
        Ok(UserTurn {
            message: payload.message,
            stop: payload.stop,
            usage: payload.usage,
        })
    }

    fn supervise(
        &self,
        query: &SupervisorQuery<'_>,
        messages: &[Message],
        session: Option<&str>,
    ) -> Result<SupervisorTurn> {
        // The same bounded re-ask, and the same settle on exhaustion, as the
        // prompt-building seam: the protocol carries no correction field, so the
        // re-ask is the identical request rather than a nudged one.
        supervise_with_reask(|_ask| {
            let payload: SupervisorPayload = self.call(
                &Request::Supervisor(SupervisorFrame {
                    task: query.task,
                    persona: query.persona,
                    done_when: query.done_when,
                    worktree: query.worktree,
                    history_name: query.history_name,
                    notes: query.notes,
                    turn: &query.turn,
                    messages,
                    session,
                }),
                "supervisor",
            )?;
            let outcome = if payload.completion {
                if payload.reason.trim().is_empty() || payload.message.is_some() {
                    return Err(Error::provider_classified(
                        "supervisor",
                        "completed response requires non-empty `reason` and forbids `message`",
                        ProviderErrorKind::Protocol,
                    ));
                }
                SupervisorOutcome::Completed {
                    reason: payload.reason,
                }
            } else {
                // Never an error, and never a blank next user turn: see
                // `parse_supervisor`, which this mirrors on the other seam.
                match payload.message.filter(|m| !m.trim().is_empty()) {
                    Some(message) => SupervisorOutcome::Continue {
                        message,
                        reason: payload.reason,
                    },
                    None => SupervisorOutcome::NoInstruction {
                        reason: payload.reason,
                    },
                }
            };
            Ok(SupervisorTurn {
                outcome,
                usage: payload.usage,
            })
        })
    }

    fn judge(&self, query: &JudgeQuery<'_>, messages: &[Message]) -> Result<JudgeVerdict> {
        let request = Request::Judge(JudgeFrame::of(query, messages, None)?);
        let payload: JudgePayload = self.call(&request, "judge")?;
        // A command speaking the protocol returns a typed value directly, so no
        // tolerant text parsing is needed here — but still type-check the kind.
        match (query.kind, payload.value) {
            (JudgeKind::Boolean, JudgeValue::Number(_)) => {
                return Err(Error::provider_classified(
                    "judge",
                    "expected a boolean verdict value, got a number",
                    ProviderErrorKind::Protocol,
                ))
            }
            (JudgeKind::Numeric, JudgeValue::Bool(_)) => {
                return Err(Error::provider_classified(
                    "judge",
                    "expected a numeric verdict value, got a boolean",
                    ProviderErrorKind::Protocol,
                ))
            }
            _ => {}
        }
        Ok(JudgeVerdict {
            value: payload.value,
            reason: payload.reason,
            usage: payload.usage,
        })
    }

    fn assess(&self, prompt: &str, messages: &[Message]) -> Result<Assessment> {
        let payload: AssessmentPayload =
            self.call(&Request::Assess(AssessFrame { prompt, messages }), "assess")?;
        if payload.text.trim().is_empty() {
            return Err(Error::provider_classified(
                "assess",
                "assessment response contained empty text",
                ProviderErrorKind::Protocol,
            ));
        }
        Ok(Assessment {
            text: payload.text,
            usage: payload.usage,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_argv_is_rejected() {
        let err = CommandProvider::new(vec![]).unwrap_err();
        assert!(matches!(err, Error::Invalid(_)));
    }

    #[test]
    fn request_serializes_with_op_tag_and_no_platform_or_model() {
        let req = Request::Judge(JudgeFrame::Numeric {
            criterion: "polite",
            min: 0.0,
            max: 10.0,
            messages: &[],
            evidence: None,
        });
        let json = serde_json::to_string(&req).unwrap();
        // Byte for byte what the protocol has always written: the op, then the kind,
        // then the members in declaration order.
        assert_eq!(
            json,
            r#"{"op":"judge","kind":"numeric","criterion":"polite","min":0.0,"max":10.0,"messages":[]}"#
        );
        // Protocol v2: no harness/model selection on the wire.
        assert!(!json.contains("platform"));
        assert!(!json.contains("model"));
    }

    #[test]
    fn protocol_v6_judge_evidence_is_additive_and_omitted_without_context() {
        let histories = vec!["/state/history/agent.jsonl".to_string()];
        let with = Request::Judge(JudgeFrame::Boolean {
            criterion: "done",
            messages: &[],
            evidence: Some(EvidencePayload {
                worktree: Some("/repo"),
                history_files: &histories,
                artifacts: Vec::new(),
            }),
        });
        let json = serde_json::to_string(&with).unwrap();
        assert!(json.contains("\"evidence\":{\"worktree\":\"/repo\",\"history_files\":[\"/state/history/agent.jsonl\"]}"));
        let without = Request::Judge(JudgeFrame::Boolean {
            criterion: "done",
            messages: &[],
            evidence: None,
        });
        assert_eq!(
            serde_json::to_string(&without).unwrap(),
            r#"{"op":"judge","kind":"boolean","criterion":"done","messages":[]}"#
        );
        let docs = include_str!("../../../docs/protocol.md");
        assert!(docs.contains("**v6** additively"));
        assert!(docs.contains("\"evidence\": { \"worktree\": \"/repo\", \"history_files\""));
    }

    #[test]
    fn protocol_v7_judge_evidence_carries_resolved_artifacts_only_when_named() {
        let named = vec!["/abs/design.md".to_string(), ".plans".to_string()];
        let context = EvidenceContext {
            worktree: Some("/repo"),
            history_files: &[],
            artifacts: &named,
        };
        let with = Request::Judge(JudgeFrame::Boolean {
            criterion: "done",
            messages: &[],
            evidence: Some(EvidencePayload {
                worktree: context.worktree,
                history_files: context.history_files,
                artifacts: context.resolved_artifacts(),
            }),
        });
        let resolved = std::path::Path::new("/repo")
            .join(".plans")
            .display()
            .to_string();
        let value = serde_json::to_value(&with).unwrap();
        assert_eq!(
            value["evidence"]["artifacts"],
            serde_json::json!(["/abs/design.md", resolved])
        );
        let docs = include_str!("../../../docs/protocol.md");
        assert!(docs.contains("**v8** (current)"));
        assert!(docs.contains("**v7** added `artifacts`"));
        // The documented request is the wire shape, member for member.
        let snippet = docs
            .lines()
            .find(|line| line.contains("\"artifacts\""))
            .expect("protocol.md shows a judge request carrying artifacts");
        let documented: serde_json::Value = serde_json::from_str(snippet).unwrap();
        // The member set, not the map's iteration order: that follows how
        // `serde_json` was built, not what the doc says.
        let mut members: Vec<&String> =
            documented["evidence"].as_object().unwrap().keys().collect();
        members.sort();
        assert_eq!(members, ["artifacts", "history_files", "worktree"]);
    }

    #[test]
    fn respond_request_omits_absent_session() {
        let req = Request::Respond(RespondFrame {
            skill: SkillPayload {
                name: "s",
                path: "/s",
                instructions: "do x",
            },
            messages: &[],
            session: None,
        });
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("session"));
        assert!(!json.contains("platform"));
        assert!(!json.contains("model"));
    }

    #[test]
    fn a_numeric_query_without_a_scale_is_refused_before_anything_is_spawned() {
        // The program does not exist, so reaching the spawn would be a `Spawn` error:
        // the refusal is the frame's, and nothing ran.
        let provider =
            CommandProvider::new(vec!["definitely-not-a-real-binary-xyz".into()]).unwrap();
        let err = provider
            .judge(
                &JudgeQuery {
                    kind: JudgeKind::Numeric,
                    criterion: "quality",
                    scale: None,
                },
                &[],
            )
            .unwrap_err();
        assert!(matches!(&err, Error::Invalid(why) if why.contains("`min` and `max`")));
        assert!(provider.spawned_processes().is_empty());
    }

    #[test]
    fn spawn_failure_is_classified() {
        let provider =
            CommandProvider::new(vec!["definitely-not-a-real-binary-xyz".into()]).unwrap();
        let err = provider
            .judge(
                &JudgeQuery {
                    kind: JudgeKind::Boolean,
                    criterion: "x",
                    scale: None,
                },
                &[],
            )
            .unwrap_err();
        assert_eq!(err.kind(), Some(ProviderErrorKind::Spawn));
    }
}
