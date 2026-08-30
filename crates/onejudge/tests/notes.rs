//! End-to-end journeys for the **note delivery seam**, driven through onejudge's
//! own library API across a **real subprocess boundary**.
//!
//! Every case here goes through the public surface a consumer has —
//! [`Notes::channel`], [`Engine::with_notes`], [`Plan::with_notes`] — and never a
//! command line. The providers are pointed at the deterministic test-double
//! binaries (`onejudge-echo-provider`, `onejudge-fake-oneharness`), so the note
//! really crosses a process boundary into whichever party is live, and the framing
//! each party is handed is asserted against the text that double *received* rather
//! than against a re-derivation of it.
//!
//! **How a live turn is driven deterministically.** The doubles hold a turn open
//! (`[[worker-dwell:MS:PATH]]` / `[[judge-dwell:MS:PATH]]`), touching `PATH` as the
//! turn opens. The sending thread waits for that file and only then sends, so the
//! note demonstrably arrives while that party's turn is live rather than between
//! turns — and the dwell is an order of magnitude longer than the wait's poll
//! interval, so the window is not a race.
#![cfg(feature = "fake-provider")]

use std::ops::ControlFlow;
use std::sync::mpsc;
use std::time::Duration;

use onejudge::{
    Accepted, Addressee, CommandProvider, Conversation, Engine, Note, Notes, Observation,
    OneharnessProvider, Party, Role, Settings, SimulatedUser, Skill, Undelivered,
};
// The plan is the second entry point an embedder has, and it lives behind the
// non-default `cli` feature — so only the journey that drives it is gated on one.
#[cfg(feature = "cli")]
use onejudge::cli;

mod support;

use support::{await_path, scratch_path};

/// A [`CommandProvider`] pointed at the built echo test double.
fn echo() -> CommandProvider {
    CommandProvider::new(vec![
        env!("CARGO_BIN_EXE_onejudge-echo-provider").to_string()
    ])
    .unwrap()
}

/// An [`OneharnessProvider`] pointed at the built fake-oneharness test double.
fn fake_oneharness() -> OneharnessProvider {
    OneharnessProvider::new().with_bin(env!("CARGO_BIN_EXE_onejudge-fake-oneharness"))
}

/// Every request the echo double logged, in the order it received them.
fn requests(path: &std::path::Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("the double logs one request per line"))
        .collect()
}

/// Just the `op` requests, in order.
fn of_op(all: &[serde_json::Value], op: &str) -> Vec<serde_json::Value> {
    all.iter()
        .filter(|request| request.get("op").and_then(serde_json::Value::as_str) == Some(op))
        .cloned()
        .collect()
}

/// The latest user message of a `respond` request — what the worker was handed.
fn handed_to_worker(request: &serde_json::Value) -> String {
    request
        .get("messages")
        .and_then(serde_json::Value::as_array)
        .and_then(|messages| {
            messages
                .iter()
                .rev()
                .find(|m| m.get("role").and_then(serde_json::Value::as_str) == Some("user"))
        })
        .and_then(|m| m.get("content").and_then(serde_json::Value::as_str))
        .unwrap_or_default()
        .to_string()
}

