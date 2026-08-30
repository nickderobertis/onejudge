//! The **note delivery seam**: a role-addressed correction that reaches whichever
//! party of a running conversation is live, while the other party receives it with
//! that party's response.
//!
//! A note delivered to the worker alone leaves the judge evaluating work against a
//! task that never mentioned it; a note that binds the judge alone cannot interrupt
//! the worker. Neither reaches both. This module is the seam that does:
//!
//! * [`Notes::send`] hands one [`Note`] to a running conversation. It is delivered
//!   to **whoever is live** — the worker's turn, the judge's turn, or, between
//!   turns, the next turn to open.
//! * The party that receives it is told **which role it is for** ([`Addressee`]), so
//!   a judge handed an update to the *worker's* task does not take the worker's job
//!   on.
//! * A note may carry a [`Criterion`], and a delivered one enters the acceptance
//!   criteria the judge evaluates against ([`Criteria`]) — at both judging sites —
//!   rather than appearing only as narration.
//! * A note that arrives once the conversation has completed **raises**
//!   ([`Undelivered`]), naming that it was not delivered and why, because a caller
//!   can choose what to do about a refusal and can do nothing at all about a
//!   silence.
//!
//! # What "live delivery" means here
//!
//! An interrupt has never delivered *into* a running turn — oneharness's own
//! control channel aborts the turn and reopens the next one on the same session
//! with the message as its prompt. onejudge's in-process engine keeps that
//! semantics and its ordering guarantee, at the seam it owns:
//!
//! * A note arriving during the **worker's** turn is handed to the worker in a
//!   reopened turn *before the judge is consulted*, so the judge receives it
//!   together with the worker's response to it. The worker's finished reply is kept
//!   rather than discarded — it is real work, and a redirect throws away a turn's
//!   words, never its commits.
//! * A note arriving during the **judge's** turn reaches the judge: the decision
//!   taken without it is discarded and re-taken with the note in hand, which costs
//!   a second judge invocation. A caller sending notes into a tight loop pays per
//!   note.
//! * If that re-taken decision is completion, **nothing is delivered to the
//!   worker** — the judge passed the work with the note in hand
//!   ([`Accepted::JudgedWith`]).
//!
//! # Example
//!
//! ```no_run
//! use onejudge::{Addressee, Conversation, Engine, Note, Notes, OneharnessProvider, Settings, Skill};
//!
//! let (notes, inbox) = Notes::channel();
//! let provider = OneharnessProvider::new();
//! let engine = Engine::new(&provider, Settings::new()).with_notes(inbox);
//!
//! // …from another thread, while the conversation runs:
//! std::thread::spawn(move || {
//!     notes.send(Note::to(Addressee::Worker, "the reviewer asked for a smaller diff"))
//! });
//!
//! let outcome = engine.run(&Conversation::single_turn(
//!     Skill::new("demo", ".", "do the work"),
//!     "start",
//! ))?;
//! # let _ = outcome;
//! # Ok::<(), onejudge::Error>(())
//! ```

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};

use serde::{Deserialize, Serialize};

/// Who a note is for.
///
/// Required; there is no default, because a note whose addressee is guessed is a
/// note the judge may read as an instruction to itself. One run saw a simulated
/// user compose a four-point "manager ruling" in-conversation and instruct the
/// worker to post it over the run channel, and the worker complied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Addressee {
    /// An update to the *worker's* task.
    Worker,
    /// An update to the *supervisor's* brief.
    Supervisor,
    /// Addressed to both parties.
    Both,
}

impl Addressee {
    /// The stable wire string (`worker` / `supervisor` / `both`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Addressee::Worker => "worker",
            Addressee::Supervisor => "supervisor",
            Addressee::Both => "both",
        }
    }

    /// Whether this addressee is [`Addressee::Worker`] — the shape a caller that
    /// says nothing gets on a wire format that defaults the field.
    #[must_use]
    pub fn is_worker(&self) -> bool {
        matches!(self, Addressee::Worker)
    }
}

/// Which party of the conversation a delivery reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Party {
    /// The agent under test.
    Worker,
    /// The simulated user / completion supervisor.
    Supervisor,
}

/// The property a bound note requires of the finished work.
///
/// A validated newtype, checked in the conversion that builds it, so a [`Note`]
/// carrying an unusable criterion is unrepresentable rather than
/// representable-and-refused-somewhere-later. The rules are the ones an
/// orchestrator already enforces on authored plan criteria
/// (`orchestrator/criteria_guard.py` in `nickderobertis/ai-orchestrator`), ported
/// here so the *seam that accepts the note* refuses at the moment of binding
/// rather than at render time — after the turn has been composed — or by the
/// judge, which has already lost.
///
/// What the rules cannot catch, stated plainly: "this is a mechanism the author
/// preferred rather than a property the node owes" needs the author's intent, and
/// no pattern has it. What stands in for it is the rendered framing
/// ([`Criteria::rendered`]), which instructs the judge to evaluate the property a
/// named mechanism was serving.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Criterion(String);

impl Criterion {
    /// The criterion text, trimmed.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Criterion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Criterion({:?})", self.0)
    }
}

impl std::fmt::Display for Criterion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<Criterion> for String {
    fn from(criterion: Criterion) -> Self {
        criterion.0
    }
}

impl TryFrom<String> for Criterion {
    type Error = CriterionRefused;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        let trimmed = text.trim();
        match refusal(trimmed) {
            Some(why) => Err(CriterionRefused {
                criterion: text,
                why,
            }),
            None => Ok(Criterion(trimmed.to_string())),
        }
    }
}

impl TryFrom<&str> for Criterion {
    type Error = CriterionRefused;

    fn try_from(text: &str) -> Result<Self, Self::Error> {
        Criterion::try_from(text.to_string())
    }
}

impl std::str::FromStr for Criterion {
    type Err = CriterionRefused;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Criterion::try_from(text.to_string())
    }
}

/// A criterion the seam refused, naming the text and the rule it broke.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the criterion {criterion:?} was refused: {why}")]
pub struct CriterionRefused {
    /// The offending text, exactly as offered.
    pub criterion: String,
    /// Which rule refused it, and why that rule exists.
    pub why: String,
}

/// What a party reads: a note's prose, guaranteed to be something.
///
/// A validated newtype for the reason [`Criterion`] is one — the invariant belongs
/// to the value rather than to the moment it was built, so a note whose text nobody
/// can read stays unrepresentable for its whole life and not only at construction.
/// Trimmed on the way in, so the same words written with stray whitespace are the
/// same note.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct NoteText(String);

