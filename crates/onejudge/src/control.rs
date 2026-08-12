//! Out-of-band turn control: the **address** of a controllable turn.
//!
//! `oneharness run --control` opens a unix socket for the run's lifetime, and a
//! separate `oneharness interrupt` process redirects the in-flight turn through
//! it. onejudge's part is deliberately narrow: it can *ask* for a controllable
//! turn on the agent side (`provider.control: true`, off by default), and it
//! reports **where the turn it got is addressed**. It never interrupts anything
//! itself — the lever belongs to whatever is supervising the run.
//!
//! A [`ControlAddress`] is exactly the three values `oneharness interrupt` takes
//! to reach a turn (`--session`, `--session-dir`, `--cwd`), and nothing else: the
//! socket path is not among them because `interrupt` derives it from the store
//! directory and the handle, and re-reporting it would be a second source for one
//! fact. `docs/control.md` is the contract.
//!
//! The whole thing is one three-state answer, [`ControlOutcome`], because "not
//! asked for" and "asked for and refused" are different facts that a supervisor
//! must be able to tell apart — an `Option<ControlAddress>` alone cannot.

use serde::{Deserialize, Serialize};

/// Where an `oneharness interrupt` process addresses the controllable turn
/// onejudge's agent side opened.
///
/// The three fields are the whole of `interrupt`'s addressing:
///
/// ```text
/// oneharness interrupt --session <session> --session-dir <session_dir> \
///                      --cwd <cwd> --input "<what to do instead>"
/// ```
///
/// They are read back from the oneharness run report rather than re-derived from
/// what onejudge asked for, so the address names the session the run actually
/// bound — including in a fallback chain, where the handle is bound to the
/// candidate that *ran*, not the first one tried.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "sdk-schema", derive(schemars::JsonSchema))]
pub struct ControlAddress {
    /// The caller-owned session handle the turn is addressed by, as oneharness
    /// sanitized and stored it (`interrupt --session`).
    pub session: String,
    /// The session store directory holding both the handle and its
    /// `control/<session>.sock` (`interrupt --session-dir`). Absolute: the
    /// address is handed to a different process, so a relative path would name a
    /// different directory depending on who read it.
    pub session_dir: String,
    /// The working directory the turn runs in, which is how `interrupt` resolves
    /// *which project's* session the handle belongs to (`interrupt --cwd`). It is
    /// the same string onejudge passed as `oneharness run --cwd`, because
    /// oneharness slugs that string to key the store.
    pub cwd: String,
}

/// What onejudge can say about out-of-band turn control for a finished run.
///
/// Three states, not an `Option`: a supervisor that asked for control and got
/// none needs to know *that it asked and was refused*, which is a different
/// situation from never having asked — the first is a harness or platform it must
/// route around, the second is its own configuration.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ControlOutcome {
    /// `provider.control` was off: no socket was asked for and nothing changed
    /// about the run.
    #[default]
    NotRequested,
    /// A controllable turn was opened, addressed here.
    Open(ControlAddress),
    /// Control was asked for and could not be provided — the reason says why (a
    /// harness with no control mechanism, a platform with no unix sockets, a run
    /// shape oneharness refuses to control). The run itself went ahead without it.
    Unavailable(String),
}

impl ControlOutcome {
    /// The address, when a controllable turn was actually opened.
    #[must_use]
    pub fn address(&self) -> Option<&ControlAddress> {
        match self {
            ControlOutcome::Open(address) => Some(address),
            ControlOutcome::NotRequested | ControlOutcome::Unavailable(_) => None,
        }
    }

    /// Why an *asked-for* control channel is missing, when one is.
    #[must_use]
    pub fn unavailable_reason(&self) -> Option<&str> {
        match self {
            ControlOutcome::Unavailable(reason) => Some(reason),
            ControlOutcome::NotRequested | ControlOutcome::Open(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address() -> ControlAddress {
        ControlAddress {
            session: "run-42-skill".into(),
            session_dir: "/state/oneharness/sessions".into(),
            cwd: "/work/repo".into(),
        }
    }

    #[test]
    fn the_three_states_are_distinguishable() {
        assert_eq!(ControlOutcome::default(), ControlOutcome::NotRequested);
        assert!(ControlOutcome::NotRequested.address().is_none());
        assert!(ControlOutcome::NotRequested.unavailable_reason().is_none());

        let open = ControlOutcome::Open(address());
        assert_eq!(open.address(), Some(&address()));
        assert!(open.unavailable_reason().is_none());

        let refused = ControlOutcome::Unavailable("no control mechanism".into());
        assert!(refused.address().is_none());
        assert_eq!(refused.unavailable_reason(), Some("no control mechanism"));
    }

    #[test]
    fn the_address_round_trips_through_serde() {
        let json = serde_json::to_string(&address()).unwrap();
        assert_eq!(
            serde_json::from_str::<ControlAddress>(&json).unwrap(),
            address()
        );
        // Exactly the three fields `oneharness interrupt` addresses a turn with —
        // no socket path, no mechanism, nothing a consumer could mistake for one.
        assert_eq!(
            json,
            r#"{"session":"run-42-skill","session_dir":"/state/oneharness/sessions","cwd":"/work/repo"}"#
        );
    }
}
