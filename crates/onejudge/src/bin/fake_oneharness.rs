//! `onejudge-fake-oneharness` — a deterministic stand-in for the real `oneharness`
//! CLI, so the `OneharnessProvider` path is exercised end-to-end (real argv,
//! real subprocess, real report parsing) without a live harness or model.
//!
//! Its report is built from **oneharness's own types**
//! (`oneharness_core::domain::report`) and then serialized, never hand-written
//! JSON. That is what keeps the double honest: the document the e2e suite feeds
//! the real reader is the document oneharness's own contract produces, so a field
//! that changes upstream changes here by construction instead of leaving the suite
//! passing against a shape nothing emits any more.
//!
//! It reads the prompt from stdin (`--prompt-file -`), classifies it by shape
//! (skill respond / simulated user / judge), and emits a `oneharness run` JSON
//! report on stdout. It mirrors the real `run` flag contract (unrecognized flags
//! exit non-zero) and the `oneharness init [PATH] [--force]` subcommand, so both a
//! live-path arg bug and `onejudge init` are caught/covered here. Markers in
//! `--system` steer the skill turn: `[[reply:TEXT]]` sets the reply,
//! `[[event:CMD]]` adds a `bash` tool event, `[[fail:KIND]]` returns a classified
//! `failure_kind`, `[[status:TOKEN]]` returns a terminal status that carries no
//! `failure_kind` at all (`timeout`, `spawn-error`, `skipped`), and
//! `[[reject-session]]` makes a `--session` run exit non-zero with oneharness's
//! `does not support --session` text (the graceful-retry path).
//!
//! **Fallback chains.** `[[fallback:ID|REASON,…]]` reports the run the way
//! `run_mode = "fallback"` does: one result per *attempted* candidate — the listed
//! ones fallen through, in order — then the candidate that ran, with the matching
//! `fallback` block. `[[fallback-exhausted:ID|REASON,…]]` reports the chain where
//! nothing could run (`fallback.ran` null), which is the shape onejudge must turn
//! into one classified error rather than a turn.
//!
//! **Telemetry and history.** The measured trace always rides on the result that
//! ran, as `RunResult::telemetry` (oneharness report schema `0.5`). `[[history:PATH]]`
//! additionally writes the per-candidate history record oneharness writes for every
//! attempt — its real event-sourced JSONL (`HistoryLine::Run`) — to PATH and reports
//! it as `history_file`, so the suite drives onejudge's read of it through
//! oneharness's own reader. That record is the only source of a `history_id`, and
//! its *measurements* are deliberately different sentinels: onejudge must read the
//! result, so a build that re-read the file reports those instead and fails
//! loudly. They still have to be a coherent record — oneharness validates its own
//! run lines on read (`model_ms + tool_ms <= duration_ms`) and drops one that is
//! not, which would take the `history_id` with it. Without
//! the marker there is no record to name, so the id alone is inlined on the result
//! (the "producer that supplies its own id" case onejudge also accepts), which
//! keeps the rest of the suite independent of a shared on-disk store.
//!
//! Under `--stream` it speaks the **streamed provider protocol**
//! (`docs/streaming.md`) instead: one `{"type":"event","event":{…}}` line per tool
//! event, flushed as it is written, then the terminal
//! `{"type":"result","report":{…}}` line. Further markers steer that stream:
//! `[[stream-wait:PATH]]` blocks before the terminal line until `PATH` exists (so
//! a test can prove its events really arrived *during* the turn, and a build that
//! buffered them deadlocks into a loud timeout instead of passing),
//! `[[stream-bare]]` writes the bare report a degraded run writes,
//! `[[stream-garbage]]` a non-JSON line, `[[stream-unknown]]` an envelope type the
//! protocol does not model, `[[stream-truncate]]` ends after the events with no
//! terminal line, `[[stream-trailing:unknown|event|result]]` writes one more line
//! *after* the terminal one, and `[[stream-then-fail]]` writes a complete stream
//! and then exits non-zero.
//!
//! `[[stream-descendant:HANDLE]]` models oneharness's **cancellation** contract
//! instead: it spawns a harness stand-in in its own process group (unreachable by
//! anything onejudge signals), publishes that process's pid and a liveness port to
//! HANDLE, and streams until its stdout breaks — the signal oneharness turns into
//! `StreamStep::Stop` and a `Finish::Terminate` of the tree it owns. A consumer
//! that kills this process instead never gets there, and the stand-in survives.
//!
//! `[[stream-silent-descendant:HANDLE]]` (Unix) models the same contract for a
//! harness that produces **no output**, which is the case a broken pipe cannot
//! reach: it writes one event and then goes silent forever, tearing the stand-in
//! down only on SIGTERM — exactly as real oneharness does, by polling for
//! cancellation on its own slice rather than only when the harness writes.
//!
//! Built only under the `fake-provider` feature; never shipped to a consumer.
#![allow(missing_docs)]

