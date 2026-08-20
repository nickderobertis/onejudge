//! The oneharness report boundary, read through **oneharness's own contract
//! types** (`oneharness_core::domain::report`) rather than through shadow serde
//! structs declared here.
//!
//! That is the whole point of the `oneharness-core` dependency: the fields
//! onejudge reads are the fields oneharness declares, so an upstream rename is a
//! compile error in this file instead of a silently-null value at runtime, and
//! the failure taxonomy is [`FailureKind`]/[`Status`] — two closed enums — instead
//! of strings matched by hand.
//!
//! Two decisions live here that a stringly-typed reader could not make correctly:
//!
//! * **Which result is the turn.** Under `run_mode = "fallback"` oneharness returns
//!   one result per *attempted* candidate — the ones it fell through, in priority
//!   order, then the one that actually ran. Reading `results[0]` there reports the
//!   fallen-through candidate's `auth`/`quota` refusal as the turn's outcome even
//!   though a later candidate ran the task fine. [`select`] uses the typed
//!   [`FallbackReport`] to pick the candidate that ran, and turns "no candidate
//!   could run at all" into one classified error that names the whole chain.
//! * **What counts as a failure.** A candidate that timed out, could not be
//!   spawned, or was skipped carries no `failure_kind` — its [`Status`] is the
//!   signal. Reading only `failure_kind` makes those a vacuously empty turn, which
//!   the crate's boundary invariant forbids.

use oneharness_core::domain::events::ActionEvent;
use oneharness_core::domain::report::{ExecutionTelemetry, RunReport, RunResult, Status};
use oneharness_core::domain::signals::FailureKind;
use serde::Deserialize;
use serde_json::Value;

use crate::error::{Error, ProviderErrorKind, Result};
use crate::transcript::ToolEvent;
use crate::usage::Usage;

/// One parsed `oneharness run` invocation: oneharness's whole typed report, which
/// of its results is the turn onejudge consumes, and the classified failure it
/// carries (if any).
///
/// A *readable* report is always an `Invocation`, even when the run failed. That
/// is deliberate: the failure is exactly the case whose per-candidate attribution
/// a consumer needs, so the caller records the invocation's telemetry and only
/// then surfaces the failure with [`Invocation::into_ok`].
#[derive(Debug)]
pub(crate) struct Invocation {
    /// oneharness's report, verbatim and typed.
    pub(crate) report: RunReport,
    /// Index into `report.results` of the candidate that actually ran; `None` when
    /// no candidate could run at all.
    pub(crate) ran: Option<usize>,
    /// The per-invocation signals oneharness keeps on its *history* record rather
    /// than on the run report. See [`Supplemental`].
    pub(crate) supplemental: Supplemental,
    /// Why this invocation produced no usable turn, when it did not.
    failure: Option<Error>,
}

/// The invocation signals onejudge reports that `oneharness run`'s report does not
/// carry: oneharness keeps them on the history record (`domain::history`), which is
/// read separately by [`super::history`].
///
/// They are still read off the result object here, because a producer standing in
/// for oneharness on this protocol (the e2e double, a wrapper that inlines its own
/// timings) may supply them directly, and dropping that would silently empty
/// onejudge's own `telemetry` contract. Whichever source provides them, the
/// history read wins when it has a value — it is oneharness's own measurement.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct Supplemental {
    #[serde(default)]
    pub(crate) model_ms: Option<u64>,
    #[serde(default)]
    pub(crate) tool_ms: Option<u64>,
    #[serde(default)]
    pub(crate) time_to_first_token_ms: Option<u64>,
    #[serde(default)]
    pub(crate) started_at: Option<String>,
    #[serde(default)]
    pub(crate) finished_at: Option<String>,
    #[serde(default)]
    pub(crate) history_id: Option<String>,
}

impl Invocation {
    /// Surface the classified failure this invocation carries, if any. Called
    /// after the caller has recorded its telemetry, so a failed run still
    /// attributes itself to the identities that were attempted.
    pub(crate) fn into_ok(mut self) -> Result<Self> {
        match self.failure.take() {
            Some(error) => Err(error),
            None => Ok(self),
        }
    }

