//! The **spawn seam**: how an in-process embedder gets the processes onejudge
//! spawns into a process group it owns, so terminating that group still reaps the
//! whole harness tree.
//!
//! # Why this exists
//!
//! Driving onejudge as a *subprocess* gave an embedder OS-level grouping for free:
//! it spawned one `onejudge` process, put that process in a group, and everything
//! onejudge went on to spawn inherited the group. Driving onejudge as a *library*
//! silently removes that — the harness processes are now spawned by the embedder's
//! own process, inside whatever group it happens to be in, and a cancel that
//! terminates "the run" can no longer name a tree to terminate. An agent harness
//! that survives a cancellation keeps calling the model and keeps billing, so this
//! is not a tidiness problem.
//!
//! [`SpawnHook`] is the seam that gives it back, without onejudge taking ownership
//! of any grouping policy: the **embedder owns the group**, because the embedder is
//! the party that must later terminate it and may already have one of its own.
//! onejudge only offers each process it is about to create, and records what the
//! hook says it did with it.
//!
//! # The two halves, and why both
//!
//! Grouping is spelled differently on each platform, and the two spellings need
//! different moments:
//!
//! - **POSIX process groups** are configured *before* the fork:
//!   [`SpawnHook::spawning`] hands over the [`Command`], where
//!   `std::os::unix::process::CommandExt::process_group` puts the child in a new or
//!   existing group.
//! - **Windows job objects** are assigned *after* the process exists:
//!   [`SpawnHook::spawned`] hands over the live [`Child`], whose
//!   `std::os::windows::io::AsRawHandle` handle is what
//!   `AssignProcessToJobObject` takes.
//!
//! Both have a default no-op body, so a hook implements only the half its platform
//! needs and stays portable.
//!
//! # "Before spawned processes begin work"
//!
//! [`SpawnHook::spawned`] runs after the child exists but **before onejudge writes
//! the request to its stdin**, and every process onejudge spawns blocks reading
//! stdin until that write (`oneharness run --prompt-file -`, and the one request
//! object of the [`CommandProvider`](crate::CommandProvider) protocol). So a child
//! observed here has not run the harness, and has not spawned a descendant that
//! could escape the assignment.
//!
//! # Not installing one changes nothing
//!
//! An embedder that installs no hook gets exactly today's behaviour: onejudge
//! spawns into its own group and claims nothing. It is never silently a no-op in
//! the other direction either — a hook that fails to place a process is a loud
//! [`ProviderErrorKind::Spawn`] error with the child torn down, because running a
//! harness the embedder cannot terminate is the very defect the hook exists to
//! prevent.
//!
//! The record onejudge keeps ([`SpawnedProcess`], surfaced on
//! [`Report::processes`](crate::Report::processes) and therefore on the CLI's
//! `--format json`) reports the group **only when a hook named one**. onejudge
//! never invents a group it did not observe.
//!
//! See `docs/spawn-hook.md` for the embedder-facing walkthrough.

use std::cell::RefCell;
use std::io;
use std::process::{Child, Command};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::{Error, ProviderErrorKind, Result};
use crate::telemetry::TelemetryRole;

/// What onejudge is about to spawn, so a hook can group the two sides of a run
/// differently (or refuse one) without inspecting argv.
///
/// Non-exhaustive: onejudge may describe a spawn more precisely in a later
/// version without that being a breaking change for a hook that reads these
/// fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct SpawnContext<'a> {
    /// Whether this process serves the agent under evaluation or the
    /// judge/simulated-user side.
    pub role: TelemetryRole,
    /// The provider operation asking for the process (`respond`, `user`,
    /// `supervisor`, `judge`, `assess`).
    pub op: &'a str,
    /// The program onejudge resolves and runs (`argv[0]`).
    pub program: &'a str,
}

