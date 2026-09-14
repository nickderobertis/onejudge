//! Drift gate for onejudge's versioned [`Report`] contract. It builds a canonical
//! report through the public API, serializes it, and diffs the result against a
//! checked-in example and generated JSON Schema goldens. Any change to the
//! wire form — a renamed field, a new key, a changed default — fails here, so it
//! must be a deliberate edit that also bumps `SCHEMA_VERSION`, never a silent
//! break for the SDKs that compose over this contract.

use onejudge::{
    CandidateAttempt, ControlAddress, ControlOutcome, Decision, FellThrough, HarnessAttribution,
    JudgeDecision, JudgeKind, JudgeValue, JudgeVerdict, JudgedTurn, Message, NamedVerdict,
    PartyTelemetry, Report, SessionLink, SpawnedProcess, Telemetry, TelemetryRole, ToolEvent,
    Transcript, Usage, SCHEMA_VERSION,
};

/// The canonical report the golden is generated from: one tool-using assistant
/// turn, one boolean verdict, and usage — exercising every embedded contract type.
fn canonical_report() -> Report {
    let mut transcript = Transcript::from_input("commit the fix");
    transcript.push(
        Message::assistant("Committed.").with_events(vec![ToolEvent {
            kind: "tool_call".into(),
            name: Some("bash".into()),
            input: Some(serde_json::json!({"command": "git commit -m fix"})),
            output: None,
            index: 0,
            // The v10 addition: the harness's own call identity, on the wire so a
            // consumer can join a call to its result. Optional and omitted when the
            // harness exposed none — pinned by `transcript::tests`.
            tool_call_id: Some("toolu_01A".into()),
        }]),
    );
    let mut report = Report::new(
        transcript,
        vec![NamedVerdict::new(
            "the change was committed",
            JudgeKind::Boolean,
            JudgeVerdict {
                value: JudgeValue::Bool(true),
                reason: "a git commit ran".into(),
                usage: None,
            },
        )],
        Some(Usage {
            input_tokens: Some(12),
            output_tokens: Some(3),
            cache_read_tokens: Some(9),
            cache_write_tokens: Some(4),
            cost_usd: None,
        }),
        false,
    )
    .with_assessment("No follow-up work remains.");
    // A completed run, so `settled_reason` stays absent: the two are mutually
    // exclusive by construction (the engine writes one *or* the other), and a
    // golden carrying both would pin a document no run can produce. The settled
    // half is pinned by `report::tests::a_settled_run_round_trips_its_reason` and
    // by the schema golden below, which lists the key either way.
    report.completion_reason = Some("the commit completed the task".into());
    report.telemetry = Some(Telemetry {
        wall_ms: 40,
        agent: PartyTelemetry {
            model_ms: Some(20),
            tool_ms: Some(5),
            time_to_first_token_ms: Some(3),
            usage: Some(Usage {
                input_tokens: Some(8),
                output_tokens: Some(2),
                cache_read_tokens: Some(4),
                cache_write_tokens: Some(1),
                cost_usd: Some(0.01),
            }),
            session_ids: vec!["native-agent-1".into()],
        },
        judge: PartyTelemetry {
            model_ms: Some(10),
            tool_ms: Some(0),
            time_to_first_token_ms: None,
            usage: None,
            session_ids: vec![],
        },
        orchestration_ms: 5,
        sessions: vec![SessionLink {
            session_id: "native-agent-1".into(),
            role: TelemetryRole::Agent,
            turn_index: 1,
            started_at: "2026-01-01T00:00:00Z".into(),
            finished_at: Some("2026-01-01T00:00:00.025Z".into()),
            history_id: Some("019b76e0-history".into()),
            // An agent-side record: no judge of a panel made it, so no label.
            judge: None,
        }],
        attribution: vec![HarnessAttribution {
            role: TelemetryRole::Agent,
            turn_index: 1,
            ran: Some("claude-code".into()),
            fell_through: vec![FellThrough {
                harness: "codex".into(),
                reason: "quota".into(),
            }],
            candidates: vec![
                CandidateAttempt {
                    harness: "codex".into(),
                    harness_id: "codex:work".into(),
                    variant: Some("work".into()),
                    model: Some("gpt-5.5".into()),
                    status: "nonzero".into(),
                    available: true,
                    ran: false,
                    failure_kind: Some("quota".into()),
                    failure_kind_source: Some("stderr".into()),
                    exit_code: Some(1),
                    duration_ms: Some(4),
                    error: Some("out of credit".into()),
                    session_id: None,
                    history_id: Some("019b76e0-codex".into()),
                    usage: None,
                },
                CandidateAttempt {
                    harness: "claude-code".into(),
                    harness_id: "claude-code".into(),
                    variant: None,
                    model: None,
                    status: "ok".into(),
                    available: true,
                    ran: true,
                    failure_kind: None,
                    failure_kind_source: None,
                    exit_code: Some(0),
                    duration_ms: Some(25),
                    error: None,
                    session_id: Some("native-agent-1".into()),
                    history_id: Some("019b76e0-history".into()),
                    usage: Some(Usage {
                        input_tokens: Some(8),
                        output_tokens: Some(2),
                        cache_read_tokens: Some(4),
                        cache_write_tokens: Some(1),
                        cost_usd: Some(0.01),
                    }),
                },
            ],
            history_file: Some("/state/oneharness/history/run-1-skill.jsonl".into()),
            judge: None,
        }],
    });
    // The v12 addition: one entry per supervisor turn, each judge of the panel
    // attributed by label and kind with the wire token of what it decided. Both
    // `done`, because this is a completed run: a judge that continued or failed on
    // its only turn is a document no completed run produces. The other tokens —
    // `error` included, which is how a judge that could not run is kept from
    // reading as a pass — are pinned by the schema golden's enum and by
    // `report::tests`.
    report.judge_decisions = vec![JudgedTurn {
        turn: 1,
        decisions: vec![
            JudgeDecision {
                judge: "reviewer".into(),
                kind: "oneharness".into(),
                decision: Decision::Done,
                reason: "a git commit ran".into(),
            },
            JudgeDecision {
                judge: "command".into(),
                kind: "command".into(),
                decision: Decision::Done,
                reason: "the commit script passed".into(),
            },
        ],
    }];
    // The address a supervisor interrupts this run's agent turn at — exactly the
    // three values `oneharness interrupt` takes, and nothing else.
    report = report.with_control(&ControlOutcome::Open(ControlAddress {
        session: "run-1-skill".into(),
        session_dir: "/state/oneharness/sessions".into(),
        cwd: "/work/repo".into(),
    }));
    // The v11 addition, and the whole reason it is a second pair rather than a
    // second meaning for the first: the supervisor's turn is a different session on
    // a different socket, so this document carries an agent address that is open
    // and a judge ask that was refused — the two states one field could not hold.
    report = report.with_supervisor_control(&ControlOutcome::Unavailable(
        "harness `qwen` has no out-of-band turn control".into(),
    ));
    // Both halves of the grouping contract: a process an embedder's spawn hook
    // claimed, and one it did not — the latter serialized WITHOUT a `group`, so a
    // consumer can never read a group onejudge did not observe.
    report.processes = vec![
        SpawnedProcess {
            role: TelemetryRole::Agent,
            op: "respond".into(),
            program: "oneharness".into(),
            pid: 4242,
            group: Some("job:run-1".into()),
            judge: None,
        },
        // …and the v12 half of the same discipline for the judge label: the
        // judge-side process carries the label of the panel judge that spawned
        // it, and the agent-side one carries none.
        SpawnedProcess {
            role: TelemetryRole::Judge,
            op: "judge".into(),
            program: "oneharness".into(),
            pid: 4243,
            group: None,
            judge: Some("reviewer".into()),
        },
    ];
    report
}