impl NoteText {
    /// The note's prose, trimmed.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for NoteText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "NoteText({:?})", self.0)
    }
}

impl std::fmt::Display for NoteText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for NoteText {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<NoteText> for String {
    fn from(text: NoteText) -> Self {
        text.0
    }
}

impl TryFrom<String> for NoteText {
    type Error = NoteRefused;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err(NoteRefused::Blank);
        }
        Ok(NoteText(trimmed.to_string()))
    }
}

impl std::str::FromStr for NoteText {
    type Err = NoteRefused;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        NoteText::try_from(text.to_string())
    }
}

/// One note: what the worker reads, who it is for, and the property it binds.
///
/// Built through [`Note::new`] / [`Note::to`] / [`Note::binding`] and in no other
/// way — `non_exhaustive` closes the struct literal, and deserialization goes
/// through the same conversion — so a note whose text nobody can read, or whose
/// criterion is unusable, is unrepresentable rather than representable and refused
/// somewhere later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "NoteWire")]
#[non_exhaustive]
pub struct Note {
    /// Who the note is for. Required, no default.
    pub addressee: Addressee,
    /// What the addressee reads. Blank is refused by [`NoteText`], so it stays
    /// readable however the field is later assigned.
    pub text: NoteText,
    /// The property the finished work must have, when this note binds one.
    ///
    /// `None` — the default, and the only thing a caller that says nothing gets —
    /// is an ordinary observational note: it reaches whoever is live, it is shown
    /// to the judge as context, and it touches no acceptance criterion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub criterion: Option<Criterion>,
}

/// The wire shape a `Note` is deserialized from, so an arriving note is held to the
/// same rules a locally-built one is.
#[derive(Deserialize)]
struct NoteWire {
    addressee: Addressee,
    text: String,
    #[serde(default)]
    criterion: Option<Criterion>,
}

impl TryFrom<NoteWire> for Note {
    type Error = NoteRefused;

    fn try_from(wire: NoteWire) -> Result<Self, Self::Error> {
        Ok(Note {
            addressee: wire.addressee,
            text: NoteText::try_from(wire.text)?,
            criterion: wire.criterion,
        })
    }
}

impl Note {
    /// A note addressed to `addressee` that binds nothing.
    ///
    /// # Errors
    /// [`NoteRefused::Blank`] when `text` is empty or whitespace: a note nobody can
    /// read is not a note.
    pub fn new(addressee: Addressee, text: impl Into<String>) -> Result<Self, NoteRefused> {
        Ok(Self {
            addressee,
            text: NoteText::try_from(text.into())?,
            criterion: None,
        })
    }

    /// [`Note::new`], panicking on a blank note — for a caller with a literal in
    /// hand, where a blank is a programming error rather than input.
    ///
    /// # Panics
    /// If `text` is empty or whitespace.
    #[must_use]
    pub fn to(addressee: Addressee, text: impl Into<String>) -> Self {
        Note::new(addressee, text).expect("a note carries text")
    }

    /// Bind this note to a property the finished work must have (builder style).
    ///
    /// # Errors
    /// [`NoteRefused::Criterion`] when the criterion breaks one of the rules
    /// [`Criterion`] documents.
    pub fn binding(mut self, criterion: impl Into<String>) -> Result<Self, NoteRefused> {
        self.criterion = Some(Criterion::try_from(criterion.into())?);
        Ok(self)
    }

    /// Whether this note is part of the bar. `criterion.is_some()`.
    #[must_use]
    pub fn binds(&self) -> bool {
        self.criterion.is_some()
    }
}

/// Why a note could not be built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NoteRefused {
    /// The note carried no text.
    #[error("a note carries text a party can read; this one was blank")]
    Blank,
    /// The note's criterion broke one of [`Criterion`]'s rules.
    #[error(transparent)]
    Criterion(#[from] CriterionRefused),
}

/// One note as it was handed to a party, with the party it reached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveredNote {
    /// The note itself, addressee included.
    pub note: Note,
    /// The party that was handed it — **not** the same thing as its addressee: a
    /// note is delivered to whoever is live, and the addressee is what the
    /// recipient is told the note is *for*.
    pub delivered_to: Party,
}

/// What became of one accepted note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Accepted {
    /// Queued between turns; the next turn to open gets it.
    Queued,
    /// It reached a live turn, which was redirected to carry it.
    Interrupted {
        /// The party whose turn it reached.
        party: Party,
    },
    /// It reached the supervisor's live turn, and that turn's re-taken answer was
    /// completion — so the work was passed with the note in hand and nothing was
    /// delivered to the worker. Not a failure, and not an [`Undelivered`].
    JudgedWith {
        /// The supervisor's completion reason, decided with the note in hand.
        completion_reason: String,
    },
}

/// Why a note will never be read.
///
/// Returned rather than deferred, because a caller can choose relaunch, tweak or
/// follow-up in response to a refusal and can do nothing at all about a silence.
/// One measured node accepted a note after the worker had reported completion, did
/// another forty minutes of correct work, and was failed for a completion report
/// that preceded its own subsequent commits.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Undelivered {
    /// The supervisor already answered completion. Carries its reason.
    #[error(
        "the note was not delivered: the conversation's supervisor already answered completion \
         ({completion_reason}), so nothing will read it. Relaunch the work, amend the task for a \
         later dispatch, or record the note as a follow-up."
    )]
    ConversationCompleted {
        /// The supervisor's completion reason.
        completion_reason: String,
    },
    /// The conversation ended before the note could be delivered.
    ///
    /// Named for the graph layer's word for one conversation — a *member* of a run —
    /// because this enum is mirrored one-to-one by the transports that carry a note
    /// in from outside the process, and a variant renamed on one side of that
    /// mapping is a variant silently dropped on the other.
    #[error(
        "the note was not delivered: the conversation had already ended ({outcome}), so nothing \
         will read it. Relaunch the work, amend the task for a later dispatch, or record the note \
         as a follow-up."
    )]
    MemberSettled {
        /// How the conversation ended.
        outcome: String,
    },
    /// Nothing ever ran the inbox this note was sent to.
    #[error(
        "the note was not delivered: no conversation ever read this note channel ({reason}). \
         Relaunch the work, amend the task for a later dispatch, or record the note as a \
         follow-up."
    )]
    NoConversation {
        /// What became of the channel instead.
        reason: String,
    },
}

