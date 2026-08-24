//! `onejudge-fake-harness` — a deterministic stand-in for a **harness** CLI, so
//! the in-process seam can be driven end to end without a paid model call.
//!
//! The other double, `onejudge-fake-oneharness`, stands in for `oneharness`
//! itself: it is what a provider **spawns**. Since a turn now runs through
//! `oneharness_core::io::run::run`, there is no `oneharness` process to replace —
//! the engine is real and linked in. What is left to fake is the thing oneharness
//! spawns, which is a harness CLI, and that is this binary. Faking one level
//! further down means the whole of oneharness (harness selection, argv
//! construction, event normalization, streaming, cancellation, teardown) is the
//! *real* code under test, and only the model is faked — the same discipline as
//! the other double, one layer deeper.
//!
//! **It is reached the way a deployment pins a binary**, not through a test hook:
//! an `oneharness.toml` with `[harness.claude-code] bin = "<this binary>"`. That
//! is ordinary oneharness config, so the seam the e2e suite drives is one a real
//! caller can drive too.
//!
//! It models two harnesses, selected by the argv oneharness builds — never by a
//! flag of its own, so which one it is playing is decided by the same registry
//! entry a real run would be decided by:
//!
//! * **claude-code** — `-p <prompt> … --output-format <json|stream-json>`. The
//!   requested format is honoured, because that is the whole of what oneharness
//!   parses back: a single `result` document for `json`, and the Anthropic
//!   content-block NDJSON that oneharness normalizes into `events` for
//!   `stream-json`. Under `--control` the prompt arrives as a JSON frame on
//!   stdin (`--input-format stream-json`) instead of positionally.
//! * **opencode** — `run --format json <system>\n\n<prompt>`, answering with the
//!   line-delimited `part` events oneharness reconstructs its text from. It is
//!   here for one reason: its control mechanism drives the turn over its own HTTP
//!   protocol and implements no resume request, which is the case oneharness
//!   0.12 refuses a named session's *continuation* on.
//!
//! # Markers
//!
//! Behaviour is steered by `[[marker:arg]]` tokens in the prompt (which for this
//! harness is the `-p` positional), matching the convention the other doubles use:
//!
//! * `[[reply:TEXT]]` — the final assistant text. Defaults to `ok`.
//! * `[[event:CMD]]` — emit one `Bash` tool call for `CMD`. Repeatable, and
//!   emitted in order, so a streamed run has more than one event to observe.
//! * `[[stream-wait:PATH]]` — after the events, block until `PATH` exists. A
//!   consumer that only saw the events when the turn *ended* would never create
//!   it, so this is what makes incremental delivery provable rather than assumed.
//! * `[[descendant:PATH]]` — spawn a child process that publishes `<pid> <port>`
//!   to `PATH` and then idles, and go **silent forever** afterwards. That is a
//!   real harness with work in flight and nothing more to say: the only thing that
//!   can reap it is oneharness terminating the tree it owns, which is what a
//!   cancelled run must do.
//! * `[[echo-resume]]` — reply with the **native session token this run was
//!   resumed on** (`--resume` for claude-code, `--session` for opencode), or
//!   `none` when the run opened a fresh conversation. A caller-owned handle that
//!   silently started over and one that genuinely continued are otherwise the
//!   same successful turn, and telling them apart is the whole point of the
//!   session-and-control journeys.

use std::io::Write as _;
use std::path::Path;
use std::time::{Duration, Instant};

/// Shared with the other doubles, because the detached children that must stay
/// out of the coverage merge are spawned from more than one binary.
#[path = "support/coverage.rs"]
mod coverage;

use coverage::{detached_profile, publish_profile};

/// How long a marker that waits for the world is given before failing loudly.
/// Generous against a loaded CI box, and far short of a test-runner timeout, so a
/// build that broke incremental delivery reports *why* instead of hanging.
const WAIT_LIMIT: Duration = Duration::from_secs(20);

