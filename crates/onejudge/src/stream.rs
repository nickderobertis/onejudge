//! The **streamed provider protocol**: how onejudge consumes a provider whose
//! stdout is NDJSON — one `{"type":"event","event":{…}}` line the instant each
//! tool event is observed, then a terminal `{"type":"result","report":{…}}` line
//! carrying the same report a buffered provider writes at the end. See
//! `docs/streaming.md`.
//!
//! It exists so a 600–2000 second agent turn is *visible while it runs* instead of
//! only when it ends. The reader is a pure function over a [`BufRead`], separate
//! from the `spawn + wait` shell in [`oneharness`](crate::OneharnessProvider), so
//! every protocol decision below is unit-tested directly.
//!
//! The grammar is exactly `event* result EOF`. Loudness is the point: a provider
//! that declared streaming and then departs from it — bad JSON, a `type` this
//! build has no rule for, a stream that stops before its terminal line, or *any*
//! content after that line — fails with a classified
//! [`ProviderErrorKind::Protocol`] error naming the line, never a silently
//! swallowed event or a vacuously empty turn. The single deliberate tolerance is a
//! line with **no** `type` at all: that is the bare report a provider writes when
//! it did not (or could not) stream, and accepting it is what keeps declaring
//! streaming safe for a backend that degrades.
//!
//! The tagged half of the grammar is oneharness's own
//! [`RunStreamEnvelope`](oneharness_core::domain::report::RunStreamEnvelope): the
//! set of envelope types, the payload each promises, and the shape of an event are
//! oneharness's declarations, so this reader cannot drift from the producer it
//! reads. What stays here is what is onejudge's own: the line framing, the
//! untagged-bare-report tolerance, the sink's short-circuit, and the `EOF` rule.

use std::io::BufRead;
use std::ops::ControlFlow;

use oneharness_core::domain::report::RunStreamEnvelope;
use serde_json::Value;

use crate::error::{Error, ProviderErrorKind, Result};
use crate::oneharness::tool_event;
use crate::transcript::ToolEvent;

/// How consuming a streamed provider's stdout ended.
pub(crate) enum StreamOutcome {
    /// The terminal report, ready for the ordinary report parser — either the
    /// `result` line's `report` object or a bare report document.
    Report(Value),
    /// The event sink returned [`ControlFlow::Break`]: the caller asked to stop, so
    /// the turn was abandoned mid-stream and there is no report.
    Aborted,
}

/// One recognized line of the streamed protocol.
enum StreamLine {
    /// A live tool event.
    Event(ToolEvent),
    /// The terminal report.
    Report(Value),
}

/// Build the classified protocol error every violation below reports.
fn protocol(op: &str, message: impl std::fmt::Display) -> Error {
    Error::provider_classified(op.to_string(), message, ProviderErrorKind::Protocol)
}

/// Consume a streamed provider's stdout, delivering each tool event to `on_event`
/// as it arrives and collecting it into `events`, until the terminal report line
/// (or an abort) ends the turn.
///
/// # Errors
/// A classified [`ProviderErrorKind::Protocol`] error if a line is unreadable, is
/// not valid JSON, is not a JSON object, carries a `type` this protocol does not
/// model, is missing the payload its `type` promises, or if the stream ends with
/// no terminal line.
pub(crate) fn read_stream(
    op: &str,
    reader: &mut dyn BufRead,
    events: &mut Vec<ToolEvent>,
    on_event: &mut dyn FnMut(&ToolEvent) -> ControlFlow<()>,
) -> Result<StreamOutcome> {
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader
            .read_line(&mut line)
            .map_err(|e| protocol(op, format!("could not read the provider stream: {e}")))?;
        if read == 0 {
            return Err(protocol(
                op,
                "the streamed provider ended without a terminal \
                 `{\"type\":\"result\",\"report\":{…}}` line",
            ));
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match parse_line(op, trimmed)? {
            StreamLine::Event(event) => {
                let flow = on_event(&event);
                events.push(event);
                if flow.is_break() {
                    return Ok(StreamOutcome::Aborted);
                }
            }
            StreamLine::Report(report) => {
                // The grammar is `event* result EOF`. Keep reading so EOF is
                // actually checked — that both proves nothing followed the terminal
                // line and drains the pipe, so a child that keeps writing can still
                // exit rather than block on a full one.
                read_eof(op, reader)?;
                return Ok(StreamOutcome::Report(report));
            }
        }
    }
}

