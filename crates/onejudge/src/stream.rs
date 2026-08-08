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
//! Loudness is the point: a provider that declared streaming and then wrote a line
//! this protocol does not model (bad JSON, a `type` it has no rule for, a stream
//! that stops before its terminal line) fails with a classified
//! [`ProviderErrorKind::Protocol`] error naming the line — never a silently
//! swallowed event or a vacuously empty turn. The single deliberate tolerance is a
//! line with **no** `type` at all: that is the bare report a provider writes when
//! it did not (or could not) stream, and accepting it is what keeps declaring
//! streaming safe for a backend that degrades.

use std::io::BufRead;
use std::ops::ControlFlow;

use serde_json::Value;

use crate::error::{Error, ProviderErrorKind, Result};
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
                // The terminal line ends the protocol. Drain whatever follows it so
                // a child that keeps writing can still exit instead of blocking on a
                // full pipe — nothing past this line can change the report.
                let _ = std::io::copy(reader, &mut std::io::sink());
                return Ok(StreamOutcome::Report(report));
            }
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
    let Some(tag) = object.get("type") else {
        // No discriminator at all: the bare report a provider writes when it did
        // not stream. It is onejudge's document already, so take it as terminal.
        return Ok(StreamLine::Report(value));
    };
    match tag.as_str() {
        Some("event") => {
            let raw = object.get("event").ok_or_else(|| {
                protocol(
                    op,
                    format!("streamed `event` line carried no `event` object; got: {line}"),
                )
            })?;
            let event: ToolEvent = serde_json::from_value(raw.clone()).map_err(|e| {
                protocol(
                    op,
                    format!("streamed `event` line carried a malformed event: {e}; got: {line}"),
                )
            })?;
            Ok(StreamLine::Event(event))
        }
        Some("result") => {
            let report = object.get("report").cloned().ok_or_else(|| {
                protocol(
                    op,
                    format!("streamed `result` line carried no `report` object; got: {line}"),
                )
            })?;
            Ok(StreamLine::Report(report))
        }
        Some(other) => Err(protocol(
            op,
            format!(
                "unrecognized streamed provider line type `{other}` \
                 (expected `event` or `result`); got: {line}"
            ),
        )),
        None => Err(protocol(
            op,
            format!("streamed provider line `type` was not a string; got: {line}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn events_arrive_before_the_terminal_report() {
        let (outcome, events, seen) = read(
            "{\"type\":\"event\",\"event\":{\"kind\":\"tool_call\",\"name\":\"bash\",\
             \"input\":{\"command\":\"ls\"},\"index\":0}}\n\
             \n\
             {\"type\":\"event\",\"event\":{\"kind\":\"tool_result\",\"index\":1}}\n\
             {\"type\":\"result\",\"report\":{\"results\":[{\"text\":\"done\"}]}}\n",
        );
        assert_eq!(seen, 2);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].name.as_deref(), Some("bash"));
        match outcome.unwrap() {
            StreamOutcome::Report(report) => {
                assert_eq!(report["results"][0]["text"], "done");
            }
            StreamOutcome::Aborted => panic!("expected the terminal report"),
        }
    }

    #[test]
    fn a_bare_report_document_is_still_accepted() {
        // The degraded path: a provider that declared streaming but answered with
        // the one document a non-streaming run writes.
        let (outcome, events, seen) = read("{\"results\":[{\"text\":\"buffered\"}]}\n");
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
        let mut bytes = "{\"type\":\"event\",\"event\":{\"kind\":\"tool_call\",\"index\":0}}\n\
             {\"type\":\"event\",\"event\":{\"kind\":\"tool_call\",\"index\":1}}\n"
            .as_bytes();
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
            ("not json at all\n", "was not valid JSON"),
            ("[1,2,3]\n", "was not a JSON object"),
            ("{\"type\":\"progress\",\"pct\":10}\n", "`progress`"),
            ("{\"type\":7}\n", "`type` was not a string"),
            ("{\"type\":\"event\"}\n", "carried no `event` object"),
            (
                "{\"type\":\"event\",\"event\":{\"name\":\"bash\"}}\n",
                "malformed event",
            ),
            ("{\"type\":\"result\"}\n", "carried no `report` object"),
            (
                "{\"type\":\"event\",\"event\":{\"kind\":\"tool_call\",\"index\":0}}\n",
                "ended without a terminal",
            ),
            ("", "ended without a terminal"),
        ] {
            let (outcome, _, _) = read(text);
            let err = outcome.err().unwrap_or_else(|| panic!("{text} must fail"));
            assert_eq!(err.kind(), Some(ProviderErrorKind::Protocol));
            assert!(
                err.to_string().contains(needle),
                "expected `{needle}` in: {err}"
            );
        }
    }
}
