//! The `--format text` view: a run drawn for a person reading along, one
//! observation at a time, as it happens.
//!
//! onejudge draws only its own framing — a rule per turn, each judge's decision,
//! the feedback handed back, and a closing summary. The worker's events are
//! drawn by oneharness's public renderer ([`render_event`]), so a command, a file
//! change, the agent's text and its reasoning read the same whether the run came
//! through oneharness or through onejudge; a judge's tool events go through the
//! same renderer under that judge's label. `ToolEvent::summary` is not used here:
//! it is the compact form a judge prompt carries, not a view for a person.
//!
//! The view reads [`Seen`], an owned copy of an [`Observation`](crate::Observation)
//! taken from its serialized form. That is deliberate: a live run and `onejudge
//! watch` both hand the view the same JSON — the run serializes each observation
//! it publishes, `watch` reads the published line back — so the two print the
//! same text by construction rather than by two renderers agreeing.

use oneharness_core::domain::render::{printable, render_event};
use serde::{Deserialize, Serialize};

use crate::{ActionEvent, Decision, Report, Role, ToolEvent};

use super::{render_eval, render_usage, status_line, EvalOutcome, EvalResult, RunSummary};

/// How wide a rule is drawn, in characters.
const RULE_WIDTH: usize = 64;

/// The widest a one-line excerpt (the task, a reply, a reason) is drawn.
const EXCERPT_CHARS: usize = 120;

/// One observation as the text view reads it — the serialized
/// [`Observation`](crate::Observation), owned. Fields the view does not draw
/// (every turn index but the opening's, which it titles the turn by) are
/// ignored, and a `type` it does not know is [`Seen::Other`], so a stream written
/// by a newer onejudge still renders.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Seen {
    /// A turn opened.
    TurnOpened {
        turn: usize,
        role: Role,
        instruction: String,
    },
    /// Every event the worker's harness reported.
    Action { event: ActionEvent },
    /// A party's own words for a turn.
    Message { role: Role, text: String },
    /// A turn closed.
    TurnClosed {
        role: Role,
        started_at: String,
        finished_at: String,
        #[serde(default)]
        harness: Option<String>,
        #[serde(default)]
        model: Option<String>,
    },
    /// One judge decided.
    JudgeDecided {
        judge: String,
        kind: String,
        decision: Decision,
        reason: String,
        #[serde(default)]
        run_id: Option<String>,
    },
    /// A tool event a judge's harness reported.
    JudgeTool { judge: String, event: ToolEvent },
    /// Anything else — a worker `tool` observation, drawn as its `action`
    /// instead, or a kind this build does not draw.
    #[serde(other)]
    Other,
}

/// The text view's state across one run: the worker's reply, held from its
/// message to its turn's close, where it is summarized.
#[derive(Debug, Default)]
pub(crate) struct TextView {
    reply: Option<String>,
}

impl TextView {
    /// The text `seen` draws — one or more lines, with no trailing newline — or
    /// `None` for an observation this view draws nothing for.
    pub(crate) fn render(&mut self, seen: &Seen) -> Option<String> {
        match seen {
            Seen::TurnOpened {
                turn,
                role: Role::Assistant,
                instruction,
            } => Some(if *turn == 1 {
                format!(
                    "{}\ntask: {}",
                    rule(&format!("turn {turn} · worker")),
                    excerpt(instruction)
                )
            } else {
                rule(&format!("turn {turn} · worker ← feedback"))
            }),
            Seen::TurnOpened { turn, .. } => Some(rule(&format!("turn {turn} · judges"))),
            // Exactly what oneharness's renderer returns: the lines a reader
            // compares with `oneharness run --stream --format text`.
            Seen::Action { event, .. } => render_event(event),
            Seen::Message {
                role: Role::Assistant,
                text,
                ..
            } => {
                self.reply = Some(text.clone());
                None
            }
            Seen::Message { text, .. } => Some(block("feedback → worker: ", text)),
            Seen::TurnClosed {
                role: Role::Assistant,
                started_at,
                finished_at,
                harness,
                model,
                ..
            } => {
                let mut parts = Vec::new();
                if let Some(ms) = between(started_at, finished_at) {
                    parts.push(format!("done in {}", duration(ms)));
                } else {
                    parts.push("done".to_string());
                }
                if let Some(harness) = harness {
                    parts.push(match model {
                        Some(model) => format!("{} ({})", printable(harness), printable(model)),
                        None => printable(harness),
                    });
                }
                if let Some(reply) = self.reply.take().filter(|r| !r.trim().is_empty()) {
                    parts.push(excerpt(&reply));
                }
                Some(format!("  {}", parts.join(" · ")))
            }
            Seen::TurnClosed { .. } | Seen::Other => None,
            Seen::JudgeTool { judge, event, .. } => {
                render_event(&crate::oneharness::action_event(event))
                    .map(|line| under(&format!("{}: ", printable(judge)), &line))
            }
            Seen::JudgeDecided {
                judge,
                kind,
                decision,
                reason,
                run_id,
            } => {
                let mut line = format!("{}  {}", printable(judge), decision.as_str());
                if !reason.trim().is_empty() {
                    line.push_str("  ");
                    line.push_str(&excerpt(reason));
                }
                // The exact command that shows the run behind the decision, on
                // its own line so it copies whole.
                if let Some(run_id) = run_id {
                    line.push_str(&format!(
                        "\n  {} history {}",
                        printable(kind),
                        printable(run_id)
                    ));
                }
                Some(line)
            }
        }
    }
}

