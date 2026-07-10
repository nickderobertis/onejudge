//! `onejudge-fake-oneharness` — a deterministic stand-in for the real `oneharness`
//! CLI, so the `OneharnessProvider` path is exercised end-to-end (real argv,
//! real subprocess, real report parsing) without a live harness or model.
//!
//! It reads the prompt from stdin (`--prompt-file -`), classifies it by shape
//! (skill respond / simulated user / judge), and emits a `oneharness run` JSON
//! report on stdout. It mirrors the real `run` flag contract (unrecognized flags
//! exit non-zero), so a live-path arg bug is caught here. Markers in `--system`
//! steer the
//! skill turn: `[[reply:TEXT]]` sets the reply, `[[event:CMD]]` adds a `bash`
//! tool event, `[[fail:KIND]]` returns a classified `failure_kind`. The judge
//! turn decides `true` iff the criterion appears in the transcript it was given —
//! tool-event lines included — so an events-backed criterion is really decided by
//! what the skill did.
//!
//! Built only under the `fake-provider` feature; never shipped to a consumer.
#![allow(missing_docs)]

use std::collections::HashMap;
use std::io::{Read as _, Write as _};

use serde_json::{json, Value};

fn main() {
    let flags = parse_flags();
    let mut prompt = String::new();
    if std::io::stdin().read_to_string(&mut prompt).is_err() {
        emit_error("could not read prompt from stdin");
    }

    let system = flags.get("--system").map_or("", String::as_str);
    let session = flags.get("--session").cloned();

    // Marker for the e2e non-zero-exit error path: a real oneharness process
    // failure (as opposed to a harness failure, which is reported in the JSON).
    if system.contains("[[proc-exit]]") {
        emit_error("deliberate non-zero exit for the e2e error path");
    }

    let result = if prompt.contains("role-playing the USER") {
        json!({ "status": "ok", "text": "Understood — please continue.", "usage": usage(&prompt) })
    } else if prompt.contains("Criterion:") && prompt.contains("single-line JSON object") {
        json!({ "status": "ok", "text": judge_text(&prompt), "usage": usage(&prompt) })
    } else {
        respond_result(system, session.as_deref(), &prompt)
    };

    let mut report = json!({
        "schema_version": "fake",
        "results": [result],
    });
    if let Some(name) = session {
        report["session"] = json!({ "name": name, "phase": "create" });
    }
    let mut out = serde_json::to_string(&report).expect("report serializes");
    out.push('\n');
    std::io::stdout()
        .write_all(out.as_bytes())
        .expect("write report");
}

/// A real oneharness never exits non-zero on a *harness* failure — it reports it
/// in the JSON. So a stdin read failure (a harness-runner bug) is the only path
/// that exits non-zero, matching oneharness's "spawn/protocol error" behavior.
fn emit_error(message: &str) -> ! {
    eprintln!("fake-oneharness: {message}");
    std::process::exit(2);
}

/// Parse argv, mirroring the real `oneharness run` flag contract so an invalid
/// flag onejudge might pass (e.g. a `--format` that `run` does not accept) is
/// caught here instead of slipping through a lenient double. Unrecognized `--`
/// flags exit non-zero, exactly as the real CLI would.
fn parse_flags() -> HashMap<String, String> {
    // The value-bearing and toggle flags `oneharness run` actually exposes.
    const VALUE_FLAGS: &[&str] = &[
        "--harness",
        "--model",
        "--system",
        "--system-file",
        "--session",
        "--session-dir",
        "--prompt",
        "--prompt-file",
        "--output-format",
    ];
    const TOGGLES: &[&str] = &["--events", "--compact"];

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut flags = HashMap::new();
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if VALUE_FLAGS.contains(&arg) {
            let value = args
                .get(i + 1)
                .cloned()
                .unwrap_or_else(|| emit_error(&format!("{arg} needs a value")));
            flags.insert(arg.to_string(), value);
            i += 2;
            continue;
        }
        if TOGGLES.contains(&arg) {
            flags.insert(arg.to_string(), String::new());
        } else if arg.starts_with("--") {
            emit_error(&format!(
                "unrecognized flag `{arg}` (the fake mirrors `oneharness run`)"
            ));
        }
        // `run` (the subcommand) and any trailing positional fall through.
        i += 1;
    }
    flags
}

fn usage(text: &str) -> Value {
    json!({ "input_tokens": text.len(), "output_tokens": 1 })
}

/// Extract a `[[marker:ARG]]` directive's argument from `text`.
fn marker<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("[[{name}:");
    let start = text.find(&open)? + open.len();
    let rest = &text[start..];
    rest.find("]]").map(|end| &rest[..end])
}

fn respond_result(system: &str, session: Option<&str>, prompt: &str) -> Value {
    if let Some(kind) = marker(system, "fail") {
        return json!({
            "status": "error",
            "failure_kind": kind,
            "error": format!("fake harness failure ({kind})"),
        });
    }
    // Echo the caller-owned session name back as the reply, so the e2e suite can
    // observe that the engine threaded one name across the real subprocess.
    if system.contains("[[echo-session]]") {
        return json!({
            "status": "ok",
            "text": session.unwrap_or("no-session"),
            "usage": usage(prompt),
        });
    }
    let reply = marker(system, "reply")
        .map(str::to_string)
        .unwrap_or_else(|| {
            format!(
                "echo: {}",
                prompt.trim().chars().take(60).collect::<String>()
            )
        });
    let mut result = json!({ "status": "ok", "text": reply, "usage": usage(prompt) });
    if let Some(cmd) = marker(system, "event") {
        result["events"] = json!([{
            "kind": "tool_call",
            "name": "bash",
            "input": { "command": cmd },
            "index": 0
        }]);
    }
    result
}

/// Build a judge verdict as the harness reply text, deciding `true` iff the
/// criterion appears in the transcript portion of the prompt (events included).
fn judge_text(prompt: &str) -> String {
    let criterion = prompt
        .lines()
        .find_map(|l| l.strip_prefix("Criterion: "))
        .unwrap_or("")
        .trim()
        .to_lowercase();
    let transcript = prompt
        .split("lines):\n")
        .nth(1)
        .and_then(|after| after.split("\n\n").next())
        .unwrap_or("")
        .to_lowercase();
    let matched = !criterion.is_empty() && transcript.contains(&criterion);

    if prompt.contains("Score how well") {
        let (min, max) = parse_scale(prompt);
        let value = if matched { max } else { min };
        format!("{{\"value\": {value}, \"reason\": \"fake numeric\"}}")
    } else {
        format!("{{\"value\": {matched}, \"reason\": \"fake boolean\"}}")
    }
}

/// Parse `(min, max)` out of a `scale from X to Y` phrase, defaulting to `(0,10)`.
fn parse_scale(prompt: &str) -> (f64, f64) {
    let tail = match prompt.split("scale from ").nth(1) {
        Some(t) => t,
        None => return (0.0, 10.0),
    };
    let tokens: Vec<&str> = tail.split_whitespace().collect();
    // Shape: "<min> to <max> (inclusive)."
    let min = tokens.first().and_then(|t| t.parse().ok());
    let max = tokens
        .get(2)
        .and_then(|t| t.trim_end_matches(['.', ',']).parse().ok());
    match (min, max) {
        (Some(lo), Some(hi)) => (lo, hi),
        _ => (0.0, 10.0),
    }
}
