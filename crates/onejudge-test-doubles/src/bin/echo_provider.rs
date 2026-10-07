//! `onejudge-echo-provider` — a deterministic `CommandProvider` test double.
//!
//! It speaks the JSON-lines protocol (`docs/protocol.md`): one request object in
//! on stdin, one response object out on stdout. Behavior is driven by conventions
//! so the e2e suite can steer specific journeys without a live model:
//!
//! * `respond` echoes the latest user message. A `[[event:CMD]]` marker anywhere
//!   in the skill instructions or the latest user turn emits a `bash` tool event
//!   running `CMD`; `[[done]]` sets the turn's `done` flag.
//! * `user` replies with a canned continuation; `[[stop]]` in the persona ends it.
//! * `supervisor` completes when `done_when` occurs in the normalized transcript,
//!   otherwise returns one canned next-user message in the same response.
//!   `[[supervisor-noop]]` in the persona instead returns the same valid-looking
//!   instruction that asks for nothing, every time it is asked — the loop the
//!   engine settles after `NOOP_SETTLE_LIMIT` exchanges.
//! * `[[record:PATH]]` anywhere in the request appends the whole request JSON to
//!   `PATH`, one line per call, so a test can assert on exactly what each party was
//!   given across the real subprocess boundary.
//! * `[[worker-dwell:MS:PATH]]` (in the skill instructions or the latest user turn)
//!   and `[[judge-dwell:MS:PATH]]` (in the persona) touch `PATH` and then hold the
//!   turn open for `MS` milliseconds, so a note can be sent *while that party's turn
//!   is live* rather than between turns.
//! * `[[complete-on-note]]` in the persona makes the supervisor answer
//!   `completion:true` exactly once it has been shown a delivered note — the judge
//!   passing the work with the note in hand.
//! * `[[supervisor-exit-on-note]]` in the persona makes the supervisor exit
//!   non-zero exactly once it has been shown a delivered note — a run that fails on
//!   the decision re-taken with the note in hand.
//! * `judge` returns `true` (or the numeric high) iff the criterion text appears
//!   in the transcript it is given — **including the rendered tool events** — so an
//!   events-backed criterion is genuinely decided by what the skill did.
//!
//! **Argv markers.** A judge of a panel is handed the same persona, task and
//! transcript as every other judge, so a journey that needs two judges to behave
//! differently steers each through its own argv instead: every argument after the
//! program name is scanned for markers exactly as the request text is, so
//! `command: [onejudge-echo-provider, "[[supervisor-continue:Add a test.]]"]`
//! configures that judge alone. The `supervisor`-op markers:
//!
//! * `[[supervisor-continue:MSG]]` — always `completion:false` with `MSG`;
//! * `[[supervisor-complete:REASON]]` — always `completion:true` with `REASON`;
//! * `[[supervisor-sleep:MS]]` — hold the decision for `MS` milliseconds first;
//! * `[[supervisor-stamp:PATH]]` — append `<start> <end>` (ms since the epoch)
//!   for the decision to `PATH`, so a journey can prove two judges overlapped;
//! * `[[supervisor-exit]]` — exit non-zero on the `supervisor` op only, leaving
//!   the judge's other ops (`judge`, `assess`) working;
//! * `[[record:PATH]]` — as the request marker, but for this judge's requests
//!   alone, so two judges of one panel log to two files.
//!
//! Lives in the `publish = false` `onejudge-test-doubles` crate; never shipped to a
//! consumer.

use std::io::{Read as _, Write as _};

use onejudge::note::DeliveredNote;
use onejudge::{Message, Role};
use serde::Deserialize;
use serde_json::{json, Value};

/// One protocol request, read into the frame shapes `docs/protocol.md` defines —
/// the fields this double reads, typed, so a request missing one or carrying it
/// in the wrong type is refused here instead of being read as empty. Fields the
/// double does not read (`session`, `task`, `worktree`, `evidence`, ...) are
/// allowed through, as a command must allow them.
#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
enum Request {
    Respond {
        skill: Skill,
        messages: Vec<Message>,
    },
    User {
        persona: String,
    },
    Supervisor {
        persona: String,
        #[serde(default)]
        done_when: Option<String>,
        #[serde(default)]
        notes: Vec<DeliveredNote>,
        turn: Turn,
        messages: Vec<Message>,
    },
    Judge(Judgement),
    Assess {
        prompt: String,
        messages: Vec<Message>,
    },
}

