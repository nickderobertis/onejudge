//! End-to-end journeys for a note **interrupting** the live turn it is addressed
//! to — and for every note that does not, being delivered once that turn ends.
//!
//! Each journey runs a real onejudge conversation with oneharness running for real
//! **in process** (`OneharnessProvider`, the linked `oneharness-core`), and doubles
//! only the paid harness: `onejudge-fake-harness`, pinned as `claude-code` through
//! ordinary `[harness.claude-code] bin` config. So the redirect is the real one —
//! oneharness's own control socket, its own interrupt frame on the harness's
//! stdin, its own replacement turn — and what the double recorded is what the
//! harness was really handed.
//!
//! **How a live turn is held.** `[[worker-dwell:MS:PATH]]` / `[[judge-dwell:…]]`
//! hold the party's first turn open and touch `PATH` as it opens; the sending
//! thread waits for that file, so the note demonstrably arrives mid-turn. The hold
//! is long (it is never meant to run out on its own) and ends early when the
//! journey creates `PATH.release`, or when oneharness's interrupt aborts it. The
//! `[[worker-log:…]]` / `[[judge-log:…]]` record is the harness's own account:
//! every prompt it was handed, and whether a held turn was aborted or finished.
#![cfg(unix)]

use std::time::{Duration, Instant};

use onejudge::{
    Accepted, Addressee, Conversation, Delivery, Engine, Message, Note, NoteInbox, Notes,
    OneharnessProvider, Party, Role, Settings, SimulatedUser, Skill, Undelivered,
};
use onejudge_test_doubles::{self as doubles, support};
use onemessagebus::Unanswered;

use support::{await_path, scratch_path, use_private_session_store};

const NOTE: &str = "the reviewer asked for a smaller diff before this lands";
const CRITERION: &str = "the diff touches only the parser module";

/// A hold far longer than any journey waits, so a turn that ends early ended
/// because something ended it.
const HOLD_MS: u64 = 30_000;

/// A project whose oneharness config pins `claude-code` to the fake-harness
/// double, for both parties.
fn harness_project(name: &str) -> std::path::PathBuf {
    let dir = scratch_path(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("oneharness.toml"),
        format!(
            "harnesses = [\"claude-code\"]\nhistory_dir = {:?}\n\n[harness.claude-code]\nbin = {:?}\n",
            dir.join("history").display().to_string(),
            doubles::fake_harness(),
        ),
    )
    .unwrap();
    dir
}

/// The in-process provider over `dir`'s harness, with turn control as asked.
fn provider(dir: &std::path::Path, control: bool) -> OneharnessProvider {
    OneharnessProvider::new()
        .with_judge_config(dir.join("oneharness.toml"))
        .with_streaming(true)
        .with_control(control)
}

/// One line the double logged for a party's turns.
#[derive(Debug, PartialEq)]
enum Logged {
    Prompt(String),
    Aborted,
    Finished,
}

fn log(path: &std::path::Path) -> Vec<Logged> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|line| {
            let line: serde_json::Value = serde_json::from_str(line).expect("one JSON line");
            match line["event"].as_str() {
                Some("prompt") => Logged::Prompt(line["text"].as_str().unwrap().to_string()),
                Some("aborted") => Logged::Aborted,
                Some("finished") => Logged::Finished,
                other => panic!("an unknown turn-log event {other:?}"),
            }
        })
        .collect()
}

