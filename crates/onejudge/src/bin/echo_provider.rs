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
//! * `judge` returns `true` (or the numeric high) iff the criterion text appears
//!   in the transcript it is given — **including the rendered tool events** — so an
//!   events-backed criterion is genuinely decided by what the skill did.
//!
//! Built only under the `fake-provider` feature; never shipped to a consumer.
#![allow(missing_docs)]

use std::io::{Read as _, Write as _};

use serde_json::{json, Value};

fn main() {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        fail("could not read request from stdin");
    }
    // Protocol-violation markers, so the e2e suite can drive the engine's error
    // branches across a real subprocess: emit nothing, or exit non-zero.
    if input.contains("[[emit-empty]]") {
        std::process::exit(0);
    }
    if input.contains("[[emit-exit]]") {
        fail("deliberate non-zero exit for the e2e error path");
    }
    let request: Value = match serde_json::from_str(input.trim()) {
        Ok(v) => v,
        Err(e) => fail(&format!("request was not valid JSON: {e}")),
    };
    if let Some(path) = marker(&input, "record") {
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
    let op = request.get("op").and_then(Value::as_str).unwrap_or("");
    let response = match op {
        "respond" => respond(&request),
        "user" => user(&request),
        "supervisor" => supervisor(&request),
        "judge" => judge(&request),
        "assess" => assess(&request),
        other => fail(&format!("unknown op `{other}`")),
    };
    let mut out = serde_json::to_string(&response).expect("response serializes");
    out.push('\n');
    std::io::stdout()
        .write_all(out.as_bytes())
        .expect("write response");
}

/// Print an error to stderr and exit non-zero — the protocol's failure signal.
fn fail(message: &str) -> ! {
    eprintln!("echo-provider: {message}");
    std::process::exit(1);
}

fn latest_user(messages: &[Value]) -> String {
    messages
        .iter()
        .rev()
        .find(|m| m.get("role").and_then(Value::as_str) == Some("user"))
        .and_then(|m| m.get("content").and_then(Value::as_str))
        .unwrap_or("")
        .to_string()
}

fn messages_of(request: &Value) -> Vec<Value> {
    request
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
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

fn respond(request: &Value) -> Value {
    let messages = messages_of(request);
    let latest = latest_user(&messages);
    let instructions = request
        .get("skill")
        .and_then(|s| s.get("instructions"))
        .and_then(Value::as_str)
        .unwrap_or("");
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

fn user(request: &Value) -> Value {
    let persona = request.get("persona").and_then(Value::as_str).unwrap_or("");
    let stop = persona.contains("[[stop]]");
    json!({
        "message": "Thanks — and what about the next step?",
        "stop": stop,
        "usage": { "input_tokens": persona.len(), "output_tokens": 1,
                   "cache_read_tokens": 3, "cache_write_tokens": 1 },
    })
}

fn supervisor(request: &Value) -> Value {
    let persona = request.get("persona").and_then(Value::as_str).unwrap_or("");
    if let Some(spec) = marker(persona, "judge-dwell") {
        dwell(spec);
    }
    // The judge passing the work with the note in hand: completion is answered only
    // on the decision that was re-taken carrying the note, never the one before it.
    if persona.contains("[[complete-on-note]]") {
        let shown = request
            .get("notes")
            .and_then(Value::as_array)
            .is_some_and(|notes| !notes.is_empty());
        return if shown {
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
    let criterion = request
        .get("done_when")
        .and_then(Value::as_str)
        .unwrap_or("");
    let transcript = render(&messages_of(request)).to_lowercase();
    let completion = persona.contains("[[stop]]")
        || (!criterion.is_empty() && transcript.contains(&criterion.to_lowercase()));
    if completion {
        json!({"completion": true, "reason": "completion criterion found in transcript", "usage": {"input_tokens": 1, "output_tokens": 1}})
    } else {
        json!({"completion": false, "message": "Thanks — and what about the next step?", "reason": "completion criterion not yet met", "usage": {"input_tokens": 1, "output_tokens": 1}})
    }
}

/// Render the transcript the judge is given, including tool-event summaries, so a
/// criterion can match on what the skill *did*.
fn render(messages: &[Value]) -> String {
    let mut out = String::new();
    for m in messages {
        let role = m.get("role").and_then(Value::as_str).unwrap_or("");
        let content = m.get("content").and_then(Value::as_str).unwrap_or("");
        out.push_str(role);
        out.push_str(": ");
        out.push_str(content);
        out.push('\n');
        if let Some(events) = m.get("events").and_then(Value::as_array) {
            for e in events {
                if let Some(input) = e.get("input") {
                    out.push_str(&serde_json::to_string(input).unwrap_or_default());
                    out.push('\n');
                }
            }
        }
    }
    out
}

fn judge(request: &Value) -> Value {
    let kind = request
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("boolean");
    let criterion = request
        .get("criterion")
        .and_then(Value::as_str)
        .unwrap_or("");
    let transcript = render(&messages_of(request)).to_lowercase();
    let matched = !criterion.is_empty() && transcript.contains(&criterion.to_lowercase());

    // `[[wrong-type]]` returns the *opposite* value type so the engine's verdict
    // type-check error path is exercised end to end.
    let wrong_type = criterion.contains("[[wrong-type]]");
    let numeric = (kind == "numeric") != wrong_type;
    let value = if numeric {
        let max = request.get("max").and_then(Value::as_f64).unwrap_or(10.0);
        let min = request.get("min").and_then(Value::as_f64).unwrap_or(0.0);
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

fn assess(request: &Value) -> Value {
    let prompt = request.get("prompt").and_then(Value::as_str).unwrap_or("");
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
    let transcript = render(&messages_of(request));
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