/// The completion criterion actually in force: the configured one, plus every
/// criterion a delivered binding note added.
///
/// Composed once and read at **both** judging sites — the per-turn supervisor
/// decision and the authoritative re-judge against the finished transcript — so a
/// note that bound a criterion cannot be invisible to the verdict that decides
/// whether the work is done. On a run where no note bound anything,
/// [`Criteria::rendered`] returns the configured criterion byte for byte.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Criteria {
    configured: Option<String>,
    bound: Vec<Criterion>,
}

/// The framing the bound criteria are rendered under.
///
/// The first sentence states its own authority, and the last is the one lever
/// against a criterion written as a mechanism: work that reached the same property
/// another way has met it.
const CRITERIA_FRAME: &str = "\
## Additional acceptance criteria delivered during this run

Each item below is an additional criterion about the FINISHED WORK, delivered into \
the worker's task after this run began. It is an update to the WORKER's task and is \
not an instruction to you: do not perform it yourself, and do not judge the worker on \
anything beyond it. Where an item names a mechanism rather than a property, judge the \
property that mechanism was serving — work that reached the same property another way \
has met it.
";

impl Criteria {
    /// Compose the configured criterion with the criteria `delivered` notes bound,
    /// in delivery order.
    #[must_use]
    pub fn compose(configured: Option<&str>, delivered: &[DeliveredNote]) -> Self {
        Self {
            configured: configured.map(str::to_string),
            bound: delivered
                .iter()
                .filter_map(|d| d.note.criterion.clone())
                .collect(),
        }
    }

    /// What both judging sites ask.
    ///
    /// `None` only when there was no configured criterion and no note has bound
    /// one.
    #[must_use]
    pub fn rendered(&self) -> Option<String> {
        if self.bound.is_empty() {
            return self.configured.clone();
        }
        let mut out = String::new();
        if let Some(configured) = &self.configured {
            out.push_str(configured);
            out.push_str("\n\n");
        }
        out.push_str(CRITERIA_FRAME);
        for (index, criterion) in self.bound.iter().enumerate() {
            out.push_str(&format!("\n{}. {}", index + 1, criterion.as_str()));
        }
        Some(out)
    }

    /// The criteria bound during this run, in delivery order, for a caller that
    /// wants them separately (a report, a journal).
    #[must_use]
    pub fn bound(&self) -> &[Criterion] {
        &self.bound
    }
}

// --- The channel ----------------------------------------------------------

/// Where a conversation is, as a note's arrival sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Phase {
    /// No conversation has opened a turn yet; the first turn gets the note.
    NotStarted,
    /// Between turns; the next turn to open gets the note.
    BetweenTurns,
    /// The worker's turn is live.
    WorkerLive,
    /// The supervisor's turn is live.
    SupervisorLive,
    /// The supervisor answered completion.
    Completed(String),
    /// The conversation ended without a completion decision.
    Ended(String),
    /// Nothing ever ran this inbox.
    Abandoned(String),
}

#[derive(Debug)]
struct Accepting {
    id: u64,
    note: Note,
}

#[derive(Debug)]
struct State {
    phase: Phase,
    pending: Vec<Accepting>,
    delivered: Vec<DeliveredNote>,
    /// Accepted, taken out of `pending`, and not yet dispositioned: `None` until a
    /// party has actually been handed it, then the party that was. Closed out on
    /// the way down so no sender is ever left waiting on a note the loop dropped.
    awaiting: HashMap<u64, Option<Party>>,
    dispositions: HashMap<u64, Result<Accepted, Undelivered>>,
    next_id: u64,
}

#[derive(Debug)]
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        // A panicking engine still has to answer every waiting sender, so a
        // poisoned lock is read through rather than propagated: the alternative is
        // the silence this seam exists to refuse.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The caller's end of a running conversation's note channel.
///
/// `Clone`, `Send` and `Sync`: it is meant to be held by whatever supervises the
/// run from outside it, and used from a different thread than the one driving the
/// engine.
#[derive(Debug, Clone)]
pub struct Notes(Arc<Shared>);

/// The engine's end of the channel, handed to an [`Engine`](crate::Engine) with
/// [`Engine::with_notes`](crate::Engine::with_notes) or to a
/// `Plan` with `Plan::with_notes`.
#[derive(Debug)]
pub struct NoteInbox(Arc<Shared>);

impl Notes {
    /// Open a note channel: the caller's handle and the engine's inbox.
    #[must_use]
    pub fn channel() -> (Notes, NoteInbox) {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                phase: Phase::NotStarted,
                pending: Vec::new(),
                delivered: Vec::new(),
                awaiting: HashMap::new(),
                dispositions: HashMap::new(),
                next_id: 0,
            }),
            changed: Condvar::new(),
        });
        (Notes(Arc::clone(&shared)), NoteInbox(shared))
    }

    /// Hand `note` to the conversation reading this channel.
    ///
    /// Blocks until the note's disposition is known. That is immediate when
    /// nothing is live (the next turn gets it); when a turn *is* live it is the
    /// moment the note is handed to that party — and for the supervisor, the
    /// moment its re-taken decision comes back, because that decision is what
    /// distinguishes [`Accepted::Interrupted`] from [`Accepted::JudgedWith`].
    ///
    /// So **send from a thread other than the one driving the engine**. Only that
    /// thread can make the conversation move, and this call waits for it to.
    ///
    /// # Errors
    /// [`Undelivered`] when nothing will ever read it — the conversation completed,
    /// ended, or never ran.
    pub fn send(&self, note: Note) -> Result<Accepted, Undelivered> {
        let mut state = self.0.lock();
        match &state.phase {
            Phase::Completed(reason) => {
                return Err(Undelivered::ConversationCompleted {
                    completion_reason: reason.clone(),
                })
            }
            Phase::Ended(outcome) => {
                return Err(Undelivered::MemberSettled {
                    outcome: outcome.clone(),
                })
            }
            Phase::Abandoned(reason) => {
                return Err(Undelivered::NoConversation {
                    reason: reason.clone(),
                })
            }
            Phase::NotStarted | Phase::BetweenTurns | Phase::WorkerLive | Phase::SupervisorLive => {
            }
        }
        let id = state.next_id;
        state.next_id += 1;
        // Nothing has opened a turn yet, so there is nothing to wait on and no
        // conversation to make progress: answering here is what keeps a caller that
        // primes a channel before starting the run from waiting on itself.
        let answer_now = state.phase == Phase::NotStarted;
        state.pending.push(Accepting { id, note });
        self.0.changed.notify_all();
        if answer_now {
            return Ok(Accepted::Queued);
        }
        loop {
            if let Some(disposition) = state.dispositions.remove(&id) {
                return disposition;
            }
            state = self
                .0
                .changed
                .wait(state)
                .unwrap_or_else(|e| e.into_inner());
        }
    }

    /// Every note this channel has delivered, in delivery order — what the judge is
    /// shown as context and what [`Criteria`] is composed from.
    #[must_use]
    pub fn delivered(&self) -> Vec<DeliveredNote> {
        self.0.lock().delivered.clone()
    }
}