const EXAMPLE_GOLDEN: &str = include_str!("golden/report.example-v12.json");
#[cfg(feature = "sdk-schema")]
const SCHEMA_GOLDEN: &str = include_str!("golden/report.schema-v12.json");

#[test]
fn report_matches_the_golden_example_v12() {
    assert_eq!(SCHEMA_VERSION, 12, "golden is for schema v12");
    let actual = serde_json::to_string_pretty(&canonical_report()).unwrap();
    assert_eq!(
        actual.trim(),
        EXAMPLE_GOLDEN.trim(),
        "the Report wire form changed. If this is intentional, bump SCHEMA_VERSION \
         and update the v12 contract goldens. Actual serialization:\n{actual}"
    );
}

#[test]
fn golden_deserializes_back_to_the_canonical_report() {
    let back: Report = serde_json::from_str(EXAMPLE_GOLDEN).unwrap();
    assert_eq!(back, canonical_report());
}

/// Every `"schema_version": N` the contract doc spells out, in order.
fn documented_schema_versions() -> Vec<u32> {
    include_str!("../../../docs/contract.md")
        .lines()
        .filter_map(|line| line.trim().strip_prefix("\"schema_version\":"))
        .filter_map(|rest| {
            rest.trim_start()
                .trim_end_matches(',')
                .split_whitespace()
                .next()?
                .trim_end_matches(',')
                .parse()
                .ok()
        })
        .collect()
}