    /// The candidate whose result is this turn, when one ran.
    pub(crate) fn result(&self) -> Option<&RunResult> {
        self.ran.map(|index| &self.report.results[index])
    }

    /// The reply text, falling back to raw stdout for the (contractually rare)
    /// case where a harness produced output but oneharness left `text` null.
    pub(crate) fn reply(&self) -> String {
        self.result().map_or_else(String::new, |result| {
            result
                .text
                .clone()
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| result.stdout.clone())
        })
    }

    /// The turn's normalized tool events, lifted from oneharness's [`ActionEvent`].
    pub(crate) fn events(&self) -> Vec<ToolEvent> {
        self.result()
            .and_then(|result| result.events.as_deref())
            .unwrap_or_default()
            .iter()
            .map(tool_event)
            .collect()
    }

    /// The turn's token/cost accounting, or `None` when nothing was reported.
    pub(crate) fn usage(&self) -> Option<Usage> {
        let usage = self.result().map(usage).unwrap_or_default();
        (!usage.is_empty()).then_some(usage)
    }
}

/// The measurements onejudge's `telemetry` contract reports for one candidate,
/// read off **that candidate's own result**.
///
/// Since oneharness report schema `0.5` these ride on [`RunResult::telemetry`], so
/// a consumer reads them off the run it just made. Before that they existed only
/// on the history record, which is why onejudge used to re-open the history file
/// the same run had just written — a second read of a side channel for numbers the
/// report already had. See `docs/oneharness-library.md`.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Measured {
    pub(crate) started_at: Option<String>,
    pub(crate) finished_at: Option<String>,
    pub(crate) model_ms: Option<u64>,
    pub(crate) tool_ms: Option<u64>,
    pub(crate) time_to_first_token_ms: Option<u64>,
}

/// Read one result's [`ExecutionTelemetry`], reporting only what the variant
/// actually claims to have measured.
///
/// The variant is the whole point of reading this typed: each one states a
/// *different* thing, and flattening them would report a guess as a measurement.
/// In particular [`ExecutionTelemetry::StdoutObserved`] is deliberately **not**
/// read as `tool_ms` — it is the union of tool intervals seen at the stdout pipe
/// for a harness whose transcript carries no provider trace, which is a different
/// quantity from the provider-measured split onejudge's contract reports (upstream
/// keeps them in separate history fields for exactly that reason).
pub(crate) fn measured(result: &RunResult) -> Measured {
    match &result.telemetry {
        Some(ExecutionTelemetry::ProviderMeasured {
            started_at,
            finished_at,
            model_ms,
            tool_ms,
            time_to_first_token_ms,
        }) => Measured {
            started_at: Some(started_at.as_str().to_string()),
            finished_at: finished_at.as_ref().map(|at| at.as_str().to_string()),
            model_ms: millis(*model_ms),
            tool_ms: millis(*tool_ms),
            time_to_first_token_ms: millis(*time_to_first_token_ms),
        },
        // A run whose provider trace never completed: when it started is measured,
        // the model/tool split is not. Reporting the bounds is what lets an
        // operator place a failure in time.
        Some(ExecutionTelemetry::PartialInvocation { started_at }) => Measured {
            started_at: Some(started_at.as_str().to_string()),
            ..Measured::default()
        },
        Some(ExecutionTelemetry::StdoutObserved { .. }) | None => Measured::default(),
    }
}

/// A `u128` millisecond measurement narrowed to the `u64` onejudge's telemetry
/// contract reports. A value that cannot fit is not a plausible duration, so it is
/// reported as unknown rather than wrapped into a wrong one.
pub(crate) fn millis(value: Option<u128>) -> Option<u64> {
    value.and_then(|v| u64::try_from(v).ok())
}

/// Lift one of oneharness's normalized actions into onejudge's transcript event.
/// oneharness's [`ActionEvent`] carries history-only lifecycle fields on top of
/// these; the transcript keeps what a consumer asserts on — the four content
/// fields, the ordering, and the call identity that joins a call to its result.
pub(crate) fn tool_event(event: &ActionEvent) -> ToolEvent {
    ToolEvent {
        kind: event.kind.clone(),
        name: event.name.clone(),
        input: event.input.clone(),
        output: event.output.clone(),
        index: event.index,
        // The identity oneharness has carried all along: it is what joins a call
        // to its result, and dropping it left a consumer with no way to say *which*
        // call a live observation belongs to.
        tool_call_id: event.tool_call_id.clone(),
    }
}

