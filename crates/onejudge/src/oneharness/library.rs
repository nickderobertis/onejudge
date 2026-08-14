//! Running a turn **in process**, through `oneharness_core::io::run::run`.
//!
//! This is the seam onejudge executes a turn on. `run` returns the
//! [`RunReport`](oneharness_core::domain::report::RunReport) as a value instead of
//! printing it, publishes each normalized event to a caller-supplied
//! [`EventSink`] *as it occurs*, and takes a [`CancelToken`] that tears each
//! harness tree down through oneharness's own `Finish::Terminate` path — the three
//! things the spawned CLI was being parsed for.
//!
//! **`signal_cancel` stays `false`.** It installs process-global SIGINT/SIGTERM
//! handlers, and oneharness's own docs name this exact split: the CLI sets it, "an
//! embedder with its own signal handling leaves it `false` and cancels
//! `RunControls::cancel` itself". onejudge is an embedder — its host owns the
//! process's signal disposition — so it cancels the token.
//!
//! **Cancelling is the token, not a signal.** onejudge can never signal a harness:
//! oneharness makes each one its own process-group leader precisely so a signal
//! aimed at the runner does not race it. In process there is no longer even an
//! `oneharness` pid to signal — the runner *is* this thread — so the three-rung
//! escalation the spawning seam needs (close stdout, SIGTERM, kill) collapses to
//! one thing oneharness offers directly, and it reaches the case that escalation
//! existed for: a harness that has gone **silent** never observes a closed pipe,
//! but `run` tears its tree down through `Finish::Terminate` regardless.
//!
//! A sink that breaks both trips the token and returns [`SinkStep::Stop`].
//! **Either alone is sufficient** — that is measured, not assumed: reverting
//! each in turn leaves `cancelling_an_in_process_turn_terminates_the_harness_tree_oneharness_owns`
//! green, because they are independent paths into the same teardown (`Stop` ends
//! the stream reader; the token is polled by the runner). Both are set because
//! neither costs anything and onejudge's own break always arrives *on* an event,
//! where which path is consulted first is oneharness's business, not this
//! crate's. What that test does gate is the teardown itself: a build that
//! cancelled through neither leaves a paid harness running, which is the failure
//! this seam exists to not have.

use std::ops::ControlFlow;

use oneharness_core::domain::events::ActionEvent;
use oneharness_core::errors::OneharnessError;
use oneharness_core::io::cancel::CancelToken;
use oneharness_core::io::run::{run, EventSink, RunControls, SinkStep};

use crate::error::{Error, ProviderErrorKind, Result};
use crate::transcript::ToolEvent;

use super::report::{parse_report_typed, tool_event, Invocation};
use super::turn::{self, TurnSpec};

/// What a streamed turn produced.
pub(crate) enum Streamed {
    /// The sink asked to stop mid-turn: the events it saw are the whole turn.
    Aborted(Vec<ToolEvent>),
    /// The turn ran to a report, which is read exactly as a buffered one is.
    /// Boxed because a whole `RunReport` dwarfs the event list beside it, and
    /// every caller matches on the variant rather than holding the enum.
    Finished(Box<Invocation>),
}

/// Run one buffered turn and return its parsed invocation.
///
/// The report is classified by the same code a spawned run's stdout was, so a
/// failed chain, a timed-out candidate and a fallback that routed around one all
/// land on the same [`ProviderErrorKind`] they always did.
pub(crate) fn run_buffered(op: &str, spec: &TurnSpec) -> Result<Invocation> {
    let mut request = turn::request(spec);
    // A buffered turn publishes nothing incrementally, so it must not ask to.
    request.stream = Some(false);
    let outcome = run(
        &request,
        RunControls {
            events: None,
            cancel: CancelToken::new(),
            signal_cancel: false,
            version: None,
        },
    )
    .map_err(|e| failed(op, &e))?;
    parse_report_typed(op, outcome.report)
}

/// Run one streamed turn, delivering each tool event to `on_event` the instant
/// oneharness observes it.
pub(crate) fn run_streaming(
    op: &str,
    spec: &TurnSpec,
    on_event: &mut dyn FnMut(&ToolEvent) -> ControlFlow<()>,
) -> Result<Streamed> {
    let mut request = turn::request(spec);
    request.stream = Some(true);
    let cancel = CancelToken::new();
    let mut sink = TurnEvents {
        on_event,
        seen: Vec::new(),
        cancel: cancel.clone(),
        aborted: false,
    };
    let outcome = run(
        &request,
        RunControls {
            events: Some(&mut sink),
            cancel,
            signal_cancel: false,
            version: None,
        },
    );
    let TurnEvents { seen, aborted, .. } = sink;
    // An abandoned turn is over either way: the report a cancelled run still
    // returns describes a harness that was torn down, not the turn the caller
    // asked for, and an error raised by the teardown is not the caller's finding.
    if aborted {
        return Ok(Streamed::Aborted(seen));
    }
    let outcome = outcome.map_err(|e| failed(op, &e))?;
    Ok(Streamed::Finished(Box::new(parse_report_typed(
        op,
        outcome.report,
    )?)))
}

/// The sink onejudge hands `run`: it lifts each of oneharness's own
/// [`ActionEvent`]s into the engine's [`ToolEvent`] and offers it to the caller
/// before the turn ends.
struct TurnEvents<'a> {
    on_event: &'a mut dyn FnMut(&ToolEvent) -> ControlFlow<()>,
    /// Every event delivered so far, so an aborted turn can still report what it
    /// observed — which is the whole of what a short-circuiting caller gets.
    seen: Vec<ToolEvent>,
    cancel: CancelToken,
    aborted: bool,
}

impl EventSink for TurnEvents<'_> {
    fn event(&mut self, _harness_id: &str, event: &ActionEvent) -> SinkStep {
        let event = tool_event(event);
        let step = (self.on_event)(&event);
        self.seen.push(event);
        if step.is_break() {
            self.aborted = true;
            // Both paths into the same teardown; see this module's header for why
            // neither is claimed to reach a case the other cannot.
            self.cancel.cancel();
            return SinkStep::Stop;
        }
        SinkStep::Continue
    }
}

/// The classified error for a `run` that could not produce a report at all.
///
/// oneharness reports a *harness* failure in the report (which is why a non-zero
/// run is still parsed), so everything that reaches here is oneharness refusing
/// the call before it could run one — a rejected `--session`, a refused
/// `--control`, an unreadable config. Those are exactly what the spawned seam
/// classified from a non-zero exit and stderr, so they keep that classification,
/// and oneharness's own words stay in the message: the `--session` and `--control`
/// recoveries both key off them.
fn failed(op: &str, err: &OneharnessError) -> Error {
    Error::provider_classified(
        op.to_string(),
        format!("oneharness run failed: {err}"),
        ProviderErrorKind::Protocol,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refused_session_keeps_oneharnesss_own_words_so_the_retry_can_key_off_them() {
        let err = failed(
            "respond",
            &OneharnessError::SessionUnsupported {
                id: "goose".into(),
                supported: "claude-code, codex".into(),
            },
        );
        assert!(super::super::is_session_unsupported(&err));
        assert!(matches!(
            err,
            Error::Provider {
                kind: Some(ProviderErrorKind::Protocol),
                ..
            }
        ));
    }

    #[test]
    fn a_refused_control_ask_is_recognised_as_a_refusal_not_a_failed_turn() {
        let err = failed(
            "respond",
            &OneharnessError::ControlUnsupported {
                id: "goose".into(),
                supported: "claude-code".into(),
            },
        );
        assert!(super::super::is_control_refused(&err));
    }
}