fn prompts(logged: &[Logged]) -> Vec<&str> {
    logged
        .iter()
        .filter_map(|line| match line {
            Logged::Prompt(text) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// The file a held turn touches as it opens, and the release file beside it
/// (`<marker>.release`), both cleared of an earlier run.
fn hold_marker(name: &str) -> std::path::PathBuf {
    let marker = scratch_path(&format!("{name}.held"));
    let _ = std::fs::remove_file(release_of(&marker));
    marker
}

fn release_of(marker: &std::path::Path) -> std::path::PathBuf {
    let mut name = marker.as_os_str().to_owned();
    name.push(".release");
    std::path::PathBuf::from(name)
}

/// End a hold early, as a turn that ran its course.
fn release(marker: &std::path::Path) {
    std::fs::write(release_of(marker), b"go\n").unwrap();
}

fn binding_note(addressee: Addressee) -> Note {
    Note::to(addressee, NOTE)
        .binding(CRITERION)
        .expect("a property, not a procedure")
}

#[test]
fn a_worker_note_interrupts_the_workers_live_turn_and_the_judge_decides_with_it() {
    let _store = use_private_session_store("note-int-worker");
    let dir = harness_project("note-int-worker");
    let held = hold_marker("note-int-worker");
    let worker_log = scratch_path("note-int-worker.worker.log");
    let judge_log = scratch_path("note-int-worker.judge.log");
    let (notes, inbox) = Notes::channel();

    let sending = std::thread::spawn({
        let held = held.clone();
        move || {
            await_path(&held, "the worker's turn never opened");
            notes.send(binding_note(Addressee::Worker))
        }
    });

    let provider = provider(&dir, true);
    let engine = Engine::new(
        &provider,
        Settings::new().with_session_name("note-int-worker"),
    )
    .with_notes(inbox);
    let started = Instant::now();
    let outcome = engine
        .run_streaming(
            &Conversation::multi_turn(
                Skill::new(
                    "demo",
                    dir.to_str().unwrap(),
                    format!(
                        "[[say:reading the parser first]][[event:cat parser.rs]]\
                         [[worker-dwell:{HOLD_MS}:{}]][[worker-log:{}]][[reply:trimmed the diff]]",
                        held.display(),
                        worker_log.display()
                    ),
                ),
                "make the change",
                SimulatedUser::new(format!(
                    "A reviewer. [[judge-log:{}]][[evaluate]]",
                    judge_log.display()
                ))
                .done_when("the change is made")
                .max_turns(3),
            ),
            &mut |_| std::ops::ControlFlow::Continue(()),
        )
        .expect("a run whose turn a note interrupted is an ordinary run");

    assert_eq!(
        sending.join().unwrap(),
        Ok(Accepted::Interrupted {
            party: Party::Worker
        }),
        "a note that redirected the live turn says so"
    );
    assert!(
        started.elapsed() < Duration::from_millis(HOLD_MS / 2),
        "the held turn was not cut short: {:?}",
        started.elapsed()
    );

    // The harness's own account: the held turn was aborted and never finished,
    // and the very next prompt it was handed is the note — once.
    let logged = log(&worker_log);
    assert_eq!(logged.len(), 3, "{logged:?}");
    assert_eq!(logged[1], Logged::Aborted, "{logged:?}");
    assert!(!logged.contains(&Logged::Finished), "{logged:?}");
    let next = prompts(&logged)[1];
    assert_eq!(next.matches(NOTE).count(), 1, "{next}");
    assert!(next.contains("delivered to YOU, the worker"), "{next}");

    // The transcript keeps the interrupted turn — its words and its tool call
    // stand — then the note it was redirected with, then the reply.
    let messages = &outcome.transcript.messages;
    assert_eq!(messages[1].role, Role::Assistant);
    assert_eq!(messages[1].content, "reading the parser first");
    assert_eq!(messages[1].events.len(), 1, "{:?}", messages[1].events);
    assert_eq!(messages[2], Message::user(next));
    assert_eq!(messages[3].role, Role::Assistant);
    assert_eq!(messages[3].content, "trimmed the diff");

    // The judge decided afterwards, with the note in hand and its criterion in the
    // bar it judged against.
    let judged = log(&judge_log);
    let first = prompts(&judged)[0];
    assert!(first.contains(NOTE), "{first}");
    assert!(
        first.contains("## Additional acceptance criteria delivered during this run")
            && first.contains(&format!("1. {CRITERION}")),
        "the judge's criteria did not include the note's: {first}"
    );
    assert_eq!(
        engine
            .criteria(Some("the change is made"))
            .bound()
            .iter()
            .map(|c| c.as_str())
            .collect::<Vec<_>>(),
        [CRITERION]
    );
    let delivered = engine.delivered_notes();
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].delivered_to, Party::Worker);
}

#[test]
fn a_supervisor_note_redirects_the_judges_live_turn_and_its_decision_carries_the_criterion() {
    let _store = use_private_session_store("note-int-judge");
    let dir = harness_project("note-int-judge");
    let held = hold_marker("note-int-judge");
    let worker_log = scratch_path("note-int-judge.worker.log");
    let judge_log = scratch_path("note-int-judge.judge.log");
    let (notes, inbox) = Notes::channel();

    let sending = std::thread::spawn({
        let held = held.clone();
        move || {
            await_path(&held, "the judge's turn never opened");
            notes.send(binding_note(Addressee::Supervisor))
        }
    });

    let provider = provider(&dir, true);
    let engine = Engine::new(
        &provider,
        Settings::new().with_session_name("note-int-judge"),
    )
    .with_notes(inbox);
    let outcome = engine
        .run(&Conversation::multi_turn(
            Skill::new(
                "demo",
                dir.to_str().unwrap(),
                format!("[[worker-log:{}]][[reply:done]]", worker_log.display()),
            ),
            "make the change",
            SimulatedUser::new(format!(
                "A reviewer. [[judge-dwell:{HOLD_MS}:{}]][[judge-log:{}]][[evaluate]]\
                 [[decide-on-redirect]]",
                held.display(),
                judge_log.display()
            ))
            .done_when("the change is made")
            .max_turns(2),
        ))
        .expect("a run whose judge a note redirected is an ordinary run");

    // The redirected turn decided to continue, so the note is answered as the
    // interruption it was rather than as a completion.
    assert_eq!(
        sending.join().unwrap(),
        Ok(Accepted::Interrupted {
            party: Party::Supervisor
        })
    );

    let judged = log(&judge_log);
    assert_eq!(judged[1], Logged::Aborted, "{judged:?}");
    assert!(!judged.contains(&Logged::Finished), "{judged:?}");
    let replacement = prompts(&judged)[1];
    assert!(replacement.contains(NOTE), "{replacement}");
    assert!(
        replacement.contains(&format!("1. {CRITERION}")),
        "{replacement}"
    );

    // The decision is the redirected turn's own, and it carries the criterion:
    // the next instruction the worker got names it.
    let instruction = format!("Meet the criterion delivered into this turn: {CRITERION}");
    let worker = log(&worker_log);
    let handed = prompts(&worker)[1];
    assert!(handed.contains(&instruction), "{handed}");
    // …with the note, framed as the supervisor's and not the worker's own task.
    assert!(
        handed.contains("## Notes delivered to the supervisor during this run"),
        "{handed}"
    );
    assert_eq!(outcome.transcript.assistant_turns(), 2);
    let delivered = engine.delivered_notes();
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].delivered_to, Party::Supervisor);
}