#[derive(Deserialize)]
struct Skill {
    instructions: String,
}

/// The outcome of the worker turn a supervisor decides on.
#[derive(Deserialize)]
#[serde(tag = "outcome", rename_all = "lowercase")]
enum Turn {
    Taken,
    Lost {
        #[allow(dead_code, reason = "read for its shape: a lost turn names its cause")]
        cause: Cause,
    },
}

/// A lost turn's cause as the protocol bounds it: one line of 1 to 80 characters.
#[derive(Deserialize)]
#[serde(try_from = "String")]
struct Cause(#[allow(dead_code, reason = "held only once validated")] String);

impl TryFrom<String> for Cause {
    type Error = String;

    fn try_from(cause: String) -> Result<Self, String> {
        let length = cause.chars().count();
        if !(1..=80).contains(&length) || cause.contains(['\r', '\n']) {
            return Err(format!(
                "a lost turn's cause is one line of 1 to 80 characters, not {cause:?}"
            ));
        }
        Ok(Self(cause))
    }
}

/// A `judge` request, by the kind of verdict it asks for.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum Judgement {
    Boolean {
        criterion: String,
        messages: Vec<Message>,
    },
    Numeric {
        criterion: String,
        min: f64,
        max: f64,
        messages: Vec<Message>,
    },
}

fn main() {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        fail("could not read request from stdin");
    }
    // Markers on this process's own argv, for a judge that has to be steered
    // apart from the others in its panel (see the module docs).
    let argv: String = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    // Protocol-violation markers, so the e2e suite can drive the engine's error
    // branches across a real subprocess: emit nothing, or exit non-zero.
    if input.contains("[[emit-empty]]") {
        std::process::exit(0);
    }
    if input.contains("[[emit-exit]]") {
        fail("deliberate non-zero exit for the e2e error path");
    }
    let request: Request = match serde_json::from_str(input.trim()) {
        Ok(request) => request,
        Err(e) => fail(&format!("request is not a protocol frame: {e}")),
    };
    if let Some(path) = marker(&input, "record").or_else(|| marker(&argv, "record")) {
        let mut line = input.trim().to_string();
        line.push('\n');
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap_or_else(|e| fail(&format!("could not open the request log: {e}")));
        file.write_all(line.as_bytes())
            .unwrap_or_else(|e| fail(&format!("could not write the request log: {e}")));
    }
    if matches!(request, Request::Respond { .. }) {
        if let Some(path) = marker(&input, "emit-exit-once") {
            if !std::path::Path::new(path).exists() {
                std::fs::write(path, b"failed once\n")
                    .unwrap_or_else(|e| fail(&format!("could not write once marker: {e}")));
                fail("deliberate one-time non-zero exit for the e2e recovery path");
            }
        }
    }
    let response = match &request {
        Request::Respond { skill, messages } => respond(&skill.instructions, messages),
        Request::User { persona } => user(persona),
        Request::Supervisor {
            persona,
            done_when,
            notes,
            turn,
            messages,
        } => supervisor(
            &Decision {
                persona,
                done_when: done_when.as_deref().unwrap_or(""),
                noted: !notes.is_empty(),
                turn,
                messages,
            },
            &argv,
        ),
        Request::Judge(judgement) => judge(judgement),
        Request::Assess { prompt, messages } => assess(prompt, messages),
    };
    let mut out = response.to_string();
    out.push('\n');
    std::io::stdout()
        .write_all(out.as_bytes())
        .unwrap_or_else(|e| fail(&format!("could not write the response: {e}")));
}

/// What a `supervisor` request asks this double to decide on.
struct Decision<'a> {
    persona: &'a str,
    done_when: &'a str,
    /// Whether any note has been delivered into the run.
    noted: bool,
    turn: &'a Turn,
    messages: &'a [Message],
}

/// Print an error to stderr and exit non-zero — the protocol's failure signal.
fn fail(message: &str) -> ! {
    eprintln!("echo-provider: {message}");
    std::process::exit(1);
}

fn latest_user(messages: &[Message]) -> &str {
    messages
        .iter()
        .rev()
        .find(|m| m.role == Role::User)
        .map_or("", |m| m.content.as_str())
}

/// Extract the argument of a `[[marker:ARG]]` directive, if present in `text`.
fn marker<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("[[{name}:");
    let start = text.find(&open)? + open.len();
    let rest = &text[start..];
    let end = rest.find("]]")?;
    Some(&rest[..end])
}