use std::collections::HashMap;
use std::io::{Read as _, Write as _};
use std::time::{Duration, Instant};

use oneharness_core::domain::events::ActionEvent;
use oneharness_core::domain::history::{
    HistoryId, HistoryLabels, HistoryLine, HistoryRecord, HistoryRunRecord,
};
use oneharness_core::domain::mode::PermissionMode;
use oneharness_core::domain::report::{
    ExecutionTelemetry, FallThrough, FallbackReport, OutputFormat, RunReport, RunResult,
    SessionReport, Status,
};
use oneharness_core::domain::session::SessionPhase;
use oneharness_core::domain::signals::{FailureKind, Usage};
use serde_json::{json, Value};

/// The harness this double claims to be, so a result carries a real identity.
const HARNESS: &str = "claude-code";

fn main() {
    // `oneharness init [PATH] [--force]` scaffolds a config file, mirroring the
    // real subcommand so `onejudge init` can be driven end-to-end without a live
    // oneharness. It is a positional-first verb, so handle it before flag parsing.
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.first().map(String::as_str) == Some("init") {
        run_init(&argv[1..]);
    }
    // Not an oneharness verb: this is how the double re-execs itself as the
    // *harness* stand-in for `[[stream-descendant:…]]`. Handled before flag
    // parsing for the same reason `init` is.
    if argv.first().map(String::as_str) == Some("--descendant") {
        let Some(handle) = argv.get(1) else {
            emit_error("--descendant needs a handle path");
        };
        run_descendant(handle);
    }

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

    // Session-degradation path: mimic oneharness rejecting `--session` on a harness
    // that exposes no session id headlessly. onejudge must retry without --session.
    if session.is_some() && system.contains("[[reject-session]]") {
        eprintln!(
            "harness `goose` does not support --session: it exposes no session id \
             headlessly, so a named handle cannot be mapped to it. supported: claude-code"
        );
        std::process::exit(1);
    }

    let is_agent = !(prompt.contains("completion supervisor")
        || prompt.contains("role-playing the USER")
        || prompt.contains("Assessment request:")
        || prompt.contains("Criterion:") && prompt.contains("single-line JSON object"));
    let mut ran = if prompt.contains("completion supervisor") {
        ok_result(supervisor_text(&prompt), &prompt)
    } else if prompt.contains("role-playing the USER") {
        ok_result("Understood — please continue.".into(), &prompt)
    } else if prompt.contains("Criterion:") && prompt.contains("single-line JSON object") {
        ok_result(judge_text(&prompt), &prompt)
    } else if prompt.contains("Assessment request:") {
        // `[[assess-empty]]` yields a well-formed reply with empty text, driving
        // the provider's empty-assessment guard across the real subprocess.
        let text = if prompt.contains("[[assess-empty]]") {
            String::new()
        } else {
            "No follow-up work remains.".into()
        };
        ok_result(text, &prompt)
    } else {
        respond_result(system, session.as_deref(), &prompt)
    };
    if let Some(native) = session
        .as_deref()
        .or(Some(if is_agent { "agent" } else { "judge" }))
    {
        ran.session_id = Some(format!("native-{native}"));
    }

    // A fallback chain reports one result per ATTEMPTED candidate: the ones it fell
    // through first, then the one that ran (or, when exhausted, only the failures).
    let exhausted = marker(system, "fallback-exhausted");
    let chain = exhausted.or_else(|| marker(system, "fallback"));
    let fell_through: Vec<(String, String)> = chain
        .map(|spec| {
            spec.split(',')
                .filter(|entry| !entry.is_empty())
                .map(|entry| {
                    let (id, reason) = entry.split_once('|').unwrap_or((entry, "auth"));
                    (id.to_string(), reason.to_string())
                })
                .collect()
        })
        .unwrap_or_default();
    let mut results: Vec<RunResult> = fell_through
        .iter()
        .map(|(id, reason)| fell_through_result(id, reason))
        .collect();
    let fallback = chain.map(|_| FallbackReport {
        ran: exhausted.is_none().then(|| ran.harness_id.clone()),
        fell_through: fell_through
            .iter()
            .map(|(id, reason)| FallThrough {
                harness: id.split(':').next().unwrap_or(id).to_string(),
                reason: reason.clone(),
            })
            .collect(),
    });
    let ran_index = (exhausted.is_none()).then(|| {
        results.push(ran.clone());
        results.len() - 1
    });

    // The measured trace oneharness puts on the result it ran (report schema
    // `0.5`). A fallen-through candidate gets none — it never reached the boundary
    // a trace is measured between.
    if let Some(index) = ran_index {
        if matches!(results[index].status, Status::Ok) && results[index].failure_kind.is_none() {
            results[index].telemetry = Some(provider_measured(is_agent));
        }
    }

    let failed = ran_index.is_none()
        || results[ran_index.unwrap_or(0)].failure_kind.is_some()
        || !matches!(
            results[ran_index.unwrap_or(0)].status,
            Status::Ok | Status::Nonzero
        );

    // The history record oneharness writes per attempt, when the test asked for it.
    let history_file = marker(system, "history").map(|path| {
        write_history(path, &results);
        path.to_string()
    });

    let report = RunReport {
        schema_version: oneharness_core::domain::report::SCHEMA_VERSION.into(),
        oneharness_version: "0.6.9-fake".into(),
        prompt: prompt.clone(),
        model: None,
        models: None,
        resume: None,
        fork: false,
        session: session.as_deref().map(|name| SessionReport {
            name: name.to_string(),
            phase: SessionPhase::Create,
            token: Some(format!("native-{name}")),
            store_file: None,
        }),
        permission_mode: PermissionMode::Default,
        bypass_permissions: false,
        dry_run: false,
        schema: None,
        schema_max_retries: None,
        batch: None,
        fallback,
        mock_rules: None,
        spy_file: None,
        history_file,
        config_files: Vec::new(),
        results,
    };

    let mut document = serde_json::to_value(&report).expect("the report serializes");
    // Without an on-disk history store there is no record to name, so the id rides
    // on the result instead — the "producer supplies its own history id" case.
    // `[[history:PATH]]` exercises the real store, which is the only source of a
    // record id. The *measurements* are on the result either way, above.
    if report.history_file.is_none() {
        if let Some(index) = ran_index.filter(|_| !failed) {
            let telemetry = inline_telemetry(is_agent, &prompt);
            if let Some(result) = document["results"].get_mut(index) {
                for (key, value) in telemetry {
                    result[key] = value;
                }
            }
        }
    }

    if flags.contains_key("--stream") {
        emit_stream(system, &document);
    } else {
        write_line(&document);
    }
    // oneharness reports a harness failure in the JSON *and* exits non-zero.
    if failed {
        std::process::exit(1);
    }
}