/// Run a two-turn conversation whose first worker turn is held, send `note` once
/// it is live through `send`, and hand back the sender's answer, what the worker
/// harness logged, and the outcome.
struct HeldRun {
    answer: Result<Accepted, Undelivered>,
    worker: Vec<Logged>,
    delivered: Vec<onejudge::DeliveredNote>,
}

fn held_worker_turn(
    name: &str,
    control: bool,
    markers: impl FnOnce(&std::path::Path) -> String,
    send: impl FnOnce(&Notes, &std::path::Path) -> Result<Accepted, Undelivered> + Send + 'static,
) -> HeldRun {
    let _store = use_private_session_store(name);
    let dir = harness_project(name);
    let held = hold_marker(name);
    let worker_log = scratch_path(&format!("{name}.worker.log"));
    let (notes, inbox): (Notes, NoteInbox) = Notes::channel();
    let sending = std::thread::spawn({
        let held = held.clone();
        move || send(&notes, &held)
    });
    let provider = provider(&dir, control);
    let engine = Engine::new(&provider, Settings::new().with_session_name(name)).with_notes(inbox);
    engine
        .run(&Conversation::multi_turn(
            Skill::new(
                "demo",
                dir.to_str().unwrap(),
                format!(
                    "{}[[worker-log:{}]][[reply:done]]",
                    markers(&held),
                    worker_log.display()
                ),
            ),
            "make the change",
            SimulatedUser::new("A reviewer. [[evaluate]]")
                .done_when("the change is made")
                .max_turns(3),
        ))
        .expect("an ordinary run");
    HeldRun {
        answer: sending.join().unwrap(),
        worker: log(&worker_log),
        delivered: engine.delivered_notes(),
    }
}

