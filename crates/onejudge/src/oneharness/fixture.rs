//! Typed oneharness fixtures for this crate's unit tests.
//!
//! Every fixture is **built as oneharness's own struct and then serialized**,
//! never hand-written JSON. That is what keeps a unit test honest: the document a
//! test feeds the reader is exactly the document oneharness's types produce, so a
//! field added or renamed upstream changes the fixture by construction instead of
//! leaving a stale literal that only the fixture still believes in.

use oneharness_core::domain::events::ActionEvent;
use oneharness_core::domain::history::{HistoryId, HistoryLabels, HistoryRecord};
use oneharness_core::domain::mode::PermissionMode;
use oneharness_core::domain::report::{
    ExecutionTelemetry, FallThrough, FallbackReport, OutputFormat, RunReport, RunResult, Status,
};
use oneharness_core::domain::signals::{FailureKind, Usage};

/// One result for `harness`, successful and carrying `text`.
pub(crate) fn result(harness_id: &str, text: &str) -> RunResult {
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
        command: vec![harness_id.into()],
        output_format: OutputFormat::Json,
        text: Some(text.into()),
        text_source: Some("json:result".into()),
        usage: Usage::default(),
        usage_source: None,
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

/// One result that failed with oneharness's classified `kind`.
pub(crate) fn failed(harness_id: &str, status: Status, kind: Option<FailureKind>) -> RunResult {
    let mut result = result(harness_id, "");
    result.text = None;
    result.status = status;
    result.exit_code = Some(1);
    result.failure_kind = kind;
    result.failure_kind_source = kind.map(|_| "stderr".into());
    result.error = Some(format!("fixture failure for `{harness_id}`"));
    result
}

/// The token/cost accounting a harness reports.
pub(crate) fn usage(input: u64) -> Usage {
    Usage {
        input_tokens: Some(input),
        output_tokens: Some(1),
        cache_read_tokens: Some(9),
        cache_write_tokens: Some(4),
        cost_usd: None,
    }
}

/// One normalized tool call.
pub(crate) fn event(index: usize, name: &str, command: &str) -> ActionEvent {
    ActionEvent {
        kind: "tool_call".into(),
        name: Some(name.into()),
        input: Some(serde_json::json!({ "command": command })),
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

/// A `oneharness run` report carrying `results`.
pub(crate) fn report(results: Vec<RunResult>) -> RunReport {
    RunReport {
        schema_version: oneharness_core::domain::report::SCHEMA_VERSION.into(),
        oneharness_version: "0.6.8".into(),
        prompt: "p".into(),
        model: None,
        models: None,
        resume: None,
        fork: false,
        session: None,
        permission_mode: PermissionMode::Default,
        bypass_permissions: false,
        dry_run: false,
        schema: None,
        schema_max_retries: None,
        batch: None,
        fallback: None,
        mock_rules: None,
        spy_file: None,
        history_file: None,
        config_files: Vec::new(),
        results,
    }
}

/// A complete provider-measured trace, as oneharness puts it on a result since
/// report schema `0.5`.
pub(crate) fn telemetry() -> ExecutionTelemetry {
    ExecutionTelemetry::ProviderMeasured {
        started_at: "2026-01-01T00:00:00.000Z".parse().expect("a run instant"),
        finished_at: Some("2026-01-01T00:00:00.030Z".parse().expect("a run instant")),
        model_ms: Some(11),
        tool_ms: Some(4),
        time_to_first_token_ms: Some(3),
    }
}

/// The fallback block for a chain that settled on `ran` (or nowhere).
pub(crate) fn fallback(ran: Option<&str>, fell_through: &[(&str, &str)]) -> FallbackReport {
    FallbackReport {
        ran: ran.map(str::to_string),
        fell_through: fell_through
            .iter()
            .map(|(harness, reason)| FallThrough {
                harness: (*harness).into(),
                reason: (*reason).into(),
            })
            .collect(),
    }
}

/// One history record for `harness_id`, with a complete provider-measured trace.
pub(crate) fn record(harness_id: &str) -> HistoryRecord {
    let (harness, variant) = match harness_id.split_once(':') {
        Some((base, variant)) => (base.to_string(), Some(variant.to_string())),
        None => (harness_id.to_string(), None),
    };
    HistoryRecord {
        schema_version: oneharness_core::domain::history::SCHEMA_VERSION.into(),
        history_id: HistoryId::legacy(harness_id.as_bytes()),
        session: "s".into(),
        name: "s".into(),
        labels: HistoryLabels::default(),
        project: "/tmp".into(),
        timestamp: "2026-01-01T00:00:00Z".into(),
        harness,
        variant,
        harness_id: harness_id.into(),
        model: None,
        prompt: "p".into(),
        permission_mode: PermissionMode::Default,
        status: Status::Ok,
        exit_code: Some(0),
        duration_ms: Some(30),
        started_at: Some("2026-01-01T00:00:00Z".into()),
        finished_at: Some("2026-01-01T00:00:00.030Z".into()),
        model_ms: Some(10),
        tool_ms: Some(3),
        time_to_first_token_ms: Some(2),
        observed_tool_ms: None,
        text: Some("hi".into()),
        text_source: Some("json:result".into()),
        usage: Usage::default(),
        session_id: None,
        events: None,
        failure_kind: None,
        error: None,
    }
}

/// `report` as the single-line JSON document `oneharness run --compact` writes.
pub(crate) fn json(report: &RunReport) -> String {
    serde_json::to_string(report).expect("a oneharness report serializes")
}