#[test]
fn the_contract_doc_states_the_version_this_build_stamps() {
    // `docs/contract.md` hand-copies the version into every example it shows, and
    // an ungated copy drifts: the `FailureReport` example sat at 7 for three bumps
    // because nothing reconciled it with what `FailureReport::new` actually stamps.
    let documented = documented_schema_versions();
    assert!(!documented.is_empty(), "the doc shows no versioned example");
    for version in &documented {
        assert_eq!(
            *version, SCHEMA_VERSION,
            "docs/contract.md shows schema_version {version}, but this build \
             stamps {SCHEMA_VERSION}; every example in that doc is the same \
             contract and must say so"
        );
    }
}

#[cfg(feature = "sdk-schema")]
#[test]
fn generated_report_schema_matches_the_schema_v12_golden() {
    let actual = serde_json::to_value(onejudge::sdk_schema::bundle().report).unwrap();
    let golden: serde_json::Value = serde_json::from_str(SCHEMA_GOLDEN).unwrap();
    assert_eq!(
        actual, golden,
        "the generated Report schema changed. If the wire contract changed, bump \
         SCHEMA_VERSION and update the versioned schema golden"
    );
}

/// The body of the one fenced block in `docs/contract.md` opened with `info`.
fn fenced(info: &str) -> &'static str {
    let doc = include_str!("../../../docs/contract.md");
    let opening = format!("```{info}\n");
    let mut blocks = doc.match_indices(&opening);
    let (start, _) = blocks
        .next()
        .unwrap_or_else(|| panic!("docs/contract.md has no ```{info} block"));
    assert!(
        blocks.next().is_none(),
        "docs/contract.md has more than one ```{info} block"
    );
    let body = &doc[start + opening.len()..];
    &body[..body.find("```").expect("the block is closed")]
}

#[test]
fn the_note_shape_the_contract_doc_copies_reads_through_the_re_exports_unchanged() {
    let copy = fenced("json");
    let read: onejudge::note::DeliveredNote =
        serde_json::from_str(copy).expect("the copy is a note the declaration reads");
    // Read through onejudge's path, held as the profile's own type: one declaration.
    let declared: onemessagebus_agent::note::DeliveredNote = read;
    assert_eq!(
        serde_json::to_value(&declared).unwrap(),
        serde_json::from_str::<serde_json::Value>(copy).unwrap(),
        "the note shape docs/contract.md copies is not the one onemessagebus-agent declares"
    );
    assert_eq!(declared.note.addressee, onejudge::Addressee::Worker);
    assert!(declared.note.binds());
}

