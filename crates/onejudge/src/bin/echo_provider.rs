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
    let op = request.get("op").and_then(Value::as_str).unwrap_or("");
    let response = match op {
        "respond" => respond(&request),
        "user" => user(&request),
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

fn respond(request: &Value) -> Value {
    let messages = messages_of(request);
    let latest = latest_user(&messages);
    let instructions = request
        .get("skill")
        .and_then(|s| s.get("instructions"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let scope = format!("{instructions}\n{latest}");

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
