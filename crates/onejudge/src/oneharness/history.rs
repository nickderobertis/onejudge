//! The per-candidate history record oneharness writes for every attempt, read
//! back through **oneharness's own reader** (`oneharness_core::io::history`).
//!
//! onejudge already asks every invocation for `--history`, so oneharness appends
//! one normalized record per *attempted* candidate — including the ones a fallback
//! chain fell through.
//!
//! **This is read for `history_id` and nothing else.** The measurements onejudge's
//! `telemetry` contract reports — the invocation bounds and the
//! model/tool/time-to-first-token split — now ride on the run report itself, as
//! `RunResult::telemetry` (oneharness report schema `0.5`), and are read there by
//! [`super::report::measured`]. Before `0.5` they existed only here, which forced
//! onejudge to re-open the file the same run had just written. `history_id` is the
//! one signal with no counterpart on the report: it names the record in
//! oneharness's own store, so it is only knowable by reading that store.
//!
//! Correlation is positional and checked, never assumed: oneharness appends one
//! record per result in result order, so this reads the session file's tail and
//! uses it **only** when the tail's harness identities line up with the report's,
//! result for result. A mismatch (a concurrent writer, a rotated file, a
//! not-yet-flushed record) yields no attribution rather than an attribution
//! pinned to the wrong identity.
//!
//! Everything here is best-effort by design, exactly as oneharness treats its own
//! history writes: history is a side channel, and a run whose record could not be
//! read is still a completed run.

use std::path::Path;

use oneharness_core::domain::history::HistoryRecord;
use oneharness_core::domain::report::RunReport;

/// The history records for one invocation's attempted candidates, aligned with
/// `report.results`. Empty when history was off or the tail could not be matched.
pub(crate) fn read_attempts(report: &RunReport) -> Vec<HistoryRecord> {
    let Some(path) = report.history_file.as_deref() else {
        return Vec::new();
    };
    let Ok(records) = oneharness_core::io::history::read_session(Path::new(path)) else {
        return Vec::new();
    };
    take_matching_tail(report, records)
}

/// The tail of `records` that describes `report.results`, or nothing when it does
/// not line up identity-for-identity.
fn take_matching_tail(report: &RunReport, records: Vec<HistoryRecord>) -> Vec<HistoryRecord> {
    let wanted = report.results.len();
    if wanted == 0 || records.len() < wanted {
        return Vec::new();
    }
    let tail = &records[records.len() - wanted..];
    let aligned = tail
        .iter()
        .zip(&report.results)
        .all(|(record, result)| record.harness_id == result.harness_id);
    if aligned {
        tail.to_vec()
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oneharness::fixture;

    fn with_history(results: Vec<oneharness_core::domain::report::RunResult>) -> RunReport {
        let mut report = fixture::report(results);
        report.history_file = Some("s.jsonl".into());
        report
    }

    #[test]
    fn the_tail_is_taken_only_when_every_identity_lines_up() {
        // A session file accumulates across turns, so the records for THIS run are
        // its tail — one per attempted candidate, in result order.
        let records = vec![
            fixture::record("codex"),
            fixture::record("codex"),
            fixture::record("claude-code"),
        ];
        let report = with_history(vec![
            fixture::result("codex", "a"),
            fixture::result("claude-code", "b"),
        ]);
        let taken = take_matching_tail(&report, records.clone());
        assert_eq!(taken.len(), 2);
        assert_eq!(taken[0].harness_id, "codex");
        assert_eq!(taken[1].harness_id, "claude-code");
        assert_eq!(taken[1].model_ms, Some(10));

        // A tail that describes different candidates is not this run's: attributing
        // it would pin a measurement to the wrong identity.
        let mismatched = with_history(vec![
            fixture::result("codex", "a"),
            fixture::result("goose", "b"),
        ]);
        assert!(take_matching_tail(&mismatched, records.clone()).is_empty());

        // Fewer records than results: the run's own records have not all landed.
        let short = with_history(vec![
            fixture::result("a", ""),
            fixture::result("b", ""),
            fixture::result("c", ""),
            fixture::result("d", ""),
        ]);
        assert!(take_matching_tail(&short, records).is_empty());
    }

    #[test]
    fn history_that_is_off_or_unreadable_yields_no_attribution() {
        assert!(read_attempts(&fixture::report(vec![fixture::result("codex", "a")])).is_empty());
        let mut missing = with_history(vec![fixture::result("codex", "a")]);
        missing.history_file = Some("/definitely/not/a/history/file.jsonl".into());
        assert!(read_attempts(&missing).is_empty());
    }

    #[test]
    fn a_real_session_file_is_read_back_through_oneharnesss_own_reader() {
        // Write the JSONL oneharness writes — its own `HistoryLine::Run` shape — and
        // read it back with its own reader, so the correlation is proven against the
        // real format rather than a shape this crate invented.
        let dir = std::env::temp_dir().join("onejudge-history-read-back");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("session.jsonl");
        let line = oneharness_core::domain::history::HistoryLine::Run(
            oneharness_core::domain::history::HistoryRunRecord::from_record(&fixture::record(
                "claude-code",
            )),
        );
        std::fs::write(
            &path,
            format!(
                "{}\n",
                serde_json::to_string(&line).expect("record serializes")
            ),
        )
        .expect("write history");

        let mut report = with_history(vec![fixture::result("claude-code", "hi")]);
        report.history_file = Some(path.display().to_string());
        let attempts = read_attempts(&report);
        assert_eq!(
            attempts.len(),
            1,
            "oneharness's reader materialized the run"
        );
        assert_eq!(attempts[0].harness_id, "claude-code");
        assert_eq!(attempts[0].model_ms, Some(10));
        assert_eq!(
            attempts[0].started_at.as_deref(),
            Some("2026-01-01T00:00:00Z")
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