/// An embedder-installed observer of process creation, used to place what
/// onejudge spawns into a group the embedder owns and can later terminate.
///
/// Implement [`spawning`](SpawnHook::spawning) for POSIX process groups,
/// [`spawned`](SpawnHook::spawned) for Windows job objects, or both for a hook
/// that runs on either. Both default to doing nothing, so an unimplemented half is
/// explicit rather than accidental.
///
/// A hook is shared across the whole run (and across a
/// [`SplitProvider`](crate::SplitProvider)'s two backends), so it must be
/// `Send + Sync`; use interior mutability to accumulate state.
///
/// # Example
///
/// ```
/// use std::io;
/// use std::process::{Child, Command};
/// use std::sync::Mutex;
///
/// use onejudge::{SpawnContext, SpawnHook};
///
/// /// Every spawned process leads its own POSIX process group, recorded so the
/// /// embedder can `killpg` the whole tree when the run is cancelled.
/// #[derive(Default)]
/// struct OwnGroupPerProcess {
///     groups: Mutex<Vec<u32>>,
/// }
///
/// impl SpawnHook for OwnGroupPerProcess {
///     #[cfg(unix)]
///     fn spawning(&self, command: &mut Command, _context: &SpawnContext<'_>) -> io::Result<()> {
///         use std::os::unix::process::CommandExt as _;
///         command.process_group(0);
///         Ok(())
///     }
///
///     fn spawned(&self, child: &Child, _context: &SpawnContext<'_>) -> io::Result<Option<String>> {
///         // `process_group(0)` makes the child its own group leader, so its pid
///         // *is* the group id the embedder will terminate.
///         self.groups.lock().expect("not poisoned").push(child.id());
///         Ok(Some(format!("pgid:{}", child.id())))
///     }
/// }
/// ```
pub trait SpawnHook: Send + Sync {
    /// Configure `command` before onejudge spawns it — the moment a POSIX process
    /// group has to be chosen, and where Windows creation flags belong.
    ///
    /// # Errors
    /// Any error refuses the spawn: the process is never created and the
    /// operation fails with [`ProviderErrorKind::Spawn`].
    fn spawning(&self, command: &mut Command, context: &SpawnContext<'_>) -> io::Result<()> {
        let _ = (command, context);
        Ok(())
    }

    /// Observe `child` after it exists but before onejudge writes its request, so
    /// a Windows embedder can assign the process to its job object while the child
    /// is still blocked on stdin and has spawned nothing.
    ///
    /// Return the name of the group the hook placed the process in — any label the
    /// embedder can later resolve back to the group. `Ok(None)` means "deliberately
    /// not grouped", and onejudge reports no group for it.
    ///
    /// # Errors
    /// Any error tears the child down and fails the operation with
    /// [`ProviderErrorKind::Spawn`]: a process the embedder could not group is a
    /// harness it could not cancel.
    fn spawned(&self, child: &Child, context: &SpawnContext<'_>) -> io::Result<Option<String>> {
        let _ = (child, context);
        Ok(None)
    }
}

/// A [`SpawnHook`] shared by every provider in one run — including both backends
/// of a [`SplitProvider`](crate::SplitProvider), so one embedder-owned group can
/// span the whole two-party harness tree.
pub type SharedSpawnHook = Arc<dyn SpawnHook>;

/// One process onejudge spawned for a run, and the group an embedder's
/// [`SpawnHook`] placed it in.
///
/// Carried on [`Report::processes`](crate::Report::processes) (and on the CLI's
/// `--format json` failure document), so what an in-process embedder can observe
/// through the hook is also machine-readable from the command line.
///
/// `group` is present **only** when a hook named one: onejudge never reports a
/// group that does not exist, so a `null` group is the honest statement that this
/// process is in whatever group onejudge itself was in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "sdk-schema", derive(schemars::JsonSchema))]
pub struct SpawnedProcess {
    /// Which side of the conversation the process served.
    pub role: TelemetryRole,
    /// The provider operation that asked for it.
    pub op: String,
    /// The program that was run (`argv[0]`).
    pub program: String,
    /// The operating-system process id it was given.
    pub pid: u32,
    /// The embedder-owned group a [`SpawnHook`] reported placing it in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

/// The provider-side half of the seam: holds the optional hook and the record of
/// what was spawned under it. Each provider that creates processes owns one.
#[derive(Default, Clone)]
pub(crate) struct Spawner {
    hook: Option<SharedSpawnHook>,
    records: RefCell<Vec<SpawnedProcess>>,
}

impl std::fmt::Debug for Spawner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // A `dyn SpawnHook` is not `Debug`; whether one is installed is the part a
        // reader of a provider's `Debug` output actually needs.
        f.debug_struct("Spawner")
            .field("hook", &self.hook.is_some())
            .field("records", &self.records.borrow().len())
            .finish()
    }
}

impl Spawner {
    /// Install `hook`, replacing any previous one.
    pub(crate) fn install(&mut self, hook: SharedSpawnHook) {
        self.hook = Some(hook);
    }

    /// The processes spawned since the last [`Spawner::reset`].
    pub(crate) fn records(&self) -> Vec<SpawnedProcess> {
        self.records.borrow().clone()
    }

    /// Discard records retained from an earlier run.
    pub(crate) fn reset(&self) {
        self.records.borrow_mut().clear();
    }

