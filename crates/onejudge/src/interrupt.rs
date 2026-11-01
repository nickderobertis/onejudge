//! Redirecting a **live turn** for the conversation's own notes.
//!
//! A note that arrives while the party it addresses is mid-turn reaches that turn
//! by interrupting it, when the turn was opened with control: onejudge pulls the
//! lever itself, through the linked core's `oneharness_core::io::control::send`,
//! against the address the provider said the turn would have
//! ([`Provider::turn_control`](crate::Provider::turn_control)). No new oneharness
//! flag is sent and nothing is spawned for it — the request is a frame on the
//! turn's own control socket, the one `oneharness interrupt` dials.
//!
//! One [`Watch`] runs beside one live turn, on its own thread, because the engine's
//! thread is inside the provider call for the whole of the turn. It answers to one
//! rule: **take a note only when it is about to be presented.**
//!
//! * Until a socket is listening at the address, it takes nothing. A turn opened
//!   without control never has one, so a note sent to it stays untaken — still
//!   withdrawable, its deadline still running — until the turn ends.
//! * It claims only an [`Delivery::Interrupt`] note addressed to the live party
//!   (`Inbox::take_if_within`). Every other note — a [`Delivery::TurnEnd`] note, a
//!   note to the other party — is never taken while the turn runs: it keeps its
//!   place, its sender can still withdraw it, and it never stands in the way of a
//!   later note. It is taken once the turn ends, as a turn-end delivery.
//! * A note it takes is redirected at once. Only `Served { redirected: true }`
//!   counts: oneharness says that only once the replacement turn has opened with
//!   the text, and on every other answer it never writes the text later.
//! * A note it cannot present is handed back untaken with `Delivered::release`. A
//!   redirect oneharness does not confirm ends the watch: each refusal it gives
//!   (`no_active_turn`, `not_running`, `unsupported`) means no redirect can land in
//!   this turn any more, so the note waits for the next turn. A note whose framing
//!   is no redirect at all (past the size a redirect carries) is handed back and
//!   passed over for the rest of the turn, and the watch goes on waiting for the
//!   next one. A released note whose deadline has passed expires in that same
//!   moment.
//!
//! So until the turn ends, an interrupt note to the live party is presented as it
//! arrives, whatever is queued ahead of it and whatever was withdrawn.

use std::path::PathBuf;
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::Duration;

use oneharness_core::domain::control::{socket_path, ControlRequest, RedirectInput};
use oneharness_core::domain::events::ActionEvent;
use onemessagebus::Delivered;

use crate::control::ControlAddress;
use crate::note::{Accepted, Addressee, DeliveredNote, Delivery, Note, NoteInbox, Party};

/// How often a watch looks for the socket, and waits for a note, while the turn
/// is live. Short against a turn that runs for minutes, and long enough that an
/// idle watch costs nothing measurable.
const POLL: Duration = Duration::from_millis(20);

/// How much of the live turn the engine's own sink had observed: the tool events
/// and the worker's own message text, in order.
#[derive(Debug, Clone, Default)]
pub(crate) struct Observed {
    pub(crate) tools: usize,
    pub(crate) texts: Vec<String>,
}

/// One redirect oneharness confirmed.
pub(crate) struct Redirect {
    /// The note the replacement turn opened with.
    pub(crate) note: Note,
    /// The text the replacement turn was opened with — exactly what the party
    /// was handed.
    pub(crate) input: String,
    /// The sender, still waiting, when its answer depends on what the redirected
    /// turn decides (a supervisor turn); `None` once it has been answered.
    pub(crate) held: Option<Delivered<Note, Accepted>>,
    /// What had been observed of the turn when the redirect was confirmed: the
    /// interrupted turn's share of it.
    pub(crate) at: Observed,
}

/// How a redirected note is rendered for the turn it opens, given every note
/// redirected into this turn so far (the new one last).
pub(crate) type Framing<'a> = Box<dyn Fn(&[DeliveredNote]) -> String + Send + Sync + 'a>;