/// The measured trace oneharness reports on the result it ran.
fn provider_measured(is_agent: bool) -> ExecutionTelemetry {
    let at = |instant: &str| instant.parse().expect("a run instant");
    ExecutionTelemetry::ProviderMeasured {
        started_at: at(if is_agent {
            "2026-01-01T00:00:00.000Z"
        } else {
            "2026-01-01T00:00:01.000Z"
        }),
        finished_at: Some(at(if is_agent {
            "2026-01-01T00:00:00.013Z"
        } else {
            "2026-01-01T00:00:01.006Z"
        })),
        model_ms: Some(if is_agent { 10 } else { 5 }),
        tool_ms: Some(if is_agent { 3 } else { 1 }),
        time_to_first_token_ms: Some(if is_agent { 2 } else { 1 }),
    }
}

/// The record id a producer that keeps no history store inlines on its result.
fn inline_telemetry(is_agent: bool, prompt: &str) -> Vec<(&'static str, Value)> {
    let role = if is_agent { "agent" } else { "judge" };
    vec![(
        "history_id",
        json!(format!("history-{role}-{}", prompt.len())),
    )]
}

/// A successful result carrying `text`.
fn ok_result(text: String, prompt: &str) -> RunResult {
    let mut result = base_result(HARNESS);
    result.text = Some(text);
    result.usage = usage(prompt);
    result
}

