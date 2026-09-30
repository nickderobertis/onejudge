//! The names judge-side calls are recorded under in oneharness's history store.
//!
//! Every judge-side call is named after the **run's base session** (the engine's
//! `session_name`, the CLI's `--session`), so `oneharness history show <name>`
//! finds exactly one run's turns of one party rather than every run's turns under
//! a prompt-derived name. (From oneharness 0.21.1 that lookup reads the last 7
//! UTC days by default; an older run needs `--since <date>` or `--all-time`.) The
//! scheme is computed here and nowhere else:
//!
//! | call | bare provider / panel of one | panel of more than one |
//! | --- | --- | --- |
//! | supervisor / simulated user | `<base>-user` | `<base>-user-<label>` |
//! | `judge` (boolean or numeric) | `<base>-judge` | `<base>-judge-<label>` |
//! | `assess` | `<base>-assess` | `<base>-assess-<label>` |
//!
//! The supervisor's name is the session it is handed — the engine threads
//! `<base>-user` and a [`JudgePanel`](crate::JudgePanel) suffixes it with each
//! judge's label through [`labelled`], the same function this scheme uses. A
//! provider with no scope set (a library caller invoking it directly) records no
//! history name, and oneharness names the record as it always has. See
//! `docs/judges.md`.

/// Which judge-side call a history name is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JudgeSideCall {
    /// A supervisor decision or a simulated-user turn — the session-carrying call.
    User,
    /// A stateless boolean or numeric verdict.
    Judge,
    /// A free-text assessment.
    Assess,
}

impl JudgeSideCall {
    fn suffix(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Judge => "judge",
            Self::Assess => "assess",
        }
    }
}

/// The run a provider's judge-side calls belong to: the base session and, inside
/// a panel of more than one judge, that judge's label.
///
/// The engine sets it on its provider through
/// [`Provider::set_history_scope`](crate::Provider::set_history_scope) before the
/// run's first judge-side call; a panel hands each judge a labelled copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryScope {
    base: String,
    label: Option<String>,
}

impl HistoryScope {
    /// The scope of a run whose base session is `base`, with no judge label.
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into(),
            label: None,
        }
    }

    /// This scope for the judge called `label` in a panel of more than one.
    #[must_use]
    pub fn labelled(&self, label: impl Into<String>) -> Self {
        Self {
            base: self.base.clone(),
            label: Some(label.into()),
        }
    }

    /// The run's base session.
    #[must_use]
    pub fn base(&self) -> &str {
        &self.base
    }

    /// The judge's label, inside a panel of more than one.
    #[must_use]
    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    /// The history name `call` is recorded under: `<base>-<call>`, suffixed
    /// `-<label>` in a panel of more than one.
    #[must_use]
    pub fn name(&self, call: JudgeSideCall) -> String {
        labelled(
            &format!("{}-{}", self.base, call.suffix()),
            self.label.as_deref(),
        )
    }
}

/// `name`, suffixed `-<label>` when there is a label. The one place a judge's
/// label joins a name, shared by the history scheme and the panel's per-judge
/// session so the two cannot drift apart.
pub(crate) fn labelled(name: &str, label: Option<&str>) -> String {
    match label {
        Some(label) => format!("{name}-{label}"),
        None => name.to_string(),
    }
}

/// The history name for `call` under `scope`, or none when there is no scope.
#[must_use]
pub(crate) fn history_name(scope: Option<&HistoryScope>, call: JudgeSideCall) -> Option<String> {
    scope.map(|scope| scope.name(call))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_call_is_named_after_the_base_session() {
        let scope = HistoryScope::new("run-7");
        assert_eq!(scope.name(JudgeSideCall::User), "run-7-user");
        assert_eq!(scope.name(JudgeSideCall::Judge), "run-7-judge");
        assert_eq!(scope.name(JudgeSideCall::Assess), "run-7-assess");
    }

    #[test]
    fn a_labelled_judge_suffixes_its_label() {
        let scope = HistoryScope::new("run-7").labelled("reviewer");
        assert_eq!(scope.label(), Some("reviewer"));
        assert_eq!(scope.base(), "run-7");
        assert_eq!(scope.name(JudgeSideCall::User), "run-7-user-reviewer");
        assert_eq!(scope.name(JudgeSideCall::Judge), "run-7-judge-reviewer");
        assert_eq!(scope.name(JudgeSideCall::Assess), "run-7-assess-reviewer");
    }

    #[test]
    fn no_scope_names_nothing() {
        assert_eq!(history_name(None, JudgeSideCall::User), None);
        assert_eq!(history_name(None, JudgeSideCall::Judge), None);
        assert_eq!(history_name(None, JudgeSideCall::Assess), None);
    }

    #[test]
    fn the_supervisor_name_is_the_session_a_panel_hands_its_judge() {
        let base = HistoryScope::new("run-7");
        let session = base.name(JudgeSideCall::User);
        assert_eq!(
            labelled(&session, Some("reviewer")),
            base.labelled("reviewer").name(JudgeSideCall::User)
        );
    }
}
