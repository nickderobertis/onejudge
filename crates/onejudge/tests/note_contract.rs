//! The note contract onejudge declares, held to the bytes `onejudge` 0.8.1 wrote
//! and driven over the bus channel it rides.
//!
//! `tests/golden/onejudge-0.8.1-note.json` is what `onejudge` v0.8.1's own
//! `note.rs`, compiled unchanged, serialized, refused and rendered for the values
//! its note unit tests use. Every value here is built again with `onejudge::note`
//! and compared byte for byte, and every refusal word for word, so a shape that
//! moved between that release, `onemessagebus-agent`, and this crate's own
//! declaration is a shape that changed — and fails here.

use std::time::Duration;

use onejudge::note::prelude::*;
use onejudge::note::{
    supervisor_block, worker_block, Accepted, Addressee, Criteria, Criterion, DeliveredNote, Note,
    NoteInbox, NoteRefused, NoteText, Notes, Party, Undelivered,
};
use onemessagebus::{BackendError, Carry, Closed, Inbox, Spool, Undelivered as InboxUndelivered};
use serde_json::{json, Value};

const GOLDEN: &str = include_str!("golden/onejudge-0.8.1-note.json");

fn golden() -> Value {
    serde_json::from_str(GOLDEN).expect("the golden document is JSON")
}

fn to_json<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).expect("serializes")
}

fn delivered(note: Note, to: Party) -> DeliveredNote {
    DeliveredNote {
        note,
        delivered_to: to,
    }
}