/// The envelope every result shares.
fn base_result(harness_id: &str) -> RunResult {
    let (harness, variant) = match harness_id.split_once(':') {
        Some((base, variant)) => (base.to_string(), Some(variant.to_string())),
        None => (harness_id.to_string(), None),
    };
    RunResult {
        harness,
        variant,
        harness_id: harness_id.into(),
        bin: harness_id.into(),
        available: true,
        status: Status::Ok,
        prompt: None,
        model: None,
        exit_code: Some(0),
        duration_ms: Some(30),
        telemetry: None,
        command: vec![harness_id.into(), "--print".into()],
        output_format: OutputFormat::Json,
        text: None,
        text_source: Some("json:result".into()),
        usage: Usage::default(),
        usage_source: Some("json".into()),
        session_id: None,
        events: None,
        events_source: None,
        structured: None,
        schema_valid: None,
        schema_attempts: None,
        schema_error: None,
        failure_kind: None,
        failure_kind_source: None,
        stdout: String::new(),
        stderr: String::new(),
        error: None,
    }
}

/// One candidate a fallback chain routed around, shaped the way oneharness shapes
/// it: `not-installed` never ran (`skipped`), the rest were refused before doing
/// any work (a classified `failure_kind` on a non-zero run).
fn fell_through_result(harness_id: &str, reason: &str) -> RunResult {
    let mut result = base_result(harness_id);
    result.exit_code = Some(1);
    result.duration_ms = None;
    result.error = Some(format!("candidate `{harness_id}` could not run ({reason})"));
    match reason {
        "not-installed" => {
            result.status = Status::Skipped;
            result.available = false;
            result.exit_code = None;
        }
        "spawn-error" => result.status = Status::SpawnError,
        other => {
            result.status = Status::Nonzero;
            result.failure_kind = Some(failure_kind(&other.replace('-', "_")));
            result.failure_kind_source = Some("stderr".into());
        }
    }
    result
}

/// oneharness's classified failure for a `[[fail:KIND]]` / fall-through token.
fn failure_kind(token: &str) -> FailureKind {
    match token {
        "auth" => FailureKind::Auth,
        "rate_limit" => FailureKind::RateLimit,
        "model_not_found" => FailureKind::ModelNotFound,
        "quota" => FailureKind::Quota,
        "tool_deferred" => FailureKind::ToolDeferred,
        other => emit_error(&format!(
            "`{other}` is not a oneharness failure_kind (the double mirrors the real taxonomy)"
        )),
    }
}

/// oneharness's terminal status for a `[[status:TOKEN]]` marker.
fn status(token: &str) -> Status {
    match token {
        "ok" => Status::Ok,
        "nonzero" => Status::Nonzero,
        "timeout" => Status::Timeout,
        "spawn-error" => Status::SpawnError,
        "skipped" => Status::Skipped,
        "planned" => Status::Planned,
        other => emit_error(&format!("`{other}` is not a oneharness run status")),
    }
}