/// How long the silent arm idles before giving up on its own.
///
/// It exists only so a bug in the *test* cannot leak this process onto a runner
/// forever; the case it models is a harness that never returns, and a cancelled
/// run is expected to reap it long before this.
const SILENT_LIMIT: Duration = Duration::from_secs(60);

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // The re-exec of this binary as the idle descendant: publish the handle the
    // test reads and then answer until something reaps us.
    if args.first().map(String::as_str) == Some("--descendant") {
        let Some(handle) = args.get(1) else {
            eprintln!("onejudge-fake-harness: --descendant needs a handle path");
            std::process::exit(2);
        };
        descendant(handle);
        return;
    }
    let prompt = steering(&args);
    let stream = args
        .windows(2)
        .any(|w| w[0] == "--output-format" && w[1] == "stream-json");
    // OpenCode is the one harness here whose argv carries no `-p`/`--output-format`
    // at all: oneharness builds `run --format json <message>`. Reading the shape
    // rather than being told which harness to be keeps the double honest — it plays
    // whichever one the registry entry under test actually invoked.
    let opencode = args.first().map(String::as_str) == Some("run")
        && args
            .windows(2)
            .any(|w| w[0] == "--format" && w[1] == "json");

    let mut reply = marker(&prompt, "reply").unwrap_or_else(|| "ok".to_string());
    if prompt.contains("[[echo-resume]]") {
        reply = resumed_on(&args).unwrap_or_else(|| "none".to_string());
    }
    let events = markers(&prompt, "event");

    if opencode {
        // OpenCode's `run --format json` answers with one JSON event per line; the
        // visible answer is the `text` parts, and `sessionID` is the handle
        // oneharness stores for `--session`. Tool parts are not modelled: the
        // journeys that need events drive the claude-code shape above.
        emit(&format!(
            r#"{{"type":"text","sessionID":"fake-opencode-session","part":{{"type":"text","text":{}}}}}"#,
            json_string(&reply)
        ));
        return;
    }

    // Spawned *before* the first event, and blocking until it has published: the
    // consumer's cancel is triggered by an event, so a descendant spawned after
    // one could be reaped before it ever existed — which would pass the
    // cancellation journey without proving anything.
    let descendant = marker(&prompt, "descendant");
    if let Some(handle) = &descendant {
        spawn_descendant(handle);
    }

    if stream {
        for (index, command) in events.iter().enumerate() {
            emit(&format!(
                r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"t{index}","name":"Bash","input":{{"command":{}}}}}]}}}}"#,
                json_string(command)
            ));
        }
    }

    if descendant.is_some() {
        // Silent from here: no further write, so nothing this process does can
        // end the turn. Only oneharness tearing the tree down can.
        idle(SILENT_LIMIT);
        return;
    }

    if let Some(path) = marker(&prompt, "stream-wait") {
        wait_for(Path::new(&path));
    }

    let terminal = format!(
        r#"{{"type":"result","subtype":"success","is_error":false,"result":{},"session_id":"fake-harness-session"}}"#,
        json_string(&reply)
    );
    emit(&terminal);
}

/// Everything this run was told, as one string to scan for markers.
///
/// The whole argv rather than just the `-p` positional, because oneharness
/// delivers a onejudge turn's two halves through two different flags — the
/// prompt positionally and the skill's instructions on `--append-system-prompt` —
/// and a journey may steer the double from either. Stdin is folded in for the
/// cases where the command layer moved the prompt off the argv: a large prompt
/// (`--input-format text`), and every `--control` turn, whose prompt is a JSON
/// frame on stdin so the handle can stay open for the interrupt frame afterwards.
///
/// **One line, never to EOF.** A controlled turn's stdin is held open by
/// oneharness for the whole turn precisely so it can deliver that interrupt — so
/// a read to EOF here waits for a close that waits for this process to answer,
/// and the turn deadlocks. One line is the whole prompt frame either way: both
/// input formats oneharness writes are line-delimited.
fn steering(args: &[String]) -> String {
    let mut text = args.join("\u{1f}");
    if args.windows(2).any(|w| w[0] == "--input-format") {
        let mut buffer = String::new();
        let _ = std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut buffer);
        text.push('\u{1f}');
        text.push_str(&buffer);
    }
    text
}