/// Touch the marker path and then hold the turn open, so a note can be sent while
/// this party's turn is genuinely live. `MS:PATH`.
fn dwell(spec: &str) {
    let (millis, path) = spec
        .split_once(':')
        .unwrap_or_else(|| fail("a dwell marker is `MS:PATH`"));
    let millis: u64 = millis
        .parse()
        .unwrap_or_else(|e| fail(&format!("a dwell marker's MS is a number: {e}")));
    std::fs::write(path, b"live\n")
        .unwrap_or_else(|e| fail(&format!("could not publish the dwell marker: {e}")));
    std::thread::sleep(std::time::Duration::from_millis(millis));
}

fn respond(instructions: &str, messages: &[Message]) -> Value {
    let latest = latest_user(messages);
    let scope = format!("{instructions}\n{latest}");
    if let Some(spec) = marker(&scope, "worker-dwell") {
        dwell(spec);
    }

    let mut response = json!({
        "message": format!("echo: {latest}"),
        "usage": { "input_tokens": latest.len(), "output_tokens": 1,
                   "cache_read_tokens": 3, "cache_write_tokens": 1 },
    });
    if let Some(cmd) = marker(&scope, "event") {
        response["events"] = json!([{
            "kind": "tool_call",
            "name": "bash",
            "input": { "command": cmd },
            "index": 0
        }]);
    }
    if scope.contains("[[done]]") {
        response["done"] = json!(true);
    }
    response
}

fn user(persona: &str) -> Value {
    let stop = persona.contains("[[stop]]");
    json!({
        "message": "Thanks — and what about the next step?",
        "stop": stop,
        "usage": { "input_tokens": persona.len(), "output_tokens": 1,
                   "cache_read_tokens": 3, "cache_write_tokens": 1 },
    })
}

fn supervisor(request: &Decision<'_>, argv: &str) -> Value {
    let persona = request.persona;
    if let Some(spec) = marker(persona, "judge-dwell") {
        dwell(spec);
    }
    // The argv-steered judge: sleep, stamp, fail, or answer a canned decision,
    // before any of the persona-driven behaviour below is consulted.
    let started = epoch_millis();
    if let Some(millis) = marker(argv, "supervisor-sleep") {
        let millis: u64 = millis
            .parse()
            .unwrap_or_else(|e| fail(&format!("a supervisor-sleep marker's MS is a number: {e}")));
        std::thread::sleep(std::time::Duration::from_millis(millis));
    }
    if let Some(path) = marker(argv, "supervisor-stamp") {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap_or_else(|e| fail(&format!("could not open the stamp file: {e}")));
        file.write_all(format!("{started} {}\n", epoch_millis()).as_bytes())
            .unwrap_or_else(|e| fail(&format!("could not write the stamp file: {e}")));
    }
    if argv.contains("[[supervisor-exit]]") {
        fail("deliberate non-zero exit on the supervisor op");
    }
    if let Some(message) = marker(argv, "supervisor-continue-on-lost") {
        return if matches!(request.turn, Turn::Lost { .. }) {
            json!({"completion": false, "message": message, "reason": "recover the lost turn"})
        } else {
            json!({"completion": true, "reason": "the recovery turn was taken"})
        };
    }
    if argv.contains("[[supervisor-no-instruction]]") {
        return json!({"completion": false, "reason": "no instruction"});
    }
    if let Some(message) = marker(argv, "supervisor-continue") {
        return json!({"completion": false, "message": message, "reason": format!("continue: {message}"), "usage": {"input_tokens": 1, "output_tokens": 1}});
    }
    if let Some(reason) = marker(argv, "supervisor-complete") {
        return json!({"completion": true, "reason": reason, "usage": {"input_tokens": 1, "output_tokens": 1}});
    }
    // The judge failing on the decision re-taken carrying the note.
    if persona.contains("[[supervisor-exit-on-note]]") && request.noted {
        fail("deliberate non-zero exit on the supervisor op shown a note");
    }
    // The judge passing the work with the note in hand: completion is answered only
    // on the decision that was re-taken carrying the note, never the one before it.
    if persona.contains("[[complete-on-note]]") {
        return if request.noted {
            json!({"completion": true, "reason": "the note was in hand when the work was passed", "usage": {"input_tokens": 1, "output_tokens": 1}})
        } else {
            json!({"completion": false, "message": "Thanks — and what about the next step?", "reason": "no note yet", "usage": {"input_tokens": 1, "output_tokens": 1}})
        };
    }
    if let Some(path) = marker(persona, "count") {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap_or_else(|e| fail(&format!("could not open invocation counter: {e}")));
        file.write_all(b"supervisor\n")
            .unwrap_or_else(|e| fail(&format!("could not write invocation counter: {e}")));
    }
    if persona.contains("[[malformed-supervisor]]") {
        return json!({"completion": false});
    }
    // The supervisor that is never done and never asks for anything: a valid,
    // substantive-looking `continue` whose instruction is the verbatim sentence one
    // measured run re-prompted a released dispatch with 137 times.
    if persona.contains("[[supervisor-noop]]") {
        return json!({
            "completion": false,
            "message": "No further action; keep this dispatch released.",
            "reason": "the dispatch is released; nothing further is required",
            "usage": {"input_tokens": 1, "output_tokens": 1},
        });
    }
    let criterion = request.done_when;
    let transcript = render(request.messages).to_lowercase();
    let completion = persona.contains("[[stop]]")
        || (!criterion.is_empty() && transcript.contains(&criterion.to_lowercase()));
    if completion {
        json!({"completion": true, "reason": "completion criterion found in transcript", "usage": {"input_tokens": 1, "output_tokens": 1}})
    } else {
        json!({"completion": false, "message": "Thanks — and what about the next step?", "reason": "completion criterion not yet met", "usage": {"input_tokens": 1, "output_tokens": 1}})
    }
}