/// What a finished run publishes as its last record, and what its closing
/// summary is drawn from — so `watch` closes exactly as the run did.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Finished {
    /// The exit code the run returned.
    pub(crate) exit_code: i32,
    /// How long the run took, start to finish.
    pub(crate) elapsed_ms: u64,
    /// Whether the task completed.
    pub(crate) completed: bool,
    /// Whether the loop stopped at the turn cap.
    pub(crate) hit_max_turns: bool,
    /// The turn cap in effect.
    pub(crate) max_turns: u32,
    /// The completion criterion re-judged against the finished transcript.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) done_when: Option<FinishedCheck>,
    /// Each eval's outcome, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) evals: Vec<FinishedEval>,
    /// The versioned report — exactly the document `--format json` prints.
    pub(crate) report: Report,
}

/// A re-judged completion criterion.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct FinishedCheck {
    pub(crate) criterion: String,
    pub(crate) satisfied: bool,
}

/// One eval's outcome, carrying its kind's own payload.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum FinishedEval {
    Boolean {
        criterion: String,
        passed: bool,
        reason: String,
    },
    Numeric {
        criterion: String,
        score: f64,
        reason: String,
    },
}

impl Finished {
    /// The record for `summary`, which took `elapsed_ms` and exits `exit_code`.
    pub(crate) fn new(summary: &RunSummary, exit_code: i32, elapsed_ms: u64) -> Self {
        Self {
            exit_code,
            elapsed_ms,
            completed: summary.completed,
            hit_max_turns: summary.hit_max_turns,
            max_turns: summary.max_turns,
            done_when: summary.done_when.as_ref().map(|done| FinishedCheck {
                criterion: done.criterion.clone(),
                satisfied: done.satisfied,
            }),
            evals: summary
                .eval_results
                .iter()
                .map(|eval| match eval.outcome {
                    EvalOutcome::Boolean(passed) => FinishedEval::Boolean {
                        criterion: eval.criterion.clone(),
                        passed,
                        reason: eval.reason.clone(),
                    },
                    EvalOutcome::Numeric(score) => FinishedEval::Numeric {
                        criterion: eval.criterion.clone(),
                        score,
                        reason: eval.reason.clone(),
                    },
                })
                .collect(),
            report: summary.report.clone(),
        }
    }
}