impl NoteInbox {
    /// The conversation has begun: the next turn to open takes what is queued.
    pub(crate) fn begin(&self) {
        let mut state = self.0.lock();
        if matches!(state.phase, Phase::NotStarted) {
            state.phase = Phase::BetweenTurns;
        }
        self.0.changed.notify_all();
    }

    pub(crate) fn enter_worker_turn(&self) {
        self.enter(Phase::WorkerLive);
    }

    pub(crate) fn enter_supervisor_turn(&self) {
        self.enter(Phase::SupervisorLive);
    }

    pub(crate) fn between_turns(&self) {
        self.enter(Phase::BetweenTurns);
    }

    fn enter(&self, phase: Phase) {
        let mut state = self.0.lock();
        if !matches!(
            state.phase,
            Phase::Completed(_) | Phase::Ended(_) | Phase::Abandoned(_)
        ) {
            state.phase = phase;
        }
        self.0.changed.notify_all();
    }

    /// Take everything accepted and not yet handed to a party.
    pub(crate) fn take_pending(&self) -> Vec<(u64, Note)> {
        let mut state = self.0.lock();
        let taken: Vec<(u64, Note)> = state
            .pending
            .drain(..)
            .map(|accepting| (accepting.id, accepting.note))
            .collect();
        for (id, _) in &taken {
            state.awaiting.insert(*id, None);
        }
        taken
    }

    /// Record that `notes` were handed to `party`, and answer their senders.
    pub(crate) fn record_delivery(
        &self,
        notes: Vec<(u64, Note)>,
        party: Party,
        accepted: Option<Accepted>,
    ) -> Vec<DeliveredNote> {
        let mut state = self.0.lock();
        let mut out = Vec::with_capacity(notes.len());
        for (id, note) in notes {
            let delivered = DeliveredNote {
                note,
                delivered_to: party,
            };
            state.delivered.push(delivered.clone());
            match accepted.clone() {
                Some(accepted) => {
                    state.awaiting.remove(&id);
                    state.dispositions.insert(id, Ok(accepted));
                }
                None => {
                    state.awaiting.insert(id, Some(party));
                }
            }
            out.push(delivered);
        }
        self.0.changed.notify_all();
        out
    }

    /// Answer the senders of notes whose disposition was only decided once the
    /// redirected supervisor turn came back.
    pub(crate) fn settle(&self, ids: &[u64], accepted: &Accepted) {
        let mut state = self.0.lock();
        for id in ids {
            state.awaiting.remove(id);
            state.dispositions.insert(*id, Ok(accepted.clone()));
        }
        self.0.changed.notify_all();
    }

    /// Everything handed to a party so far.
    pub(crate) fn delivered_notes(&self) -> Vec<DeliveredNote> {
        self.0.lock().delivered.clone()
    }

    /// The conversation's supervisor answered completion.
    pub(crate) fn complete(&self, reason: &str) {
        self.close(Phase::Completed(reason.to_string()));
    }

    /// The conversation ended without a completion decision.
    pub(crate) fn end(&self, outcome: &str) {
        self.close(Phase::Ended(outcome.to_string()));
    }

    fn close(&self, phase: Phase) {
        let mut state = self.0.lock();
        if matches!(state.phase, Phase::Abandoned(_)) {
            return;
        }
        state.phase = phase;
        answer_everyone_waiting(&mut state);
        self.0.changed.notify_all();
    }
}

impl Drop for NoteInbox {
    fn drop(&mut self) {
        // Whatever happened to the engine — a clean end, an error, a panic — every
        // sender still waiting has to be answered, and a note nothing ever read has
        // to say so.
        let mut state = self.0.lock();
        if matches!(state.phase, Phase::NotStarted) {
            state.phase = Phase::Abandoned(
                "the conversation's note inbox was dropped before any turn opened".into(),
            );
        } else if !matches!(state.phase, Phase::Completed(_) | Phase::Ended(_)) {
            state.phase = Phase::Ended("the conversation's note inbox was dropped".into());
        }
        answer_everyone_waiting(&mut state);
        self.0.changed.notify_all();
    }
}

/// Close out every sender still waiting: a note never handed to anyone is refused
/// with the reason, and one that *was* handed over reports the party it reached,
/// because that delivery really happened whatever became of the run afterwards.
fn answer_everyone_waiting(state: &mut State) {
    let undelivered = terminal_refusal(&state.phase);
    let orphaned: Vec<u64> = state
        .pending
        .drain(..)
        .map(|accepting| accepting.id)
        .collect();
    for id in orphaned {
        state.dispositions.insert(id, Err(undelivered.clone()));
    }
    let handed: Vec<(u64, Option<Party>)> = state.awaiting.drain().collect();
    for (id, party) in handed {
        let answer = match party {
            Some(party) => Ok(Accepted::Interrupted { party }),
            // Taken out of the queue and then never handed to anyone: the one shape
            // that would otherwise leave a sender waiting on a note the loop lost.
            None => Err(undelivered.clone()),
        };
        state.dispositions.insert(id, answer);
    }
}

fn terminal_refusal(phase: &Phase) -> Undelivered {
    match phase {
        Phase::Completed(reason) => Undelivered::ConversationCompleted {
            completion_reason: reason.clone(),
        },
        Phase::Abandoned(reason) => Undelivered::NoConversation {
            reason: reason.clone(),
        },
        Phase::Ended(outcome) => Undelivered::MemberSettled {
            outcome: outcome.clone(),
        },
        other => Undelivered::MemberSettled {
            outcome: format!("the conversation is {other:?}"),
        },
    }
}

