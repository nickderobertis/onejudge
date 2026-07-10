//! [`CommandProvider`]: a [`Provider`] backed by an external command speaking a
//! small JSON-lines protocol (one request object in on stdin, one response object
//! out on stdout, per op). It backs the deterministic test doubles the e2e suite
//! drives and any custom provider a consumer writes. The wire contract is
//! documented in `docs/protocol.md`.

use std::io::Write as _;
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

use crate::error::{Error, ProviderErrorKind, Result};
use crate::provider::{
    AssistantTurn, JudgeKind, JudgeQuery, JudgeValue, JudgeVerdict, Provider, SkillRef, UserTurn,
};
use crate::transcript::{Message, ToolEvent};
use crate::usage::Usage;

// --- Wire types (the JSON-lines protocol) ---------------------------------

#[derive(Serialize)]
struct SkillPayload<'a> {
    name: &'a str,
    path: &'a str,
    instructions: &'a str,
}

#[derive(Serialize)]
#[serde(tag = "op", rename_all = "lowercase")]
enum Request<'a> {
    Respond {
        platform: &'a str,
        model: &'a str,
        skill: SkillPayload<'a>,
        messages: &'a [Message],
        #[serde(skip_serializing_if = "Option::is_none")]
        session: Option<&'a str>,
    },
    User {
        model: &'a str,
        persona: &'a str,
        messages: &'a [Message],
        #[serde(skip_serializing_if = "Option::is_none")]
        session: Option<&'a str>,
    },
    Judge {
        model: &'a str,
        kind: &'a str,
        criterion: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        min: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        max: Option<f64>,
        messages: &'a [Message],
    },
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
struct JudgePayload {
    value: JudgeValue,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    usage: Option<Usage>,
}

// --- CommandProvider ------------------------------------------------------

/// A [`Provider`] backed by an external command speaking the JSON-lines protocol.
#[derive(Debug, Clone)]
pub struct CommandProvider {
    argv: Vec<String>,
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
        Ok(Self { argv })
    }

    /// Send one request and parse the single response object from stdout.
    fn call<T: for<'de> Deserialize<'de>>(&self, request: &Request<'_>, op: &str) -> Result<T> {
        let payload = serde_json::to_vec(request).map_err(|e| {
            Error::provider(op.to_string(), format!("could not encode request: {e}"))
        })?;

        let mut child = Command::new(&self.argv[0])
            .args(&self.argv[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                Error::provider_classified(
                    op.to_string(),
                    format!(
                        "could not run provider `{}`: {e}. Is it installed and on PATH?",
                        self.argv[0]
                    ),
                    ProviderErrorKind::Spawn,
                )
            })?;

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
    fn respond(
        &self,
        platform: &str,
        model: &str,
        skill: &SkillRef<'_>,
        messages: &[Message],
        session: Option<&str>,
    ) -> Result<AssistantTurn> {
        let request = Request::Respond {
            platform,
            model,
            skill: SkillPayload {
                name: skill.name,
                path: skill.dir,
                instructions: skill.instructions,
            },
            messages,
            session,
        };
        let payload: RespondPayload = self.call(&request, "respond")?;
        Ok(AssistantTurn {
            message: payload.message,
            done: payload.done,
            usage: payload.usage,
            events: payload.events.unwrap_or_default(),
        })
    }

    fn simulate_user(
        &self,
        model: &str,
        persona: &str,
        messages: &[Message],
        session: Option<&str>,
    ) -> Result<UserTurn> {
        let request = Request::User {
            model,
            persona,
            messages,
            session,
        };
        let payload: UserPayload = self.call(&request, "user")?;
        Ok(UserTurn {
            message: payload.message,
            stop: payload.stop,
            usage: payload.usage,
        })
    }

    fn judge(
        &self,
        model: &str,
        query: &JudgeQuery<'_>,
        messages: &[Message],
    ) -> Result<JudgeVerdict> {
        let (min, max) = match query.scale {
            Some((lo, hi)) => (Some(lo), Some(hi)),
            None => (None, None),
        };
        let request = Request::Judge {
            model,
            kind: query.kind.as_str(),
            criterion: query.criterion,
            min,
            max,
            messages,
        };
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
    fn request_serializes_with_op_tag() {
        let req = Request::Judge {
            model: "m",
            kind: "numeric",
            criterion: "polite",
            min: Some(0.0),
            max: Some(10.0),
            messages: &[],
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"op\":\"judge\""));
        assert!(json.contains("\"kind\":\"numeric\""));
    }

    #[test]
    fn respond_request_omits_absent_session() {
        let req = Request::Respond {
            platform: "claude-code",
            model: "m",
            skill: SkillPayload {
                name: "s",
                path: "/s",
                instructions: "do x",
            },
            messages: &[],
            session: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("session"));
    }

    #[test]
    fn spawn_failure_is_classified() {
        let provider =
            CommandProvider::new(vec!["definitely-not-a-real-binary-xyz".into()]).unwrap();
        let err = provider
            .judge(
                "m",
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