/// The closing summary a finished run prints: a rule naming the outcome, then
/// the status, completion check, usage, evals and assessment.
pub(crate) fn closing(finished: &Finished) -> String {
    let report = &finished.report;
    let turns = report.transcript.assistant_turns();
    let outcome = if finished.completed {
        "completed"
    } else {
        "incomplete"
    };
    let mut out = rule(&format!(
        "done: {outcome} after {turns} turn{} ({})",
        if turns == 1 { "" } else { "s" },
        duration(finished.elapsed_ms)
    ));
    out.push('\n');
    out.push_str(&format!(
        "Status: {}\n",
        status_line(
            finished.completed,
            report.settled_reason.as_deref(),
            finished.hit_max_turns,
            finished.max_turns
        )
    ));
    if let Some(done) = &finished.done_when {
        out.push_str(&format!(
            "Completion: \"{}\" — {}\n",
            printable(&done.criterion),
            if done.satisfied {
                "satisfied"
            } else {
                "not satisfied"
            }
        ));
    }
    out.push_str(&format!("Usage: {}\n", render_usage(report.usage.as_ref())));
    for eval in &finished.evals {
        let result = match eval {
            FinishedEval::Boolean {
                criterion,
                passed,
                reason,
            } => EvalResult {
                criterion: criterion.clone(),
                outcome: EvalOutcome::Boolean(*passed),
                reason: reason.clone(),
            },
            FinishedEval::Numeric {
                criterion,
                score,
                reason,
            } => EvalResult {
                criterion: criterion.clone(),
                outcome: EvalOutcome::Numeric(*score),
                reason: reason.clone(),
            },
        };
        out.push_str(&format!("Eval: {}\n", printable(&render_eval(&result))));
    }
    if let Some(assessment) = &report.assessment {
        out.push_str(&block("Assessment: ", assessment));
        out.push('\n');
    }
    out
}

/// The closing line a run that produced no report prints.
pub(crate) fn failed(message: &str) -> String {
    rule(&format!("failed: {}", excerpt(message)))
}

/// `── title ───…`, padded to [`RULE_WIDTH`].
fn rule(title: &str) -> String {
    let head = format!("── {} ", printable(title).replace('\n', " "));
    let pad = RULE_WIDTH.saturating_sub(head.chars().count()).max(2);
    format!("{head}{}", "─".repeat(pad))
}

/// The first line of `text`, flattened and cut to [`EXCERPT_CHARS`], noting how
/// many lines were left out.
fn excerpt(text: &str) -> String {
    let text = printable(text);
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let first = lines.next().unwrap_or_default();
    let rest = lines.count();
    let mut out = if first.chars().count() > EXCERPT_CHARS {
        let cut: String = first.chars().take(EXCERPT_CHARS - 1).collect();
        format!("{cut}…")
    } else {
        first.to_string()
    };
    if rest > 0 {
        out.push_str(&format!(
            " (+{rest} line{})",
            if rest == 1 { "" } else { "s" }
        ));
    }
    out
}

/// `label` followed by the whole of `text`, continuation lines indented under
/// it.
fn block(label: &str, text: &str) -> String {
    under(label, printable(text).trim())
}

/// `text` with `label` before its first line and each later line indented to
/// sit under the first.
fn under(label: &str, text: &str) -> String {
    let indent = " ".repeat(label.chars().count());
    let mut out = String::new();
    for (i, line) in text.lines().enumerate() {
        if i == 0 {
            out.push_str(label);
        } else {
            out.push('\n');
            if !line.is_empty() {
                out.push_str(&indent);
            }
        }
        out.push_str(line);
    }
    if out.is_empty() {
        out.push_str(label.trim_end());
    }
    out
}

/// A duration as a person reads it: `850ms`, `12s`, `7m37s`, `1h02m`.
fn duration(ms: u64) -> String {
    let secs = ms / 1000;
    if secs == 0 {
        format!("{ms}ms")
    } else if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    }
}

/// Milliseconds from `start` to `end`, both RFC 3339 UTC instants with
/// millisecond precision (the spelling every observation carries); `None` when
/// either is not one.
fn between(start: &str, end: &str) -> Option<u64> {
    let (start, end) = (epoch_millis(start)?, epoch_millis(end)?);
    u64::try_from(end - start).ok()
}