/// The note texts a supervisor request carried.
fn notes_shown(request: &serde_json::Value) -> Vec<String> {
    request
        .get("notes")
        .and_then(serde_json::Value::as_array)
        .map(|notes| {
            notes
                .iter()
                .filter_map(|delivered| {
                    delivered
                        .get("note")
                        .and_then(|note| note.get("text"))
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Everything the fake-oneharness double recorded, prompts and terminators alike:
/// one text to assert the framing a party was handed against.
fn prompts(path: &std::path::Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

const NOTE: &str = "the reviewer asked for a smaller diff before this lands";

// --- The note carries the role it addresses -------------------------------

#[test]
fn a_note_addressed_to_the_worker_is_shown_to_the_judge_as_an_update_to_the_workers_task() {
    let judge_log = scratch_path("notes-role-judge.log");
    let agent_log = scratch_path("notes-role-agent.log");
    let (notes, inbox) = Notes::channel();
    let provider = fake_oneharness();
    let engine =
        Engine::new(&provider, Settings::new().with_session_name("notes-role")).with_notes(inbox);

    let accepted = notes
        .send(Note::to(Addressee::Worker, NOTE))
        .expect("nothing has completed, so the note is accepted");
    assert_eq!(accepted, Accepted::Queued);

    let outcome = engine
        .run(&Conversation::multi_turn(
            Skill::new(
                "demo",
                "/skills/demo",
                format!("[[reply:on it]][[record-prompt:{}]]", agent_log.display()),
            ),
            "ship the change",
            SimulatedUser::new(format!(
                "A reviewer. [[record-prompt:{}]]",
                judge_log.display()
            ))
            .done_when("the change is shipped")
            .max_turns(2),
        ))
        .expect("a run with a note delivered into it is an ordinary run");
    assert!(!outcome.transcript.messages.is_empty());

    // The judge is told whose task this updates, and told not to take it on.
    let judge = prompts(&judge_log);
    assert!(
        judge.contains("## Notes delivered to the worker during this run"),
        "the judge was never shown the note: {judge}"
    );
    assert!(
        judge.contains("addressed to it and not to you"),
        "the judge was not told the note is the worker's, not its own: {judge}"
    );
    assert!(
        judge.contains(NOTE),
        "the note's text never reached the judge"
    );
    assert!(
        judge.contains("Judge the work against the completion criterion above"),
        "the note was handed to the judge without being framed as context: {judge}"
    );

    // …and the worker is told it is for the worker, and to act on it.
    let agent = prompts(&agent_log);
    assert!(
        agent.contains("## Notes delivered to you during this run"),
        "the worker was never handed the note: {agent}"
    );
    assert!(
        agent.contains("delivered to YOU, the worker"),
        "the worker was not told the note is addressed to it: {agent}"
    );
    assert!(agent.contains("Act on it as part of the work"));
}

#[test]
fn a_note_addressed_to_the_supervisor_is_not_handed_to_the_worker_as_its_own_task() {
    let agent_log = scratch_path("notes-role-supervisor-agent.log");
    let (notes, inbox) = Notes::channel();
    let provider = fake_oneharness();
    let engine = Engine::new(
        &provider,
        Settings::new().with_session_name("notes-role-sup"),
    )
    .with_notes(inbox);

    notes
        .send(Note::to(
            Addressee::Supervisor,
            "hold the bar at the stated one",
        ))
        .expect("nothing has completed");

    engine
        .run(&Conversation::single_turn(
            Skill::new(
                "demo",
                "/skills/demo",
                format!("[[reply:on it]][[record-prompt:{}]]", agent_log.display()),
            ),
            "ship the change",
        ))
        .expect("a run with a note delivered into it is an ordinary run");

    let agent = prompts(&agent_log);
    assert!(
        agent.contains("## Notes delivered to the supervisor during this run"),
        "the worker was not told whose note this is: {agent}"
    );
    assert!(
        agent.contains("addressed to it and not to you"),
        "the worker was handed the supervisor's note as its own instruction: {agent}"
    );
    assert!(!agent.contains("Act on it as part of the work"));
}

// --- The four delivery cases ----------------------------------------------

#[test]
fn a_note_arriving_during_the_workers_turn_reaches_the_worker_and_the_judge_with_its_response() {
    let log = scratch_path("notes-worker-live.log");
    let live = scratch_path("notes-worker-live.marker");
    let (notes, inbox) = Notes::channel();

    let sender = std::thread::spawn({
        let live = live.clone();
        move || {
            await_path(&live, "the worker's turn never opened");
            notes.send(Note::to(Addressee::Worker, NOTE))
        }
    });

    let provider = echo();
    let engine = Engine::new(&provider, Settings::new()).with_notes(inbox);
    let outcome = engine
        .run(&Conversation::multi_turn(
            Skill::new(
                "demo",
                "/skills/demo",
                format!(
                    "[[worker-dwell:600:{}]][[record:{}]]",
                    live.display(),
                    log.display()
                ),
            ),
            "start the long job",
            SimulatedUser::new(format!("A reviewer. [[record:{}]]", log.display())).max_turns(3),
        ))
        .expect("a note arriving mid-turn does not fail the run");

    let accepted = sender
        .join()
        .expect("the sending thread finished")
        .expect("the conversation was live, so the note was accepted");
    assert_eq!(
        accepted,
        Accepted::Interrupted {
            party: Party::Worker
        },
        "a note arriving during the worker's turn reaches the worker"
    );

    let all = requests(&log);
    let responds = of_op(&all, "respond");
    assert!(
        responds
            .iter()
            .any(|request| handed_to_worker(request).contains(NOTE)),
        "the worker was never handed the note"
    );

    // …and the judge received it *with* the worker's response to it: the very first
    // supervisor call already carries the note, and the transcript it is given
    // already holds the worker's answer to it.
    let supervisors = of_op(&all, "supervisor");
    let first = supervisors
        .first()
        .expect("the supervisor was consulted after the redirected worker turn");
    assert_eq!(
        notes_shown(first),
        vec![NOTE.to_string()],
        "the judge was consulted before the note reached it"
    );
    let transcript = serde_json::to_string(first.get("messages").expect("messages")).unwrap();
    assert!(
        transcript.matches(NOTE).count() >= 2,
        "the judge saw the note but not the worker's response to it: {transcript}"
    );
    assert!(outcome.transcript.assistant_turns() >= 2);
}

#[test]
fn a_note_arriving_during_the_judges_turn_reaches_the_judge_and_the_worker_with_its_response() {
    let log = scratch_path("notes-judge-live.log");
    let live = scratch_path("notes-judge-live.marker");
    let (notes, inbox) = Notes::channel();

    let sender = std::thread::spawn({
        let live = live.clone();
        move || {
            await_path(&live, "the judge's turn never opened");
            notes.send(Note::to(Addressee::Worker, NOTE))
        }
    });

    let provider = echo();
    let engine = Engine::new(&provider, Settings::new()).with_notes(inbox);
    engine
        .run(&Conversation::multi_turn(
            Skill::new(
                "demo",
                "/skills/demo",
                format!("[[record:{}]]", log.display()),
            ),
            "start the job",
            SimulatedUser::new(format!(
                "A reviewer. [[judge-dwell:600:{}]][[record:{}]]",
                live.display(),
                log.display()
            ))
            .max_turns(3),
        ))
        .expect("a note arriving mid-judge-turn does not fail the run");

    let accepted = sender
        .join()
        .expect("the sending thread finished")
        .expect("the conversation was live, so the note was accepted");
    assert_eq!(
        accepted,
        Accepted::Interrupted {
            party: Party::Supervisor
        },
        "a note arriving during the judge's turn reaches the judge"
    );

    let all = requests(&log);
    let supervisors = of_op(&all, "supervisor");
    assert!(
        supervisors
            .first()
            .map(|request| notes_shown(request).is_empty())
            .unwrap_or(false),
        "the first decision was taken before the note arrived"
    );
    assert!(
        supervisors
            .iter()
            .any(|request| notes_shown(request) == vec![NOTE.to_string()]),
        "the decision was never re-taken with the note in hand"
    );

    // The worker receives it with the judge's response: one user turn carrying both.
    let handed = of_op(&all, "respond")
        .iter()
        .map(handed_to_worker)
        .find(|text| text.contains(NOTE))
        .expect("the worker was never handed the note");
    assert!(
        handed.contains("Thanks — and what about the next step?"),
        "the note reached the worker without the judge's response: {handed}"
    );
}

#[test]
fn a_note_the_judge_completes_on_is_delivered_to_the_judge_and_never_to_the_worker() {
    let log = scratch_path("notes-judged-with.log");
    let live = scratch_path("notes-judged-with.marker");
    let (notes, inbox) = Notes::channel();

    let sender = std::thread::spawn({
        let live = live.clone();
        move || {
            await_path(&live, "the judge's turn never opened");
            notes.send(Note::to(Addressee::Worker, NOTE))
        }
    });

    let provider = echo();
    let engine = Engine::new(&provider, Settings::new()).with_notes(inbox);
    let outcome = engine
        .run(&Conversation::multi_turn(
            Skill::new(
                "demo",
                "/skills/demo",
                format!("[[record:{}]]", log.display()),
            ),
            "start the job",
            SimulatedUser::new(format!(
                "A reviewer. [[judge-dwell:600:{}]][[complete-on-note]][[record:{}]]",
                live.display(),
                log.display()
            ))
            .max_turns(4),
        ))
        .expect("a run the judge completes is an ordinary run");

    let accepted = sender
        .join()
        .expect("the sending thread finished")
        .expect("the note reached the judge, so it was accepted");
    let Accepted::JudgedWith { completion_reason } = accepted else {
        panic!("the judge passed the work with the note in hand, so: {accepted:?}");
    };
    assert_eq!(
        outcome.completion_reason.as_deref(),
        Some(completion_reason.as_str()),
        "the completion decision is the one taken with the note in hand"
    );

    // Nothing was delivered to the worker: the judge passed the work.
    assert!(
        !outcome
            .transcript
            .messages
            .iter()
            .any(|message| message.content.contains(NOTE)),
        "the note reached the worker after the judge had already passed the work"
    );
    assert!(
        !of_op(&requests(&log), "respond")
            .iter()
            .any(|request| handed_to_worker(request).contains(NOTE)),
        "the worker was handed a note the judge had already completed on"
    );
    let delivered = engine.delivered_notes();
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].delivered_to, Party::Supervisor);
}

#[test]
fn a_note_arriving_between_turns_is_delivered_to_the_next_turn() {
    let log = scratch_path("notes-between-turns.log");
    let (notes, inbox) = Notes::channel();
    let (open, wait) = mpsc::channel::<()>();

    let sender = std::thread::spawn(move || {
        wait.recv().expect("the run reached a turn boundary");
        notes.send(Note::to(Addressee::Worker, NOTE))
    });

    let provider = echo();
    let engine = Engine::new(&provider, Settings::new()).with_notes(inbox);
    let mut signalled = false;
    // The window between the supervisor's turn closing and the next worker turn
    // opening: the engine is held inside it while the note is sent, so the arrival
    // is genuinely between turns rather than during either party's turn.
    let mut at_the_boundary = |observation: &Observation<'_>| {
        if let Observation::TurnClosed(closed) = observation {
            if closed.role == Role::User && !signalled {
                signalled = true;
                open.send(()).expect("the sending thread is waiting");
                std::thread::sleep(Duration::from_millis(400));
            }
        }
        ControlFlow::Continue(())
    };
    engine
        .run_observing(
            &Conversation::multi_turn(
                Skill::new(
                    "demo",
                    "/skills/demo",
                    format!("[[record:{}]]", log.display()),
                ),
                "start the job",
                SimulatedUser::new("A reviewer.").max_turns(3),
            ),
            &mut at_the_boundary,
        )
        .expect("a note arriving between turns does not fail the run");

    let accepted = sender
        .join()
        .expect("the sending thread finished")
        .expect("nothing had completed, so the note was accepted");
    assert_eq!(accepted, Accepted::Queued);
    assert!(
        signalled,
        "the run never reached a boundary between two turns"
    );
    assert!(
        of_op(&requests(&log), "respond")
            .iter()
            .any(|request| handed_to_worker(request).contains(NOTE)),
        "the note queued between turns never reached the next turn"
    );
}

// --- The riders: the amendment, and the criteria --------------------------

#[test]
fn the_judge_is_shown_the_workers_full_task_including_the_amendment_in_force() {
    let judge_log = scratch_path("notes-amendment-judge.log");
    let amended_task = "Ship the change.\n\n\
         ## Amendment\n\n\
         Where this section and anything above it disagree, this section wins. \
         The change ships behind a flag, not on by default.";
    let (notes, inbox) = Notes::channel();
    let provider = fake_oneharness();
    let engine =
        Engine::new(&provider, Settings::new().with_session_name("notes-amend")).with_notes(inbox);

    // An amendment delivered mid-run binds the judge as well: addressed to both, it
    // is an update to the worker's task *and* a criterion the finished work owes.
    notes
        .send(
            Note::to(Addressee::Both, "the bar moved: the flag defaults to off")
                .binding("the change is reachable only behind a flag that defaults to off")
                .expect("a property, not a procedure"),
        )
        .expect("nothing has completed");

    engine
        .run(&Conversation::multi_turn(
            Skill::new("demo", "/skills/demo", "[[reply:on it]]"),
            amended_task,
            SimulatedUser::new(format!(
                "A reviewer. [[record-prompt:{}]]",
                judge_log.display()
            ))
            .done_when("the change is shipped")
            .max_turns(2),
        ))
        .expect("an amended task is an ordinary task");

    let judge = prompts(&judge_log);
    // The task the worker was actually given, amendment and precedence sentence
    // included — otherwise the judge scores the work against a task that never
    // mentioned it.
    assert!(
        judge.contains("## Amendment"),
        "the judge was given a task with the amendment stripped: {judge}"
    );
    assert!(judge.contains("Where this section and anything above it disagree"));
    assert!(judge.contains("The change ships behind a flag, not on by default."));
    // …and the amendment delivered during the run is in force too — on the bar the
    // judge decides against, not merely somewhere in the prose it was shown.
    let criterion = judge
        .split("Completion criterion:\n")
        .nth(1)
        .and_then(|rest| rest.split("\n\n## Notes").next())
        .unwrap_or_default();
    assert!(
        criterion.contains("the change is reachable only behind a flag that defaults to off"),
        "the mid-run amendment never reached the judge's bar: {criterion}"
    );
    assert!(
        judge.contains("(addressed to both)"),
        "the judge was not told the amendment binds both parties: {judge}"
    );
}

#[test]
fn a_delivered_note_enters_the_acceptance_criteria_rather_than_only_the_narration() {
    let judge_log = scratch_path("notes-criteria-judge.log");
    let (notes, inbox) = Notes::channel();
    let provider = fake_oneharness();
    let engine = Engine::new(
        &provider,
        Settings::new().with_session_name("notes-criteria"),
    )
    .with_notes(inbox);

    notes
        .send(
            Note::to(
                Addressee::Worker,
                "the reviewer wants the migration covered",
            )
            .binding("the migration path is covered by a test")
            .expect("a property, not a procedure"),
        )
        .expect("nothing has completed");

    engine
        .run(&Conversation::multi_turn(
            Skill::new("demo", "/skills/demo", "[[reply:on it]]"),
            "ship the change",
            SimulatedUser::new(format!(
                "A reviewer. [[record-prompt:{}]]",
                judge_log.display()
            ))
            .done_when("every acceptance criterion stated in the task is met")
            .max_turns(2),
        ))
        .expect("a run with a binding note is an ordinary run");

    // The criterion the per-turn judge was actually given.
    let judge = prompts(&judge_log);
    let criterion = judge
        .split("Completion criterion:\n")
        .nth(1)
        .and_then(|rest| rest.split("\n\n## Notes").next())
        .unwrap_or_default();
    assert!(
        criterion.contains("every acceptance criterion stated in the task is met"),
        "the configured criterion was dropped: {criterion}"
    );
    assert!(
        criterion.contains("## Additional acceptance criteria delivered during this run"),
        "the note's requirement stayed narration: {criterion}"
    );
    assert!(
        criterion.contains("1. the migration path is covered by a test"),
        "the bound criterion never reached the bar: {criterion}"
    );
    assert!(
        criterion.contains("judge the property that mechanism was serving"),
        "the criteria frame lost the mechanism-versus-property instruction: {criterion}"
    );

    // And the same composition is what the library hands any caller.
    let criteria = engine.criteria(Some("every acceptance criterion stated in the task is met"));
    assert_eq!(criteria.bound().len(), 1);
    assert_eq!(
        criteria.bound()[0].as_str(),
        "the migration path is covered by a test"
    );
}

#[cfg(feature = "cli")]
#[test]
fn a_bound_criterion_reaches_the_authoritative_re_judge_a_plan_settles_on() {
    let (notes, inbox) = Notes::channel();
    notes
        .send(
            Note::to(
                Addressee::Worker,
                "the reviewer wants the migration covered",
            )
            .binding("the migration path is covered by a test")
            .expect("a property, not a procedure"),
        )
        .expect("nothing has completed");

    let plan = cli::Plan {
        provider: cli::ProviderSpec::Command {
            command: vec![env!("CARGO_BIN_EXE_onejudge-echo-provider").to_string()],
        },
        settings: Settings::new(),
        conversation: Conversation::single_turn(
            Skill::new("demo", "/skills/demo", ""),
            "ship the change",
        ),
        evals: Vec::new(),
        done_when: Some("the change is shipped".into()),
        assessment: None,
        spawn_hook: None,
        notes: None,
    }
    .with_notes(inbox);

    let mut progress = |_: &str| {};
    let summary = cli::run_plan(plan, cli::Format::Json, &mut progress).expect("the plan runs");
    let done = summary
        .done_when
        .expect("a plan with a completion condition re-judges it");
    assert!(
        done.criterion
            .contains("## Additional acceptance criteria delivered during this run"),
        "the note's criterion never reached the authoritative re-judge: {}",
        done.criterion
    );
    assert!(done
        .criterion
        .contains("1. the migration path is covered by a test"));
    assert!(done.criterion.contains("the change is shipped"));
}

// --- Undelivered is an error ----------------------------------------------

#[test]
fn a_note_arriving_after_the_conversation_completed_raises_and_is_not_accepted() {
    let (notes, inbox) = Notes::channel();
    let provider = echo();
    let engine = Engine::new(&provider, Settings::new()).with_notes(inbox);

    let outcome = engine
        .run(&Conversation::multi_turn(
            Skill::new("demo", "/skills/demo", ""),
            "start the job",
            // `[[stop]]` makes the double's supervisor answer completion.
            SimulatedUser::new("A reviewer. [[stop]]").max_turns(3),
        ))
        .expect("the run completes");
    let reason = outcome
        .completion_reason
        .clone()
        .expect("the supervisor answered completion");

    let refused = notes
        .send(Note::to(Addressee::Worker, NOTE))
        .expect_err("a note arriving after completion is not delivered");
    assert_eq!(
        refused,
        Undelivered::ConversationCompleted {
            completion_reason: reason
        }
    );
    let message = refused.to_string();
    assert!(
        message.contains("was not delivered"),
        "the refusal does not name that it was not delivered: {message}"
    );
    assert!(
        message.contains("already answered completion"),
        "the refusal does not say why: {message}"
    );
    assert!(
        message.contains("Relaunch") && message.contains("follow-up"),
        "the refusal leaves the caller no choice to make: {message}"
    );

    // …and nothing was silently accepted: it reached no party and moved no bar.
    assert!(engine.delivered_notes().is_empty());
    assert!(notes.delivered().is_empty());
    assert_eq!(engine.criteria(Some("the job is done")).bound().len(), 0);
    assert_eq!(
        engine
            .criteria(Some("the job is done"))
            .rendered()
            .as_deref(),
        Some("the job is done"),
        "a refused note must leave the bar byte-identical"
    );
}

#[test]
fn a_note_arriving_after_a_run_that_never_completed_raises_naming_how_it_ended() {
    let (notes, inbox) = Notes::channel();
    let provider = echo();
    let engine = Engine::new(&provider, Settings::new()).with_notes(inbox);

    let outcome = engine
        .run(&Conversation::multi_turn(
            Skill::new("demo", "/skills/demo", ""),
            "start the job",
            SimulatedUser::new("A reviewer.").max_turns(1),
        ))
        .expect("the run reaches its turn cap");
    assert!(outcome.completion_reason.is_none());

    let refused = notes
        .send(Note::to(Addressee::Worker, NOTE))
        .expect_err("a note arriving after the conversation ended is not delivered");
    let Undelivered::MemberSettled { outcome } = &refused else {
        panic!("a run that ended without completing settles rather than completes: {refused:?}");
    };
    assert!(
        outcome.contains("without a completion decision"),
        "the refusal does not say how the run ended: {outcome}"
    );
    assert!(engine.delivered_notes().is_empty());
}

#[test]
fn a_note_sent_to_a_channel_no_conversation_ever_read_raises() {
    let (notes, inbox) = Notes::channel();
    drop(inbox);

    let refused = notes
        .send(Note::to(Addressee::Worker, NOTE))
        .expect_err("nothing will ever read this channel");
    assert!(matches!(refused, Undelivered::NoConversation { .. }));
    assert!(refused.to_string().contains("was not delivered"));
}

// --- A binding note is held to being a criterion --------------------------

#[test]
fn a_criterion_that_perishes_or_names_a_procedure_is_refused_where_the_note_is_built() {
    let refusal = |criterion: &str| {
        Note::to(Addressee::Worker, "the bar moved")
            .binding(criterion)
            .expect_err("the criterion is refused")
            .to_string()
    };

    assert!(refusal("the pin moves to oneharness-core 0.12.1").contains("version literal"));
    assert!(refusal("`just check` passes over the finished tree").contains("`just` invocation"));
    assert!(refusal("the suite is green && the lint is clean").contains("chained shell command"));
    assert!(refusal("`cargo nextest run` reports no failures").contains("shell invocation"));
    assert!(refusal("the note is worded verbatim as written").contains("particular string"));
    assert!(refusal("the report is of the shape described").contains("defers its content"));
    assert!(refusal("the pull request is merged").contains("work the dispatch cannot do"));
    assert!(refusal("   ").contains("blank"));

    // …and a property the finished work can actually have is accepted.
    let bound = Note::to(Addressee::Worker, "the bar moved")
        .binding("the migration path is covered by a test that fails without the migration")
        .expect("a property is a criterion");
    assert!(bound.binds());
    assert_eq!(
        bound.criterion.as_ref().map(onejudge::Criterion::as_str),
        Some("the migration path is covered by a test that fails without the migration")
    );
}

#[test]
fn a_note_arriving_after_a_run_that_failed_raises_naming_the_failure() {
    let (notes, inbox) = Notes::channel();
    let provider = echo();
    let engine = Engine::new(&provider, Settings::new()).with_notes(inbox);

    // `[[emit-exit]]` makes the double exit non-zero, so the turn fails outright.
    let error = engine
        .run(&Conversation::single_turn(
            Skill::new("demo", "/skills/demo", "[[emit-exit]]"),
            "start the job",
        ))
        .expect_err("the provider fails the turn");

    let refused = notes
        .send(Note::to(Addressee::Worker, NOTE))
        .expect_err("a note arriving after the run failed is not delivered");
    let Undelivered::MemberSettled { outcome } = &refused else {
        panic!("a failed run settles the channel: {refused:?}");
    };
    assert!(
        outcome.contains("the conversation failed"),
        "the refusal does not say the run failed: {outcome}"
    );
    assert!(
        outcome.contains(&error.to_string()),
        "the refusal does not carry the failure itself: {outcome}"
    );
    assert!(engine.delivered_notes().is_empty());
}

#[test]
fn a_note_arriving_after_a_settled_no_op_loop_raises_naming_the_settle() {
    let (notes, inbox) = Notes::channel();
    let provider = echo();
    let engine = Engine::new(&provider, Settings::new()).with_notes(inbox);

    // The supervisor that answers a valid `continue` asking for nothing, every
    // time: the loop settles on the work it has rather than running to its cap.
    let outcome = engine
        .run(&Conversation::multi_turn(
            Skill::new("demo", "/skills/demo", ""),
            "release the dispatch",
            SimulatedUser::new("[[supervisor-noop]]").max_turns(40),
        ))
        .expect("the run settles rather than failing");
    let settled = outcome.settled_reason.clone().expect("a settle reason");

    let refused = notes
        .send(Note::to(Addressee::Worker, NOTE))
        .expect_err("a note arriving after the run settled is not delivered");
    assert_eq!(
        refused,
        Undelivered::MemberSettled { outcome: settled },
        "a settled run tells the sender why it settled, not merely that it ended"
    );
}

#[test]
fn a_note_arriving_after_an_observation_short_circuited_the_run_raises_naming_that() {
    let (notes, inbox) = Notes::channel();
    let provider = echo();
    let engine = Engine::new(&provider, Settings::new()).with_notes(inbox);

    let mut stop_at_once = |_: &Observation<'_>| ControlFlow::Break(());
    let outcome = engine
        .run_observing(
            &Conversation::multi_turn(
                Skill::new("demo", "/skills/demo", ""),
                "start the job",
                SimulatedUser::new("A reviewer.").max_turns(9),
            ),
            &mut stop_at_once,
        )
        .expect("a sink that breaks short-circuits rather than failing");
    assert!(outcome.stopped_early);

    let refused = notes
        .send(Note::to(Addressee::Worker, NOTE))
        .expect_err("a note arriving after the run was short-circuited is not delivered");
    let Undelivered::MemberSettled { outcome } = &refused else {
        panic!("a short-circuited run settles the channel: {refused:?}");
    };
    assert!(
        outcome.contains("short-circuited"),
        "the refusal does not say the run was stopped early: {outcome}"
    );
}