/// The watch over one live turn.
pub(crate) struct Watch<'a> {
    inbox: &'a NoteInbox,
    party: Party,
    socket: PathBuf,
    framing: Framing<'a>,
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Default)]
struct State {
    ended: bool,
    observed: Observed,
}

impl<'a> Watch<'a> {
    /// A watch over `party`'s turn at `address`, or `None` when the address has
    /// no socket path on this platform's budget — a turn nothing could dial.
    pub(crate) fn new(
        inbox: &'a NoteInbox,
        party: Party,
        address: &ControlAddress,
        framing: Framing<'a>,
    ) -> Option<Self> {
        let socket =
            socket_path(std::path::Path::new(&address.session_dir), &address.session).ok()?;
        Some(Self {
            inbox,
            party,
            socket,
            framing,
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
        })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Fold one event the engine's sink observed into what a redirect is
    /// measured against.
    pub(crate) fn observe(&self, event: &ActionEvent) {
        let mut state = self.state();
        if crate::oneharness::tool_event(event).is_some() {
            state.observed.tools += 1;
        } else if event.kind == "message" {
            if let Some(text) = event.output.as_deref().filter(|t| !t.trim().is_empty()) {
                state.observed.texts.push(text.to_string());
            }
        }
    }

    /// The turn is over: the watch takes nothing more.
    pub(crate) fn end(&self) {
        self.state().ended = true;
        self.changed.notify_all();
    }

    fn ended(&self) -> bool {
        self.state().ended
    }

    /// Wait up to [`POLL`] for the turn to end; `true` when it has.
    fn wait(&self) -> bool {
        let state = self.state();
        if state.ended {
            return true;
        }
        self.changed
            .wait_timeout(state, POLL)
            .unwrap_or_else(PoisonError::into_inner)
            .0
            .ended
    }

    /// Whether `note` is one this watch may present by interrupting.
    fn interrupts(&self, note: &Note) -> bool {
        let addressed = match self.party {
            Party::Worker => matches!(note.addressee, Addressee::Worker | Addressee::Both),
            Party::Supervisor => matches!(note.addressee, Addressee::Supervisor | Addressee::Both),
        };
        addressed && note.delivery == Delivery::Interrupt
    }

    /// The watch's whole life: run on its own thread from the turn's opening to
    /// [`Watch::end`], and hand back every redirect oneharness confirmed.
    pub(crate) fn run(&self) -> Vec<Redirect> {
        let mut redirected: Vec<Redirect> = Vec::new();
        let mut presented: Vec<DeliveredNote> = Vec::new();
        // Nothing is taken before a socket is listening: until then the turn has
        // no lever, and a note taken now could only be handed back.
        while !self.socket.exists() {
            if self.wait() {
                return redirected;
            }
        }
        // Notes whose framing could not be a redirect: handed back, and passed
        // over for the rest of the turn rather than claimed again on every poll.
        let mut unframeable: Vec<Note> = Vec::new();
        while !self.ended() {
            let Some(delivered) = self.inbox.take_if_within(POLL, |note| {
                self.interrupts(note) && !unframeable.contains(note)
            }) else {
                continue;
            };
            presented.push(DeliveredNote {
                note: delivered.message().clone(),
                delivered_to: self.party,
            });
            let input = (self.framing)(&presented);
            let Ok(request) = RedirectInput::new(input.clone()).map(ControlRequest::redirect)
            else {
                presented.pop();
                unframeable.push(delivered.message().clone());
                delivered.release();
                continue;
            };
            let response = oneharness_core::io::control::send(&self.socket, &request);
            if !response.is_redirected() {
                delivered.release();
                break;
            }
            let at = self.state().observed.clone();
            let note = delivered.message().clone();
            // A worker turn's redirect is the whole delivery, so its sender is
            // answered now; a supervisor's waits on the decision it reaches.
            let held = match self.party {
                Party::Worker => {
                    delivered.answer(Accepted::Interrupted {
                        party: Party::Worker,
                    });
                    None
                }
                Party::Supervisor => Some(delivered),
            };
            redirected.push(Redirect {
                note,
                input,
                held,
                at,
            });
        }
        redirected
    }
}