/// Write one JSON document as a line on stdout, flushed immediately.
fn write_line(value: &Value) {
    let mut out = serde_json::to_string(value).expect("value serializes");
    out.push('\n');
    let mut stdout = std::io::stdout();
    stdout.write_all(out.as_bytes()).expect("write line");
    stdout.flush().expect("flush line");
}

/// Append oneharness's own per-attempt history lines for `results` to `path`.
///
/// These are `HistoryLine::Run` records in oneharness's event-sourced JSONL, so
/// onejudge reads them back with oneharness's own reader — the real format, not a
/// shape invented here. A fallen-through candidate gets a record with no timing
/// (it never reached the boundary a trace is measured between), exactly as
/// oneharness writes one.
fn write_history(path: &str, results: &[RunResult]) {
    if let Some(parent) = std::path::Path::new(path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        Ok(file) => file,
        Err(e) => emit_error(&format!("could not open history file {path}: {e}")),
    };
    for (index, result) in results.iter().enumerate() {
        let measured = matches!(result.status, Status::Ok) && result.failure_kind.is_none();
        let record = HistoryRecord {
            schema_version: oneharness_core::domain::history::SCHEMA_VERSION.into(),
            history_id: HistoryId::legacy(
                format!("{}-{index}-{}", result.harness_id, results.len()).as_bytes(),
            ),
            session: "fake".into(),
            name: "fake".into(),
            labels: HistoryLabels::default(),
            project: "/tmp".into(),
            timestamp: "2026-01-01T00:00:00Z".into(),
            harness: result.harness.clone(),
            variant: result.variant.clone(),
            harness_id: result.harness_id.clone(),
            model: result.model.clone(),
            prompt: "p".into(),
            permission_mode: PermissionMode::Default,
            status: result.status,
            exit_code: result.exit_code,
            duration_ms: measured.then_some(30),
            // Deliberately NOT the numbers on the result's own telemetry above.
            // Real oneharness writes the same measurement to both places, which is
            // exactly why a test could not otherwise tell which one onejudge read.
            // Since report schema `0.5` the result is the source and this file is
            // consulted for `history_id` alone, so a run that reported these
            // sentinels would be reading the side channel again.
            //
            // They still have to be a *coherent* record: oneharness validates its
            // own run lines on read (`model_ms + tool_ms <= duration_ms`, and a
            // non-empty `started_at`) and silently drops one that does not hold, so
            // an incoherent sentinel would take the `history_id` down with it.
            started_at: measured.then(|| "1999-09-09T09:09:09Z".to_string()),
            finished_at: measured.then(|| "1999-09-09T09:09:09.999Z".to_string()),
            model_ms: measured.then_some(20),
            tool_ms: measured.then_some(9),
            time_to_first_token_ms: measured.then_some(8),
            observed_tool_ms: None,
            text: result.text.clone(),
            text_source: result.text_source.clone(),
            usage: result.usage.clone(),
            session_id: result.session_id.clone(),
            events: result.events.clone(),
            failure_kind: result.failure_kind,
            error: None,
        };
        let line = serde_json::to_string(&HistoryLine::Run(HistoryRunRecord::from_record(&record)))
            .expect("a history record serializes");
        if writeln!(file, "{line}").is_err() {
            emit_error(&format!("could not append a history record to {path}"));
        }
    }
}