/// Consume the rest of `reader`, requiring it to hold nothing but whitespace.
///
/// The terminal `result` line ends the protocol, so anything after it — a further
/// event, a second result, an envelope type this build does not model — is a
/// provider that is not speaking it. Ignoring the trailing bytes would make
/// exactly the malformed output this protocol promises to reject look like a
/// clean run.
fn read_eof(op: &str, reader: &mut dyn BufRead) -> Result<()> {
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader
            .read_line(&mut line)
            .map_err(|e| protocol(op, format!("could not read the provider stream: {e}")))?;
        if read == 0 {
            return Ok(());
        }
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            return Err(protocol(
                op,
                format!(
                    "streamed provider wrote a line after its terminal `result` line; got: {trimmed}"
                ),
            ));
        }
    }
}

/// Classify one non-empty stream line.
fn parse_line(op: &str, line: &str) -> Result<StreamLine> {
    let value: Value = serde_json::from_str(line).map_err(|e| {
        protocol(
            op,
            format!("streamed provider line was not valid JSON: {e}; got: {line}"),
        )
    })?;
    let Some(object) = value.as_object() else {
        return Err(protocol(
            op,
            format!("streamed provider line was not a JSON object; got: {line}"),
        ));
    };
    if !object.contains_key("type") {
        // No discriminator at all: the bare report a provider writes when it did
        // not stream. It is the same document a buffered run writes, so take it as
        // terminal and let the report reader type it.
        return Ok(StreamLine::Report(value));
    }
    // Kept before the envelope consumes the line; the borrow of `value` ends here.
    let report = object.get("report").cloned();
    // Everything tagged is oneharness's grammar, so oneharness's own envelope
    // decides it: an unmodelled `type`, a non-string `type`, a missing payload, and
    // a malformed event are all its rejections, quoted verbatim.
    let envelope: RunStreamEnvelope = serde_json::from_value(value).map_err(|e| {
        protocol(
            op,
            format!(
                "streamed provider line broke the oneharness stream protocol: {e}; got: {line}"
            ),
        )
    })?;
    match envelope {
        RunStreamEnvelope::Event { event } => Ok(StreamLine::Event(tool_event(&event))),
        // Re-take the report from the raw line: the envelope has already proven it
        // is a well-formed report, and the report reader owns which candidate in it
        // is the turn.
        RunStreamEnvelope::Result { .. } => Ok(StreamLine::Report(report.unwrap_or(Value::Null))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oneharness::fixture;

    /// Read `text` as a stream, collecting the events the sink saw.
    fn read(text: &str) -> (Result<StreamOutcome>, Vec<ToolEvent>, usize) {
        let mut bytes = text.as_bytes();
        let mut events = Vec::new();
        let mut seen = 0;
        let outcome = read_stream(
            "respond",
            &mut bytes,
            &mut events,
            &mut |_event: &ToolEvent| {
                seen += 1;
                ControlFlow::Continue(())
            },
        );
        (outcome, events, seen)
    }

    /// The terminal `result` line for a run that replied `text`, built from
    /// oneharness's own report type so the line is one a real producer writes.
    fn result_line(text: &str) -> String {
        let report = fixture::report(vec![fixture::result("claude-code", text)]);
        serde_json::to_string(&serde_json::json!({ "type": "result", "report": report })).unwrap()
    }

    /// One live `event` line for oneharness's normalized action.
    fn event_line(index: usize, name: &str, command: &str) -> String {
        serde_json::to_string(
            &serde_json::json!({ "type": "event", "event": fixture::event(index, name, command) }),
        )
        .unwrap()
    }

    #[test]
    fn events_arrive_before_the_terminal_report() {
        let stream = format!(
            "{}\n\n{}\n{}\n",
            event_line(0, "bash", "ls"),
            event_line(1, "bash", "git status"),
            result_line("done")
        );
        let (outcome, events, seen) = read(&stream);
        assert_eq!(seen, 2);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].name.as_deref(), Some("bash"));
        assert_eq!(events[0].input.as_ref().unwrap()["command"], "ls");
        match outcome.unwrap() {
            StreamOutcome::Report(report) => {
                assert_eq!(report["results"][0]["text"], "done");
            }
            StreamOutcome::Aborted => panic!("expected the terminal report"),
        }
    }

    #[test]
    fn anything_after_the_terminal_result_is_rejected() {
        // `event* result EOF`: once the result has landed the exchange is over, so
        // a further event, a second result, and an unmodelled envelope are all the
        // same violation — a provider that is not speaking this protocol.
        let result = result_line("done");
        for trailing in [
            "{\"type\":\"unknown\"}".to_string(),
            event_line(9, "bash", "rm -rf /"),
            result.clone(),
            "trailing garbage".to_string(),
        ] {
            let (outcome, events, seen) = read(&format!("{result}\n{trailing}\n"));
            let err = outcome
                .err()
                .unwrap_or_else(|| panic!("{trailing} must fail"));
            assert_eq!(err.kind(), Some(ProviderErrorKind::Protocol));
            assert!(
                err.to_string()
                    .contains("wrote a line after its terminal `result` line"),
                "{trailing}: {err}"
            );
            // A trailing event is refused outright, never delivered to the sink.
            assert_eq!((seen, events.len()), (0, 0), "{trailing}");
        }
    }

    #[test]
    fn trailing_blank_lines_after_the_terminal_result_are_fine() {
        // Only *content* after the terminal line is a violation; a trailing newline
        // is how a well-behaved writer ends its output.
        let (outcome, _, _) = read(&format!("{}\n\n   \n", result_line("done")));
        assert!(matches!(outcome.unwrap(), StreamOutcome::Report(_)));
    }

    #[test]
    fn a_bare_report_document_is_still_accepted() {
        // The degraded path: a provider that declared streaming but answered with
        // the one document a non-streaming run writes. It carries no `type`, which
        // is the single tolerance this reader keeps for itself.
        let bare = fixture::json(&fixture::report(vec![fixture::result(
            "claude-code",
            "buffered",
        )]));
        let (outcome, events, seen) = read(&format!("{bare}\n"));
        assert_eq!((seen, events.len()), (0, 0));
        match outcome.unwrap() {
            StreamOutcome::Report(report) => {
                assert_eq!(report["results"][0]["text"], "buffered");
            }
            StreamOutcome::Aborted => panic!("expected the bare report"),
        }
    }

    #[test]
    fn a_breaking_sink_abandons_the_turn_with_the_events_so_far() {
        let stream = format!(
            "{}\n{}\n",
            event_line(0, "bash", "ls"),
            event_line(1, "bash", "pwd")
        );
        let mut bytes = stream.as_bytes();
        let mut events = Vec::new();
        let outcome = read_stream("respond", &mut bytes, &mut events, &mut |_| {
            ControlFlow::Break(())
        })
        .unwrap();
        assert!(matches!(outcome, StreamOutcome::Aborted));
        assert_eq!(events.len(), 1, "the delivered event is still recorded");
    }

    #[test]
    fn every_malformed_line_is_a_named_protocol_error() {
        for (text, needle) in [
            ("not json at all\n".to_string(), "was not valid JSON"),
            ("[1,2,3]\n".to_string(), "was not a JSON object"),
            (
                "{\"type\":\"progress\",\"pct\":10}\n".to_string(),
                "`progress`",
            ),
            ("{\"type\":7}\n".to_string(), "missing string field `type`"),
            ("{\"type\":\"event\"}\n".to_string(), "missing `event`"),
            (
                "{\"type\":\"event\",\"event\":{\"name\":\"bash\"}}\n".to_string(),
                "broke the oneharness stream protocol",
            ),
            ("{\"type\":\"result\"}\n".to_string(), "missing `report`"),
            (
                "{\"type\":\"result\",\"report\":{\"results\":[]}}\n".to_string(),
                "broke the oneharness stream protocol",
            ),
            (
                format!("{}\n", event_line(0, "bash", "ls")),
                "ended without a terminal",
            ),
            (String::new(), "ended without a terminal"),
        ] {
            let (outcome, _, _) = read(&text);
            let err = outcome.err().unwrap_or_else(|| panic!("{text} must fail"));
            assert_eq!(err.kind(), Some(ProviderErrorKind::Protocol));
            assert!(
                err.to_string().contains(needle),
                "expected `{needle}` in: {err}"
            );
        }
    }
}