fn dwell(held: &std::path::Path) -> String {
    format!("[[worker-dwell:{HOLD_MS}:{}]]", held.display())
}

/// The note reached the worker once its held turn ended: that turn finished, the
/// next one opened carrying the note exactly once, and nothing was interrupted.
fn assert_delivered_at_turn_end(run: &HeldRun) {
    assert_eq!(
        run.answer,
        Ok(Accepted::AtTurnEnd {
            party: Party::Worker
        })
    );
    assert!(!run.worker.contains(&Logged::Aborted), "{:?}", run.worker);
    let handed = prompts(&run.worker);
    assert!(!handed[0].contains(NOTE));
    assert_eq!(handed[1].matches(NOTE).count(), 1, "{}", handed[1]);
    assert_eq!(
        handed
            .iter()
            .map(|p| p.matches(NOTE).count())
            .sum::<usize>(),
        1,
        "{handed:?}"
    );
    assert_eq!(run.delivered.len(), 1);
    assert_eq!(run.delivered[0].delivered_to, Party::Worker);
}

#[test]
fn a_turn_end_note_waits_for_the_held_turn_to_finish() {
    let run = held_worker_turn("note-turn-end", true, dwell, |notes, held| {
        await_path(held, "the worker's turn never opened");
        let offer = notes
            .offer(Note::to(Addressee::Worker, NOTE).with_delivery(Delivery::TurnEnd))
            .map_err(Undelivered::from)?;
        // Long enough that a build which interrupted would already have done so.
        std::thread::sleep(Duration::from_millis(500));
        release(held);
        offer.wait().map_err(Undelivered::from)
    });
    assert!(run.worker.contains(&Logged::Finished), "{:?}", run.worker);
    assert_delivered_at_turn_end(&run);
}

#[test]
fn an_interrupt_note_to_a_turn_opened_without_control_waits_for_it_to_end() {
    let run = held_worker_turn("note-uncontrolled", false, dwell, |notes, held| {
        await_path(held, "the worker's turn never opened");
        let offer = notes
            .offer(Note::to(Addressee::Worker, NOTE))
            .map_err(Undelivered::from)?;
        std::thread::sleep(Duration::from_millis(500));
        release(held);
        offer.wait().map_err(Undelivered::from)
    });
    assert!(run.worker.contains(&Logged::Finished), "{:?}", run.worker);
    assert_delivered_at_turn_end(&run);
}

#[test]
fn an_interrupt_note_whose_redirect_is_refused_because_the_turn_ended_waits_for_the_next() {
    // The turn has answered and the run is still alive, its socket listening: the
    // redirect reaches oneharness and is refused, because there is no turn in
    // flight to replace. The note is handed back and delivered once the run ends.
    let run = held_worker_turn(
        "note-refused-ended",
        true,
        |held| format!("[[linger-after-result:{HOLD_MS}:{}]]", held.display()),
        |notes, held| {
            await_path(held, "the worker's turn never ended");
            let offer = notes
                .offer(Note::to(Addressee::Worker, NOTE))
                .map_err(Undelivered::from)?;
            std::thread::sleep(Duration::from_millis(500));
            release(held);
            offer.wait().map_err(Undelivered::from)
        },
    );
    assert_delivered_at_turn_end(&run);
}

