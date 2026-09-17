//! [`JudgePanel`]: the judge side of a run as a **list of judges**. Every judge
//! runs each judge-side operation at the same time against the same worker turn,
//! the panel waits for all of them, and hands back one combined, attributed
//! answer — so a stack of judges (an LLM reviewer, a lint run, a repository's own
//! script) costs the slowest judge rather than the sum, and the worker fixes
//! everything in one turn.
//!
//! A panel is a [`Provider`] like any other, so it composes as the judge half of
//! a [`SplitProvider`](crate::SplitProvider) — which is how the CLI builds it from
//! a `judges:` list — and an embedder building providers by hand gets the same
//! semantics. `docs/judges.md` is the contract this module implements.
//!
//! Two rules keep a panel of **one** judge byte-identical to a bare provider: the
//! judge's outcome passes through verbatim (no header, no `[label]` prefix), and
//! it is handed the bare session name. Headers, prefixes, suffixed sessions and
//! the `judge` label on telemetry appear only when the panel holds more than one.

use std::cell::RefCell;
use std::ops::ControlFlow;
use std::sync::{Mutex, PoisonError};

use crate::error::{Error, Result};
use crate::provider::{
    Assessment, AssistantTurn, EvidenceContext, JudgeKind, JudgeQuery, JudgeValue, JudgeVerdict,
    Provider, SkillRef, SupervisorOutcome, SupervisorQuery, SupervisorTurn, UserTurn,
};
use crate::report::{Decision, JudgeDecision};
use crate::spawn::SpawnedProcess;
use crate::telemetry::InvocationTelemetry;
use crate::transcript::{Message, ToolEvent};
use crate::usage::Usage;

/// Which judge-side operations a judge can take part in.
///
/// A judge that cannot score a number, write prose or play a user is simply left
/// out of that operation (a lint run has no opinion on "how readable is this, 1
/// to 5"); every judge takes part in the per-turn `supervise` decision and in a
/// boolean `judge`. The default is every ability, which is what an LLM judge and
/// a custom command both have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JudgeAbilities {
    /// Can answer a numeric [`JudgeQuery`].
    pub numeric: bool,
    /// Can write a free-text [`Assessment`].
    pub prose: bool,
    /// Can role-play the simulated user (the legacy `simulate_user` op).
    pub user: bool,
}

impl Default for JudgeAbilities {
    fn default() -> Self {
        Self {
            numeric: true,
            prose: true,
            user: true,
        }
    }
}

/// One judge of a [`JudgePanel`]: its label, its provider kind, what it can do,
/// and the provider itself.
///
/// The provider sits behind its own lock because the panel drives every judge on
/// its own OS thread: the existing backends keep `RefCell` state and are `Send`
/// but not `Sync`, and one lock per judge is what lets them run at the same time
/// without sharing anything.
pub struct JudgeEntry<J> {
    label: String,
    kind: String,
    abilities: JudgeAbilities,
    provider: Mutex<J>,
}

impl<J> JudgeEntry<J> {
    /// A judge called `label`, of provider kind `kind` (`oneharness`, `command`,
    /// …), with every ability.
    pub fn new(label: impl Into<String>, kind: impl Into<String>, provider: J) -> Self {
        Self {
            label: label.into(),
            kind: kind.into(),
            abilities: JudgeAbilities::default(),
            provider: Mutex::new(provider),
        }
    }

    /// Declare which operations this judge takes part in (builder style).
    #[must_use]
    pub fn with_abilities(mut self, abilities: JudgeAbilities) -> Self {
        self.abilities = abilities;
        self
    }

    /// The judge's label within its panel.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The judge entry's provider kind.
    #[must_use]
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// What this judge takes part in.
    #[must_use]
    pub fn abilities(&self) -> JudgeAbilities {
        self.abilities
    }

    /// Rebuild the provider in place — how a plan installs its spawn hook on
    /// every judge of a panel.
    #[must_use]
    pub fn map_provider(self, f: impl FnOnce(J) -> J) -> Self {
        Self {
            label: self.label,
            kind: self.kind,
            abilities: self.abilities,
            provider: Mutex::new(f(self
                .provider
                .into_inner()
                .unwrap_or_else(PoisonError::into_inner))),
        }
    }

    /// Run `f` against the judge's provider under its lock.
    fn with<T>(&self, f: impl FnOnce(&J) -> T) -> T {
        let guard = self.provider.lock().unwrap_or_else(PoisonError::into_inner);
        f(&guard)
    }

    /// The `## Judge` header a multi-judge message puts above this judge's words.
    fn header(&self) -> String {
        format!("## Judge `{}` ({})", self.label, self.kind)
    }
}

/// The judge side of a run as a list of judges, each run concurrently and all of
/// them awaited. See the module docs and `docs/judges.md`.
pub struct JudgePanel<J> {
    judges: Vec<JudgeEntry<J>>,
    /// Every decision recorded since the last [`Provider::take_judge_decisions`].
    decisions: RefCell<Vec<JudgeDecision>>,
}