    /// Spawn `command` under the installed hook and record the result.
    ///
    /// `spawn_error` renders the provider's own message for a command that could
    /// not start at all, so each provider keeps its "is it installed and on PATH?"
    /// advice; a *hook* failure is reported as itself rather than as a missing
    /// binary.
    pub(crate) fn spawn(
        &self,
        command: &mut Command,
        context: &SpawnContext<'_>,
        spawn_error: impl FnOnce(io::Error) -> Error,
    ) -> Result<Child> {
        if let Some(hook) = &self.hook {
            hook.spawning(command, context)
                .map_err(|e| hook_error(context, "prepare", &e))?;
        }
        let mut child = command.spawn().map_err(spawn_error)?;
        let group = match &self.hook {
            // Runs before the caller writes the request, so the child is still
            // blocked on stdin and has spawned nothing of its own.
            Some(hook) => match hook.spawned(&child, context) {
                Ok(group) => group,
                Err(e) => {
                    // A process the embedder could not group is a harness it could
                    // not cancel, so the turn fails here rather than running one
                    // that outlives its cancellation.
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(hook_error(context, "group", &e));
                }
            },
            None => None,
        };
        self.records.borrow_mut().push(SpawnedProcess {
            role: context.role,
            op: context.op.to_string(),
            program: context.program.to_string(),
            pid: child.id(),
            group,
        });
        Ok(child)
    }
}

/// The classified error for a spawn hook that refused a process.
fn hook_error(context: &SpawnContext<'_>, stage: &str, error: &io::Error) -> Error {
    Error::provider_classified(
        context.op.to_string(),
        format!(
            "the installed spawn hook could not {stage} `{}`: {error}",
            context.program
        ),
        ProviderErrorKind::Spawn,
    )
}