/// Lift oneharness's normalized accounting into onejudge's [`Usage`]. Field for
/// field, so a new upstream signal shows up here as a compile error.
pub(crate) fn usage(result: &RunResult) -> Usage {
    Usage {
        input_tokens: result.usage.input_tokens,
        output_tokens: result.usage.output_tokens,
        cache_read_tokens: result.usage.cache_read_tokens,
        cache_write_tokens: result.usage.cache_write_tokens,
        cost_usd: result.usage.cost_usd,
    }
}

/// The half of a control address the oneharness report carries: the session
/// handle the run bound, and the store directory holding both that handle and its
/// `control/<session>.sock`. The turn's working directory is the caller's to add.
pub(crate) struct ControlSocket {
    pub(crate) session: String,
    pub(crate) session_dir: String,
}

/// Read the address of the control socket a `--control` run opened, or `None`
/// when the report names none.
///
/// Both halves come off the report rather than off what onejudge asked for, and
/// that is the point:
///
/// * The **session** is [`SessionReport::name`] — the handle as oneharness
///   sanitized and *stored* it, bound by `finalize_session` to the candidate that
///   actually ran. In a fallback chain the anchor is not necessarily the runner,
///   so the name onejudge passed is not by itself the name `interrupt` will find
///   a record under.
/// * The **store directory** is the socket's own grandparent, because oneharness
///   builds the socket path as `<session-dir>/control/<name>.sock` and canonicalizes
///   it at bind time. Recomputing the platform default here would be a second
///   source for one fact, and would be wrong for any run whose store was configured.
///
/// A socket path that is not valid UTF-8 has no address a caller can pass back on
/// an argv, so it is reported as no address rather than a lossy one.
pub(crate) fn control_socket(report: &RunReport) -> Option<ControlSocket> {
    let control = report.control.as_ref()?;
    let session = report.session.as_ref()?;
    let session_dir = control
        .socket
        .as_path()
        .parent()
        .and_then(std::path::Path::parent)?
        .to_str()?
        .to_string();
    Some(ControlSocket {
        session: session.name.clone(),
        session_dir,
    })
}

/// Map oneharness's closed failure taxonomy onto onejudge's. Total on purpose: a
/// new upstream kind fails to compile here instead of quietly becoming
/// [`ProviderErrorKind::Other`] at some call site.
pub(crate) fn classify(kind: FailureKind) -> ProviderErrorKind {
    match kind {
        FailureKind::Auth => ProviderErrorKind::Auth,
        FailureKind::RateLimit => ProviderErrorKind::RateLimit,
        FailureKind::ModelNotFound => ProviderErrorKind::ModelNotFound,
        FailureKind::Quota => ProviderErrorKind::Quota,
        // A clean exit that only *deferred* a builtin tool call: a real refusal to
        // do the work, but not one of onejudge's specific environment categories.
        FailureKind::ToolDeferred => ProviderErrorKind::Other,
        // The harness never saw the session this run asked to continue. Not one of
        // onejudge's environment categories — the environment is fine and the task
        // is still runnable — so it stays `Other` rather than borrowing a category
        // that would tell a caller to stop trying.
        FailureKind::SessionNotFound => ProviderErrorKind::Other,
    }
}

/// The stable snake_case token oneharness serializes `kind` as, for the attribution
/// a consumer reads.
pub(crate) fn failure_token(kind: FailureKind) -> &'static str {
    kind.as_str()
}

/// The stable kebab-case token oneharness serializes `status` as.
pub(crate) fn status_token(status: Status) -> &'static str {
    match status {
        Status::Ok => "ok",
        Status::Nonzero => "nonzero",
        Status::Timeout => "timeout",
        Status::Cancelled => "cancelled",
        Status::SpawnError => "spawn-error",
        Status::Skipped => "skipped",
        Status::Planned => "planned",
    }
}

/// Build the classified protocol error every unreadable-report path reports.
fn protocol(op: &str, message: impl std::fmt::Display) -> Error {
    Error::provider_classified(op.to_string(), message, ProviderErrorKind::Protocol)
}