#[test]
fn a_turn_end_note_withdrawn_before_the_turn_ends_is_never_delivered() {
    let run = held_worker_turn("note-withdrawn", true, dwell, |notes, held| {
        await_path(held, "the worker's turn never opened");
        let offer = notes
            .offer(Note::to(Addressee::Worker, NOTE).with_delivery(Delivery::TurnEnd))
            .map_err(Undelivered::from)?;
        std::thread::sleep(Duration::from_millis(300));
        let withdrawn = offer.withdraw();
        release(held);
        assert!(
            withdrawn.is_ok(),
            "a note not yet presented can be taken back"
        );
        // Not delivered, so there is no disposition to report: the queued answer
        // stands in for "never reached anyone" in this journey's shape.
        Ok(Accepted::Queued)
    });
    assert!(run.delivered.is_empty(), "{:?}", run.delivered);
    assert!(
        prompts(&run.worker).iter().all(|p| !p.contains(NOTE)),
        "{:?}",
        run.worker
    );
}

#[test]
fn an_interrupt_note_with_a_deadline_to_an_uncontrolled_turn_expires_untaken() {
    let run = held_worker_turn("note-deadline", false, dwell, |notes, held| {
        await_path(held, "the worker's turn never opened");
        let offer = notes
            .offer(Note::to(Addressee::Worker, NOTE))
            .map_err(Undelivered::from)?;
        let expired = offer.wait_until(Instant::now() + Duration::from_millis(500));
        // Still held: nothing has ended the turn yet.
        let still_held = !release_exists(held);
        release(held);
        assert!(matches!(expired, Err(Unanswered::Expired)), "{expired:?}");
        assert!(still_held);
        Ok(Accepted::Queued)
    });
    assert!(run.worker.contains(&Logged::Finished), "{:?}", run.worker);
    assert!(run.delivered.is_empty(), "{:?}", run.delivered);
    assert!(
        prompts(&run.worker).iter().all(|p| !p.contains(NOTE)),
        "{:?}",
        run.worker
    );
}

fn release_exists(held: &std::path::Path) -> bool {
    release_of(held).exists()
}

#[test]
fn a_refused_redirect_hands_the_note_back_and_its_deadline_still_expires_it() {
    // The held turn's socket is replaced by one nobody listens on, so the redirect
    // is refused `not_running` while the turn is still held. The note is handed
    // back untaken, its deadline passes, and it expires before the turn ends.
    let store = use_private_session_store("note-refused-deadline");
    let socket =
        oneharness_core::domain::control::socket_path(&store, "note-refused-deadline-skill")
            .expect("the store fits a socket address");
    let run = held_worker_turn("note-refused-deadline", true, dwell, move |notes, held| {
        await_path(held, "the worker's turn never opened");
        assert!(socket.exists(), "the controlled turn opened its socket");
        std::fs::remove_file(&socket).unwrap();
        drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());
        let offer = notes
            .offer(Note::to(Addressee::Worker, NOTE))
            .map_err(Undelivered::from)?;
        let expired = offer.wait_until(Instant::now() + Duration::from_millis(800));
        release(held);
        assert!(matches!(expired, Err(Unanswered::Expired)), "{expired:?}");
        Ok(Accepted::Queued)
    });
    assert!(run.worker.contains(&Logged::Finished), "{:?}", run.worker);
    assert!(!run.worker.contains(&Logged::Aborted), "{:?}", run.worker);
    assert!(run.delivered.is_empty(), "{:?}", run.delivered);
    assert!(
        prompts(&run.worker).iter().all(|p| !p.contains(NOTE)),
        "{:?}",
        run.worker
    );
}