/// Publish `report` as the streamed protocol: one `event` envelope per tool event
/// the result carries, then the terminal `result` envelope — or whichever
/// deliberate protocol violation the `[[stream-*]]` markers asked for.
fn emit_stream(system: &str, report: &Value) {
    if system.contains("[[stream-bare]]") {
        // A run that declared streaming but degraded to the one buffered document.
        write_line(report);
        return;
    }
    if system.contains("[[stream-garbage]]") {
        let mut stdout = std::io::stdout();
        stdout.write_all(b"not json at all\n").expect("write line");
        stdout.flush().expect("flush line");
        return;
    }
    let events = report["results"]
        .as_array()
        .and_then(|results| results.last())
        .and_then(|result| result["events"].as_array())
        .cloned()
        .unwrap_or_default();
    for event in &events {
        write_line(&json!({ "type": "event", "event": event }));
    }
    if system.contains("[[stream-unknown]]") {
        write_line(&json!({ "type": "progress", "pct": 50 }));
        return;
    }
    if system.contains("[[stream-truncate]]") {
        // Every event, then silence: the stream never reaches its terminal line.
        return;
    }
    if let Some(handle) = marker(system, "stream-descendant") {
        stream_until_the_consumer_leaves(handle);
    }
    #[cfg(unix)]
    if let Some(handle) = marker(system, "stream-silent-descendant") {
        idle_until_signalled(handle);
    }
    if let Some(path) = marker(system, "stream-wait") {
        wait_for(path);
    }
    write_line(&json!({ "type": "result", "report": report }));
    // Content after the terminal line, which the grammar `event* result EOF`
    // forbids however well-formed the line itself is.
    match marker(system, "stream-trailing") {
        Some("unknown") => write_line(&json!({ "type": "progress", "pct": 100 })),
        Some("event") => {
            write_line(&json!({ "type": "event", "event": tool_event(9, "ls") }));
        }
        Some("result") => write_line(&json!({ "type": "result", "report": report })),
        _ => {}
    }
    if system.contains("[[stream-then-fail]]") {
        // A well-formed stream from a process that then died on teardown.
        emit_error("deliberate non-zero exit after a complete stream");
    }
}

/// Block until `path` exists, so a test's own event handler is what releases the
/// terminal line. A build that buffered the events instead of publishing them
/// never creates it — the bounded wait then fails the run loudly rather than
/// hanging the suite forever.
fn wait_for(path: &str) {
    wait_for_path(path, "the streamed events never reached a live consumer");
}