// --- Rendering ------------------------------------------------------------

/// The block a party is handed when notes reach it, framed by the role each note
/// is addressed to. Empty when `notes` is empty.
#[must_use]
pub(crate) fn worker_block(notes: &[DeliveredNote]) -> String {
    let mut out = String::new();
    let mine: Vec<&DeliveredNote> = notes
        .iter()
        .filter(|d| !matches!(d.note.addressee, Addressee::Supervisor))
        .collect();
    let theirs: Vec<&DeliveredNote> = notes
        .iter()
        .filter(|d| matches!(d.note.addressee, Addressee::Supervisor))
        .collect();
    if !mine.is_empty() {
        out.push_str(
            "## Notes delivered to you during this run\n\n\
             The following were delivered to YOU, the worker, after this run began. Each is an \
             update to your task and takes precedence over anything earlier that disagrees with \
             it. Act on it as part of the work.\n",
        );
        for delivered in &mine {
            out.push_str(&format!("\n- {}", delivered.note.text));
            if let Some(criterion) = &delivered.note.criterion {
                out.push_str(&format!(
                    "\n  This note also added an acceptance criterion the finished work is \
                     judged against: {}",
                    criterion.as_str()
                ));
            }
        }
        out.push('\n');
    }
    if !theirs.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(
            "## Notes delivered to the supervisor during this run\n\n\
             The following were delivered to the SUPERVISOR, addressed to it and not to you. They \
             are not an instruction to you: do not act on them directly. They are here so you can \
             read the supervisor's response in light of what it was told.\n",
        );
        for delivered in &theirs {
            out.push_str(&format!("\n- {}", delivered.note.text));
        }
        out.push('\n');
    }
    out
}

/// The notes block the supervisor prompt renders beside the transcript, framed so
/// the judge knows which role each note is for and does not take the worker's job
/// on. `None` when nothing has been delivered.
#[must_use]
pub fn supervisor_block(notes: &[DeliveredNote]) -> Option<String> {
    if notes.is_empty() {
        return None;
    }
    let mut out = String::new();
    let worker_observational: Vec<&DeliveredNote> = notes
        .iter()
        .filter(|d| matches!(d.note.addressee, Addressee::Worker) && !d.note.binds())
        .collect();
    let worker_binding: Vec<&DeliveredNote> = notes
        .iter()
        .filter(|d| matches!(d.note.addressee, Addressee::Worker) && d.note.binds())
        .collect();
    let yours: Vec<&DeliveredNote> = notes
        .iter()
        .filter(|d| matches!(d.note.addressee, Addressee::Supervisor | Addressee::Both))
        .collect();

    if !worker_observational.is_empty() {
        out.push_str(
            "## Notes delivered to the worker during this run\n\n\
             The following were delivered to the WORKER, addressed to it and not to you. They \
             report observed state and add no acceptance criteria. Judge the work against the \
             completion criterion above; these are here so you can read what the worker did in \
             light of what it was told.\n",
        );
        for delivered in &worker_observational {
            out.push_str(&format!("\n- {}", delivered.note.text));
        }
        out.push('\n');
    }
    if !worker_binding.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(
            "## Notes delivered to the worker during this run, which also added a criterion\n\n\
             The following were delivered to the WORKER, addressed to it and not to you. Each one \
             also added an acceptance criterion, listed with the completion criterion above; the \
             text here is what the worker was told, so you can read what it did in light of it. \
             They are not an instruction to you: do not perform them yourself.\n",
        );
        for delivered in &worker_binding {
            out.push_str(&format!("\n- {}", delivered.note.text));
        }
        out.push('\n');
    }
    if !yours.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(
            "## Notes delivered to you during this run\n\n\
             The following were delivered to YOU, the supervisor. Where one is addressed to both \
             parties it is an update to the worker's task as well, and any acceptance criterion it \
             added is listed with the completion criterion above.\n",
        );
        for delivered in &yours {
            out.push_str(&format!(
                "\n- (addressed to {}) {}",
                delivered.note.addressee.as_str(),
                delivered.note.text
            ));
        }
        out.push('\n');
    }
    Some(out)
}

// --- Criterion validation -------------------------------------------------

/// Work the dispatch cannot perform: it happens after the worker settles.
const OUT_OF_DISPATCH: [&str; 7] = [
    "branch publishes",
    "is published",
    "pr is merged",
    "pull request is merged",
    "lands on master",
    "lands on main",
    "deploy",
];

/// A criterion with no content of its own: a lead-in that defers to prose
/// elsewhere, followed by the word it defers with.
const DEFERRAL_LEAD: [&[&str]; 6] = [
    &["of", "the", "shape"],
    &["as"],
    &["exactly", "as"],
    &["in", "the", "form"],
    &["matching", "the", "wording"],
    &["per", "the"],
];
const DEFERRAL_TAIL: [&[&str]; 7] = [
    &["described"],
    &["specified"],
    &["stated"],
    &["set", "out"],
    &["given"],
    &["above"],
    &["below"],
];

/// A demand for a particular string rather than a particular property.
const PHRASE: [&[&str]; 5] = [
    &["verbatim"],
    &["word", "for", "word"],
    &["the", "exact", "phrase"],
    &["the", "exact", "wording"],
    &["the", "exact", "words"],
];

/// Shell binaries whose named invocation inside a code span is procedure rather
/// than property.
const SHELL_BINARIES: [&str; 6] = ["npm", "pnpm", "nx", "cargo", "pytest", "git"];

