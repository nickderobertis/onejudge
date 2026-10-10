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
//! * `[[say:TEXT]]` / `[[think:TEXT]]` — emit a `text` / `thinking` content
//!   block, which oneharness normalizes into the agent's own `message` /
//!   `reasoning` event. Repeatable, and interleaved with `[[event:…]]` in the
//!   order the markers appear.
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
//! * `[[artifact-evaluator:PATH]]` — on a judge-side turn (one whose prompt
//!   carries the evidence contract), append everything the turn was told to
//!   `PATH`, followed by `=== end of prompt ===`, and answer in that turn's shape:
//!   a completed supervisor decision, a passing boolean or top numeric verdict,
//!   or assessment prose. A worker turn is left to the other markers. It refuses
//!   to answer unless the read-only tool allowlist is exactly today's, so a run
//!   naming artifacts cannot have widened it. `[[artifact-supervisor-turns:N]]`
//!   makes the supervisor continue until it has been asked `N` times (default 1),
//!   so a journey gets more than one judge-side turn to compare.
//! * `[[record-harness:PATH]]` — append one JSON line per invocation to `PATH`:
//!   `{"argv": [...], "stdin": "..."}`, exactly what oneharness spawned this
//!   harness with. It is how a journey holds the *harness* argv and the prompt a
//!   judge was handed to a recorded baseline, on either seam.
//! * `[[evaluate]]` — on a judge-side turn (one whose prompt carries the evidence
//!   contract, the same in every posture), answer in that turn's shape — a completed
//!   supervisor decision, a passing boolean or top numeric verdict, or assessment
//!   prose — whatever tools the turn was granted. A worker turn is left alone.
//! * `[[judge-event:CMD]]` — on a judge-side turn that asked for events, emit one
//!   `Bash` tool call for `CMD` and its result, in order. Repeatable. Worker turns
//!   ignore it, which is what lets one task text script both parties.

use std::io::Write as _;
use std::path::Path;