/// A directory only this test case writes, under the target dir cargo gives tests.
fn scratch(case: &str) -> std::path::PathBuf {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("note-{case}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir
}

/// The notes `onejudge`'s role-block test hands a party.
fn mixed() -> Vec<DeliveredNote> {
    vec![
        delivered(Note::to(Addressee::Worker, "observed state"), Party::Worker),
        delivered(
            Note::to(Addressee::Worker, "and this one moved the bar")
                .binding("the migration is covered")
                .expect("binds"),
            Party::Worker,
        ),
        delivered(
            Note::to(Addressee::Supervisor, "hold the bar where it is"),
            Party::Worker,
        ),
        delivered(
            Note::to(Addressee::Both, "the ruling applies to both of you"),
            Party::Worker,
        ),
    ]
}

/// The notes `onejudge`'s criteria test composes.
fn bound() -> Vec<DeliveredNote> {
    vec![
        delivered(
            Note::to(Addressee::Worker, "first")
                .binding("the migration is covered")
                .expect("binds"),
            Party::Worker,
        ),
        delivered(
            Note::to(Addressee::Both, "second")
                .binding("the flag defaults to off")
                .expect("binds"),
            Party::Supervisor,
        ),
        delivered(
            Note::to(Addressee::Worker, "third, binding nothing"),
            Party::Worker,
        ),
    ]
}

fn text(value: &Value) -> &str {
    value.as_str().expect("a string in the golden document")
}

#[test]
fn every_shape_serializes_to_the_bytes_onejudge_wrote() {
    let golden = golden();
    let serialized = &golden["serialized"];
    let built = [
        ("addressee.worker", to_json(&Addressee::Worker)),
        ("addressee.supervisor", to_json(&Addressee::Supervisor)),
        ("addressee.both", to_json(&Addressee::Both)),
        ("party.worker", to_json(&Party::Worker)),
        ("party.supervisor", to_json(&Party::Supervisor)),
        (
            "criterion",
            to_json(
                &Criterion::try_from("  the migration path is covered by a test ")
                    .expect("a criterion"),
            ),
        ),
        (
            "note_text",
            to_json(
                &"  look again at the migration "
                    .parse::<NoteText>()
                    .expect("text"),
            ),
        ),
        (
            "note.plain",
            to_json(
                &Note::new(Addressee::Worker, "  look again at the migration ").expect("a note"),
            ),
        ),
        (
            "note.binding",
            to_json(
                &Note::to(Addressee::Both, "the bar moved")
                    .binding("the flag defaults to off")
                    .expect("binds"),
            ),
        ),
    ];
    for (case, bytes) in built {
        assert_eq!(bytes, text(&serialized[case]), "{case}");
    }
    for (case, notes) in [
        ("delivered_note.bound", bound()),
        ("delivered_note.mixed", mixed()),
    ] {
        let expected: Vec<&str> = serialized[case]
            .as_array()
            .expect("a list")
            .iter()
            .map(text)
            .collect();
        let written: Vec<String> = notes.iter().map(to_json).collect();
        assert_eq!(written, expected, "{case}");
        for (bytes, note) in expected.iter().zip(&notes) {
            assert_eq!(
                &serde_json::from_str::<DeliveredNote>(bytes).expect("reads back"),
                note,
                "{case} did not read back as the note it was"
            );
        }
    }
}

#[test]
fn every_criterion_is_refused_or_accepted_in_onejudges_words() {
    let golden = golden();
    for case in golden["criteria"].as_array().expect("a list") {
        let offered = text(&case["text"]);
        match Criterion::try_from(offered) {
            Ok(criterion) => assert_eq!(
                to_json(&criterion),
                text(&case["accepted"]),
                "{offered:?} was accepted as something else"
            ),
            Err(refused) => {
                assert_eq!(
                    refused.to_string(),
                    case["refused"].as_str().unwrap_or_else(|| panic!(
                        "{offered:?} is refused here and was accepted by onejudge: {refused}"
                    )),
                );
                assert_eq!(refused.why, text(&case["why"]));
                assert_eq!(refused.criterion, offered);
            }
        }
    }
}

#[test]
fn every_note_arriving_over_the_wire_is_read_as_onejudge_read_it() {
    let golden = golden();
    for case in golden["notes"].as_array().expect("a list") {
        let input = text(&case["input"]);
        match serde_json::from_str::<Note>(input) {
            Ok(note) => assert_eq!(to_json(&note), text(&case["accepted"]), "{input}"),
            Err(refused) => assert_eq!(
                refused.to_string(),
                case["refused"].as_str().unwrap_or_else(|| panic!(
                    "{input} is refused here and was read by onejudge: {refused}"
                )),
                "{input}"
            ),
        }
    }
}

#[test]
fn criteria_and_role_blocks_render_what_onejudge_rendered() {
    let golden = golden();
    let rendered = &golden["rendered"];
    let optional = |value: Option<String>| value.map_or(Value::Null, Value::String);
    let built = [
        (
            "criteria.none",
            optional(Criteria::compose(Some("the task is done"), &[]).rendered()),
        ),
        ("criteria.default", optional(Criteria::default().rendered())),
        (
            "criteria.configured",
            optional(Criteria::compose(Some("the task is done"), &bound()).rendered()),
        ),
        (
            "criteria.alone",
            optional(Criteria::compose(None, &bound()).rendered()),
        ),
        ("supervisor_block.empty", optional(supervisor_block(&[]))),
        (
            "supervisor_block.mixed",
            optional(supervisor_block(&mixed())),
        ),
        ("worker_block.empty", Value::String(worker_block(&[]))),
        ("worker_block.mixed", Value::String(worker_block(&mixed()))),
    ];
    for (case, value) in built {
        assert_eq!(value, rendered[case], "{case}");
    }
    assert_eq!(
        Criteria::compose(Some("the task is done"), &bound())
            .bound()
            .len(),
        2
    );
}

#[test]
fn every_refusal_says_what_onejudge_said() {
    let golden = golden();
    let refusals = &golden["refusals"];
    let built = [
        ("note_refused.blank", NoteRefused::Blank.to_string()),
        (
            "note_new.blank",
            Note::new(Addressee::Worker, " \n ")
                .expect_err("blank")
                .to_string(),
        ),
        (
            "note_binding.version",
            Note::to(Addressee::Worker, "look")
                .binding("the pin moves to 1.2.3")
                .expect_err("a version literal")
                .to_string(),
        ),
        (
            "undelivered.conversation_completed",
            Undelivered::ConversationCompleted {
                completion_reason: "the work is done".into(),
            }
            .to_string(),
        ),
        (
            "undelivered.member_settled",
            Undelivered::MemberSettled {
                outcome: "the member was condemned by its heartbeat watchdog".into(),
            }
            .to_string(),
        ),
        (
            "undelivered.no_conversation",
            Undelivered::NoConversation {
                reason: "nothing ever read this channel".into(),
            }
            .to_string(),
        ),
        (
            "debug.criterion",
            format!(
                "{:?}",
                Criterion::try_from("the migration is covered").expect("a criterion")
            ),
        ),
        (
            "debug.note_text",
            format!("{:?}", "look".parse::<NoteText>().expect("text")),
        ),
    ];
    for (case, said) in built {
        assert_eq!(said, text(&refusals[case]), "{case}");
    }
    assert_eq!(
        json!([
            Addressee::Worker.as_str(),
            Addressee::Supervisor.as_str(),
            Addressee::Both.as_str()
        ]),
        refusals["addressee.as_str"]
    );
}

/// The id a note is registered under, and the schema onejudge publishes for it:
/// read through the registry a bus checks arriving records against, so a client in
/// another language validating against `schemas/note.json` is validating against
/// what this engine reads a note with.
#[cfg(feature = "sdk-schema")]
#[test]
fn a_note_is_onejudges_message_and_its_published_schema_is_what_the_engine_reads() {
    // Scoped here rather than at the top: `Message` (which carries `SCHEMA`) and
    // `Registry` are this feature's surface, and the crate builds without it.
    use onemessagebus::{Message, Registry};

    assert_eq!(Note::SCHEMA.to_string(), "agent.note@1");

    let mut registry = Registry::new();
    onejudge::sdk_schema::register_note(&mut registry).expect("the note schema registers");
    registry
        .check(
            &Note::SCHEMA,
            &json!({"addressee": "worker", "text": "look again", "criterion": "the diff is small"}),
        )
        .expect("a note conforms to its registered schema");
    assert!(registry
        .check(&Note::SCHEMA, &json!({"addressee": "manager", "text": "x"}))
        .is_err());

    // The committed bundle is that schema, at bundle version 1, and nothing else.
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("schemas/note.json");
    onejudge::sdk_schema::check_note_bundle(&path).expect("the committed bundle is generated");
    let bundle = onejudge::sdk_schema::note_bundle();
    assert_eq!(bundle.version().to_string(), "1");
    assert_eq!(bundle.schemas().len(), 1);
    assert_eq!(bundle.schemas()[0].id, Note::SCHEMA);

    // …and a hand edit is refused, naming the command that regenerates it.
    let edited = scratch("hand-edited").join("note.json");
    std::fs::write(&edited, "{}\n").expect("writes");
    let failure = onejudge::sdk_schema::check_note_bundle(&edited).expect_err("a hand edit");
    assert!(
        failure.contains("differs from the generated note schema"),
        "{failure}"
    );
    assert!(failure.contains("generate_note_schema"), "{failure}");
}

#[test]
fn a_notes_answers_cross_the_wire_in_the_spelling_the_spool_mirror_carries() {
    for (accepted, wire) in [
        (Accepted::Queued, json!("queued")),
        (
            Accepted::Interrupted {
                party: Party::Supervisor,
            },
            json!({"interrupted": {"party": "supervisor"}}),
        ),
        (
            Accepted::JudgedWith {
                completion_reason: "passed with the note in hand".into(),
            },
            json!({"judged_with": {"completion_reason": "passed with the note in hand"}}),
        ),
    ] {
        assert_eq!(serde_json::to_value(&accepted).expect("serializes"), wire);
        assert_eq!(
            serde_json::from_value::<Accepted>(wire).expect("reads"),
            accepted
        );
    }
    let refusal = Undelivered::MemberSettled {
        outcome: "ended".into(),
    };
    assert_eq!(
        serde_json::to_value(&refusal).expect("serializes"),
        json!({"member_settled": {"outcome": "ended"}})
    );
}

#[test]
fn a_note_channel_answers_its_caller_and_records_which_party_each_note_reached() {
    let (notes, inbox): (Notes, NoteInbox) = Notes::channel();
    let answers = [
        Accepted::Interrupted {
            party: Party::Worker,
        },
        Accepted::Queued,
        Accepted::JudgedWith {
            completion_reason: "done with it in hand".into(),
        },
    ];
    for (index, answer) in answers.iter().enumerate() {
        let sender = notes.clone();
        let sending = std::thread::spawn(move || {
            sender.send(Note::to(Addressee::Worker, format!("note {index}")))
        });
        let taken = inbox
            .take_within(Duration::from_secs(10))
            .expect("the note arrives");
        assert_eq!(taken.message().text.as_str(), format!("note {index}"));
        taken.answer(answer.clone());
        assert_eq!(sending.join().expect("finishes"), Ok(answer.clone()));
    }
    let delivered = inbox.delivered();
    assert_eq!(
        delivered
            .iter()
            .map(|d| (d.note.text.as_str().to_owned(), d.delivered_to))
            .collect::<Vec<_>>(),
        [
            ("note 0".to_owned(), Party::Worker),
            ("note 2".to_owned(), Party::Supervisor),
        ],
        "a queued note has reached no party yet"
    );
    // What a judge is shown is composed from exactly these.
    assert!(supervisor_block(&delivered).is_some());
}

#[test]
fn a_note_refusal_carried_in_a_close_reaches_the_caller_as_the_refusal_it_was() {
    for refusal in [
        Undelivered::ConversationCompleted {
            completion_reason: "its supervisor judged the task complete".into(),
        },
        Undelivered::MemberSettled {
            outcome: "the member was condemned by its heartbeat watchdog".into(),
        },
        Undelivered::NoConversation {
            reason: "nothing ever read this channel".into(),
        },
    ] {
        let (notes, inbox) = Notes::channel();
        inbox.close((&refusal).into());
        let refused: Undelivered = notes
            .send(Note::to(Addressee::Worker, "too late"))
            .expect_err("a closed channel refuses")
            .into();
        assert_eq!(refused, refusal);
        assert!(
            refused.to_string().contains("was not delivered"),
            "{refused}"
        );
    }

    // A close in anyone's own words is a conversation that ended with them.
    let (notes, inbox) = Notes::channel();
    drop(inbox);
    match Undelivered::from(
        notes
            .send(Note::to(Addressee::Worker, "after the end"))
            .expect_err("a dropped inbox refuses"),
    ) {
        Undelivered::MemberSettled { outcome } => {
            assert!(outcome.contains("dropped"), "{outcome}");
        }
        other => panic!("{other:?}"),
    }

    // A backend that could not produce an answer is a note nothing read.
    let dir = scratch("silent-spool");
    let silent = Spool::connect_within::<Note, Accepted>(&dir, Duration::from_millis(50))
        .send(Note::to(Addressee::Worker, "into a spool nobody services"))
        .expect_err("nothing takes it");
    assert!(matches!(
        &silent,
        InboxUndelivered::Backend(BackendError::Elapsed { .. })
    ));
    match Undelivered::from(silent) {
        Undelivered::NoConversation { reason } => {
            assert!(reason.contains("withdrawn"), "{reason}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        Closed::from(&Undelivered::NoConversation { reason: "r".into() }).reason,
        r#"{"no_conversation":{"reason":"r"}}"#
    );
}

#[test]
fn a_note_crosses_a_spool_and_a_carry_store_as_the_note_it_was() {
    let dir = scratch("spool-and-carry");
    let inbox = Inbox::<Note, Accepted>::new();
    let spool = Spool::bind(dir.join("notes"), &inbox).expect("binds");
    let notes: Notes = Spool::connect(spool.address());
    let note = Note::to(Addressee::Both, "the ruling applies to both of you")
        .binding("the flag defaults to off")
        .expect("binds");
    let sent = note.clone();
    let sending = std::thread::spawn(move || notes.send(sent));
    let taken = inbox
        .take_within(Duration::from_secs(10))
        .expect("the note arrives");
    assert_eq!(taken.message(), &note);
    taken.answer(Accepted::Interrupted {
        party: Party::Supervisor,
    });
    assert_eq!(
        sending.join().expect("finishes"),
        Ok(Accepted::Interrupted {
            party: Party::Supervisor
        })
    );

    // A note its text rules refuse is refused by the receiver, in its words.
    match Spool::deliver(
        spool.address(),
        &json!({"addressee": "worker", "text": "   "}),
        Duration::from_secs(10),
    ) {
        Err(InboxUndelivered::Backend(BackendError::Refused { why, .. })) => {
            assert!(why.contains("blank"), "{why}");
        }
        other => panic!("{other:?}"),
    }

    // Carried to a conversation that is not running: queued for its next turn.
    let store = dir.join("carried.ndjson");
    let carried: Notes = Carry::sender(&store);
    assert_eq!(carried.send(note.clone()), Ok(Accepted::Queued));
    let next = NoteInbox::new();
    assert_eq!(next.adopt_carried(&store), Ok(1));
    assert_eq!(next.take().expect("the carried note").message(), &note);
}