/// Why `text` cannot be a criterion, or `None` when it can.
fn refusal(text: &str) -> Option<String> {
    if text.is_empty() {
        return Some(
            "it is blank, which is a bar nobody can clear. State the property the finished work \
             must have."
                .into(),
        );
    }
    let lowered = text.to_lowercase();
    for phrase in OUT_OF_DISPATCH {
        if lowered.contains(phrase) {
            return Some(format!(
                "it names '{phrase}' — that is work the dispatch cannot do, so finished work \
                 fails against it. State the worker-side precondition instead."
            ));
        }
    }
    let words = words_of(&lowered);
    if let Some(found) = deferral(&words) {
        return Some(format!(
            "it defers its content to prose elsewhere ('{found}'). A criterion the judge has to \
             reconstruct is one it reconstructs as a wording demand. State the property here, in \
             full."
        ));
    }
    for phrase in PHRASE {
        if let Some(found) = sequence_at(&words, phrase) {
            return Some(format!(
                "it demands a particular string ('{found}') rather than a particular property. The \
                 property can be met and the wording failed."
            ));
        }
    }
    if let Some(found) = version_literal(text) {
        return Some(format!(
            "it names a version literal ('{found}'). A release published between this note being \
             written and the work being judged makes finished work fail against it. State the \
             property that version stands in for."
        ));
    }
    if let Some(found) = code_span_invocation(text, &["just"]) {
        return Some(format!(
            "it names a `just` invocation ('{found}'). Criteria state properties; a judge fails \
             the spelling of a command."
        ));
    }
    if text.contains("&&") {
        return Some(
            "it names a chained shell command ('&&'). Criteria state properties; a judge fails \
             the spelling of a command."
                .into(),
        );
    }
    if let Some(found) = code_span_invocation(text, &SHELL_BINARIES) {
        return Some(format!(
            "it names a shell invocation ('{found}'). Criteria state properties; a judge fails \
             the spelling of a command."
        ));
    }
    None
}

/// Split `text` into lowercase alphanumeric words, the unit the phrase rules match
/// on, so punctuation between them cannot hide a match.
fn words_of(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_string)
        .collect()
}

/// The word sequence `phrase` as it appears in `words`, or `None`.
fn sequence_at(words: &[String], phrase: &[&str]) -> Option<String> {
    words
        .windows(phrase.len())
        .find(|window| window.iter().zip(phrase).all(|(word, part)| word == part))
        .map(|window| window.join(" "))
}

/// A deferral lead-in immediately followed by the word it defers with, or the
/// bare `the wording above`.
fn deferral(words: &[String]) -> Option<String> {
    if let Some(found) = sequence_at(words, &["the", "wording", "above"]) {
        return Some(found);
    }
    for lead in DEFERRAL_LEAD {
        for start in 0..words.len() {
            if words[start..].len() < lead.len() {
                continue;
            }
            if !words[start..start + lead.len()]
                .iter()
                .zip(lead)
                .all(|(word, part)| word == part)
            {
                continue;
            }
            let rest = &words[start + lead.len()..];
            for tail in DEFERRAL_TAIL {
                if rest.len() >= tail.len()
                    && rest[..tail.len()]
                        .iter()
                        .zip(tail)
                        .all(|(word, part)| word == part)
                {
                    return Some(format!("{} {}", lead.join(" "), tail.join(" ")));
                }
            }
        }
    }
    None
}

/// A release number written into a criterion. Three shapes, and no bare
/// `<n>.<n>`: an unprefixed two-component number is a duration, a percentage or a
/// schema version far more often than it is a release, and a false refusal blocks
/// correct work.
fn version_literal(text: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    for start in 0..chars.len() {
        // `<major>.<minor>.<patch>`, optionally prefixed `v` and suffixed.
        if is_boundary(&chars, start) {
            let mut at = start;
            if chars[at] == 'v' {
                at += 1;
            }
            if let Some(end) = dotted(&chars, at, 3) {
                let end = suffix(&chars, end);
                return Some(chars[start..end].iter().collect());
            }
            // `v<major>.<minor>`.
            if chars[start] == 'v' {
                if let Some(end) = dotted(&chars, start + 1, 2) {
                    if !matches!(chars.get(end), Some('.')) {
                        return Some(chars[start..end].iter().collect());
                    }
                }
            }
        }
        // A comparator against `<major>.<minor>`.
        if matches!(chars[start], '>' | '<' | '=' | '~' | '^' | '!') {
            let mut at = start + 1;
            if matches!(chars.get(at), Some('=')) {
                at += 1;
            }
            while matches!(chars.get(at), Some(c) if c.is_whitespace()) {
                at += 1;
            }
            if let Some(end) = dotted(&chars, at, 2) {
                return Some(chars[start..end].iter().collect::<String>().trim().into());
            }
        }
    }
    None
}

/// Whether a token may start at `at`: nothing word-like immediately before it.
fn is_boundary(chars: &[char], at: usize) -> bool {
    at == 0 || !(chars[at - 1].is_alphanumeric() || chars[at - 1] == '_')
}

/// The end of `components` dot-separated digit runs starting at `at`, or `None`.
fn dotted(chars: &[char], at: usize, components: usize) -> Option<usize> {
    let mut at = at;
    for index in 0..components {
        if index > 0 {
            if !matches!(chars.get(at), Some('.')) {
                return None;
            }
            at += 1;
        }
        let digits = chars[at..]
            .iter()
            .take_while(|c| c.is_ascii_digit())
            .count();
        if digits == 0 {
            return None;
        }
        at += digits;
    }
    Some(at)
}

/// Consume a `-`/`+`/`.`-introduced prerelease or build suffix after a version.
fn suffix(chars: &[char], at: usize) -> usize {
    if !matches!(chars.get(at), Some('-' | '+' | '.')) {
        return at;
    }
    let mut end = at + 1;
    while matches!(chars.get(end), Some(c) if c.is_ascii_alphanumeric() || *c == '.' || *c == '-') {
        end += 1;
    }
    end
}