use oneharness_core::domain::report::OutputFormat;
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
            fail("--descendant needs a handle path");
        };
        descendant(handle);
        return;
    }
    let (prompt, stdin) = steering(&args);
    if let Some(path) = marker(&prompt, "record-harness") {
        record_invocation(&path, &args, &stdin);
    }
    let evaluator = prompt.contains(onejudge::EVIDENCE_PROMPT_MARKER);
    let invoked = invocation(&args);
    let stream = matches!(invoked, Invocation::ClaudeCode { stream: true });
    let opencode = matches!(invoked, Invocation::OpenCode);

    let mut reply = restrictive_evaluator_reply(&prompt, &args)
        .or_else(|| artifact_evaluator_reply(&prompt, &args))
        .or_else(|| evaluated_reply(&prompt, evaluator))
        .or_else(|| marker(&prompt, "reply"))
        .unwrap_or_else(|| "ok".to_string());
    if prompt.contains("[[echo-resume]]") {
        reply = resumed_on(&args).unwrap_or_else(|| "none".to_string());
    }
    // A judge-side turn plays only its own script: the worker's `[[event:…]]`
    // markers reach it inlined in the transcript it is judging.
    let events = if evaluator {
        Vec::new()
    } else {
        activity(&prompt)
    };
    let judge_events = if evaluator {
        markers(&prompt, "judge-event")
    } else {
        Vec::new()
    };

    if opencode {
        // OpenCode's `run --format json` answers with one JSON event per line; the
        // visible answer is the `text` parts, and `sessionID` is the handle
        // oneharness stores for `--session`. Tool parts are not modelled: the
        // journeys that need events drive the claude-code shape below.
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
        for (index, command) in judge_events.iter().enumerate() {
            emit(&format!(
                r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"j{index}","name":"Bash","input":{{"command":{}}}}}]}}}}"#,
                json_string(command)
            ));
            emit(&format!(
                r#"{{"type":"user","message":{{"content":[{{"type":"tool_result","tool_use_id":"j{index}","content":"ran"}}]}}}}"#
            ));
        }
        let mut calls = 0;
        for (kind, text) in &events {
            let block = match kind {
                Activity::Say => format!(r#"{{"type":"text","text":{}}}"#, json_string(text)),
                Activity::Think => {
                    format!(r#"{{"type":"thinking","thinking":{}}}"#, json_string(text))
                }
                Activity::Event => {
                    let block = format!(
                        r#"{{"type":"tool_use","id":"t{calls}","name":"Bash","input":{{"command":{}}}}}"#,
                        json_string(text)
                    );
                    calls += 1;
                    block
                }
            };
            emit(&format!(
                r#"{{"type":"assistant","message":{{"content":[{block}]}}}}"#
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

/// Script the evidence-aware evaluator journey while leaving ordinary doubles
/// untouched. Each next request is selected only from the typed result onejudge
/// appended, so skipping a fixed operation cannot accidentally reach a verdict.
fn restrictive_evaluator_reply(prompt: &str, args: &[String]) -> Option<String> {
    if prompt.contains("[[allowlist-only]]") {
        return Some(if exact_read_only_tools(args) {
            r#"{"value":true,"reason":"exact read-only allowlist"}"#.into()
        } else {
            "read-only tool allowlist drifted".into()
        });
    }
    let proof = marker(prompt, "restrict-evidence")?;
    if !exact_read_only_tools(args) {
        return Some("read-only tool allowlist drifted".into());
    }
    let refused = prompt.matches("Evidence tool request refused:").count();
    if !prompt.contains("Evidence tool result:") {
        return Some(r#"{"tool":"git_status"}"#.into());
    }
    if !prompt.contains("unstaged:\n") {
        if !prompt.contains("MM tracked") || !prompt.contains("?? untracked") {
            return Some("git_status did not expose tracked and untracked changes".into());
        }
        return Some(r#"{"tool":"git_diff"}"#.into());
    }
    if !prompt.contains("+staged") || !prompt.contains("+unstaged") {
        return Some("git_diff did not expose staged and unstaged patches".into());
    }
    if refused == 0 {
        return Some(r#"{"tool":"write_file","path":"escape","text":"changed"}"#.into());
    }
    if refused == 1 {
        return Some(r#"{"tool":"git_diff","args":["--output=escape"]}"#.into());
    }

    let history = prompt
        .lines()
        .find_map(|line| line.strip_prefix("  - "))
        .and_then(|path| std::fs::read_to_string(path).ok())
        .unwrap_or_default();
    if !history.contains("HISTORY_SENTINEL_UNTRUNCATED") {
        return Some("history evidence was not readable".into());
    }
    let kind = if prompt.contains("completion supervisor") {
        "supervisor"
    } else if prompt.contains("Assessment request:") {
        "assessment"
    } else if prompt.contains("Score how well") {
        "numeric"
    } else {
        "boolean"
    };
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(proof)
        .ok()?;
    writeln!(file, "{kind}:read-full-history").ok()?;
    Some(match kind {
        "supervisor" => r#"{"completion":true,"reason":"evidence inspected"}"#.into(),
        "assessment" => "evidence inspected without mutation".into(),
        "numeric" => r#"{"value":10,"reason":"evidence inspected"}"#.into(),
        _ => r#"{"value":true,"reason":"evidence inspected"}"#.into(),
    })
}

/// Append what this invocation was spawned with; see `[[record-harness:PATH]]`.
fn record_invocation(path: &str, args: &[String], stdin: &str) {
    let line = serde_json::json!({ "argv": args, "stdin": stdin });
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap_or_else(|e| fail(&format!("could not open the harness record {path}: {e}")));
    // One write per line: a panel's judges run concurrently and append to the
    // same record, and a line split across two writes can interleave with theirs.
    file.write_all(format!("{line}\n").as_bytes())
        .unwrap_or_else(|e| fail(&format!("could not write the harness record {path}: {e}")));
}

/// Answer a judge-side turn in its own shape; see `[[evaluate]]`.
fn evaluated_reply(prompt: &str, evaluator: bool) -> Option<String> {
    if !evaluator || !prompt.contains("[[evaluate]]") {
        return None;
    }
    Some(if prompt.contains("completion supervisor") {
        r#"{"completion":true,"reason":"evaluated"}"#.into()
    } else if prompt.contains("Assessment request:") {
        "evaluated".into()
    } else if prompt.contains("Score how well") {
        r#"{"value":10,"reason":"evaluated"}"#.into()
    } else {
        r#"{"value":true,"reason":"evaluated"}"#.into()
    })
}

/// Record a judge-side turn's prompt and answer it in its own shape; see the
/// `[[artifact-evaluator:PATH]]` marker.
fn artifact_evaluator_reply(prompt: &str, args: &[String]) -> Option<String> {
    let log = marker(prompt, "artifact-evaluator")?;
    if !prompt.contains(onejudge::EVIDENCE_PROMPT_MARKER) {
        return None;
    }
    if !exact_read_only_tools(args) {
        return Some("read-only tool allowlist drifted".into());
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .ok()?;
    write!(file, "{prompt}\n=== end of prompt ===\n").ok()?;
    drop(file);
    if prompt.contains("completion supervisor") {
        let wanted = marker(prompt, "artifact-supervisor-turns").map_or(1, |n| {
            n.parse::<usize>().unwrap_or_else(|e| {
                fail(&format!(
                    "`[[artifact-supervisor-turns:{n}]]` is not a turn count: {e}"
                ))
            })
        });
        let asked = std::fs::read_to_string(&log)
            .unwrap_or_default()
            .split("=== end of prompt ===")
            .filter(|recorded| recorded.contains("completion supervisor"))
            .count();
        return Some(if asked < wanted {
            r#"{"completion":false,"message":"keep going","reason":"not yet"}"#.into()
        } else {
            r#"{"completion":true,"reason":"artifacts named"}"#.into()
        });
    }
    Some(if prompt.contains("Assessment request:") {
        "artifacts assessed".into()
    } else if prompt.contains("Score how well") {
        r#"{"value":10,"reason":"artifacts named"}"#.into()
    } else {
        r#"{"value":true,"reason":"artifacts named"}"#.into()
    })
}

fn exact_read_only_tools(args: &[String]) -> bool {
    let tools = args.iter().position(|arg| arg == "--tools").map(|index| {
        args[index + 1..]
            .iter()
            .take_while(|arg| !arg.starts_with("--"))
            .map(String::as_str)
            .collect::<Vec<_>>()
    });
    tools.as_deref() == Some(&["Read", "Grep", "Glob", "WebFetch", "WebSearch"][..])
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
/// **One line, never to EOF, for a control stream.** A controlled turn's stdin is
/// held open by oneharness for the whole turn precisely so it can deliver that
/// interrupt — so a read to EOF there waits for a close that waits for this
/// process to answer, and the turn deadlocks; its prompt frame is one line. A
/// large prompt (`--input-format text`) is a plain blob whose stdin oneharness
/// closes after writing, and whose newlines are the prompt's own, so it is read
/// whole.
///
/// The stdin read is returned beside it, so `[[record-harness:…]]` can record the
/// prompt exactly as it was delivered.
fn steering(args: &[String]) -> (String, String) {
    let mut text = args.join("\u{1f}");
    let mut buffer = String::new();
    if let Some(format) = args.windows(2).find(|w| w[0] == "--input-format") {
        let read = match format[1].as_str() {
            "text" => std::io::Read::read_to_string(&mut std::io::stdin().lock(), &mut buffer),
            "stream-json" => std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut buffer),
            other => fail(&format!(
                "`--input-format {other}` is not one oneharness sends"
            )),
        };
        if let Err(e) = read {
            fail(&format!("could not read the prompt from stdin: {e}"));
        }
        if format[1] == "stream-json" {
            check_control_frame(&buffer);
        }
        text.push('\u{1f}');
        text.push_str(&buffer);
    }
    (text, buffer)
}

/// Which harness oneharness spawned this binary as, read from the argv shape its
/// registry entry makes it build — never from a flag of this double's own.
enum Invocation {
    /// `-p … --output-format <format>`, `stream` when the format is `stream-json`.
    ClaudeCode { stream: bool },
    /// `run --format json <message>`: the one harness here whose argv carries no
    /// `-p` / `--output-format` at all.
    OpenCode,
}

/// The harness `args` invoke, or a refusal naming why they are neither: an
/// invocation oneharness would never build is one this stand-in will not answer.
fn invocation(args: &[String]) -> Invocation {
    use oneharness_core::domain::harness::by_id;
    let value = |flag: &str| {
        args.windows(2)
            .find(|pair| pair[0] == flag)
            .map(|pair| pair[1].as_str())
    };
    if args.first().map(String::as_str) == Some("run") {
        if value("--format") != Some("json") {
            fail(&format!(
                "opencode is invoked `run --format json …`, not {args:?}"
            ));
        }
        return Invocation::OpenCode;
    }
    if !args.iter().any(|arg| arg == "-p") {
        fail(&format!(
            "neither claude-code's `-p …` nor opencode's `run …`, as oneharness builds them: {args:?}"
        ));
    }
    let Some(spec) = by_id("claude-code") else {
        fail("oneharness's registry no longer declares claude-code");
    };
    let declared: Vec<OutputFormat> = std::iter::once(spec.output_format)
        .chain(spec.events_format)
        .chain(spec.session_formats.iter().copied())
        .collect();
    let format = value("--output-format")
        .unwrap_or_else(|| fail("claude-code is always given --output-format by oneharness"));
    let Some(format) = declared.iter().find(|declared| declared.as_str() == format) else {
        fail(&format!(
            "`--output-format {format}` is not one claude-code's registry entry declares"
        ));
    };
    Invocation::ClaudeCode {
        stream: *format == OutputFormat::StreamJson,
    }
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

fn marker(text: &str, name: &str) -> Option<String> {
    markers(text, name).into_iter().next()
}

/// One kind of scripted worker activity, named by the marker that scripts it.
#[derive(Clone, Copy)]
enum Activity {
    /// `[[event:CMD]]`: a `Bash` tool call running `CMD`.
    Event,
    /// `[[say:TEXT]]`: visible text.
    Say,
    /// `[[think:TEXT]]`: a reasoning block.
    Think,
}

impl Activity {
    const ALL: [Activity; 3] = [Activity::Event, Activity::Say, Activity::Think];

    fn marker(self) -> &'static str {
        match self {
            Activity::Event => "event",
            Activity::Say => "say",
            Activity::Think => "think",
        }
    }
}

/// The worker's scripted activity — every `[[event:CMD]]`, `[[say:TEXT]]` and
/// `[[think:TEXT]]` in `text` — as `(kind, value)` in the order it appears.
fn activity(text: &str) -> Vec<(Activity, String)> {
    let mut found: Vec<(usize, Activity, String)> = Vec::new();
    for kind in Activity::ALL {
        let open = format!("[[{}:", kind.marker());
        let mut from = 0;
        while let Some(at) = text[from..].find(&open) {
            let start = from + at + open.len();
            let Some(end) = text[start..].find("]]") else {
                break;
            };
            found.push((from + at, kind, text[start..start + end].to_string()));
            from = start + end;
        }
    }
    found.sort_by_key(|(at, ..)| *at);
    found
        .into_iter()
        .map(|(_, kind, value)| (kind, value))
        .collect()
}

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
    writeln!(out, "{line}")
        .and_then(|()| out.flush())
        .unwrap_or_else(|e| fail(&format!("could not write to stdout: {e}")));
}

/// Refuse a control stream whose first line is not exactly the frame
/// oneharness's own `prompt_frame` renders for the prompt it carries.
fn check_control_frame(line: &str) {
    use oneharness_core::domain::control::{prompt_frame, ControlShape};
    let prompt = serde_json::from_str::<serde_json::Value>(line)
        .ok()
        .and_then(|frame| {
            frame
                .pointer("/message/content/0/text")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| {
            fail(&format!(
                "the control stream's first line carries no prompt: {line:?}"
            ))
        });
    if prompt_frame(ControlShape::ClaudeControlRequest, &prompt).as_deref() != Some(line) {
        fail(&format!(
            "the control stream's first line is not oneharness's prompt frame: {line:?}"
        ));
    }
}

fn json_string(value: &str) -> String {
    serde_json::Value::from(value).to_string()
}

/// Report why this harness cannot go on, and exit non-zero — what a real harness
/// does on an error the turn cannot recover from.
fn fail(message: &str) -> ! {
    eprintln!("onejudge-fake-harness: {message}");
    std::process::exit(2);
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
    let exe = std::env::current_exe()
        .unwrap_or_else(|e| fail(&format!("could not resolve this harness's own path: {e}")));
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
        .unwrap_or_else(|e| fail(&format!("could not spawn the descendant: {e}")));
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
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap_or_else(|e| fail(&format!("could not bind a liveness port: {e}")));
    let port = listener
        .local_addr()
        .unwrap_or_else(|e| fail(&format!("could not read the liveness port: {e}")))
        .port();
    // Written whole, then renamed, so a reader never sees a half-written handle.
    let staging = format!("{handle}.partial");
    std::fs::write(&staging, format!("{} {port}", std::process::id()))
        .unwrap_or_else(|e| fail(&format!("could not write the handle {staging}: {e}")));
    std::fs::rename(&staging, handle)
        .unwrap_or_else(|e| fail(&format!("could not publish the handle {handle}: {e}")));
    let deadline = Instant::now() + SILENT_LIMIT;
    while Instant::now() < deadline {
        drop(listener.accept());
    }
}