/// Block until `path` exists, failing loudly after 30s rather than hanging.
fn wait_for_path(path: &str, why: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !std::path::Path::new(path).exists() {
        if Instant::now() >= deadline {
            emit_error(&format!("timed out waiting for {path}: {why}"));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Model oneharness's *cancellation* contract, which is what makes a cancelled
/// turn terminate the harness rather than orphan it.
///
/// Spawn the harness stand-in the way `oneharness_core::io::process` spawns every
/// harness — in its own process group, unreachable by anything onejudge signals —
/// then stream events until the consumer goes away. A failed write is the broken
/// pipe oneharness treats as `StreamStep::Stop`, and like oneharness's
/// `Finish::Terminate` this tears the harness down before exiting. A consumer that
/// kills this process instead never gets here, and leaves the stand-in running.
fn stream_until_the_consumer_leaves(handle: &str) -> ! {
    let mut harness = spawn_harness(handle);
    let mut index = 0usize;
    // The consumer's break reaches us only as a failed write, so keep publishing.
    while write_line_checked(&json!({ "type": "event", "event": tool_event(index, "sleep 600") }))
        .is_ok()
    {
        index += 1;
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = harness.kill();
    let _ = harness.wait();
    std::process::exit(0);
}

/// Model oneharness's cancellation contract for the case a broken pipe cannot
/// reach: a harness that produces **no output at all**.
///
/// Real `oneharness run` (v0.6.9+) installs SIGINT/SIGTERM handlers and polls for
/// cancellation on its own time slice, so it notices even while the harness it is
/// reading from says nothing — and then reaps the tree through `Finish::Terminate`.
/// This models exactly that, and deliberately models nothing else: after the single
/// event above it **never touches stdout again**, so the closed-pipe short-circuit
/// [`stream_until_the_consumer_leaves`] relies on is unobservable here, just as it
/// is for a real silent harness. A consumer that kills this process instead of
/// signalling it never runs the teardown, and the stand-in outlives the turn.
#[cfg(unix)]
fn idle_until_signalled(handle: &str) -> ! {
    let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    if let Err(e) = signal_hook::flag::register(signal_hook::consts::SIGTERM, cancelled.clone()) {
        emit_error(&format!("could not install the cancellation handler: {e}"));
    }
    let mut harness = spawn_harness(handle);
    // One event, published only once the stand-in is live, so the consumer's sink
    // has something to cancel on *and* a running descendant to assert against.
    // After this line stdout is never touched again.
    write_line(&json!({ "type": "event", "event": tool_event(0, "sleep 600") }));
    // Bounded so a *failing* test cannot leak an immortal process onto a runner.
    // Far longer than any assertion window, so it can never make a failure pass.
    let deadline = Instant::now() + Duration::from_secs(30);
    while !cancelled.load(std::sync::atomic::Ordering::SeqCst) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = harness.kill();
    let _ = harness.wait();
    std::process::exit(0);
}

/// Re-exec this binary as the harness stand-in and block until it has published
/// its handle. No inherited pipes: a stand-in holding the consumer's stdout or
/// stderr would keep them from ever reaching EOF.
fn spawn_harness(handle: &str) -> std::process::Child {
    let _ = std::fs::remove_file(handle);
    let exe = std::env::current_exe().unwrap_or_else(|e| emit_error(&format!("current exe: {e}")));
    let mut command = std::process::Command::new(exe);
    command
        .arg("--descendant")
        .arg(handle)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    detach(&mut command);
    let child = command
        .spawn()
        .unwrap_or_else(|e| emit_error(&format!("could not spawn the harness stand-in: {e}")));
    wait_for_path(handle, "the harness stand-in never published its handle");
    child
}

/// Put `command` in its own process group, exactly as oneharness does for a
/// harness, so no signal aimed at this process or its group can reach it.
#[cfg(unix)]
fn detach(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt as _;
    command.process_group(0);
}

/// The Windows equivalent: a new process group, detached from the parent's.
#[cfg(windows)]
fn detach(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt as _;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(CREATE_NEW_PROCESS_GROUP);
}

/// The harness stand-in: publish `<pid> <port>`, then idle on a listening socket
/// so a test can ask from the outside whether this process is still running.
///
/// The self-imposed deadline is hygiene, not behaviour: a *failing* cancellation
/// test must not leak an immortal process onto a CI runner. It is far longer than
/// any assertion window, so it can never make a failing test pass.
fn run_descendant(handle: &str) -> ! {
    let listener =
        std::net::TcpListener::bind("127.0.0.1:0").unwrap_or_else(|e| emit_error(&format!("{e}")));
    let port = listener
        .local_addr()
        .unwrap_or_else(|e| emit_error(&format!("{e}")))
        .port();
    listener
        .set_nonblocking(true)
        .unwrap_or_else(|e| emit_error(&format!("{e}")));
    // Publish via rename so a reader never sees a half-written handle.
    let staged = format!("{handle}.staging");
    let _ = std::fs::write(&staged, format!("{} {port}", std::process::id()));
    let _ = std::fs::rename(&staged, handle);
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        // Answering at all is the liveness signal; the connection itself is not.
        drop(listener.accept());
        std::thread::sleep(Duration::from_millis(10));
    }
    std::process::exit(0);
}

/// Like [`write_line`], but reports a broken pipe instead of panicking on it —
/// the signal a consumer that closed the stream is meant to deliver.
fn write_line_checked(value: &Value) -> std::io::Result<()> {
    let mut out = serde_json::to_string(value).expect("value serializes");
    out.push('\n');
    let mut stdout = std::io::stdout();
    stdout.write_all(out.as_bytes())?;
    stdout.flush()
}

/// A real oneharness never exits non-zero on a *harness* failure without also
/// reporting it in the JSON. So a stdin read failure (a harness-runner bug) is the
/// path that exits 2, matching oneharness's own usage/spawn-error exit code.
fn emit_error(message: &str) -> ! {
    eprintln!("fake-oneharness: {message}");
    std::process::exit(2);
}

/// Scaffold a starter config file, mirroring `oneharness init [PATH] [--force]`:
/// refuse to clobber an existing file without `--force`, else write a minimal
/// valid toml and print the confirmation line the real CLI emits.
fn run_init(args: &[String]) -> ! {
    let force = args.iter().any(|a| a == "--force");
    let path = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .cloned()
        .unwrap_or_else(|| "oneharness.toml".to_string());
    if std::path::Path::new(&path).exists() && !force {
        eprintln!("{path} already exists (use --force to overwrite)");
        std::process::exit(1);
    }
    if std::fs::write(&path, "harnesses = [\"claude-code\"]\n").is_err() {
        eprintln!("could not write {path}");
        std::process::exit(1);
    }
    println!("wrote {path}");
    std::process::exit(0);
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
        "--config",
        "--system",
        "--system-file",
        "--session",
        "--session-dir",
        "--prompt",
        "--prompt-file",
        "--output-format",
        "--cwd",
        "--history-name",
    ];
    const TOGGLES: &[&str] = &["--events", "--compact", "--history", "--stream"];

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

fn usage(text: &str) -> Usage {
    // Deterministic prompt-cache counts so the e2e suite can prove they flow from
    // the oneharness report through the typed reader into the transcript usage.
    Usage {
        input_tokens: Some(text.len() as u64),
        output_tokens: Some(1),
        cache_read_tokens: Some(7),
        cache_write_tokens: Some(2),
        cost_usd: None,
    }
}

/// One normalized tool call, in oneharness's own event shape.
fn tool_event(index: usize, command: &str) -> ActionEvent {
    ActionEvent {
        kind: "tool_call".into(),
        name: Some("bash".into()),
        input: Some(json!({ "command": command })),
        output: None,
        index,
        tool_call_id: None,
        started_at: None,
        finished_at: None,
        duration_ms: None,
        status: None,
        timing_source: None,
    }
}

/// Extract a `[[marker:ARG]]` directive's argument from `text`.
fn marker<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("[[{name}:");
    let start = text.find(&open)? + open.len();
    let rest = &text[start..];
    rest.find("]]").map(|end| &rest[..end])
}

fn respond_result(system: &str, session: Option<&str>, prompt: &str) -> RunResult {
    if let Some(kind) = marker(system, "fail") {
        let mut result = base_result(HARNESS);
        result.status = Status::Nonzero;
        result.exit_code = Some(1);
        result.failure_kind = Some(failure_kind(kind));
        result.failure_kind_source = Some("stderr".into());
        result.error = Some(format!("fake harness failure ({kind})"));
        return result;
    }
    if let Some(token) = marker(system, "status") {
        // A terminal status that carries NO failure_kind — a timeout, an
        // unspawnable binary, a candidate that never ran.
        let mut result = base_result(HARNESS);
        result.status = status(token);
        result.exit_code = None;
        result.error = Some(format!("fake harness ended with status `{token}`"));
        return result;
    }
    // Echo the caller-owned session name back as the reply, so the e2e suite can
    // observe that the engine threaded one name across the real subprocess.
    if system.contains("[[echo-session]]") {
        return ok_result(session.unwrap_or("no-session").to_string(), prompt);
    }
    let reply = marker(system, "reply")
        .map(str::to_string)
        .unwrap_or_else(|| {
            format!(
                "echo: {}",
                prompt.trim().chars().take(60).collect::<String>()
            )
        });
    let mut result = ok_result(reply, prompt);
    if let Some(cmd) = marker(system, "event") {
        result.events = Some(vec![tool_event(0, cmd)]);
        result.events_source = Some("stream-json:content-blocks".into());
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

fn supervisor_text(prompt: &str) -> String {
    let criterion = prompt
        .split("Completion criterion:\n")
        .nth(1)
        .and_then(|s| s.split("\n\n").next())
        .unwrap_or("")
        .to_lowercase();
    let transcript = prompt
        .split("never raw dumps):\n")
        .nth(1)
        .and_then(|s| s.split("\n\nJudge-side").next())
        .unwrap_or("")
        .to_lowercase();
    if !criterion.is_empty() && transcript.contains(&criterion) {
        "{\"completion\":true,\"reason\":\"fake supervisor found criterion\"}".into()
    } else {
        "{\"completion\":false,\"message\":\"Understood — please continue.\",\"reason\":\"not complete\"}".into()
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