#[test]
fn every_note_item_onejudge_exported_at_0_8_1_is_the_agent_profiles_at_the_same_path() {
    use onejudge::note as here;
    use onemessagebus_agent::note as there;

    // Each coercion compiles only when both paths name one type.
    fn same<T>(value: T) -> T {
        value
    }
    let _: fn(here::Addressee) -> there::Addressee = same;
    let _: fn(here::Party) -> there::Party = same;
    let _: fn(here::Criterion) -> there::Criterion = same;
    let _: fn(here::CriterionRefused) -> there::CriterionRefused = same;
    let _: fn(here::NoteText) -> there::NoteText = same;
    let _: fn(here::Note) -> there::Note = same;
    let _: fn(here::NoteRefused) -> there::NoteRefused = same;
    let _: fn(here::DeliveredNote) -> there::DeliveredNote = same;
    let _: fn(here::Accepted) -> there::Accepted = same;
    let _: fn(here::Undelivered) -> there::Undelivered = same;
    let _: fn(here::Criteria) -> there::Criteria = same;
    let _: fn(here::Notes) -> there::Notes = same;
    let _: fn(here::NoteInbox) -> there::NoteInbox = same;
    let _: fn(&[here::DeliveredNote]) -> Option<String> = here::supervisor_block;

    // The channel keeps the calls a caller makes on it.
    let (notes, inbox): (here::Notes, here::NoteInbox) = here::Notes::channel();
    let _: fn(&here::Notes, here::Note) -> Result<here::Accepted, onemessagebus::Undelivered> =
        here::Notes::send;
    use here::prelude::*;
    let delivered: Vec<here::DeliveredNote> = inbox.delivered();
    assert!(delivered.is_empty());
    drop(notes);

    // …and the rendering is the profile's, word for word.
    let handed = [here::DeliveredNote {
        note: here::Note::to(here::Addressee::Both, "the ruling applies to both of you"),
        delivered_to: here::Party::Supervisor,
    }];
    assert_eq!(
        here::supervisor_block(&handed),
        there::supervisor_block(&handed)
    );
}

#[test]
fn the_contract_doc_generates_the_bundle_with_the_example_the_crate_declares() {
    assert_eq!(
        fenced("console").trim(),
        "cargo run -q -p onejudge --features sdk-schema --example generate_sdk_schema"
    );
    assert!(include_str!("../Cargo.toml")
        .contains("name = \"generate_sdk_schema\"\nrequired-features = [\"sdk-schema\"]"));
}

#[cfg(feature = "fake-provider")]
#[test]
fn the_contract_doc_builds_a_report_the_way_the_api_does() {
    use onejudge::{CommandProvider, Conversation, Engine, Settings, Skill};

    let example = fenced("rust");
    for call in [
        "engine.run(&conversation)?",
        "engine.judge_boolean(\"the change was committed\", &outcome.transcript)?",
        "outcome.into_report(",
        "onejudge::NamedVerdict::new(\"the change was committed\", onejudge::JudgeKind::Boolean, verdict)",
        "assert_eq!(report.schema_version, onejudge::SCHEMA_VERSION);",
    ] {
        assert!(example.contains(call), "the example no longer makes `{call}`");
    }
    // The same calls, over the echo double.
    let provider =
        CommandProvider::new(vec![env!("CARGO_BIN_EXE_onejudge-echo-provider").into()]).unwrap();
    let engine = Engine::new(&provider, Settings::new());
    let conversation = Conversation::single_turn(
        Skill::new("demo", "/skills/demo", ""),
        "the change was committed",
    );
    let outcome = engine.run(&conversation).unwrap();
    let verdict = engine
        .judge_boolean("the change was committed", &outcome.transcript)
        .unwrap();
    let report = outcome.into_report(vec![NamedVerdict::new(
        "the change was committed",
        JudgeKind::Boolean,
        verdict,
    )]);
    assert_eq!(report.schema_version, SCHEMA_VERSION);
}