/// The native session token this run was told to continue, or `None` when it
/// opened a fresh conversation.
///
/// Two spellings because the two harnesses spell it differently, and both are
/// oneharness's own argv rather than anything this double chose: claude-code
/// resumes with `--resume <token>`, opencode with `--session <token>`.
fn resumed_on(args: &[String]) -> Option<String> {
    args.windows(2)
        .find(|w| w[0] == "--resume" || w[0] == "--session")
        .map(|w| w[1].clone())
}

/// The first `[[name:value]]` in `text`, if any.
fn marker(text: &str, name: &str) -> Option<String> {
    markers(text, name).into_iter().next()
}

/// Every `[[name:value]]` in `text`, in order.
fn markers(text: &str, name: &str) -> Vec<String> {
    let open = format!("[[{name}:");
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(&open) {
        let after = &rest[start + open.len()..];
        let Some(end) = after.find("]]") else { break };
        out.push(after[..end].to_string());
        rest = &after[end + 2..];
    }
    out
}

/// One NDJSON line, flushed — a buffered write would defeat the very thing the
/// streaming journeys assert.
fn emit(line: &str) {
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

/// `value` as a JSON string literal. Enough for the marker text these doubles
/// carry; a control character or a lone surrogate is not something a marker can
/// express.
fn json_string(value: &str) -> String {
    let escaped = value.replace('\\', r"\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// Block until `path` exists, failing loudly rather than hanging if it never does.
fn wait_for(path: &Path) {
    let deadline = Instant::now() + WAIT_LIMIT;
    while !path.exists() {
        if Instant::now() >= deadline {
            eprintln!(
                "onejudge-fake-harness: `{}` never appeared within {WAIT_LIMIT:?} — the consumer \
                 did not see this turn's events while it was still running",
                path.display()
            );
            std::process::exit(3);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Say nothing at all for `limit`. The point is the silence: a producer with no
/// further output gives its parent no broken pipe to notice.
fn idle(limit: Duration) {
    std::thread::sleep(limit);
}

/// Spawn the idle descendant and block until it has published its handle, so the
/// turn cannot proceed past a descendant a test could not yet observe.
///
/// Deliberately *not* placed in its own process group: it must inherit this
/// harness's, because that group is exactly what oneharness terminates when it
/// tears the tree down. A descendant that escaped the group would prove nothing.
fn spawn_descendant(handle: &str) {
    let exe = std::env::current_exe().expect("the fake harness's own path");
    // The `Child` is dropped, not waited on: dropping it does not kill the
    // process, which is the point — it must survive this harness the way a real
    // harness's own descendants do, so that only oneharness's teardown reaps it.
    // Waiting here would defeat the whole marker; the descendant's own deadline
    // keeps it from leaking onto a runner.
    #[allow(
        clippy::zombie_processes,
        reason = "the descendant must OUTLIVE this harness; that is what the marker models"
    )]
    std::process::Command::new(exe)
        .arg("--descendant")
        .arg(handle)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        // It outlives this harness by design, so it must not write into the
        // profile set `cargo llvm-cov` merges when the suite ends.
        .envs(detached_profile())
        .spawn()
        .expect("the descendant spawns");
    wait_for(Path::new(handle));
}

/// The descendant: publish `<pid> <port>`, then answer on that port until reaped.
///
/// Answering *at all* is the liveness signal a test reads from outside the tree —
/// which is the only vantage point from which "the harness oneharness spawned is
/// really gone" can be asserted.
fn descendant(handle: &str) {
    // Before the handle, so a test that waits on the handle can read this without
    // racing it.
    publish_profile(handle);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a liveness port");
    let port = listener.local_addr().expect("the bound address").port();
    // Written whole, then renamed, so a reader never sees a half-written handle.
    let staging = format!("{handle}.partial");
    std::fs::write(&staging, format!("{} {port}", std::process::id()))
        .expect("the handle is written");
    std::fs::rename(&staging, handle).expect("the handle is published");
    let deadline = Instant::now() + SILENT_LIMIT;
    while Instant::now() < deadline {
        drop(listener.accept());
    }
}