/// Parse a `oneharness run` stdout document into its typed report.
pub(crate) fn parse_report(op: &str, stdout: &str) -> Result<Invocation> {
    let value: Value = serde_json::from_str(stdout.trim()).map_err(|e| {
        protocol(
            op,
            format!(
                "oneharness report was not valid JSON: {e}; got: {}",
                stdout.trim()
            ),
        )
    })?;
    parse_report_value(op, value)
}

/// Classify a report oneharness handed back **as a value** — the in-process
/// [`run`](oneharness_core::io::run::run) seam, which never serializes it.
///
/// The selection and failure classification are the same code a spawned run's
/// stdout goes through, so an in-process turn and a spawned one read a fallback
/// chain, a timed-out candidate and an exhausted chain identically.
///
/// [`Supplemental`] is empty here, and correctly so: those are *history*-record
/// fields that a real `RunResult` does not carry (its measurements are on
/// `RunResult::telemetry`, which [`measured`] reads). They exist only for a
/// producer standing in for oneharness on the JSON protocol, which by definition
/// is not this path.
pub(crate) fn parse_report_typed(op: &str, report: RunReport) -> Result<Invocation> {
    let (ran, chain_failure) = select(op, &report)?;
    let failure =
        chain_failure.or_else(|| ran.and_then(|index| self::failure(op, &report.results[index])));
    Ok(Invocation {
        report,
        ran,
        supplemental: Supplemental::default(),
        failure,
    })
}

/// Parse an already-decoded oneharness report document. The streamed protocol's
/// terminal `result` line carries the report as JSON, so both paths land here and
/// a streamed report is read exactly as a bare one is.
pub(crate) fn parse_report_value(op: &str, value: Value) -> Result<Invocation> {
    let report: RunReport = serde_json::from_value(value.clone()).map_err(|e| {
        protocol(
            op,
            format!("oneharness report had an unreadable shape: {e}; got: {value}"),
        )
    })?;
    let (ran, chain_failure) = select(op, &report)?;
    // The history-only signals are read off the SAME result the typed report chose,
    // so the two views can never describe different candidates.
    let supplemental = ran
        .and_then(|index| value.get("results").and_then(|results| results.get(index)))
        .and_then(|result| serde_json::from_value(result.clone()).ok())
        .unwrap_or_default();
    let failure =
        chain_failure.or_else(|| ran.and_then(|index| self::failure(op, &report.results[index])));
    Ok(Invocation {
        report,
        ran,
        supplemental,
        failure,
    })
}

/// Pick the result that is this turn, honouring fallback's "one result per
/// attempted candidate" shape. Returns the index and, when the chain produced no
/// turn at all, the classified failure that says so.
///
/// * No `fallback` block — a parallel/single-harness run: the one result.
/// * `fallback.ran` set — the candidate that ran, matched by its composed id and
///   falling back to the last result (oneharness appends it after the ones it fell
///   through).
/// * `fallback.ran` unset — every candidate failed to start. Nothing executed, so
///   there is no turn: one classified failure naming the whole chain.
///
/// The `Err` arm is reserved for a document that cannot describe a run at all; a
/// run that *failed* still yields an index (or `None`) plus a failure, so the
/// caller can attribute it before surfacing it.
fn select(op: &str, report: &RunReport) -> Result<(Option<usize>, Option<Error>)> {
    if let Some(fallback) = &report.fallback {
        let Some(ran) = fallback.ran.as_deref() else {
            let chain = fallback
                .fell_through
                .iter()
                .map(|f| format!("{} [{}]", f.harness, f.reason))
                .collect::<Vec<_>>()
                .join(", ");
            // Classify by the *last* reason tried: it is the one that decided the
            // chain was exhausted, and it is what a caller retries against.
            let kind = fallback
                .fell_through
                .last()
                .map_or(ProviderErrorKind::Spawn, |f| reason_kind(&f.reason));
            return Ok((
                None,
                Some(Error::provider_classified(
                    op.to_string(),
                    format!(
                        "no oneharness fallback candidate could run the turn; \
                         all {} candidate(s) failed to start ({chain})",
                        fallback.fell_through.len()
                    ),
                    kind,
                )),
            ));
        };
        if report.results.is_empty() {
            return Err(protocol(
                op,
                format!("oneharness reported fallback harness `{ran}` but carried no results"),
            ));
        }
        // Match the composed id first (a model fan-out chain repeats a harness id
        // across candidates); the last result is oneharness's own ordering rule.
        let index = report
            .results
            .iter()
            .rposition(|result| result.harness_id == ran || result.harness == ran)
            .unwrap_or(report.results.len() - 1);
        return Ok((Some(index), None));
    }
    if report.results.is_empty() {
        return Err(protocol(op, "oneharness report carried no results"));
    }
    Ok((Some(0), None))
}

