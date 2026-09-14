//! onejudge's command-provider frames, held to the `onejudge` codec the released
//! `onemessagebus-agent` carries.
//!
//! onejudge registers each frame it writes as `agent.onejudge-frame.<op>@7`
//! (`onejudge::sdk_schema::frame_schemas`), generated from the type that writes it.
//! The bus's codec carries its own transcription of the same frames. This suite is
//! the drift gate between the two repositories, from both ends: the declarations
//! are compared shape for shape, and a real frame of every operation — written by
//! `CommandProvider` across the real subprocess boundary — is read through the
//! released codec and back.
#![cfg(feature = "sdk-schema")]

use std::collections::BTreeMap;

use onemessagebus_agent::codec::onejudge as codec;
use serde_json::{Map, Value};

/// A schema reduced to the shape it admits: every `$ref` inlined from `$defs`, each
/// `required` list sorted, and prose (`description`, `title`) and the dialect marker
/// (`$schema`) dropped. The two sides name their types and word their docs
/// independently; neither is anything a frame carries.
fn shape(schema: &Value) -> Value {
    let defs = schema.get("$defs").cloned().unwrap_or(Value::Null);
    reduce(schema, &defs)
}

fn reduce(node: &Value, defs: &Value) -> Value {
    match node {
        Value::Object(map) => {
            if let Some(Value::String(reference)) = map.get("$ref") {
                let name = reference.rsplit('/').next().unwrap_or_default();
                return reduce(&defs[name], defs);
            }
            let mut out = Map::new();
            for (key, value) in map {
                match key.as_str() {
                    "$defs" | "$schema" | "description" | "title" => {}
                    // Member names are the shape; only their schemas are reduced.
                    "properties" => {
                        let members = value
                            .as_object()
                            .into_iter()
                            .flatten()
                            .map(|(name, schema)| (name.clone(), reduce(schema, defs)))
                            .collect();
                        out.insert(key.clone(), Value::Object(members));
                    }
                    "required" => {
                        let mut names: Vec<Value> = value.as_array().cloned().unwrap_or_default();
                        names.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
                        out.insert(key.clone(), Value::Array(names));
                    }
                    _ => {
                        out.insert(key.clone(), reduce(value, defs));
                    }
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(|item| reduce(item, defs)).collect()),
        other => other.clone(),
    }
}

/// Every place `ours` and `theirs` differ, as a JSON pointer and what each side has.
fn differences(ours: &Value, theirs: &Value, at: &str, found: &mut Vec<String>) {
    match (ours, theirs) {
        (Value::Object(a), Value::Object(b)) => {
            let keys: std::collections::BTreeSet<&String> = a.keys().chain(b.keys()).collect();
            for key in keys {
                let here = format!("{at}/{key}");
                match (a.get(key), b.get(key)) {
                    (Some(x), Some(y)) => differences(x, y, &here, found),
                    (Some(x), None) => found.push(format!("{here}: onejudge only: {x}")),
                    (None, Some(y)) => found.push(format!("{here}: codec only: {y}")),
                    (None, None) => {}
                }
            }
        }
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() => {
            for (index, (x, y)) in a.iter().zip(b).enumerate() {
                differences(x, y, &format!("{at}/{index}"), found);
            }
        }
        _ if ours == theirs => {}
        _ => found.push(format!("{at}: onejudge {ours}, codec {theirs}")),
    }
}

fn by_id(schemas: Vec<(onemessagebus::SchemaId, schemars::Schema)>) -> BTreeMap<String, Value> {
    schemas
        .into_iter()
        .map(|(id, schema)| (id.to_string(), schema.to_value()))
        .collect()
}

/// The differences between a frame shape of onejudge's and the released codec's
/// that are known, at `at` (the frame, or one `kind` of a `judge` frame), each
/// asserted rather than fixed — so any other drift fails, and a fix on either side
/// flips this test:
///
/// * `evidence.artifacts` — protocol **v7** added it; the codec is transcribed at
///   v6. Asserted only while it is.
/// * the four below are in the transcript members every frame carries, and on
///   onejudge's side they are the Report contract's own `Message` / `ToolEvent`
///   (report schema v12), the one source of that shape. The codec's transcription
///   narrowed them: (a) a message admits no other members, (b) nor does a tool
///   event, (c) a tool event's `index` is a `uint64` rather than a `uint`, and (d) its
///   `kind` is closed to `tool_call` / `tool_result`. They move with the codec's v7
///   transcription, which is taken from these schemas; whether onejudge's own
///   transcript should close them is a Report schema decision, not this gate's.
fn known_differences(at: &str, judged: bool) -> Vec<String> {
    let message = format!("{at}/properties/messages/items");
    let event = format!("{message}/properties/events/items");
    let mut known = vec![
        format!("{message}/additionalProperties: codec only: false"),
        format!("{event}/additionalProperties: codec only: false"),
        format!("{event}/properties/index/format: onejudge \"uint\", codec \"uint64\""),
        format!(
            "{event}/properties/kind/oneOf: codec only: \
             [{{\"type\":\"string\",\"const\":\"tool_call\"}},{{\"type\":\"string\",\"const\":\"tool_result\"}}]"
        ),
        format!("{event}/properties/kind/type: onejudge only: \"string\""),
    ];
    if judged && codec::PROTOCOL_VERSION < 7 {
        known.push(format!(
            "{at}/properties/evidence/anyOf/0/properties/artifacts: onejudge only: \
             {{\"type\":\"array\",\"items\":{{\"type\":\"string\"}}}}"
        ));
    }
    known
}

#[test]
fn every_frame_schema_differs_from_the_released_codec_only_where_it_is_known_to() {
    let ours = by_id(onejudge::sdk_schema::frame_schemas());
    let theirs = by_id(codec::schemas());
    assert_eq!(
        ours.keys().collect::<Vec<_>>(),
        [
            "agent.onejudge-frame.assess@7",
            "agent.onejudge-frame.judge@7",
            "agent.onejudge-frame.respond@7",
            "agent.onejudge-frame.supervisor@7",
            "agent.onejudge-frame.user@7",
        ],
        "onejudge registers exactly one frame schema per operation, at the protocol version"
    );
    for op in codec::op::ALL {
        let mine = &ours[&format!("agent.onejudge-frame.{op}@7")];
        let released = &theirs[&format!("agent.onejudge-frame.{op}@{}", codec::PROTOCOL_VERSION)];
        let mut found = Vec::new();
        differences(&shape(mine), &shape(released), "", &mut found);
        let mut expected = if op == codec::op::JUDGE {
            // One document per `kind`: boolean, then numeric.
            let mut both = known_differences("/oneOf/0", true);
            both.extend(known_differences("/oneOf/1", true));
            both
        } else {
            known_differences("", false)
        };
        found.sort();
        expected.sort();
        assert_eq!(
            found, expected,
            "`{op}`'s frame schema drifted from the released codec's beyond the known differences"
        );
    }
}

#[cfg(feature = "fake-provider")]
mod through_the_released_codec {
    use std::path::{Path, PathBuf};

    use onejudge::{
        Addressee, CommandProvider, DeliveredNote, EvidenceContext, JudgeKind, JudgeQuery, Message,
        Note, Party, Provider, SkillRef, SupervisorQuery, ToolEvent,
    };
    use onemessagebus::{Config, Layouts, QueueConfig, ServeError, ServeOptions, TransportKinds};
    use onemessagebus_agent::codec::onejudge as codec;
    use serde_json::Value;

    /// A fresh path under the integration-test tmp dir.
    fn scratch(name: &str) -> PathBuf {
        let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir_all(&path);
        path
    }

    /// The echo double, recording every frame it is handed to `log`.
    fn recording(log: &Path) -> CommandProvider {
        CommandProvider::new(vec![
            env!("CARGO_BIN_EXE_onejudge-echo-provider").to_string(),
            format!("[[record:{}]]", log.display()),
        ])
        .unwrap()
    }

    /// The frames the double received, one line each, exactly as written.
    fn frames(log: &Path) -> Vec<String> {
        std::fs::read_to_string(log)
            .unwrap()
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(str::to_string)
            .collect()
    }

    /// A transcript carrying every member a frame's `messages` can: both roles, and
    /// a tool call with arguments, output and the harness's own call id.
    fn transcript() -> Vec<Message> {
        vec![
            Message::user("ship the change"),
            Message::assistant("Committed.").with_events(vec![ToolEvent {
                kind: "tool_call".into(),
                name: Some("bash".into()),
                input: Some(serde_json::json!({"command": "git commit -m fix"})),
                output: Some("[main 1a2b3c] fix".into()),
                index: 0,
                tool_call_id: Some("toolu_01A".into()),
            }]),
        ]
    }

    /// A bus with one queue to serve on, kept under `dir`.
    fn bus(dir: &Path) -> onemessagebus::Bus {
        let mut config = Config::local(dir, None);
        config
            .queues
            .insert("frames".parse().unwrap(), QueueConfig::default());
        config
            .resolve(&Layouts::new(), &TransportKinds::builtin())
            .unwrap()
    }

    /// Serve one frame line through the released `onejudge` codec, and answer how
    /// the session ended.
    fn serve(dir: &Path, frame: &str) -> Result<Vec<u8>, ServeError> {
        let bus = bus(dir);
        let mut written = Vec::new();
        bus.serve(
            &"frames".parse().unwrap(),
            &mut codec::Onejudge::new(),
            &ServeOptions::default(),
            Box::new(std::io::Cursor::new(format!("{frame}\n"))),
            &mut written,
        )
        .map(|_| written)
    }

    #[test]
    fn a_frame_of_every_operation_reads_through_the_released_codec_and_back_unchanged() {
        let log = scratch("frames-every-op.log");
        let provider = recording(&log);
        let messages = transcript();
        let delivered = [DeliveredNote {
            note: Note::to(Addressee::Worker, "the reviewer asked for a smaller diff")
                .binding("the diff touches only the migration")
                .unwrap(),
            delivered_to: Party::Worker,
        }];
        let histories = ["/state/history/agent.jsonl".to_string()];

        provider
            .respond(
                &SkillRef {
                    name: "demo",
                    dir: "/skills/demo",
                    instructions: "do the work",
                },
                &messages,
                Some("frames-skill"),
            )
            .unwrap();
        provider
            .simulate_user("A hurried reviewer.", &messages, Some("frames-user"))
            .unwrap();
        provider
            .supervise(
                &SupervisorQuery {
                    task: "ship the change",
                    persona: "A strict reviewer.",
                    done_when: Some("the change is shipped"),
                    worktree: "/repo",
                    history_name: "frames-skill",
                    notes: &delivered,
                },
                &messages,
                Some("frames-user"),
            )
            .unwrap();
        let evidence = EvidenceContext {
            worktree: Some("/repo"),
            history_files: &histories,
            artifacts: &[],
        };
        provider
            .judge_with_evidence(
                &JudgeQuery {
                    kind: JudgeKind::Boolean,
                    criterion: "a git commit ran",
                    scale: None,
                },
                &messages,
                evidence,
            )
            .unwrap();
        provider
            .judge(
                &JudgeQuery {
                    kind: JudgeKind::Numeric,
                    criterion: "how small the diff is",
                    scale: Some((0.0, 10.0)),
                },
                &messages,
            )
            .unwrap();
        provider
            .assess("Identify useful follow-up work.", &messages)
            .unwrap();

        let written = frames(&log);
        let ops: Vec<String> = written
            .iter()
            .map(|line| serde_json::from_str::<Value>(line).unwrap()["op"].to_string())
            .collect();
        assert_eq!(
            ops,
            [
                "\"respond\"",
                "\"user\"",
                "\"supervisor\"",
                "\"judge\"",
                "\"judge\"",
                "\"assess\""
            ],
            "the double did not receive one frame per call"
        );
        for line in &written {
            let frame: codec::Frame = serde_json::from_str(line).unwrap_or_else(|e| {
                panic!("the released codec refused a frame onejudge wrote: {e}\n{line}")
            });
            let put_in: Value = serde_json::from_str(line).unwrap();
            assert_eq!(
                serde_json::to_value(&frame).unwrap(),
                put_in,
                "the frame read out of the released codec is not the frame onejudge wrote"
            );
        }
    }

    #[test]
    fn a_frame_the_released_codec_refuses_is_read_as_the_refusal_the_protocol_documents() {
        let log = scratch("frames-refused.log");
        let provider = recording(&log);
        let messages = transcript();
        let named = ["/repo/.plans/design.md".to_string()];
        provider
            .judge_with_evidence(
                &JudgeQuery {
                    kind: JudgeKind::Boolean,
                    criterion: "the design is sound",
                    scale: None,
                },
                &messages,
                EvidenceContext {
                    worktree: Some("/repo"),
                    history_files: &[],
                    artifacts: &named,
                },
            )
            .unwrap();
        provider
            .respond(
                &SkillRef {
                    name: "demo",
                    dir: "/skills/demo",
                    instructions: "do the work",
                },
                &messages,
                None,
            )
            .unwrap();
        let written = frames(&log);
        let (judge, respond) = (&written[0], &written[1]);
        let protocol = include_str!("../../../docs/protocol.md");

        // A v7 judge frame naming artifacts is not a v6 frame: the codec's
        // declaration refuses the member, and so does the codec serving it.
        let declared = serde_json::from_str::<codec::Frame>(judge)
            .map(|_| ())
            .unwrap_err();
        assert!(
            declared.to_string().contains("unknown field `artifacts`"),
            "{declared}"
        );
        let dir = scratch("frames-refused-bus");
        match serve(&dir, judge) {
            Err(ServeError::Refused(why)) => {
                assert!(
                    why.contains("the `judge` frame is not a onejudge protocol v6 frame")
                        && why.contains("unknown field `artifacts`"),
                    "{why}"
                );
            }
            other => panic!("the codec served a frame it cannot read: {other:?}"),
        }
        assert!(
            protocol.contains("the `judge` frame is not a onejudge protocol v6 frame"),
            "docs/protocol.md does not document the refusal a v7 judge frame meets"
        );

        // An operation the codec does not serve is refused by name.
        match serve(&dir, respond) {
            Err(ServeError::Refused(why)) => {
                assert!(
                    why.contains("`respond` is not an operation this codec serves"),
                    "{why}"
                );
            }
            other => panic!("the codec answered an operation it does not serve: {other:?}"),
        }
        assert!(
            protocol.contains("is not an operation this codec serves"),
            "docs/protocol.md does not document the refusal an unserved operation meets"
        );
    }
}
