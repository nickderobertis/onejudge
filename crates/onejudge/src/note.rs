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
//! # Where the shapes are declared
//!
//! Nowhere in this crate. Every item here is `onemessagebus-agent`'s note contract
//! (`onemessagebus_agent::note`, the message `agent.note@1`), re-exported at this
//! path so one declaration serves every crate that carries a note. [`Notes`] and
//! [`NoteInbox`] are the core's `Sender` and `Inbox` over it. What stays in
//! `onejudge` is the *routing* below — which party a taken note reaches, and what
//! its sender is answered — because that is the conversation's, not the inbox's.
//!
//! Two consequences of the channel being the core's, for a caller:
//!
//! * [`Notes::send`] answers the core's `onemessagebus::Undelivered`. This module's
//!   [`Undelivered`] reads it back into the variant it was:
//!   `notes.send(note).map_err(Undelivered::from)`.
//! * **[`Notes::send`] blocks until a turn takes the note** — including a note sent
//!   before the run has started, which is answered [`Accepted::Queued`] only once
//!   the first turn opens. A caller that sends and then runs the engine on the same
//!   thread waits forever. Send from a thread other than the one driving the engine.
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
//! use onejudge::{Addressee, Conversation, Engine, Note, Notes, OneharnessProvider, Settings, Skill, Undelivered};
//!
//! let (notes, inbox) = Notes::channel();
//! let provider = OneharnessProvider::new();
//! let engine = Engine::new(&provider, Settings::new()).with_notes(inbox);
//!
//! // …from another thread, while the conversation runs:
//! std::thread::spawn(move || {
//!     notes
//!         .send(Note::to(Addressee::Worker, "the reviewer asked for a smaller diff"))
//!         .map_err(Undelivered::from)
//! });
//!
//! let outcome = engine.run(&Conversation::single_turn(
//!     Skill::new("demo", ".", "do the work"),
//!     "start",
//! ))?;
//! # let _ = outcome;
//! # Ok::<(), onejudge::Error>(())
//! ```

pub use onemessagebus_agent::note::{
    prelude, supervisor_block, worker_block, Accepted, Addressee, Criteria, Criterion,
    CriterionRefused, DeliveredNote, Note, NoteInbox, NoteInboxExt, NoteRefused, NoteText, Party,
    Undelivered,
};

/// **`Notes::send` blocks until a turn takes the note**, including a note sent
/// before the run has started, which is answered `Accepted::Queued` only once the
/// first turn opens. Send from a thread other than the one driving the engine.
#[doc(inline)]
pub use onemessagebus_agent::note::Notes;