/// A named invocation inside a backtick code span: the shape a judge fails on
/// spelling. Matches oneharness's own rule of a backtick followed by non-backtick
/// text containing `<binary> `.
fn code_span_invocation(text: &str, binaries: &[&str]) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    for (start, c) in chars.iter().enumerate() {
        if *c != '`' {
            continue;
        }
        let end = chars[start + 1..]
            .iter()
            .position(|c| *c == '`')
            .map_or(chars.len(), |offset| start + 1 + offset);
        let span: Vec<char> = chars[start + 1..end].to_vec();
        for binary in binaries {
            let needle: Vec<char> = binary.chars().collect();
            for at in 0..span.len() {
                if !is_boundary(&span, at) || span.len() < at + needle.len() + 1 {
                    continue;
                }
                if span[at..at + needle.len()] == needle[..]
                    && span[at + needle.len()].is_whitespace()
                {
                    return Some(format!("{binary} "));
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(addressee: Addressee, text: &str) -> DeliveredNote {
        DeliveredNote {
            note: Note::to(addressee, text),
            delivered_to: Party::Worker,
        }
    }

    #[test]
    fn an_addressee_names_itself_on_the_wire_and_says_which_one_is_the_default() {
        assert_eq!(Addressee::Worker.as_str(), "worker");
        assert_eq!(Addressee::Supervisor.as_str(), "supervisor");
        assert_eq!(Addressee::Both.as_str(), "both");
        // The shape a caller that says nothing gets on a wire format that defaults
        // the field: omitted means the worker, which is what every correction
        // written before this field existed meant.
        assert!(Addressee::Worker.is_worker());
        assert!(!Addressee::Supervisor.is_worker());
        assert_eq!(serde_json::to_string(&Addressee::Both).unwrap(), "\"both\"");
        assert_eq!(
            serde_json::to_string(&Party::Supervisor).unwrap(),
            "\"supervisor\""
        );
    }

    #[test]
    fn a_note_carries_text_a_party_can_read() {
        assert_eq!(
            Note::new(Addressee::Worker, "   \n ").unwrap_err(),
            NoteRefused::Blank
        );
        let plain = Note::new(Addressee::Worker, "  look again at the migration ").unwrap();
        assert!(!plain.binds());
        assert!(plain.criterion.is_none());
        // The invariant is the value's, not the constructor's: it survives being
        // assigned into an existing note, and it trims on the way in either way.
        assert_eq!(plain.text.as_str(), "look again at the migration");
        assert_eq!(plain.text.to_string(), "look again at the migration");
        assert_eq!(plain.text.as_ref(), "look again at the migration");
        assert_eq!(
            format!("{:?}", plain.text),
            "NoteText(\"look again at the migration\")"
        );
        assert_eq!(
            String::from(plain.text.clone()),
            "look again at the migration"
        );
        assert_eq!(
            NoteText::try_from(" \n ".to_string()),
            Err(NoteRefused::Blank)
        );
        assert_eq!("".parse::<NoteText>(), Err(NoteRefused::Blank));
        assert_eq!(
            "still readable".parse::<NoteText>().unwrap().as_str(),
            "still readable"
        );
    }

    #[test]
    fn a_note_arriving_over_the_wire_is_held_to_the_rules_a_local_one_is() {
        let good: Note = serde_json::from_str(
            r#"{"addressee":"both","text":"the bar moved","criterion":"the flag defaults to off"}"#,
        )
        .unwrap();
        assert_eq!(good.addressee, Addressee::Both);
        assert!(good.binds());
        // A note that crossed the boundary is the note that was sent.
        let json = serde_json::to_string(&good).unwrap();
        assert_eq!(serde_json::from_str::<Note>(&json).unwrap(), good);
        // …and the two states the constructors refuse are refused here too, rather
        // than arriving through the one door that skipped them.
        assert!(serde_json::from_str::<Note>(r#"{"addressee":"worker","text":"  "}"#).is_err());
        assert!(serde_json::from_str::<Note>(
            r#"{"addressee":"worker","text":"look","criterion":"the pin moves to 1.2.3"}"#
        )
        .is_err());
    }

    #[test]
    fn a_criterion_round_trips_through_every_conversion_a_caller_has() {
        let text = "the migration path is covered by a test";
        let owned = Criterion::try_from(text.to_string()).unwrap();
        let borrowed = Criterion::try_from(text).unwrap();
        let parsed: Criterion = text.parse().unwrap();
        assert_eq!(owned, borrowed);
        assert_eq!(owned, parsed);
        assert_eq!(owned.to_string(), text);
        assert_eq!(format!("{owned:?}"), format!("Criterion({text:?})"));
        assert_eq!(String::from(owned.clone()), text);
        // Trimmed on the way in, so the same property written with stray whitespace
        // is the same criterion rather than a second one.
        assert_eq!(Criterion::try_from(format!("  {text} ")).unwrap(), owned);
        // …and it survives the wire, since a note crosses one.
        let json = serde_json::to_string(&owned).unwrap();
        assert_eq!(json, format!("{text:?}"));
        assert_eq!(serde_json::from_str::<Criterion>(&json).unwrap(), owned);
        let refused = serde_json::from_str::<Criterion>("\"the pin moves to 1.2.3\"");
        assert!(
            refused.is_err(),
            "a criterion arriving over the wire is held to the same rules"
        );
    }

    /// Every rule, at the shapes that made this host fail finished work, and the
    /// near-misses each one must NOT refuse. A rule that refuses correct work
    /// blocks a caller with no way around it.
    #[test]
    fn each_criterion_rule_refuses_its_shape_and_leaves_the_near_miss_alone() {
        let why =
            |text: &str| refusal(text).unwrap_or_else(|| panic!("expected a refusal: {text}"));

        assert!(why("v1.2 of the pin is in place").contains("version literal"));
        assert!(why("the dependency is >= 2.4").contains("version literal"));
        assert!(why("the crate is at 0.12.1-rc.1").contains("version literal"));
        assert!(why("the criteria are stated as above").contains("defers its content"));
        assert!(why("the answer matches the wording above").contains("defers its content"));
        assert!(why("the reason is set out per the stated shape").contains("defers its content"));
        assert!(why("the heading is word for word the same").contains("particular string"));
        assert!(why("it uses the exact wording").contains("particular string"));
        assert!(why("the branch publishes cleanly").contains("work the dispatch cannot do"));
        assert!(why("`pnpm test` is green").contains("shell invocation"));

        // A bare `<n>.<n>` is a duration, a percentage or a schema version far more
        // often than a release, and refusing it would block correct work.
        assert_eq!(
            refusal("the timeout is 1.5 seconds and coverage holds"),
            None
        );
        assert_eq!(refusal("the report declares schema 0.5"), None);
        // A backticked identifier that is not an invocation is fine, and so is prose
        // that merely mentions a tool without spelling a command.
        assert_eq!(
            refusal("`Report::control` is serialized even when null"),
            None
        );
        assert_eq!(refusal("the cargo manifest declares the new feature"), None);
        // "as" only defers when it is followed by the word it defers with.
        assert_eq!(refusal("the flag is off as a default"), None);
    }

    #[test]
    fn criteria_compose_the_configured_bar_with_what_notes_bound() {
        // Nothing bound: byte-identical to the configured criterion, which is what
        // keeps every run that sends no note unchanged.
        let none = Criteria::compose(Some("the task is done"), &[]);
        assert_eq!(none.rendered().as_deref(), Some("the task is done"));
        assert!(none.bound().is_empty());
        assert_eq!(Criteria::default().rendered(), None);
        assert_eq!(Criteria::compose(None, &[]).rendered(), None);

        let bound = vec![
            DeliveredNote {
                note: Note::to(Addressee::Worker, "first")
                    .binding("the migration is covered")
                    .unwrap(),
                delivered_to: Party::Worker,
            },
            DeliveredNote {
                note: Note::to(Addressee::Both, "second")
                    .binding("the flag defaults to off")
                    .unwrap(),
                delivered_to: Party::Supervisor,
            },
            note(Addressee::Worker, "third, binding nothing"),
        ];
        let composed = Criteria::compose(Some("the task is done"), &bound);
        assert_eq!(
            composed.bound().len(),
            2,
            "a note that binds nothing adds nothing"
        );
        let rendered = composed.rendered().unwrap();
        assert!(rendered.starts_with("the task is done\n\n"));
        assert!(rendered.contains("1. the migration is covered"));
        assert!(rendered.contains("2. the flag defaults to off"));
        assert!(rendered.contains("judge the property that mechanism was serving"));

        // Without a configured criterion the bound ones are the whole bar.
        let alone = Criteria::compose(None, &bound).rendered().unwrap();
        assert!(alone.starts_with("## Additional acceptance criteria"));
    }

    #[test]
    fn a_party_is_told_which_role_each_note_it_is_handed_is_for() {
        assert_eq!(supervisor_block(&[]), None);
        assert!(worker_block(&[]).is_empty());

        let mixed = vec![
            note(Addressee::Worker, "observed state"),
            DeliveredNote {
                note: Note::to(Addressee::Worker, "and this one moved the bar")
                    .binding("the migration is covered")
                    .unwrap(),
                delivered_to: Party::Worker,
            },
            note(Addressee::Supervisor, "hold the bar where it is"),
            note(Addressee::Both, "the ruling applies to both of you"),
        ];

        let judge = supervisor_block(&mixed).unwrap();
        assert!(judge.contains("## Notes delivered to the worker during this run\n"));
        assert!(judge.contains("They report observed state and add no acceptance criteria"));
        assert!(judge.contains("which also added a criterion"));
        assert!(judge.contains("listed with the completion criterion above"));
        assert!(judge.contains("## Notes delivered to you during this run"));
        assert!(judge.contains("(addressed to supervisor) hold the bar where it is"));
        assert!(judge.contains("(addressed to both) the ruling applies to both of you"));

        let worker = worker_block(&mixed);
        assert!(worker.contains("delivered to YOU, the worker"));
        assert!(worker.contains("Act on it as part of the work"));
        assert!(worker.contains(
            "This note also added an acceptance criterion the finished work is judged against"
        ));
        assert!(worker.contains("## Notes delivered to the supervisor during this run"));
        assert!(worker.contains("addressed to it and not to you"));
    }

    #[test]
    fn a_channel_nothing_ever_ran_refuses_rather_than_queueing_forever() {
        let (notes, inbox) = Notes::channel();
        drop(inbox);
        assert!(matches!(
            notes.send(Note::to(Addressee::Worker, "too late")),
            Err(Undelivered::NoConversation { .. })
        ));
    }

    #[test]
    fn a_conversation_that_began_and_was_dropped_settles_every_note_it_never_read() {
        let (notes, inbox) = Notes::channel();
        inbox.begin();
        inbox.begin(); // idempotent: a second run does not rewind the phase
        let sender = std::thread::spawn(move || {
            (
                notes.send(Note::to(Addressee::Worker, "never read")),
                notes.send(Note::to(Addressee::Worker, "nor this")),
            )
        });
        // Give the sender time to be waiting on a disposition, then take the inbox
        // away exactly as a panicking engine would.
        std::thread::sleep(std::time::Duration::from_millis(50));
        drop(inbox);
        let (first, second) = sender.join().unwrap();
        for answer in [first, second] {
            match answer {
                Err(Undelivered::MemberSettled { outcome }) => {
                    assert!(outcome.contains("dropped"), "{outcome}");
                }
                // A note that reached the drain before the drop is answered by it.
                other => panic!("a dropped inbox answers every sender: {other:?}"),
            }
        }
    }

    #[test]
    fn a_terminal_conversation_stays_terminal() {
        let (notes, inbox) = Notes::channel();
        inbox.begin();
        inbox.complete("the work is done");
        // A phase change after the decision must not reopen the channel.
        inbox.enter_worker_turn();
        inbox.between_turns();
        assert!(matches!(
            notes.send(Note::to(Addressee::Worker, "too late")),
            Err(Undelivered::ConversationCompleted { completion_reason })
                if completion_reason == "the work is done"
        ));
        // …and a second close does not overwrite the reason the first one gave.
        inbox.end("something else");
        assert!(matches!(
            notes.send(Note::to(Addressee::Worker, "still too late")),
            Err(Undelivered::MemberSettled { .. })
        ));
        drop(inbox);
    }

    #[test]
    fn a_note_taken_out_of_the_queue_and_never_handed_over_still_answers_its_sender() {
        let (notes, inbox) = Notes::channel();
        inbox.begin();
        inbox.enter_worker_turn();
        let sender = std::thread::spawn(move || notes.send(Note::to(Addressee::Worker, "lost")));
        // Drain it the way the loop does, and then end the run without delivering
        // it: the one shape that would otherwise leave a sender waiting forever.
        let taken = loop {
            let taken = inbox.take_pending();
            if !taken.is_empty() {
                break taken;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert_eq!(taken.len(), 1);
        inbox.end("the conversation ended without a completion decision");
        assert!(matches!(
            sender.join().unwrap(),
            Err(Undelivered::MemberSettled { .. })
        ));
    }

    #[test]
    fn a_note_handed_to_a_party_reports_that_party_even_if_the_run_then_dies() {
        let (notes, inbox) = Notes::channel();
        inbox.begin();
        inbox.enter_supervisor_turn();
        let sender = std::thread::spawn(move || notes.send(Note::to(Addressee::Worker, "held")));
        let taken = loop {
            let taken = inbox.take_pending();
            if !taken.is_empty() {
                break taken;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        let delivered = inbox.record_delivery(taken, Party::Supervisor, None);
        assert_eq!(delivered.len(), 1);
        assert_eq!(inbox.delivered_notes().len(), 1);
        drop(inbox);
        assert_eq!(
            sender.join().unwrap(),
            Ok(Accepted::Interrupted {
                party: Party::Supervisor
            }),
            "the delivery really happened, whatever became of the run afterwards"
        );
    }
}