/// `YYYY-MM-DDThh:mm:ss.mmmZ` as milliseconds since the Unix epoch.
fn epoch_millis(text: &str) -> Option<i64> {
    let text = text.strip_suffix('Z')?;
    let (date, time) = text.split_once('T')?;
    let mut date = date.split('-').map(str::parse::<i64>);
    let (year, month, day) = (date.next()?.ok()?, date.next()?.ok()?, date.next()?.ok()?);
    let (clock, millis) = time.split_once('.').unwrap_or((time, "0"));
    let mut clock = clock.split(':').map(str::parse::<i64>);
    let (hour, minute, second) = (
        clock.next()?.ok()?,
        clock.next()?.ok()?,
        clock.next()?.ok()?,
    );
    let millis: i64 = millis.parse().ok()?;
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 24 + hour) * 60 + minute) * 60_000 + second * 1000 + millis)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seen(json: serde_json::Value) -> Seen {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn a_worker_turn_is_a_rule_its_events_and_a_closing_line() {
        let mut view = TextView::default();
        let opened = view
            .render(&seen(serde_json::json!({
                "type": "turn_opened", "turn": 1, "role": "assistant",
                "instruction": "fix the bug\nand more", "started_at": "x"
            })))
            .unwrap();
        assert!(opened.starts_with("── turn 1 · worker ──"), "{opened}");
        assert!(
            opened.ends_with("\ntask: fix the bug (+1 line)"),
            "{opened}"
        );
        assert_eq!(opened.lines().next().unwrap().chars().count(), RULE_WIDTH);

        let event = ActionEvent {
            kind: "message".into(),
            name: None,
            input: None,
            output: Some("looking".into()),
            index: 0,
            tool_call_id: None,
            started_at: None,
            finished_at: None,
            duration_ms: None,
            status: None,
            timing_source: None,
        };
        let action = serde_json::json!({ "type": "action", "turn": 1, "event": event });
        assert_eq!(view.render(&seen(action)), render_event(&event));
        // The `tool` twin of an action draws nothing: the action is drawn.
        assert!(view
            .render(&seen(
                serde_json::json!({ "type": "tool", "turn": 1, "event": {"kind": "tool_call"} })
            ))
            .is_none());
        assert!(view
            .render(&seen(serde_json::json!({
                "type": "message", "turn": 1, "role": "assistant", "text": "fixed it"
            })))
            .is_none());
        assert_eq!(
            view.render(&seen(serde_json::json!({
                "type": "turn_closed", "turn": 1, "role": "assistant",
                "started_at": "2026-01-01T23:59:58.500Z", "finished_at": "2026-01-02T00:07:36.000Z",
                "harness": "codex", "model": "gpt-6"
            }))),
            Some("  done in 7m37s · codex (gpt-6) · fixed it".into())
        );
    }

    #[test]
    fn a_judges_turn_draws_each_decision_and_the_feedback() {
        let mut view = TextView::default();
        assert!(view
            .render(&seen(serde_json::json!({
                "type": "turn_opened", "turn": 2, "role": "user", "instruction": "x", "started_at": "x"
            })))
            .unwrap()
            .starts_with("── turn 2 · judges ──"));
        assert_eq!(
            view.render(&seen(serde_json::json!({
                "type": "judge_tool", "turn": 2, "judge": "lint",
                "event": {"kind": "tool_call", "name": "Bash", "input": {"command": "cargo test"}, "index": 0}
            }))),
            Some("lint: $ cargo test".into())
        );
        assert_eq!(
            view.render(&seen(serde_json::json!({
                "type": "judge_decided", "turn": 2, "judge": "lint", "kind": "command",
                "decision": "continue", "reason": "two findings"
            }))),
            Some("lint  continue  two findings".into())
        );
        assert_eq!(
            view.render(&seen(serde_json::json!({
                "type": "judge_decided", "turn": 2, "judge": "lint", "kind": "llmlint",
                "decision": "done", "reason": "", "run_id": "run-7"
            }))),
            Some("lint  done\n  llmlint history run-7".into())
        );
        assert_eq!(
            view.render(&seen(serde_json::json!({
                "type": "message", "turn": 2, "role": "user", "text": "fix\nthe lint"
            }))),
            Some("feedback → worker: fix\n                   the lint".into())
        );
        assert!(view
            .render(&seen(
                serde_json::json!({ "type": "some_future_kind", "turn": 2 })
            ))
            .is_none());
    }

    #[test]
    fn durations_and_instants_read_as_a_person_would() {
        assert_eq!(duration(850), "850ms");
        assert_eq!(duration(12_400), "12s");
        assert_eq!(duration(457_000), "7m37s");
        assert_eq!(duration(3_720_000), "1h02m");
        assert_eq!(
            between("2024-02-28T23:59:59.000Z", "2024-02-29T00:00:01.250Z"),
            Some(2250)
        );
        assert_eq!(between("not a time", "2024-01-01T00:00:00.000Z"), None);
        assert_eq!(epoch_millis("1970-01-01T00:00:01.000Z"), Some(1000));
    }

    #[test]
    fn nothing_a_harness_wrote_reaches_the_terminal_as_a_control_sequence() {
        assert_eq!(excerpt("\u{1b}[31mred\u{7}"), "[31mred");
        assert!(!failed("boom\u{1b}[2J").contains('\u{1b}'));
    }
}