impl<J> std::fmt::Debug for JudgePanel<J> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The judges' identities are what a reader of a panel's `Debug` output
        // needs; the providers behind them need not be `Debug` themselves.
        f.debug_struct("JudgePanel")
            .field(
                "judges",
                &self
                    .judges
                    .iter()
                    .map(|judge| format!("{} ({})", judge.label, judge.kind))
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl<J> JudgePanel<J> {
    /// Compose `judges` into one panel, in list order.
    ///
    /// # Errors
    /// [`Error::Invalid`] when the list is empty, or a label is not
    /// `[A-Za-z0-9_-]+`, or two judges share a label — the same rules the CLI's
    /// config layer enforces, so an embedder composing by hand meets the same
    /// bar.
    pub fn new(judges: Vec<JudgeEntry<J>>) -> Result<Self> {
        if judges.is_empty() {
            return Err(Error::Invalid(
                "a judge panel needs at least one judge".into(),
            ));
        }
        for (i, judge) in judges.iter().enumerate() {
            if !is_valid_label(&judge.label) {
                return Err(Error::Invalid(format!(
                    "judge label `{}` is not `[A-Za-z0-9_-]+`",
                    judge.label
                )));
            }
            if judges[..i]
                .iter()
                .any(|earlier| earlier.label == judge.label)
            {
                return Err(Error::Invalid(format!(
                    "judge label `{}` is used by more than one judge",
                    judge.label
                )));
            }
        }
        Ok(Self {
            judges,
            decisions: RefCell::new(Vec::new()),
        })
    }

    /// The judges, in list order.
    #[must_use]
    pub fn judges(&self) -> &[JudgeEntry<J>] {
        &self.judges
    }

    /// Rebuild every judge's provider in place — how a plan installs its spawn
    /// hook on each judge of the panel.
    #[must_use]
    pub fn map_judges(self, mut f: impl FnMut(J) -> J) -> Self {
        Self {
            judges: self
                .judges
                .into_iter()
                .map(|judge| judge.map_provider(&mut f))
                .collect(),
            decisions: self.decisions,
        }
    }

    /// Whether this panel attributes: more than one judge.
    fn multi(&self) -> bool {
        self.judges.len() > 1
    }

    /// The caller-owned session name judge `i` is handed: `<session>-<label>`
    /// when the panel holds more than one judge, so each keeps its own harness
    /// session, and the bare name for a panel of one.
    fn session_for(&self, i: usize, session: Option<&str>) -> Option<String> {
        session.map(|session| {
            if self.multi() {
                format!("{session}-{}", self.judges[i].label)
            } else {
                session.to_string()
            }
        })
    }

    /// `[<label>] <text>` for each part, joined with `; `.
    fn attributed<'a>(parts: impl IntoIterator<Item = (&'a str, &'a str)>) -> String {
        parts
            .into_iter()
            .map(|(label, text)| format!("[{label}] {text}").trim_end().to_string())
            .collect::<Vec<_>>()
            .join("; ")
    }
}

impl<J: Provider + Send> JudgePanel<J> {
    /// Run `f` against every judge `select` admits, **at the same time** — one OS
    /// thread per judge — and return every result in list order once **every**
    /// thread has returned. There is no early exit: a judge still running after
    /// another has failed is waited for, so nothing is ever left running unseen.
    fn each<T: Send>(
        &self,
        select: impl Fn(&JudgeEntry<J>) -> bool,
        f: impl Fn(usize, &JudgeEntry<J>, &J) -> Result<T> + Sync,
    ) -> Vec<(usize, Result<T>)> {
        std::thread::scope(|scope| {
            let handles: Vec<_> = self
                .judges
                .iter()
                .enumerate()
                .filter(|(_, judge)| select(judge))
                .map(|(i, judge)| {
                    let f = &f;
                    (
                        i,
                        scope.spawn(move || judge.with(|provider| f(i, judge, provider))),
                    )
                })
                .collect();
            handles
                .into_iter()
                .map(|(i, handle)| {
                    let result = handle
                        .join()
                        .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
                    (i, result)
                })
                .collect()
        })
    }

    /// Once every judge has returned: if any failed, the classified error naming
    /// the judge (`<op>[<label>]`, that judge's own `kind`) and, per judge, what
    /// it decided or how it failed. A panel of one passes its judge's error
    /// through untouched, so a single-judge run fails exactly as it always has.
    fn failure<T>(
        &self,
        op: &str,
        results: &[(usize, Result<T>)],
        decided: impl Fn(&T) -> String,
    ) -> Option<Error> {
        let (first, error) = results
            .iter()
            .find_map(|(i, result)| result.as_ref().err().map(|error| (*i, error)))?;
        if !self.multi() {
            return Some(clone_error(error));
        }
        let per_judge: Vec<(&str, String)> = results
            .iter()
            .map(|(i, result)| {
                (
                    self.judges[*i].label.as_str(),
                    match result {
                        Ok(value) => decided(value),
                        Err(error) => format!("failed: {error}"),
                    },
                )
            })
            .collect();
        let message = Self::attributed(per_judge.iter().map(|(l, t)| (*l, t.as_str())));
        Some(Error::Provider {
            context: format!("{op}[{}]", self.judges[first].label),
            message,
            kind: error.kind(),
        })
    }