/// The role a provider operation belongs to: `respond` is the agent under
/// evaluation, everything else is the judge/simulated-user side.
pub(crate) fn role_of(op: &str) -> TelemetryRole {
    if op == "respond" {
        TelemetryRole::Agent
    } else {
        TelemetryRole::Judge
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A hook that records what it saw and answers however the test asked it to.
    #[derive(Default)]
    struct Recording {
        seen: Mutex<Vec<(TelemetryRole, String, String)>>,
        prepared: Mutex<u32>,
        fail_spawning: bool,
        fail_spawned: bool,
        group: Option<&'static str>,
    }

    impl SpawnHook for Recording {
        fn spawning(&self, _command: &mut Command, _context: &SpawnContext<'_>) -> io::Result<()> {
            *self.prepared.lock().unwrap() += 1;
            if self.fail_spawning {
                return Err(io::Error::other("no group available"));
            }
            Ok(())
        }

        fn spawned(
            &self,
            _child: &Child,
            context: &SpawnContext<'_>,
        ) -> io::Result<Option<String>> {
            self.seen.lock().unwrap().push((
                context.role,
                context.op.to_string(),
                context.program.to_string(),
            ));
            if self.fail_spawned {
                return Err(io::Error::other("job object assignment failed"));
            }
            Ok(self.group.map(String::from))
        }
    }

    /// A command that exits immediately on every platform the crate supports.
    fn trivial() -> Command {
        #[cfg(windows)]
        let mut command = {
            let mut c = Command::new("cmd");
            c.args(["/C", "exit"]);
            c
        };
        #[cfg(not(windows))]
        let mut command = {
            let mut c = Command::new(std::env::current_exe().expect("a test binary"));
            // `--list` makes the test harness print and exit rather than run.
            c.args(["--list"]);
            c
        };
        command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        command
    }

    fn context<'a>(program: &'a str) -> SpawnContext<'a> {
        SpawnContext {
            role: TelemetryRole::Agent,
            op: "respond",
            program,
        }
    }

    fn spawn_error(e: io::Error) -> Error {
        Error::provider_classified("respond", e.to_string(), ProviderErrorKind::Spawn)
    }

    #[test]
    fn without_a_hook_a_spawn_is_recorded_with_no_group() {
        let spawner = Spawner::default();
        let mut child = spawner
            .spawn(&mut trivial(), &context("trivial"), spawn_error)
            .expect("the trivial command spawns");
        let _ = child.wait();
        let records = spawner.records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].role, TelemetryRole::Agent);
        assert_eq!(records[0].op, "respond");
        assert_eq!(records[0].program, "trivial");
        assert!(records[0].pid > 0);
        // The library never claims a group it did not observe.
        assert_eq!(records[0].group, None);
        spawner.reset();
        assert!(spawner.records().is_empty());
    }

    #[test]
    fn an_installed_hook_names_the_group_it_placed_the_process_in() {
        let mut spawner = Spawner::default();
        spawner.install(Arc::new(Recording {
            group: Some("job:run-1"),
            ..Recording::default()
        }));
        let mut child = spawner
            .spawn(&mut trivial(), &context("trivial"), spawn_error)
            .expect("the trivial command spawns");
        let _ = child.wait();
        assert_eq!(spawner.records()[0].group.as_deref(), Some("job:run-1"));
    }

    #[test]
    fn a_hook_that_reports_no_group_is_reported_as_ungrouped() {
        let mut spawner = Spawner::default();
        spawner.install(Arc::new(Recording::default()));
        let mut child = spawner
            .spawn(&mut trivial(), &context("trivial"), spawn_error)
            .expect("the trivial command spawns");
        let _ = child.wait();
        assert_eq!(spawner.records()[0].group, None);
    }

    /// A hook that overrides neither half — the portable shape of an embedder that
    /// only needs the other platform's hook. Both defaults must be no-ops.
    struct Inert;
    impl SpawnHook for Inert {}

    #[test]
    fn a_hook_that_overrides_neither_half_leaves_the_spawn_untouched() {
        let mut spawner = Spawner::default();
        spawner.install(Arc::new(Inert));
        let mut child = spawner
            .spawn(&mut trivial(), &context("trivial"), spawn_error)
            .expect("the default hook bodies do nothing");
        let _ = child.wait();
        assert_eq!(spawner.records()[0].group, None);
    }

    #[test]
    fn a_command_that_cannot_start_reports_the_providers_own_message() {
        // A hook failure and a missing binary are both `Spawn`, but they are not
        // the same finding, so the caller's message survives.
        let mut spawner = Spawner::default();
        spawner.install(Arc::new(Recording::default()));
        let mut missing = Command::new("definitely-not-a-real-binary-xyz");
        let err = spawner
            .spawn(
                &mut missing,
                &context("definitely-not-a-real-binary-xyz"),
                |e| {
                    Error::provider_classified(
                        "respond",
                        format!("could not run the provider: {e}"),
                        ProviderErrorKind::Spawn,
                    )
                },
            )
            .unwrap_err();
        assert_eq!(err.kind(), Some(ProviderErrorKind::Spawn));
        assert!(err.to_string().contains("could not run the provider"));
        assert!(!err.to_string().contains("spawn hook"));
        assert!(spawner.records().is_empty());
    }

    #[test]
    fn a_hook_that_cannot_prepare_the_command_refuses_the_spawn() {
        let mut spawner = Spawner::default();
        let hook = Arc::new(Recording {
            fail_spawning: true,
            ..Recording::default()
        });
        spawner.install(hook.clone());
        let err = spawner
            .spawn(&mut trivial(), &context("trivial"), spawn_error)
            .unwrap_err();
        assert_eq!(err.kind(), Some(ProviderErrorKind::Spawn));
        assert!(err.to_string().contains("could not prepare"));
        // Nothing was spawned, so nothing is recorded.
        assert!(spawner.records().is_empty());
        assert!(hook.seen.lock().unwrap().is_empty());
    }

    #[test]
    fn a_hook_that_cannot_group_the_child_tears_it_down_and_fails() {
        let mut spawner = Spawner::default();
        let hook = Arc::new(Recording {
            fail_spawned: true,
            ..Recording::default()
        });
        spawner.install(hook.clone());
        let err = spawner
            .spawn(&mut trivial(), &context("trivial"), spawn_error)
            .unwrap_err();
        assert_eq!(err.kind(), Some(ProviderErrorKind::Spawn));
        assert!(err.to_string().contains("could not group"));
        // The process existed but is not reported: it was killed, never used.
        assert!(spawner.records().is_empty());
        assert_eq!(*hook.prepared.lock().unwrap(), 1);
    }

    #[test]
    fn roles_split_the_agent_turn_from_every_judge_side_call() {
        assert_eq!(role_of("respond"), TelemetryRole::Agent);
        for op in ["user", "supervisor", "judge", "assess"] {
            assert_eq!(role_of(op), TelemetryRole::Judge);
        }
    }

    #[test]
    fn debug_reports_whether_a_hook_is_installed() {
        let mut spawner = Spawner::default();
        assert!(format!("{spawner:?}").contains("hook: false"));
        spawner.install(Arc::new(Recording::default()));
        assert!(format!("{spawner:?}").contains("hook: true"));
    }
}