/// Milliseconds since the Unix epoch, for the overlap stamps.
fn epoch_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_millis())
}

/// Render the transcript the judge is given, including tool-event summaries, so a
/// criterion can match on what the skill *did*.
fn render(messages: &[Message]) -> String {
    let mut out = String::new();
    for m in messages {
        out.push_str(match m.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::System => "system",
        });
        out.push_str(": ");
        out.push_str(&m.content);
        out.push('\n');
        for input in m.events.iter().filter_map(|e| e.input.as_ref()) {
            out.push_str(&input.to_string());
            out.push('\n');
        }
    }
    out
}

fn judge(judgement: &Judgement) -> Value {
    // A boolean query carries no scale; one answered with the wrong type (below)
    // is scored on the scale the protocol's numeric default would use.
    let (criterion, messages, scale) = match judgement {
        Judgement::Boolean {
            criterion,
            messages,
        } => (criterion, messages, None),
        Judgement::Numeric {
            criterion,
            min,
            max,
            messages,
        } => {
            if min > max {
                fail(&format!(
                    "a numeric judge request's scale runs backwards: {min} to {max}"
                ));
            }
            (criterion, messages, Some((*min, *max)))
        }
    };
    let transcript = render(messages).to_lowercase();
    let matched = !criterion.is_empty() && transcript.contains(&criterion.to_lowercase());

    // `[[wrong-type]]` returns the *opposite* value type so the engine's verdict
    // type-check error path is exercised end to end.
    let wrong_type = criterion.contains("[[wrong-type]]");
    let numeric = scale.is_some() != wrong_type;
    let value = if numeric {
        let (min, max) = scale.unwrap_or((0.0, 10.0));
        json!(if matched { max } else { min })
    } else {
        json!(matched)
    };
    json!({
        "value": value,
        "reason": if matched { "criterion found in transcript" } else { "criterion not found" },
        "usage": { "input_tokens": criterion.len(), "output_tokens": 1,
                   "cache_read_tokens": 3, "cache_write_tokens": 1 },
    })
}

fn assess(prompt: &str, messages: &[Message]) -> Value {
    // `[[assess-empty]]` returns a well-formed reply whose assessment text is
    // empty, so the provider's empty-assessment guard is exercised end to end
    // across the subprocess boundary (a parsed-but-empty reply, not no output).
    if prompt.contains("[[assess-empty]]") {
        return json!({
            "text": "",
            "usage": { "input_tokens": prompt.len(), "output_tokens": 0,
                       "cache_read_tokens": 3, "cache_write_tokens": 1 },
        });
    }
    let transcript = render(messages);
    let tool_note = if transcript.contains("\"command\"") {
        " Tool actions were included."
    } else {
        ""
    };
    json!({
        "text": format!("Assessment for `{prompt}`.{tool_note}"),
        "usage": { "input_tokens": prompt.len(), "output_tokens": 4,
                   "cache_read_tokens": 3, "cache_write_tokens": 1 },
    })
}