    /// Combine every judge's supervisor decision per the table in
    /// `docs/judges.md`, recording one [`JudgeDecision`] per judge first — so the
    /// call that failed is on the record too.
    fn combine_supervision(
        &self,
        results: Vec<(usize, Result<SupervisorTurn>)>,
    ) -> Result<SupervisorTurn> {
        let mut usage = Usage::default();
        for (i, result) in &results {
            let judge = &self.judges[*i];
            let (decision, reason) = match result {
                Ok(turn) => {
                    if let Some(u) = &turn.usage {
                        usage.add(u);
                    }
                    decision_of(&turn.outcome)
                }
                Err(error) => (Decision::Error, error.to_string()),
            };
            self.decisions.borrow_mut().push(JudgeDecision {
                judge: judge.label.clone(),
                kind: judge.kind.clone(),
                decision,
                reason,
            });
        }
        if let Some(error) = self.failure("supervise", &results, |turn| {
            let (decision, reason) = decision_of(&turn.outcome);
            format!("{}: {reason}", decision.as_str())
                .trim_end()
                .to_string()
        }) {
            return Err(error);
        }
        let mut turns: Vec<(usize, SupervisorTurn)> = results
            .into_iter()
            .map(|(i, result)| (i, result.expect("no judge failed")))
            .collect();
        if !self.multi() {
            let (_, turn) = turns.pop().expect("a panel holds at least one judge");
            return Ok(turn);
        }
        let usage = (!usage.is_empty()).then_some(usage);
        let reasons: Vec<(&str, String)> = turns
            .iter()
            .map(|(i, turn)| (self.judges[*i].label.as_str(), decision_of(&turn.outcome).1))
            .collect();
        let reason = Self::attributed(reasons.iter().map(|(l, r)| (*l, r.as_str())));

        let outcomes = || {
            turns
                .iter()
                .map(|(i, turn)| (&self.judges[*i], &turn.outcome))
        };
        if outcomes().all(|(_, o)| matches!(o, SupervisorOutcome::Completed { .. })) {
            return Ok(SupervisorTurn {
                outcome: SupervisorOutcome::Completed { reason },
                usage,
            });
        }
        if outcomes().any(|(_, o)| matches!(o, SupervisorOutcome::Continue { .. })) {
            let message = outcomes()
                .filter_map(|(judge, outcome)| {
                    let body = match outcome {
                        SupervisorOutcome::Completed { .. } => return None,
                        SupervisorOutcome::Continue { message, .. } => message.clone(),
                        SupervisorOutcome::NoInstruction { reason } => no_instruction_line(reason),
                        SupervisorOutcome::Unparseable { problem, .. } => {
                            no_instruction_line(&format!("its answer did not parse: {problem}"))
                        }
                    };
                    Some(format!("{}\n\n{body}", judge.header()))
                })
                .collect::<Vec<_>>()
                .join("\n\n");
            return Ok(SupervisorTurn {
                outcome: SupervisorOutcome::Continue { message, reason },
                usage,
            });
        }
        if outcomes().any(|(_, o)| matches!(o, SupervisorOutcome::NoInstruction { .. })) {
            return Ok(SupervisorTurn {
                outcome: SupervisorOutcome::NoInstruction { reason },
                usage,
            });
        }
        // Every judge that did not complete answered in neither shape.
        let unparsed: Vec<(&str, &str, &str)> = outcomes()
            .filter_map(|(judge, outcome)| match outcome {
                SupervisorOutcome::Unparseable { problem, answer } => {
                    Some((judge.label.as_str(), problem.as_str(), answer.as_str()))
                }
                _ => None,
            })
            .collect();
        Ok(SupervisorTurn {
            outcome: SupervisorOutcome::Unparseable {
                problem: Self::attributed(unparsed.iter().map(|(l, p, _)| (*l, *p))),
                answer: Self::attributed(unparsed.iter().map(|(l, _, a)| (*l, *a))),
            },
            usage,
        })
    }

    /// One verdict across every judge that can answer `query`: the conjunction of
    /// boolean values, the **minimum** of numeric scores, reasons attributed and
    /// usage summed. A panel of one passes its judge's verdict through verbatim.
    fn judge_across(
        &self,
        query: &JudgeQuery<'_>,
        call: impl Fn(&J) -> Result<JudgeVerdict> + Sync,
    ) -> Result<JudgeVerdict> {
        let numeric = query.kind == JudgeKind::Numeric;
        let results = self.each(
            |judge| !numeric || judge.abilities.numeric,
            |_, _, provider| call(provider),
        );
        if results.is_empty() {
            return Err(Error::Invalid(
                "no judge in the panel can score a number".into(),
            ));
        }
        if let Some(error) = self.failure("judge", &results, |verdict| {
            format!("{}: {}", value_token(verdict.value), verdict.reason)
        }) {
            return Err(error);
        }
        let mut verdicts: Vec<(usize, JudgeVerdict)> = results
            .into_iter()
            .map(|(i, result)| (i, result.expect("no judge failed")))
            .collect();
        if !self.multi() {
            let (_, verdict) = verdicts.pop().expect("one judge answered");
            return Ok(verdict);
        }
        let mut usage = Usage::default();
        let mut all = true;
        let mut lowest = f64::INFINITY;
        for (i, verdict) in &verdicts {
            if let Some(u) = &verdict.usage {
                usage.add(u);
            }
            match (query.kind, verdict.value) {
                (JudgeKind::Boolean, JudgeValue::Bool(value)) => all &= value,
                (JudgeKind::Numeric, JudgeValue::Number(value)) => lowest = lowest.min(value),
                (JudgeKind::Boolean, JudgeValue::Number(_))
                | (JudgeKind::Numeric, JudgeValue::Bool(_)) => {
                    return Err(Error::provider_classified(
                        format!("judge[{}]", self.judges[*i].label),
                        format!(
                            "verdict value has the wrong type for a {} query",
                            query.kind.as_str()
                        ),
                        crate::ProviderErrorKind::Protocol,
                    ))
                }
            }
        }
        Ok(JudgeVerdict {
            value: match query.kind {
                JudgeKind::Boolean => JudgeValue::Bool(all),
                JudgeKind::Numeric => JudgeValue::Number(lowest),
            },
            reason: Self::attributed(
                verdicts
                    .iter()
                    .map(|(i, v)| (self.judges[*i].label.as_str(), v.reason.as_str())),
            ),
            usage: (!usage.is_empty()).then_some(usage),
        })
    }