/// Map a fallback `fell_through` reason token to a classified kind. The tokens are
/// oneharness's (`not-installed`, `spawn-error`, `auth`, `quota`,
/// `model-not-found`, `rate-limit`); an unknown one stays unclassified rather than
/// being guessed at.
fn reason_kind(reason: &str) -> ProviderErrorKind {
    match reason {
        "not-installed" | "spawn-error" => ProviderErrorKind::Spawn,
        "auth" => ProviderErrorKind::Auth,
        "quota" => ProviderErrorKind::Quota,
        "model-not-found" => ProviderErrorKind::ModelNotFound,
        "rate-limit" => ProviderErrorKind::RateLimit,
        _ => ProviderErrorKind::Other,
    }
}

/// The classified error for a candidate that did not produce a turn, or `None`
/// when it did.
///
/// `failure_kind` is checked first — it is oneharness's own normalized reason and
/// the finest-grained signal. `status` covers the failures that carry no kind: a
/// timeout, an unspawnable binary, and a candidate that never ran. Those must be
/// loud; oneharness leaves `text` null for them, so treating them as a turn would
/// feed the judge an empty assistant message and score it.
///
/// `Nonzero` is deliberately NOT a failure by itself: a harness can exit non-zero
/// having produced a usable answer, and oneharness's own exit code already carries
/// that signal to [`super::exit_error`].
fn failure(op: &str, result: &RunResult) -> Option<Error> {
    let identity = &result.harness_id;
    if let Some(kind) = result.failure_kind {
        let message = result
            .error
            .clone()
            .unwrap_or_else(|| format!("harness failed ({})", failure_token(kind)));
        return Some(Error::provider_classified(
            op.to_string(),
            message,
            classify(kind),
        ));
    }
    let kind = match result.status {
        Status::Timeout => ProviderErrorKind::Timeout,
        // Torn down before it finished because the run was cancelled — distinct
        // from a timeout, which was given its full deadline and exceeded it. Loud
        // for the same reason: oneharness leaves `text` null, so a cancelled
        // candidate that read as a turn would feed the judge an empty message.
        Status::Cancelled => ProviderErrorKind::Cancelled,
        Status::SpawnError | Status::Skipped => ProviderErrorKind::Spawn,
        Status::Planned => ProviderErrorKind::Protocol,
        Status::Ok | Status::Nonzero => return None,
    };
    let detail = result.error.clone().unwrap_or_else(|| {
        format!(
            "oneharness reported status `{}`",
            status_token(result.status)
        )
    });
    Some(Error::provider_classified(
        op.to_string(),
        format!("harness `{identity}` did not run the turn: {detail}"),
        kind,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oneharness::fixture;

    /// Parse a serialized oneharness report and surface the failure, exactly as the
    /// provider does once it has recorded the invocation's telemetry.
    fn parse(report: &oneharness_core::domain::report::RunReport) -> Result<Invocation> {
        parse_report("respond", &fixture::json(report)).and_then(Invocation::into_ok)
    }

    #[test]
    fn parses_text_usage_and_events_through_the_typed_contract() {
        let mut result = fixture::result("claude-code", "hi");
        result.usage = fixture::usage(3);
        result.events = Some(vec![fixture::event(0, "bash", "ls")]);
        let invocation = parse(&fixture::report(vec![result])).unwrap();

        assert_eq!(invocation.reply(), "hi");
        let usage = invocation.usage().unwrap();
        assert_eq!(usage.input_tokens, Some(3));
        // The prompt-cache reads/writes oneharness reports flow straight through.
        assert_eq!(usage.cache_read_tokens, Some(9));
        assert_eq!(usage.cache_write_tokens, Some(4));
        let lifted = invocation.events();
        assert_eq!(lifted.len(), 1);
        assert_eq!(lifted[0].name.as_deref(), Some("bash"));
        assert_eq!(lifted[0].input.as_ref().unwrap()["command"], "ls");
    }

    #[test]
    fn measurements_are_read_off_the_results_own_telemetry() {
        // Report schema `0.5` puts the measured trace on the result, so these come
        // off the run onejudge just made — no second read of the history file.
        let mut result = fixture::result("claude-code", "hi");
        result.telemetry = Some(fixture::telemetry());
        let invocation = parse(&fixture::report(vec![result])).unwrap();
        assert_eq!(
            measured(invocation.result().unwrap()),
            Measured {
                started_at: Some("2026-01-01T00:00:00.000Z".into()),
                finished_at: Some("2026-01-01T00:00:00.030Z".into()),
                model_ms: Some(11),
                tool_ms: Some(4),
                time_to_first_token_ms: Some(3),
            }
        );
    }

    #[test]
    fn each_telemetry_variant_reports_only_what_it_measured() {
        let measure = |telemetry| {
            let mut result = fixture::result("codex", "hi");
            result.telemetry = telemetry;
            measured(&result)
        };

        // A trace that stopped mid-turn knows when the run began and nothing else:
        // a model/tool split read out of it would be a guess, not a measurement.
        assert_eq!(
            measure(Some(ExecutionTelemetry::PartialInvocation {
                started_at: "2026-01-01T00:00:00Z".parse().expect("a utc instant"),
            })),
            Measured {
                started_at: Some("2026-01-01T00:00:00Z".into()),
                ..Measured::default()
            }
        );

        // Tool intervals seen at the stdout pipe are a different quantity from the
        // provider-measured split this contract reports — upstream keeps them in
        // separate fields — so they are not reported as `tool_ms`.
        assert_eq!(
            measure(Some(ExecutionTelemetry::StdoutObserved { tool_ms: 77 })),
            Measured::default()
        );

        // No telemetry at all is unknown, never zero.
        assert_eq!(measure(None), Measured::default());
    }

    #[test]
    fn implausible_durations_are_reported_as_unknown_not_wrapped() {
        assert_eq!(millis(Some(42)), Some(42));
        assert_eq!(millis(None), None);
        assert_eq!(millis(Some(u128::from(u64::MAX) + 1)), None);
    }

    #[test]
    fn falls_back_to_stdout_when_text_is_null() {
        let mut result = fixture::result("codex", "");
        result.text = None;
        result.stdout = "raw reply".into();
        let invocation = parse(&fixture::report(vec![result])).unwrap();
        assert_eq!(invocation.reply(), "raw reply");
        assert_eq!(invocation.usage(), None);
    }

    #[test]
    fn a_classified_failure_kind_is_the_error() {
        let mut result = fixture::failed("codex", Status::Nonzero, Some(FailureKind::Auth));
        result.error = Some("no key".into());
        let err = parse(&fixture::report(vec![result])).unwrap_err();
        assert_eq!(err.kind(), Some(ProviderErrorKind::Auth));
        assert!(err.to_string().contains("no key"));
    }

    #[test]
    fn a_status_with_no_failure_kind_is_still_loud() {
        // A timeout, an unspawnable binary, and a candidate that never ran all
        // carry a null `failure_kind`. Reading only that field would turn each into
        // an empty assistant turn the judge then scores.
        for (status, expected) in [
            (Status::Timeout, ProviderErrorKind::Timeout),
            // A run oneharness tore down on cancellation reports every cut-short
            // candidate this way, and it is not a timeout: the deadline never
            // expired, the caller stopped waiting.
            (Status::Cancelled, ProviderErrorKind::Cancelled),
            (Status::SpawnError, ProviderErrorKind::Spawn),
            (Status::Skipped, ProviderErrorKind::Spawn),
            (Status::Planned, ProviderErrorKind::Protocol),
        ] {
            let mut result = fixture::failed("goose", status, None);
            result.error = Some("killed after 120s".into());
            let err = parse(&fixture::report(vec![result])).unwrap_err();
            let token = status_token(status);
            assert_eq!(err.kind(), Some(expected), "{token}");
            assert!(err.to_string().contains("goose"), "{token}");
            assert!(err.to_string().contains("killed after 120s"), "{token}");
        }
    }

    #[test]
    fn a_status_ok_or_nonzero_run_is_still_a_turn() {
        // oneharness signals a non-zero harness exit through its own exit code; the
        // text a harness produced on the way out is still the turn.
        for status in [Status::Ok, Status::Nonzero] {
            let mut result = fixture::result("codex", "partial answer");
            result.status = status;
            let invocation = parse(&fixture::report(vec![result])).unwrap();
            assert_eq!(invocation.reply(), "partial answer");
        }
    }

    #[test]
    fn a_fallback_run_reads_the_candidate_that_ran_not_the_first_attempt() {
        // oneharness returns the fallen-through candidates first, then the one that
        // ran. Reading `results[0]` reports the refusal that the chain deliberately
        // routed around as if it were the turn's outcome.
        let fell = fixture::failed("codex", Status::Nonzero, Some(FailureKind::Quota));
        let mut report = fixture::report(vec![fell, fixture::result("claude-code", "done")]);
        report.fallback = Some(fixture::fallback(
            Some("claude-code"),
            &[("codex", "quota")],
        ));

        let invocation = parse(&report).unwrap();
        assert_eq!(invocation.reply(), "done");
        assert_eq!(invocation.result().unwrap().harness, "claude-code");
        assert_eq!(invocation.ran, Some(1));
    }

    #[test]
    fn a_fallback_chain_over_variants_of_one_harness_picks_the_right_identity() {
        // A chain can list several identities of the SAME harness (an account per
        // variant), so the composed id — not the base harness — selects the turn.
        let fell = fixture::failed("codex:personal", Status::Nonzero, Some(FailureKind::Quota));
        let mut report = fixture::report(vec![fell, fixture::result("codex:work", "done")]);
        report.fallback = Some(fixture::fallback(Some("codex:work"), &[("codex", "quota")]));

        let invocation = parse(&report).unwrap();
        assert_eq!(invocation.result().unwrap().harness_id, "codex:work");
        assert_eq!(
            invocation.result().unwrap().variant.as_deref(),
            Some("work")
        );
    }

    #[test]
    fn a_fallback_run_that_stopped_on_a_task_failure_still_reports_that_failure() {
        // The chain does NOT fall through a real task failure, so the candidate that
        // ran is the one that failed — and its failure must reach the caller.
        let ran = fixture::failed(
            "claude-code",
            Status::Nonzero,
            Some(FailureKind::ModelNotFound),
        );
        let mut report = fixture::report(vec![ran]);
        report.fallback = Some(fixture::fallback(Some("claude-code"), &[]));

        let err = parse(&report).unwrap_err();
        assert_eq!(err.kind(), Some(ProviderErrorKind::ModelNotFound));
        assert!(err.to_string().contains("claude-code"));
    }

    #[test]
    fn an_exhausted_fallback_chain_names_every_candidate_and_its_reason() {
        let a = fixture::failed("codex", Status::Skipped, None);
        let b = fixture::failed("claude-code", Status::Nonzero, Some(FailureKind::Auth));
        let mut report = fixture::report(vec![a, b]);
        report.fallback = Some(fixture::fallback(
            None,
            &[("codex", "not-installed"), ("claude-code", "auth")],
        ));

        let err = parse(&report).unwrap_err();
        // Classified by the last candidate tried — what a caller would retry against.
        assert_eq!(err.kind(), Some(ProviderErrorKind::Auth));
        let message = err.to_string();
        assert!(message.contains("codex [not-installed]"), "{message}");
        assert!(message.contains("claude-code [auth]"), "{message}");
    }

    #[test]
    fn an_exhausted_chain_still_carries_every_attempted_candidate() {
        // The failure is exactly the case a consumer needs attribution for, so the
        // parse still yields an invocation to read identities off.
        let mut report = fixture::report(vec![
            fixture::failed("codex", Status::Skipped, None),
            fixture::failed("claude-code", Status::Nonzero, Some(FailureKind::Auth)),
        ]);
        report.fallback = Some(fixture::fallback(
            None,
            &[("codex", "not-installed"), ("claude-code", "auth")],
        ));

        let invocation = parse_report("respond", &fixture::json(&report)).unwrap();
        assert_eq!(invocation.ran, None);
        assert_eq!(invocation.report.results.len(), 2);
        assert!(invocation.into_ok().is_err());
    }

    #[test]
    fn reason_tokens_map_to_the_kinds_a_caller_branches_on() {
        assert_eq!(reason_kind("not-installed"), ProviderErrorKind::Spawn);
        assert_eq!(reason_kind("spawn-error"), ProviderErrorKind::Spawn);
        assert_eq!(reason_kind("auth"), ProviderErrorKind::Auth);
        assert_eq!(reason_kind("quota"), ProviderErrorKind::Quota);
        assert_eq!(
            reason_kind("model-not-found"),
            ProviderErrorKind::ModelNotFound
        );
        assert_eq!(reason_kind("rate-limit"), ProviderErrorKind::RateLimit);
        assert_eq!(reason_kind("something-new"), ProviderErrorKind::Other);
    }

    #[test]
    fn every_oneharness_failure_kind_has_a_onejudge_classification() {
        for (kind, expected) in [
            (FailureKind::Auth, ProviderErrorKind::Auth),
            (FailureKind::RateLimit, ProviderErrorKind::RateLimit),
            (FailureKind::ModelNotFound, ProviderErrorKind::ModelNotFound),
            (FailureKind::Quota, ProviderErrorKind::Quota),
            (FailureKind::ToolDeferred, ProviderErrorKind::Other),
        ] {
            assert_eq!(classify(kind), expected);
            // The token onejudge surfaces is oneharness's own wire spelling.
            assert_eq!(
                serde_json::to_value(kind).unwrap(),
                serde_json::json!(failure_token(kind))
            );
        }
    }

    #[test]
    fn every_status_token_matches_oneharness_wire_spelling() {
        for status in [
            Status::Ok,
            Status::Nonzero,
            Status::Timeout,
            Status::SpawnError,
            Status::Skipped,
            Status::Planned,
        ] {
            assert_eq!(
                serde_json::to_value(status).unwrap(),
                serde_json::json!(status_token(status))
            );
        }
    }

    #[test]
    fn an_unreadable_report_is_a_named_protocol_error() {
        let empty = fixture::json(&fixture::report(vec![]));
        for (json, needle) in [
            ("not json".to_string(), "was not valid JSON"),
            (r#"{"results":[]}"#.to_string(), "unreadable shape"),
            (empty, "carried no results"),
        ] {
            let err = parse_report("respond", &json)
                .and_then(Invocation::into_ok)
                .unwrap_err();
            assert_eq!(err.kind(), Some(ProviderErrorKind::Protocol), "{json}");
            assert!(err.to_string().contains(needle), "{json}: {err}");
        }
    }

    #[test]
    fn supplemental_signals_are_read_off_the_selected_result() {
        // oneharness keeps these on its history record, but a producer standing in
        // for it may inline them — and they must then describe the candidate that
        // RAN, not the one the chain routed around.
        let fell = fixture::failed("codex", Status::Nonzero, Some(FailureKind::Quota));
        let mut report = fixture::report(vec![fell, fixture::result("claude-code", "done")]);
        report.fallback = Some(fixture::fallback(
            Some("claude-code"),
            &[("codex", "quota")],
        ));
        let mut value: Value = serde_json::from_str(&fixture::json(&report)).unwrap();
        value["results"][0]["model_ms"] = serde_json::json!(999);
        value["results"][0]["history_id"] = serde_json::json!("wrong-one");
        value["results"][1]["model_ms"] = serde_json::json!(10);
        value["results"][1]["history_id"] = serde_json::json!("right-one");

        let invocation = parse_report_value("respond", value)
            .and_then(Invocation::into_ok)
            .unwrap();
        assert_eq!(invocation.supplemental.model_ms, Some(10));
        assert_eq!(
            invocation.supplemental.history_id.as_deref(),
            Some("right-one")
        );
    }
}