    /// Every prose-capable judge's assessment, each under its `## Judge` header
    /// in list order; a panel of one passes its judge's text through verbatim.
    fn assess_across(&self, call: impl Fn(&J) -> Result<Assessment> + Sync) -> Result<Assessment> {
        let results = self.each(
            |judge| judge.abilities.prose,
            |_, _, provider| call(provider),
        );
        if results.is_empty() {
            return Err(Error::Invalid(
                "no judge in the panel can write an assessment".into(),
            ));
        }
        if let Some(error) = self.failure("assess", &results, |_| "wrote an assessment".into()) {
            return Err(error);
        }
        let mut assessments: Vec<(usize, Assessment)> = results
            .into_iter()
            .map(|(i, result)| (i, result.expect("no judge failed")))
            .collect();
        if !self.multi() {
            let (_, assessment) = assessments.pop().expect("one judge answered");
            return Ok(assessment);
        }
        let mut usage = Usage::default();
        let text = assessments
            .iter()
            .map(|(i, assessment)| {
                if let Some(u) = &assessment.usage {
                    usage.add(u);
                }
                format!("{}\n\n{}", self.judges[*i].header(), assessment.text)
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        Ok(Assessment {
            text,
            usage: (!usage.is_empty()).then_some(usage),
        })
    }

    /// The first judge that can play a user, for the legacy op.
    fn user_judge(&self) -> Result<(usize, &JudgeEntry<J>)> {
        self.judges
            .iter()
            .enumerate()
            .find(|(_, judge)| judge.abilities.user)
            .ok_or_else(|| Error::Invalid("no judge in the panel can play the user".into()))
    }

    /// The panel is the judge side only: it runs no agent turn.
    fn no_agent_side<T>() -> Result<T> {
        Err(Error::Invalid(
            "a judge panel runs no agent turn; compose it as the judge half of a `SplitProvider`"
                .into(),
        ))
    }
}

/// Whether `label` is `[A-Za-z0-9_-]+`.
#[must_use]
pub fn is_valid_label(label: &str) -> bool {
    !label.is_empty()
        && label
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// The wire decision and per-judge reason an outcome records.
fn decision_of(outcome: &SupervisorOutcome) -> (Decision, String) {
    match outcome {
        SupervisorOutcome::Completed { reason } => (Decision::Done, reason.clone()),
        SupervisorOutcome::Continue { reason, .. } => (Decision::Continue, reason.clone()),
        SupervisorOutcome::NoInstruction { reason } => (Decision::NoInstruction, reason.clone()),
        SupervisorOutcome::Unparseable { problem, .. } => (Decision::Unparseable, problem.clone()),
    }
}

/// The one line a judge that judged the work incomplete but gave no usable
/// instruction contributes under its header.
fn no_instruction_line(reason: &str) -> String {
    let reason = reason.trim();
    if reason.is_empty() {
        "This judge judged the work incomplete but gave no usable instruction.".to_string()
    } else {
        format!("This judge judged the work incomplete but gave no usable instruction ({reason}).")
    }
}

fn value_token(value: JudgeValue) -> String {
    match value {
        JudgeValue::Bool(b) => b.to_string(),
        JudgeValue::Number(n) => n.to_string(),
    }
}

/// `Error` is not `Clone`; a single-judge panel hands its judge's error through
/// field by field so nothing about it changes on the way.
fn clone_error(error: &Error) -> Error {
    match error {
        Error::Invalid(message) => Error::Invalid(message.clone()),
        Error::Provider {
            context,
            message,
            kind,
        } => Error::Provider {
            context: context.clone(),
            message: message.clone(),
            kind: *kind,
        },
    }
}

impl<J: Provider + Send> Provider for JudgePanel<J> {
    fn supervises_lost_turns(&self) -> bool {
        self.judges
            .iter()
            .all(|judge| judge.provider.lock().unwrap().supervises_lost_turns())
    }

    fn reset_telemetry(&self) {
        for judge in &self.judges {
            judge.with(Provider::reset_telemetry);
        }
        self.decisions.borrow_mut().clear();
    }

    // Each judge's records, stamped with its label when the panel holds more than
    // one — a panel of one writes exactly the records its judge wrote.
    fn invocation_telemetry(&self) -> Vec<InvocationTelemetry> {
        let multi = self.multi();
        self.judges
            .iter()
            .flat_map(|judge| {
                judge.with(|provider| {
                    let mut records = provider.invocation_telemetry();
                    if multi {
                        for record in &mut records {
                            record.judge = Some(judge.label.clone());
                        }
                    }
                    records
                })
            })
            .collect()
    }

    fn spawned_processes(&self) -> Vec<SpawnedProcess> {
        let multi = self.multi();
        self.judges
            .iter()
            .flat_map(|judge| {
                judge.with(|provider| {
                    let mut records = provider.spawned_processes();
                    if multi {
                        for record in &mut records {
                            record.judge = Some(judge.label.clone());
                        }
                    }
                    records
                })
            })
            .collect()
    }

    // The first judge's: a lever over another judge's turn is deferred.
    fn supervisor_control(&self) -> crate::ControlOutcome {
        self.judges[0].with(Provider::supervisor_control)
    }

    fn take_judge_decisions(&self) -> Vec<JudgeDecision> {
        std::mem::take(&mut *self.decisions.borrow_mut())
    }

    fn respond(
        &self,
        _skill: &SkillRef<'_>,
        _messages: &[Message],
        _session: Option<&str>,
    ) -> Result<AssistantTurn> {
        Self::no_agent_side()
    }

    fn respond_streaming(
        &self,
        _skill: &SkillRef<'_>,
        _messages: &[Message],
        _session: Option<&str>,
        _on_event: &mut dyn FnMut(&ToolEvent) -> ControlFlow<()>,
    ) -> Result<AssistantTurn> {
        Self::no_agent_side()
    }

    fn simulate_user(
        &self,
        persona: &str,
        messages: &[Message],
        session: Option<&str>,
    ) -> Result<UserTurn> {
        let (i, judge) = self.user_judge()?;
        let session = self.session_for(i, session);
        judge.with(|provider| provider.simulate_user(persona, messages, session.as_deref()))
    }

    fn supervise(
        &self,
        query: &SupervisorQuery<'_>,
        messages: &[Message],
        session: Option<&str>,
    ) -> Result<SupervisorTurn> {
        let sessions: Vec<Option<String>> = (0..self.judges.len())
            .map(|i| self.session_for(i, session))
            .collect();
        let results = self.each(
            |_| true,
            |i, _, provider| provider.supervise(query, messages, sessions[i].as_deref()),
        );
        self.combine_supervision(results)
    }

    fn supervise_with_evidence(
        &self,
        query: &SupervisorQuery<'_>,
        messages: &[Message],
        session: Option<&str>,
        evidence: EvidenceContext<'_>,
    ) -> Result<SupervisorTurn> {
        let sessions: Vec<Option<String>> = (0..self.judges.len())
            .map(|i| self.session_for(i, session))
            .collect();
        let results = self.each(
            |_| true,
            |i, _, provider| {
                provider.supervise_with_evidence(query, messages, sessions[i].as_deref(), evidence)
            },
        );
        self.combine_supervision(results)
    }

    fn judge(&self, query: &JudgeQuery<'_>, messages: &[Message]) -> Result<JudgeVerdict> {
        self.judge_across(query, |provider| provider.judge(query, messages))
    }

    fn judge_with_evidence(
        &self,
        query: &JudgeQuery<'_>,
        messages: &[Message],
        evidence: EvidenceContext<'_>,
    ) -> Result<JudgeVerdict> {
        self.judge_across(query, |provider| {
            provider.judge_with_evidence(query, messages, evidence)
        })
    }

    fn assess(&self, prompt: &str, messages: &[Message]) -> Result<Assessment> {
        self.assess_across(|provider| provider.assess(prompt, messages))
    }

    fn assess_with_evidence(
        &self,
        prompt: &str,
        messages: &[Message],
        evidence: EvidenceContext<'_>,
    ) -> Result<Assessment> {
        self.assess_across(|provider| provider.assess_with_evidence(prompt, messages, evidence))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProviderErrorKind;
    use std::cell::RefCell;

    /// An in-memory judge scripted with one supervisor answer, recording the
    /// session it was handed. `RefCell` state, like the real backends: `Send`,
    /// not `Sync`, which is exactly what the per-judge lock exists for.
    struct Canned {
        answer: Option<SupervisorOutcome>,
        verdict: JudgeVerdict,
        sessions: RefCell<Vec<Option<String>>>,
        /// Fail `judge` and `assess` too, not only `supervise`.
        broken: bool,
    }

    impl Canned {
        fn deciding(answer: SupervisorOutcome) -> Self {
            Self {
                answer: Some(answer),
                verdict: JudgeVerdict {
                    value: JudgeValue::Bool(true),
                    reason: "fine".into(),
                    usage: None,
                },
                sessions: RefCell::new(Vec::new()),
                broken: false,
            }
        }

        fn failing() -> Self {
            Self {
                answer: None,
                verdict: JudgeVerdict {
                    value: JudgeValue::Bool(true),
                    reason: "fine".into(),
                    usage: None,
                },
                sessions: RefCell::new(Vec::new()),
                broken: false,
            }
        }

        fn broken() -> Self {
            Self {
                broken: true,
                ..Self::failing()
            }
        }

        fn scoring(value: JudgeValue, reason: &str) -> Self {
            Self {
                answer: Some(SupervisorOutcome::Completed {
                    reason: "done".into(),
                }),
                verdict: JudgeVerdict {
                    value,
                    reason: reason.into(),
                    usage: Some(Usage {
                        input_tokens: Some(1),
                        ..Usage::default()
                    }),
                },
                sessions: RefCell::new(Vec::new()),
                broken: false,
            }
        }
    }

    impl Provider for Canned {
        fn respond(
            &self,
            _: &SkillRef<'_>,
            _: &[Message],
            _: Option<&str>,
        ) -> Result<AssistantTurn> {
            unreachable!("a judge never responds")
        }

        fn simulate_user(&self, _: &str, _: &[Message], session: Option<&str>) -> Result<UserTurn> {
            self.sessions.borrow_mut().push(session.map(String::from));
            Ok(UserTurn {
                message: "next".into(),
                ..UserTurn::default()
            })
        }

        fn supervise(
            &self,
            _: &SupervisorQuery<'_>,
            _: &[Message],
            session: Option<&str>,
        ) -> Result<SupervisorTurn> {
            self.sessions.borrow_mut().push(session.map(String::from));
            match &self.answer {
                Some(outcome) => Ok(SupervisorTurn {
                    outcome: outcome.clone(),
                    usage: Some(Usage {
                        input_tokens: Some(2),
                        ..Usage::default()
                    }),
                }),
                None => Err(Error::provider_classified(
                    "supervisor",
                    "provider exited with 1",
                    ProviderErrorKind::Protocol,
                )),
            }
        }

        fn judge(&self, _: &JudgeQuery<'_>, _: &[Message]) -> Result<JudgeVerdict> {
            if self.broken {
                return Err(Error::provider_classified(
                    "judge",
                    "no output",
                    ProviderErrorKind::Spawn,
                ));
            }
            Ok(self.verdict.clone())
        }

        fn assess(&self, prompt: &str, _: &[Message]) -> Result<Assessment> {
            if self.broken {
                return Err(Error::provider("assess", "no output"));
            }
            Ok(Assessment {
                text: format!("about {prompt}"),
                usage: None,
            })
        }
    }

    fn query() -> SupervisorQuery<'static> {
        SupervisorQuery {
            task: "t",
            persona: "p",
            done_when: None,
            worktree: "/w",
            history_name: "h",
            notes: &[],
            turn: crate::TurnOutcome::Taken,
        }
    }

    fn done(reason: &str) -> SupervisorOutcome {
        SupervisorOutcome::Completed {
            reason: reason.into(),
        }
    }

    fn go_on(message: &str, reason: &str) -> SupervisorOutcome {
        SupervisorOutcome::Continue {
            message: message.into(),
            reason: reason.into(),
        }
    }

    #[test]
    fn a_panel_refuses_no_judges_and_bad_or_duplicate_labels() {
        assert!(matches!(
            JudgePanel::<Canned>::new(vec![]).unwrap_err(),
            Error::Invalid(m) if m.contains("at least one")
        ));
        let bad = JudgePanel::new(vec![JudgeEntry::new(
            "not ok",
            "command",
            Canned::failing(),
        )]);
        assert!(matches!(bad.unwrap_err(), Error::Invalid(m) if m.contains("`not ok`")));
        let twice = JudgePanel::new(vec![
            JudgeEntry::new("a", "command", Canned::failing()),
            JudgeEntry::new("a", "command", Canned::failing()),
        ]);
        assert!(matches!(twice.unwrap_err(), Error::Invalid(m) if m.contains("more than one")));
        assert!(is_valid_label("A-z_09"));
        assert!(!is_valid_label(""));
        assert!(!is_valid_label("a.b"));
    }

    #[test]
    fn every_judge_done_completes_with_each_reason_attributed() {
        let panel = panel_of(vec![
            ("a", Canned::deciding(done("tests pass"))),
            ("b", Canned::deciding(done("lint clean"))),
        ]);
        let turn = panel.supervise(&query(), &[], Some("run-user")).unwrap();
        assert_eq!(
            turn.outcome,
            SupervisorOutcome::Completed {
                reason: "[a] tests pass; [b] lint clean".into()
            }
        );
        // Usage is summed across judges.
        assert_eq!(turn.usage.unwrap().input_tokens, Some(4));
        // Every judge is recorded, in list order, and the record drains once.
        let decisions = panel.take_judge_decisions();
        assert_eq!(decisions.len(), 2);
        assert_eq!(decisions[0].judge, "a");
        assert_eq!(decisions[0].kind, "command");
        assert_eq!(decisions[0].decision, Decision::Done);
        assert_eq!(decisions[1].reason, "lint clean");
        assert!(panel.take_judge_decisions().is_empty());
    }

    #[test]
    fn one_judge_continuing_puts_only_its_block_in_the_message() {
        let panel = panel_of(vec![
            ("a", Canned::deciding(done("tests pass"))),
            (
                "b",
                Canned::deciding(go_on("Add a test for the empty case.", "no test")),
            ),
        ]);
        let turn = panel.supervise(&query(), &[], None).unwrap();
        assert_eq!(
            turn.outcome,
            SupervisorOutcome::Continue {
                message: "## Judge `b` (command)\n\nAdd a test for the empty case.".into(),
                reason: "[a] tests pass; [b] no test".into(),
            }
        );
    }

    #[test]
    fn several_judges_continuing_give_their_blocks_in_list_order() {
        let panel = panel_of(vec![
            ("first", Canned::deciding(go_on("Do one.", "r1"))),
            ("done", Canned::deciding(done("ok"))),
            ("second", Canned::deciding(go_on("Do two.", "r2"))),
        ]);
        let turn = panel.supervise(&query(), &[], None).unwrap();
        assert_eq!(
            turn.outcome,
            SupervisorOutcome::Continue {
                message: "## Judge `first` (command)\n\nDo one.\n\n\
                          ## Judge `second` (command)\n\nDo two."
                    .into(),
                reason: "[first] r1; [done] ok; [second] r2".into(),
            }
        );
    }

    #[test]
    fn a_judge_with_no_usable_instruction_contributes_one_line_beside_a_continuing_one() {
        let panel = panel_of(vec![
            (
                "silent",
                Canned::deciding(SupervisorOutcome::NoInstruction {
                    reason: "not done".into(),
                }),
            ),
            (
                "prose",
                Canned::deciding(SupervisorOutcome::Unparseable {
                    problem: "not a JSON object".into(),
                    answer: "a paragraph".into(),
                }),
            ),
            ("go", Canned::deciding(go_on("Fix it.", "broken"))),
        ]);
        let turn = panel.supervise(&query(), &[], None).unwrap();
        let SupervisorOutcome::Continue { message, reason } = turn.outcome else {
            panic!("a continuing judge continues the run");
        };
        assert_eq!(
            message,
            "## Judge `silent` (command)\n\n\
             This judge judged the work incomplete but gave no usable instruction (not done).\n\n\
             ## Judge `prose` (command)\n\n\
             This judge judged the work incomplete but gave no usable instruction \
             (its answer did not parse: not a JSON object).\n\n\
             ## Judge `go` (command)\n\nFix it."
        );
        assert_eq!(
            reason,
            "[silent] not done; [prose] not a JSON object; [go] broken"
        );
    }

    #[test]
    fn no_instruction_wins_over_unparseable_and_both_are_attributed() {
        let panel = panel_of(vec![
            ("a", Canned::deciding(done("ok"))),
            (
                "b",
                Canned::deciding(SupervisorOutcome::NoInstruction {
                    reason: String::new(),
                }),
            ),
            (
                "c",
                Canned::deciding(SupervisorOutcome::Unparseable {
                    problem: "prose".into(),
                    answer: "words".into(),
                }),
            ),
        ]);
        let turn = panel.supervise(&query(), &[], None).unwrap();
        assert_eq!(
            turn.outcome,
            SupervisorOutcome::NoInstruction {
                // An empty reason keeps its prefix and nothing dangling after it.
                reason: "[a] ok; [b]; [c] prose".into()
            }
        );

        let panel = panel_of(vec![
            ("a", Canned::deciding(done("ok"))),
            (
                "b",
                Canned::deciding(SupervisorOutcome::Unparseable {
                    problem: "needs boolean `completion`".into(),
                    answer: "{}".into(),
                }),
            ),
            (
                "c",
                Canned::deciding(SupervisorOutcome::Unparseable {
                    problem: "not a JSON object".into(),
                    answer: "words".into(),
                }),
            ),
        ]);
        let turn = panel.supervise(&query(), &[], None).unwrap();
        assert_eq!(
            turn.outcome,
            SupervisorOutcome::Unparseable {
                problem: "[b] needs boolean `completion`; [c] not a JSON object".into(),
                answer: "[b] {}; [c] words".into(),
            }
        );
    }

    #[test]
    fn a_failed_judge_fails_the_call_naming_it_after_every_judge_returned() {
        let panel = panel_of(vec![
            ("a", Canned::deciding(go_on("Fix.", "broken"))),
            ("b", Canned::failing()),
        ]);
        let error = panel.supervise(&query(), &[], None).unwrap_err();
        let Error::Provider {
            context,
            message,
            kind,
        } = error
        else {
            panic!("a classified provider error");
        };
        assert_eq!(context, "supervise[b]");
        assert_eq!(kind, Some(ProviderErrorKind::Protocol));
        assert_eq!(
            message,
            "[a] continue: broken; [b] failed: provider error (supervisor): provider exited with 1"
        );
        // The failed call is on the record beside the one that decided.
        let decisions = panel.take_judge_decisions();
        assert_eq!(decisions[0].decision, Decision::Continue);
        assert_eq!(decisions[1].decision, Decision::Error);
        assert_eq!(
            decisions[1].reason,
            "provider error (supervisor): provider exited with 1"
        );
    }

    #[test]
    fn a_panel_of_one_passes_everything_through_untouched() {
        let panel = panel_of(vec![("only", Canned::deciding(go_on("Next.", "because")))]);
        let turn = panel.supervise(&query(), &[], Some("run-user")).unwrap();
        assert_eq!(turn.outcome, go_on("Next.", "because"));
        // …including the bare session name.
        assert_eq!(
            panel.judges()[0].with(|j| j.sessions.borrow().clone()),
            vec![Some("run-user".to_string())]
        );
        // A decision is still recorded — a panel of one is still a panel.
        assert_eq!(panel.take_judge_decisions().len(), 1);

        let failing = panel_of(vec![("only", Canned::failing())]);
        let error = failing.supervise(&query(), &[], None).unwrap_err();
        assert!(
            matches!(&error, Error::Provider { context, kind: Some(ProviderErrorKind::Protocol), .. } if context == "supervisor"),
            "{error}"
        );
        let verdict = failing.judge(
            &JudgeQuery {
                kind: JudgeKind::Boolean,
                criterion: "c",
                scale: None,
            },
            &[],
        );
        assert_eq!(verdict.unwrap().reason, "fine");
    }

    #[test]
    fn more_than_one_judge_suffixes_the_session_with_each_label() {
        let panel = panel_of(vec![
            ("a", Canned::deciding(done("x"))),
            ("b", Canned::deciding(done("y"))),
        ]);
        panel.supervise(&query(), &[], Some("run-user")).unwrap();
        panel.simulate_user("p", &[], Some("run-user")).unwrap();
        let sessions = |i: usize| panel.judges()[i].with(|j| j.sessions.borrow().clone());
        assert_eq!(
            sessions(0),
            vec![
                Some("run-user-a".to_string()),
                Some("run-user-a".to_string())
            ]
        );
        assert_eq!(sessions(1), vec![Some("run-user-b".to_string())]);
        // And no session stays no session.
        panel.supervise(&query(), &[], None).unwrap();
        assert_eq!(sessions(1).last().unwrap(), &None);
    }

    #[test]
    fn boolean_verdicts_conjoin_and_numeric_scores_take_the_minimum() {
        let panel = panel_of(vec![
            ("a", Canned::scoring(JudgeValue::Bool(true), "yes")),
            ("b", Canned::scoring(JudgeValue::Bool(false), "no")),
        ]);
        let verdict = panel
            .judge(
                &JudgeQuery {
                    kind: JudgeKind::Boolean,
                    criterion: "c",
                    scale: None,
                },
                &[],
            )
            .unwrap();
        assert_eq!(verdict.value, JudgeValue::Bool(false));
        assert_eq!(verdict.reason, "[a] yes; [b] no");
        assert_eq!(verdict.usage.unwrap().input_tokens, Some(2));

        let panel = JudgePanel::new(vec![
            entry("a", Canned::scoring(JudgeValue::Number(4.0), "good")),
            entry("b", Canned::scoring(JudgeValue::Number(2.5), "meh")),
            lint("c", Canned::scoring(JudgeValue::Number(9.0), "never asked")),
        ])
        .unwrap();
        let verdict = panel
            .judge_with_evidence(
                &JudgeQuery {
                    kind: JudgeKind::Numeric,
                    criterion: "c",
                    scale: Some((0.0, 10.0)),
                },
                &[],
                EvidenceContext::default(),
            )
            .unwrap();
        assert_eq!(verdict.value, JudgeValue::Number(2.5));
        // The judge that cannot score a number is left out entirely.
        assert_eq!(verdict.reason, "[a] good; [b] meh");

        let wrong = panel_of(vec![
            ("a", Canned::scoring(JudgeValue::Bool(true), "yes")),
            ("b", Canned::scoring(JudgeValue::Number(1.0), "one")),
        ]);
        let error = wrong
            .judge(
                &JudgeQuery {
                    kind: JudgeKind::Boolean,
                    criterion: "c",
                    scale: None,
                },
                &[],
            )
            .unwrap_err();
        assert!(matches!(&error, Error::Provider { context, .. } if context == "judge[b]"));
    }

    fn entry(label: &str, judge: Canned) -> JudgeEntry<Canned> {
        JudgeEntry::new(label, "command", judge)
    }

    /// A judge that can neither score a number, write prose nor play a user —
    /// the shape a lint judge takes.
    fn lint(label: &str, judge: Canned) -> JudgeEntry<Canned> {
        entry(label, judge).with_abilities(JudgeAbilities {
            numeric: false,
            prose: false,
            user: false,
        })
    }

    fn panel_of(judges: Vec<(&str, Canned)>) -> JudgePanel<Canned> {
        JudgePanel::new(
            judges
                .into_iter()
                .map(|(label, judge)| entry(label, judge))
                .collect(),
        )
        .unwrap()
    }

    #[test]
    fn assessments_stack_under_headers_and_skip_a_judge_that_cannot_write() {
        let panel = JudgePanel::new(vec![
            entry("a", Canned::scoring(JudgeValue::Bool(true), "x")),
            lint("lint", Canned::scoring(JudgeValue::Bool(true), "x")),
            entry("b", Canned::scoring(JudgeValue::Bool(true), "x")),
        ])
        .unwrap();
        let assessment = panel.assess("follow-ups", &[]).unwrap();
        assert_eq!(
            assessment.text,
            "## Judge `a` (command)\n\nabout follow-ups\n\n## Judge `b` (command)\n\nabout follow-ups"
        );
        // The legacy user op goes to the first judge that can play one.
        let user = panel.simulate_user("p", &[], Some("s")).unwrap();
        assert_eq!(user.message, "next");
        assert_eq!(
            panel.judges()[0].with(|j| j.sessions.borrow().clone()),
            vec![Some("s-a".to_string())]
        );

        let none = JudgePanel::new(vec![lint(
            "lint",
            Canned::scoring(JudgeValue::Bool(true), "x"),
        )])
        .unwrap();
        assert!(matches!(
            none.assess("p", &[]).unwrap_err(),
            Error::Invalid(m) if m.contains("assessment")
        ));
        assert!(matches!(
            none.simulate_user("p", &[], None).unwrap_err(),
            Error::Invalid(m) if m.contains("play the user")
        ));
        assert!(matches!(
            none.judge(&JudgeQuery { kind: JudgeKind::Numeric, criterion: "c", scale: None }, &[])
                .unwrap_err(),
            Error::Invalid(m) if m.contains("score a number")
        ));
    }

    #[test]
    fn a_judge_that_fails_a_verdict_or_an_assessment_fails_the_call_naming_it() {
        let panel = panel_of(vec![
            ("a", Canned::scoring(JudgeValue::Bool(true), "yes")),
            ("b", Canned::broken()),
        ]);
        let query = JudgeQuery {
            kind: JudgeKind::Boolean,
            criterion: "c",
            scale: None,
        };
        let error = panel.judge(&query, &[]).unwrap_err();
        let Error::Provider {
            context,
            message,
            kind,
        } = error
        else {
            panic!("a classified provider error")
        };
        assert_eq!(context, "judge[b]");
        // The failing judge's own classification is preserved.
        assert_eq!(kind, Some(ProviderErrorKind::Spawn));
        assert_eq!(
            message,
            "[a] true: yes; [b] failed: provider error (judge): no output"
        );

        let error = panel.assess("p", &[]).unwrap_err();
        assert!(
            matches!(&error, Error::Provider { context, kind: None, message }
                if context == "assess[b]"
                    && message == "[a] wrote an assessment; [b] failed: provider error (assess): no output"),
            "{error}"
        );
        // Verdict and assessment calls record no decision: only the per-turn
        // supervisor decision is a judge's decision on the work.
        assert!(panel.take_judge_decisions().is_empty());
    }

    #[test]
    fn a_panel_runs_no_agent_turn_and_stamps_telemetry_only_when_it_has_more_than_one_judge() {
        let panel = panel_of(vec![("only", Canned::deciding(done("x")))]);
        let skill = SkillRef {
            name: "s",
            dir: "/s",
            instructions: "i",
        };
        assert!(matches!(
            panel.respond(&skill, &[], None).unwrap_err(),
            Error::Invalid(m) if m.contains("no agent turn")
        ));
        assert!(matches!(
            panel
                .respond_streaming(&skill, &[], None, &mut |_| ControlFlow::Continue(()))
                .unwrap_err(),
            Error::Invalid(_)
        ));
        assert!(panel.invocation_telemetry().is_empty());
        assert!(panel.spawned_processes().is_empty());
        assert_eq!(
            panel.supervisor_control(),
            crate::ControlOutcome::NotRequested
        );
        assert_eq!(panel.control(), crate::ControlOutcome::NotRequested);
        panel.reset_telemetry();
        // A rebuilt panel keeps its judges in order.
        let rebuilt = panel.map_judges(|j| j);
        assert_eq!(rebuilt.judges()[0].label(), "only");
        assert_eq!(rebuilt.judges()[0].kind(), "command");
        assert!(rebuilt.judges()[0].abilities().numeric);
    }
}
