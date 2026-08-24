//! [`OneharnessProvider`]: the default [`Provider`], which runs each prompt on a
//! real harness through the [`oneharness`](https://github.com/nickderobertis/oneharness)
//! **engine, in process** — `oneharness_core::io::run::run`, the linked
//! `oneharness-core` this crate already compiles its report contract against.
//! Nothing is spawned and nothing needs to be on `PATH`; see [`library`] for the
//! call and [`Execution`] for the one seam that still spawns.
//!
//! **Harness/model selection lives in oneharness's config, not onejudge.** The
//! agent side passes no `--harness`/`--model`, so it uses oneharness's discovered
//! default config (`oneharness.toml`). The judge / simulated-user side passes
//! `--config <judge_config>` (default `oneharness.judge.toml`) so it can run on a
//! separately-configured harness/model — again without `--harness`/`--model`.
//! Scaffold both with `onejudge init` (which shells out to `oneharness init`).
//!
//! It targets **oneharness v0.11.0+** — the release embedding the `oneharness-core`
//! this crate compiles against (pinned in the workspace manifest). v0.6.9 was the
//! first whose `run` verb answers a cancellation signal by tearing its harness
//! tree down instead of dying and orphaning it, v0.6.14 added the
//! `run --control` / `interrupt` pair it reports the address of, and v0.11.0 is
//! the first that either resumes a named session on a turn-driving control
//! mechanism or refuses the continuation outright. The CLI and the engine crate
//! version independently, so the advertised floor is not the pin — see
//! `MIN_ONEHARNESS_CORE`. It always threads the uniform `--session
//! <name>` handle (the engine's caller-owned name, mapped to the harness's native
//! session in oneharness's on-disk store), and if a run fails because the harness
//! does not support `--session`, it retries the same call once **without**
//! `--session`, re-inlining the transcript — the graceful degradation that
//! replaces the old up-front capability table. It also depends on `oneharness init`
//! for scaffolding.
//!
//! It can also drive a **streamed** provider (`with_streaming`): the turn asks
//! `run` to publish incrementally and hands it an `EventSink`, so each tool event
//! reaches the caller the instant oneharness observes it instead of only when the
//! turn ends. The finished report is read exactly as a buffered one is, so a
//! streamed run and a buffered one produce the same turn. On the spawning seam the
//! same contract arrives as the NDJSON protocol in `stream.rs`
//! (`docs/streaming.md`).
//!
//! The **report** it reads back is oneharness's own typed contract
//! (`oneharness_core::domain::report`), not a shadow struct declared here — see
//! [`report`] for what that buys, including reading the candidate a fallback chain
//! actually ran instead of the first one it routed around. The per-candidate
//! history record oneharness writes for every attempt is read back through
//! oneharness's own reader; see [`history`].
//!
//! The pure pieces — turn description, report parsing, error classification — are
//! separated from the thin execution shells so they are deterministically
//! unit-tested. Both seams are proven end-to-end in the e2e suite: the in-process
//! one against `onejudge-fake-harness` (a *harness* stand-in, so the whole of
//! oneharness is real), the spawning one against `onejudge-fake-oneharness`. The
//! live tier drives a real oneharness and a real harness (`docs/live-tier.md`).
//!
//! **What the one remaining subprocess is for.** [`Execution::Process`] spawns an
//! `oneharness` executable, and exists for a single upstream gap: `RunControls`
//! cannot offer a spawned harness to the caller, and each harness is its own
//! process-group leader inside `run`, so an in-process turn leaves
//! [`SpawnHook`](crate::SpawnHook) nothing to place and `Report::processes` empty —
//! removing the very seam an embedder uses to reap an orphaned paid harness. It is
//! reached only by naming a binary or installing such a hook. The gap is written
//! up, with a proposal against oneharness, in `docs/oneharness-library.md`.
//!
//! It can also ask for a **controllable** agent turn (`with_control`): the
//! agent-side call adds `--control`, oneharness opens an out-of-band socket for the
//! turn, and the address an `oneharness interrupt` process would use is reported on
//! [`Provider::control`]. onejudge never interrupts anything — it asks, and it
//! reports where the answer is. A oneharness that refuses the ask does so before
//! spawning a harness, so the call is retried once without the flag and the refusal
//! becomes a stated reason rather than a failed run. See `docs/control.md`.
//!
//! **Cancelling a turn always terminates the harness tree, never just the party
//! onejudge is talking to.** A harness is its own process-group leader, so onejudge
//! can never signal one; only oneharness owns the tree. In process that is
//! `RunControls::cancel` plus the sink's `SinkStep::Stop` (see [`library`]). On the
//! spawning seam it is the three-rung escalation in [`terminate`] — closed stdout,
//! SIGTERM, kill — because a spawned producer has to be reached through the OS.

#[cfg(test)]
pub(crate) mod fixture;
mod history;
mod library;
mod report;
mod turn;

use std::cell::RefCell;
use std::io::{Read as _, Write};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use crate::control::{ControlAddress, ControlOutcome};
use crate::error::{Error, ProviderErrorKind, Result};
use crate::provider::{
    build_assessment_prompt, build_judge_prompt, build_supervisor_prompt, build_user_prompt,
    latest_or_inline, parse_supervisor, parse_verdict, supervise_with_reask, Assessment,
    AssistantTurn, JudgeQuery, JudgeVerdict, Provider, SkillRef, SupervisorQuery, SupervisorTurn,
    UserTurn, SUPERVISOR_REASK_NOTE,
};
use crate::spawn::{role_of, SharedSpawnHook, SpawnContext, SpawnedProcess, Spawner};
use crate::stream::{read_stream, StreamOutcome};
use crate::telemetry::{CandidateAttempt, FellThrough, InvocationTelemetry, TelemetryRole};
use crate::transcript::{Message, ToolEvent};

pub(crate) use report::tool_event;
use report::{parse_report, parse_report_value, ControlSocket, Invocation};
use turn::TurnSpec;

/// The default judge/simulated-user oneharness config filename.
const DEFAULT_JUDGE_CONFIG: &str = "oneharness.judge.toml";

/// The stable substring in oneharness's error when a harness cannot bind a
/// `--session` name (its `OneharnessError::SessionUnsupported`). Matching it lets
/// onejudge retry the call without `--session` instead of failing the run.
///
/// It is a substring because oneharness reports this one *before* it can emit a
/// report — a usage error on stderr, not a `failure_kind` — so there is nothing
/// typed on the wire to match. `session_unsupported_marker_tracks_oneharness` in
/// this module's tests pins it against `OneharnessError::SessionUnsupported`'s own
/// rendering, so an upstream rewording fails the gate here instead of silently
/// turning the graceful retry into a failed run.
const SESSION_UNSUPPORTED_MARKER: &str = "does not support --session";

/// Whether `err` is oneharness rejecting `--session` because the harness exposes no
/// session id headlessly — the one failure the provider recovers from (by retrying
/// without `--session`).
fn is_session_unsupported(err: &Error) -> bool {
    err.to_string().contains(SESSION_UNSUPPORTED_MARKER)
}

/// The stable substrings in every refusal `oneharness run --control` can answer
/// with: a harness with no control mechanism, a run shape that has no single turn
/// to address, an incompatible output format or stream, a platform with no unix
/// sockets, or a socket that could not be opened.
///
/// Substrings for the same reason [`SESSION_UNSUPPORTED_MARKER`] is one: every one
/// of these is a *usage* error oneharness reports before it can produce a report,
/// so there is nothing typed on the wire to match. Every variant either quotes the
/// flag or names the socket, and
/// `control_refusal_markers_track_oneharness` in this module's tests pins the
/// whole set against `OneharnessError`'s own rendering, so an upstream rewording
/// fails the gate here instead of silently turning a degraded run into a failed one.
const CONTROL_REFUSED_MARKERS: [&str; 3] = ["--control", "control socket", "controlled turn"];

/// Whether `err` is oneharness refusing the *control ask* — as opposed to failing
/// the turn. Only consulted on a call that actually passed `--control`.
fn is_control_refused(err: &Error) -> bool {
    let text = err.to_string();
    CONTROL_REFUSED_MARKERS
        .iter()
        .any(|marker| text.contains(marker))
}

/// Why this platform cannot carry a control channel, or `None` when it can.
///
/// A parameter rather than a `cfg!` at the call site so the Windows answer is
/// asserted on every host: the degradation this returns is the whole of what a
/// Windows caller gets, and a CI matrix that only *runs* it there would leave it
/// unproven wherever the gate actually runs.
fn control_platform_reason(unix: bool) -> Option<&'static str> {
    (!unix).then_some(
        "oneharness's turn-control socket is a unix domain socket, which this platform does not \
         provide",
    )
}

/// oneharness's own words for a refusal, as the reason a report carries.
///
/// Quoted rather than re-described: the refusal names the harness, the run shape,
/// or the control-capable alternatives, and a supervisor deciding how to route
/// around it needs that, not onejudge's paraphrase.
fn refusal(err: &Error) -> String {
    match err {
        Error::Provider { message, .. } => message.clone(),
        other => other.to_string(),
    }
}

/// How long a cancelled `oneharness run` is given to notice its closed stdout
/// before onejudge escalates to a signal.
///
/// A producer that is still writing observes the broken pipe on its very next
/// write, so this only has to cover the gap to that write. It is deliberately short:
/// a *silent* producer will never use it, and waits it out before being signalled.
const PIPE_CLOSE_GRACE: Duration = Duration::from_millis(250);

/// How long a signalled `oneharness run` is then given to tear down the harness
/// tree it owns before onejudge kills it outright.
///
/// oneharness's runner polls for cancellation on a short slice of its own, so this
/// only has to cover that slice plus the reaping of the tree — while staying short
/// enough that cancelling a turn is still responsive.
const TEARDOWN_GRACE: Duration = Duration::from_secs(2);

/// How often the grace period above re-checks the child.
const TEARDOWN_POLL: Duration = Duration::from_millis(10);

/// Tear down a `oneharness run` whose turn was abandoned, and through it the
/// harness tree it owns.
///
/// onejudge can never signal the *harness*: oneharness makes every harness its own
/// process-group leader precisely so a signal aimed at the runner does not race it.
/// Everything here is therefore addressed to **oneharness**, which owns the tree
/// and is the only party that can reap it.
///
/// Teardown escalates, cheapest and most cooperative first, because each rung
/// reaches a case the one before it cannot:
///
/// 1. **The closed stdout** (already dropped by the caller). A producer that is
///    still writing sees the broken pipe on its next write and short-circuits into
///    its own teardown. This rung alone is what onejudge used to rely on.
/// 2. **SIGTERM.** A producer whose *harness* has gone silent never writes again,
///    so it never observes rung 1 — it would sit there until the backstop killed
///    it, and an uncatchable kill denies it the teardown, orphaning a live harness
///    that keeps burning tokens. Since v0.6.9 `oneharness run` installs
///    SIGINT/SIGTERM handlers, and its runner polls for the resulting cancellation
///    on its own time slice rather than only when the harness writes, so this
///    reaches the silent case and still ends in `Finish::Terminate`.
/// 3. **SIGKILL**, for a child that answers neither.
///
/// Rung 1 is kept, and kept first, precisely because rung 2 is not free: SIGTERM's
/// *default* disposition is to terminate, so signalling a producer that would have
/// torn down on the broken pipe — an older oneharness, or the window before it
/// installs its handlers — would cut it off mid-teardown. Waiting out
/// [`PIPE_CLOSE_GRACE`] first costs a silent harness a quarter second against a
/// turn measured in hundreds of seconds.
///
/// On Windows there is no rung 2 and none is needed: oneharness puts each harness
/// tree in a Job Object with `KILL_ON_JOB_CLOSE`, so rung 3 already ends the
/// descendants.
fn terminate(child: &mut std::process::Child) {
    if exited_within(child, PIPE_CLOSE_GRACE) {
        return;
    }
    request_stop(child);
    if !exited_within(child, TEARDOWN_GRACE) {
        let _ = child.kill();
    }
}

/// Ask `child` to cancel, the way an operator's Ctrl-C would.
///
/// Best-effort by nature: a child that has already exited is an `ESRCH` this
/// deliberately ignores, because the next thing the caller does is wait for it.
#[cfg(unix)]
fn request_stop(child: &std::process::Child) {
    // The child has been spawned and not yet reaped, so its id still names it (a
    // zombie at worst) and can never have been recycled onto an unrelated process.
    let Some(pid) = i32::try_from(child.id())
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    else {
        return;
    };
    let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
}

/// Windows has no SIGTERM, and needs none: the Job Object teardown described on
/// [`terminate`] makes the backstop kill sufficient there.
#[cfg(not(unix))]
fn request_stop(_child: &std::process::Child) {}

/// Wait up to `grace` for `child` to exit on its own, returning whether it did.
///
/// A `try_wait` error is reported as "did not exit", so an unwaitable child still
/// reaches the backstop kill.
fn exited_within(child: &mut std::process::Child, grace: Duration) -> bool {
    let deadline = Instant::now() + grace;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Err(_) => return false,
            Ok(None) if Instant::now() >= deadline => return false,
            Ok(None) => std::thread::sleep(TEARDOWN_POLL),
        }
    }
}

/// How a turn is executed — the one seam, and the only place a harness turn can
/// become a subprocess.
///
/// [`Execution::Library`] is the default and what every ordinary run uses.
/// [`Execution::Process`] exists for exactly one thing the library cannot do:
/// offer a spawned process to an embedder's [`SpawnHook`](crate::SpawnHook).
/// `RunControls`' whole surface is `events`, `cancel`, `signal_cancel` and
/// `version`, and every harness is spawned *inside* `run` through a `pub(crate)`
/// `Process::spawn` — so an in-process turn has no process to offer, leaving
/// [`Report::processes`](crate::Report::processes) empty and an embedder's group
/// with nothing in it. That is a documented contract on a versioned wire, so the
/// seam stays until oneharness can offer the process (the proposal is written up
/// in `docs/oneharness-library.md`) rather than being silently dropped.
///
/// It is never reached by accident: it is selected only by naming an `oneharness`
/// binary ([`OneharnessProvider::with_bin`]) or by installing a hook that needs
/// one ([`OneharnessProvider::with_spawn_hook`]).
enum Execution {
    /// In process, through `oneharness_core::io::run::run`. See [`library`].
    Library,
    /// Spawn this `oneharness` executable and parse its stdout.
    Process(String),
}

/// The default [`Provider`]: runs each turn through the `oneharness` engine.
pub struct OneharnessProvider {
    execution: Execution,
    judge_config: Option<PathBuf>,
    /// Harness ids run against oneharness's deterministic responder instead of a
    /// paid model. Empty for an ordinary run.
    mock_harness: Vec<String>,
    stream: bool,
    control: bool,
    /// What the agent side could say about turn control on the last run: the
    /// address of the socket it opened, or why it has none. Reset with telemetry,
    /// because it describes one run and not the provider.
    control_outcome: RefCell<ControlOutcome>,
    /// The socket the agent side's last report named, before the working
    /// directory is folded in to make it an address. Written where the report is
    /// parsed (which is the only place that sees it) and consumed by
    /// [`OneharnessProvider::run_respond`], which is the only place that knows the
    /// `--cwd` the turn ran under.
    control_socket: RefCell<Option<ControlSocket>>,
    telemetry: RefCell<Vec<InvocationTelemetry>>,
    spawner: Spawner,
}

impl Default for OneharnessProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl OneharnessProvider {
    /// A provider that runs each turn through the **`oneharness` engine in
    /// process**, running the judge and simulated user under
    /// `oneharness.judge.toml` (its default config file).
    ///
    /// Nothing is spawned and nothing needs to be on `PATH`: the engine is the
    /// linked `oneharness-core`, so the version onejudge compiles its report
    /// contract against is the version that runs the turn.
    ///
    /// [`with_bin`](Self::with_bin) and [`with_spawn_hook`](Self::with_spawn_hook)
    /// are the only two ways to opt back into spawning one.
    #[must_use]
    pub fn new() -> Self {
        Self {
            execution: Execution::Library,
            judge_config: Some(PathBuf::from(DEFAULT_JUDGE_CONFIG)),
            mock_harness: Vec::new(),
            stream: false,
            control: false,
            control_outcome: RefCell::new(ControlOutcome::NotRequested),
            control_socket: RefCell::new(None),
            telemetry: RefCell::new(Vec::new()),
            spawner: Spawner::default(),
        }
    }

    /// Offer every `oneharness` process this provider spawns to `hook` before it
    /// starts work, so an in-process embedder can place it — and, through it, the
    /// harness tree oneharness goes on to own — in a group the embedder can later
    /// terminate. See [`SpawnHook`](crate::SpawnHook).
    ///
    /// **This selects the spawning seam** (`oneharness` from `PATH` unless
    /// [`with_bin`](Self::with_bin) named one): a hook offers a *process*, and an
    /// in-process turn has none to offer. Installing one and
    /// silently having it never fire would be the failure the hook exists to
    /// prevent — an embedder believing it owns a group that is empty.
    ///
    /// Install the *same* hook on both backends of a
    /// [`SplitProvider`](crate::SplitProvider) to cover the whole two-party tree.
    #[must_use]
    pub fn with_spawn_hook(mut self, hook: SharedSpawnHook) -> Self {
        if matches!(self.execution, Execution::Library) {
            self.execution = Execution::Process("oneharness".into());
        }
        self.spawner.install(hook);
        self
    }

    /// Run each turn by **spawning** this `oneharness` executable instead of the
    /// linked engine (e.g. a pinned install, or the fake binary the e2e suite
    /// drives).
    ///
    /// Naming a binary is the explicit opt-in to spawning, which exists for one
    /// contract: `RunControls` cannot offer a spawned harness to an embedder, so
    /// an in-process turn leaves [`SpawnHook`](crate::SpawnHook) nothing to place
    /// and [`Report::processes`](crate::Report::processes) empty. An ordinary
    /// caller wants [`new`](Self::new), whose engine is the `oneharness-core` this
    /// crate compiles its report contract against, so the two can never be
    /// different versions. See `docs/oneharness-library.md`.
    #[must_use]
    pub fn with_bin(mut self, bin: impl Into<String>) -> Self {
        self.execution = Execution::Process(bin.into());
        self
    }

    /// Run this harness id against **oneharness's own deterministic responder**
    /// instead of a paid model, by passing `oneharness run --mock-harness <id>`.
    /// Repeatable; every id must be one the config for that side selects.
    ///
    /// This is what makes an acceptance proof that needs a real multi-turn,
    /// multi-identity chain free: oneharness swaps the selected harness's provider
    /// process for its own `MOCK_*`-scripted responder, so the whole chain — config
    /// discovery, fallback routing, session threading, events, the report — is the
    /// real code, and only the model is scripted. The `MOCK_*` variables that script
    /// it are read from the environment the run inherits, so a caller exports them
    /// exactly as it would for a bare `oneharness run`; onejudge passes them through
    /// by not touching the environment.
    ///
    /// **This selects the spawning seam**, for the same reason
    /// [`with_spawn_hook`](Self::with_spawn_hook) does: oneharness delivers the
    /// responder by re-executing *its own binary* as the harness, and in process
    /// that binary is the embedder, which knows nothing about the contract. Naming
    /// a mock harness therefore runs `oneharness` from `PATH` unless
    /// [`with_bin`](Self::with_bin) named one.
    ///
    /// It applies to **both sides** of the conversation — the agent turn and the
    /// judge / simulated-user turn — since either one otherwise bills a model. The
    /// two sides run under different oneharness configs, so an id one config selects
    /// and the other does not is oneharness's own loud `--mock-harness` error rather
    /// than a silent paid turn; compose a [`SplitProvider`](crate::SplitProvider) of
    /// two providers when each side needs a different id.
    #[must_use]
    pub fn with_mock_harness(mut self, id: impl Into<String>) -> Self {
        if matches!(self.execution, Execution::Library) {
            self.execution = Execution::Process("oneharness".into());
        }
        self.mock_harness.push(id.into());
        self
    }

    /// Fold this provider's mock-harness selection into a freshly-described turn.
    /// One place, so neither side of the conversation can be left billing a model
    /// while the other is mocked.
    fn mocked(&self, mut spec: TurnSpec) -> TurnSpec {
        spec.mock_harness.clone_from(&self.mock_harness);
        spec
    }

    /// Override the oneharness config file the judge and simulated user run under
    /// (default `oneharness.judge.toml`), passed as `oneharness run --config
    /// <path>`. This is where the judge-side harness/model selection lives — onejudge
    /// itself passes no `--harness`/`--model`.
    #[must_use]
    pub fn with_judge_config(mut self, config: impl Into<PathBuf>) -> Self {
        self.judge_config = Some(config.into());
        self
    }

    /// Declare that the agent side **streams**: the turn asks oneharness to
    /// publish incrementally, so a tool event reaches the caller's sink the
    /// instant it is observed and the finished report is read exactly as a
    /// buffered one is. On the spawning seam the same contract arrives as the
    /// NDJSON provider protocol (`docs/streaming.md`) — an `{"type":"event",…}`
    /// line per tool event, then a terminal `{"type":"result","report":{…}}`.
    ///
    /// Off by default. Turn it on only for a binary that really streams: a
    /// declared-streaming provider that writes a line the protocol does not model
    /// is a loud [`ProviderErrorKind::Protocol`] error, not a silent empty turn.
    /// (A provider that streams *sometimes* is still safe — a bare report document
    /// remains accepted, so a degraded run is not a failed one.)
    #[must_use]
    pub fn with_streaming(mut self, stream: bool) -> Self {
        self.stream = stream;
        self
    }

    /// Ask for a **controllable** agent turn: the agent-side call adds
    /// `oneharness run --control`, which opens an out-of-band unix socket for the
    /// turn's lifetime so a separate `oneharness interrupt` process can redirect
    /// it in flight. The address is reported on [`Provider::control`] and on
    /// [`Report::control`](crate::Report::control); onejudge itself never
    /// interrupts anything.
    ///
    /// **Off by default**, and off changes nothing at all — no flag, no socket,
    /// the same argv. On, the ask is best-effort in exactly one direction: a
    /// oneharness that refuses `--control` (a harness with no control mechanism, a
    /// run shape control cannot address, a platform with no unix sockets) does so
    /// *before* anything spawns, so the call is retried once without it and the
    /// refusal is reported as [`ControlOutcome::Unavailable`] rather than failing
    /// a run the caller asked to have judged.
    ///
    /// Only the agent side is controlled. A judge or simulated-user turn is short
    /// and has nothing to redirect, and giving it a socket would put two runs on
    /// one address.
    #[must_use]
    pub fn with_control(mut self, control: bool) -> Self {
        self.control = control;
        self
    }

    /// Record why an asked-for control channel is missing, and warn — a caller
    /// that asked for a lever must not have to diff two reports to learn it has
    /// none.
    fn control_unavailable(&self, reason: impl Into<String>) {
        let reason = reason.into();
        eprintln!("onejudge: warning — no out-of-band turn control for this run: {reason}");
        *self.control_outcome.borrow_mut() = ControlOutcome::Unavailable(reason);
    }

    /// Fold the working directory the turn ran under into the socket the report
    /// named, producing the address `oneharness interrupt` takes.
    ///
    /// A controlled attempt that returned a report with no control block is the
    /// one case that cannot happen through oneharness (it refuses the flag rather
    /// than honoring it silently) and must still not be reported as a lever: it
    /// becomes an `Unavailable` naming the producer.
    fn control_address(&self, worktree: &str) {
        match self.control_socket.borrow_mut().take() {
            Some(socket) => {
                *self.control_outcome.borrow_mut() = ControlOutcome::Open(ControlAddress {
                    session: socket.session,
                    session_dir: socket.session_dir,
                    cwd: worktree.to_string(),
                });
            }
            None => self.control_unavailable(
                "the run accepted `--control` but its report named no control socket",
            ),
        }
    }

    /// Run a skill turn, threading `session` and — on a `SessionUnsupported`
    /// failure — retrying once without it, re-inlining the transcript. Tool events
    /// reach `on_event` live when the provider streams, and are replayed from the
    /// finished turn when it does not.
    ///
    /// A control ask ([`OneharnessProvider::with_control`]) rides the same ladder:
    /// the most capable call is tried first, and each retry drops exactly the one
    /// thing the previous attempt was refused for. Both refusals cost no model
    /// tokens — oneharness validates the flags before it spawns a harness — which
    /// is what makes retrying cheaper than making the caller pre-declare what its
    /// harness can do.
    fn run_respond(
        &self,
        instructions: &str,
        worktree: &str,
        messages: &[Message],
        session: Option<&str>,
        on_event: &mut dyn FnMut(&ToolEvent) -> ControlFlow<()>,
    ) -> Result<AssistantTurn> {
        let mut session = session;
        if let Some(name) = session {
            // A continued session only needs the latest user turn.
            let prompt = latest_or_inline(messages, true);
            if self.wants_control() {
                let spec = self.mocked(respond_spec(
                    instructions,
                    worktree,
                    Some(name),
                    Some(name),
                    self.stream,
                    true,
                    &prompt,
                ));
                match self.respond_once(&spec, on_event) {
                    Ok(turn) => {
                        self.control_address(worktree);
                        return Ok(turn);
                    }
                    // oneharness refused the control ask itself, before spawning
                    // anything: drop the flag and run the turn as an ordinary one.
                    Err(e) if is_control_refused(&e) => self.control_unavailable(refusal(&e)),
                    Err(e) if is_session_unsupported(&e) => {
                        // `--control` is addressed by the session name, so a
                        // harness with no session has no address either.
                        self.control_unavailable(
                            "the agent harness does not support --session, which --control needs \
                             for an address",
                        );
                        session = None;
                    }
                    Err(e) => return Err(e),
                }
            }
            if let Some(name) = session {
                let spec = self.mocked(respond_spec(
                    instructions,
                    worktree,
                    Some(name),
                    Some(name),
                    self.stream,
                    false,
                    &prompt,
                ));
                match self.respond_once(&spec, on_event) {
                    Ok(turn) => return Ok(turn),
                    Err(e) if is_session_unsupported(&e) => {
                        eprintln!(
                            "onejudge: warning — the agent harness does not support --session; \
                             retrying without it (re-inlining the transcript)"
                        );
                    }
                    Err(e) => return Err(e),
                }
            }
        }
        // Fresh or fallback call: inline the whole conversation, no `--session`.
        let prompt = latest_or_inline(messages, false);
        let spec = self.mocked(respond_spec(
            instructions,
            worktree,
            None,
            session,
            self.stream,
            false,
            &prompt,
        ));
        self.respond_once(&spec, on_event)
    }

    /// Whether this call should ask for a controllable turn.
    ///
    /// Windows is answered here rather than by oneharness, so the ask degrades to
    /// a stated reason instead of a refused run: oneharness's control socket is a
    /// unix domain socket, and there is nothing for a retry to discover.
    fn wants_control(&self) -> bool {
        if !self.control {
            return false;
        }
        match control_platform_reason(cfg!(unix)) {
            Some(reason) => {
                self.control_unavailable(reason);
                false
            }
            None => true,
        }
    }

    /// One agent-side invocation: streamed when the provider declared streaming,
    /// otherwise the buffered call with the finished turn's events replayed.
    fn respond_once(
        &self,
        spec: &TurnSpec,
        on_event: &mut dyn FnMut(&ToolEvent) -> ControlFlow<()>,
    ) -> Result<AssistantTurn> {
        if self.stream {
            return self.run_streamed("respond", spec, on_event);
        }
        let turn = assistant_turn(&self.run("respond", spec)?);
        for event in &turn.events {
            if on_event(event).is_break() {
                break;
            }
        }
        Ok(turn)
    }

    /// Read the report a finished `oneharness run` wrote, classify it, and record
    /// the invocation's telemetry.
    ///
    /// oneharness writes its JSON report on stdout **even when it exits non-zero**
    /// (a harness failure is reported, not signalled), so a failed run is parsed
    /// exactly like a successful one: that is what turns an exhausted fallback
    /// chain or a timed-out harness into a classified [`ProviderErrorKind`] with
    /// per-candidate attribution instead of a stderr blob. Only output that is not
    /// a report at all — a usage error, a rejected `--session` — falls back to the
    /// process's own exit status and stderr.
    fn finish(
        &self,
        op: &str,
        status: &ExitStatus,
        stdout: &str,
        stderr: &str,
    ) -> Result<Invocation> {
        match parse_report(op, stdout) {
            Ok(invocation) => {
                // Record BEFORE surfacing the failure: a failed invocation is
                // exactly the one whose per-candidate attribution a caller reads.
                self.record(op, &invocation);
                invocation.into_ok()
            }
            // No readable report at all. A non-zero exit here is oneharness
            // refusing the call before it could produce one (a usage error, a
            // rejected `--session`), so its own stderr is the finding.
            Err(unreadable) => {
                if status.success() {
                    Err(unreadable)
                } else {
                    Err(exit_error(op, status, stderr))
                }
            }
        }
    }

    /// Run a judge/simulated-user turn under the judge config, threading `session`
    /// and — on a `SessionUnsupported` failure — retrying once without it. The
    /// prompt already inlines the whole transcript, so the retry needs no rebuild.
    fn run_judge_side(
        &self,
        op: &str,
        prompt: &str,
        session: Option<&str>,
        cwd: Option<&str>,
    ) -> Result<Invocation> {
        if let Some(name) = session {
            let spec = self.mocked(judge_side_spec(
                self.judge_config.as_deref(),
                Some(name),
                cwd,
                prompt,
            ));
            match self.run(op, &spec) {
                Ok(result) => return Ok(result),
                Err(e) if is_session_unsupported(&e) => {
                    eprintln!(
                        "onejudge: warning — the judge harness does not support --session; \
                         retrying without it"
                    );
                }
                Err(e) => return Err(e),
            }
        }
        let spec = self.mocked(judge_side_spec(
            self.judge_config.as_deref(),
            None,
            cwd,
            prompt,
        ));
        self.run(op, &spec)
    }

    /// Run one buffered turn and return its parsed single-result report.
    ///
    /// In process this is one `oneharness_core::io::run::run` call. On the
    /// spawning seam the prompt goes over stdin (`--prompt-file -`), so an
    /// arbitrarily long transcript never trips the OS argv limit.
    fn run(&self, op: &str, spec: &TurnSpec) -> Result<Invocation> {
        let Execution::Process(bin) = &self.execution else {
            let invocation = library::run_buffered(op, spec)?;
            // Recorded before the failure is surfaced, exactly as the spawning
            // seam does: a failed invocation is the one whose per-candidate
            // attribution a caller reads.
            self.record(op, &invocation);
            return invocation.into_ok();
        };
        let mut child = self.spawn(op, bin, &turn::argv(spec))?;
        write_prompt(op, child.stdin.as_mut(), &spec.prompt)?;
        let output = child.wait_with_output().map_err(|e| {
            Error::provider(op.to_string(), format!("oneharness did not complete: {e}"))
        })?;
        self.finish(
            op,
            &output.status,
            &String::from_utf8_lossy(&output.stdout),
            &String::from_utf8_lossy(&output.stderr),
        )
    }

    /// Run one streamed turn, delivering each tool event to `on_event` the
    /// instant it is observed.
    ///
    /// In process that is an `EventSink` handed to `run`. On the spawning seam it
    /// is `oneharness run --stream`, whose NDJSON stdout is consumed line by line.
    ///
    /// The child's stderr is drained on its own thread: this reader holds stdout
    /// open for the whole turn, and a chatty child that filled the stderr pipe
    /// meanwhile would deadlock both.
    fn run_streamed(
        &self,
        op: &str,
        spec: &TurnSpec,
        on_event: &mut dyn FnMut(&ToolEvent) -> ControlFlow<()>,
    ) -> Result<AssistantTurn> {
        let Execution::Process(bin) = &self.execution else {
            return match library::run_streaming(op, spec, on_event)? {
                // The sink asked to stop mid-turn: return what the stream produced.
                library::Streamed::Aborted(events) => Ok(AssistantTurn {
                    events,
                    ..AssistantTurn::default()
                }),
                library::Streamed::Finished(invocation) => {
                    self.record(op, &invocation);
                    Ok(assistant_turn(&(*invocation).into_ok()?))
                }
            };
        };
        let mut child = self.spawn(op, bin, &turn::argv(spec))?;
        // Take stdin rather than borrow it: closing it here is what lets a child
        // reading `--prompt-file -` see EOF and start work.
        write_prompt(op, child.stdin.take().as_mut(), &spec.prompt)?;
        let mut errors = child
            .stderr
            .take()
            .ok_or_else(|| Error::provider(op.to_string(), "could not open oneharness stderr"))?;
        let draining = std::thread::spawn(move || {
            let mut buffer = Vec::new();
            let _ = errors.read_to_end(&mut buffer);
            buffer
        });
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::provider(op.to_string(), "could not open oneharness stdout"))?;

        let mut events = Vec::new();
        let mut reading = std::io::BufReader::new(stdout);
        let outcome = read_stream(op, &mut reading, &mut events, on_event);
        // Close the read end before waiting on the child, on EVERY path: nothing
        // reads this pipe from here on, so a child still writing to it would
        // otherwise block once it filled.
        drop(reading);
        if !matches!(outcome, Ok(StreamOutcome::Report(_))) {
            // Aborted or malformed: the turn is over either way, so stop the child —
            // and, through it, the harness tree it owns — rather than wait out its
            // full run. See [`terminate`] for why that is a signal and not the
            // closed pipe above.
            terminate(&mut child);
        }
        let status = child.wait().map_err(|e| {
            Error::provider(op.to_string(), format!("oneharness did not complete: {e}"))
        })?;
        let stderr = draining.join().unwrap_or_default();
        let stderr = String::from_utf8_lossy(&stderr);

        match outcome {
            // The sink asked to stop mid-turn: return what the stream produced.
            Ok(StreamOutcome::Aborted) => Ok(AssistantTurn {
                events,
                ..AssistantTurn::default()
            }),
            Ok(StreamOutcome::Report(report)) => {
                // The terminal report is read exactly as a buffered one is, so a
                // streamed run and a buffered run classify a failure identically —
                // including a chain that fell through to a different candidate.
                match parse_report_value(op, report) {
                    Ok(invocation) => {
                        self.record(op, &invocation);
                        let invocation = invocation.into_ok()?;
                        // A clean report from a process that then died on teardown
                        // is still a failed run; its stderr is the only account.
                        if !status.success() {
                            return Err(exit_error(op, &status, &stderr));
                        }
                        Ok(assistant_turn(&invocation))
                    }
                    Err(unreadable) if status.success() => Err(unreadable),
                    Err(_) => Err(exit_error(op, &status, &stderr)),
                }
            }
            // The stream violation is the finding; the child's exit status is not,
            // because the kill above may be what produced it. Its stderr still is —
            // it carries oneharness's own diagnosis (a rejected `--session`, an
            // unusable config), so it rides along on the one error.
            Err(e) => Err(with_stderr(e, &stderr)),
        }
    }

    /// Spawn the configured `oneharness` binary with `args`, all three pipes open.
    ///
    /// Routed through the [`Spawner`] so an embedder's [`SpawnHook`](crate::SpawnHook)
    /// can place the process in a group it owns *before* the prompt is written —
    /// which is what starts the turn, since `--prompt-file -` blocks on stdin.
    fn spawn(&self, op: &str, bin: &str, args: &[String]) -> Result<std::process::Child> {
        let mut command = Command::new(bin);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        self.spawner.spawn(
            &mut command,
            &SpawnContext {
                role: role_of(op),
                op,
                program: bin,
            },
            |e| {
                Error::provider_classified(
                    op.to_string(),
                    format!("could not run `{bin}`: {e}. Is oneharness installed and on PATH?"),
                    ProviderErrorKind::Spawn,
                )
            },
        )
    }

    /// Record one invocation's telemetry and per-candidate attribution against the
    /// party that made the call — and, on the agent side, the control socket the
    /// report named.
    fn record(&self, op: &str, invocation: &Invocation) {
        let role = if op == "respond" {
            TelemetryRole::Agent
        } else {
            TelemetryRole::Judge
        };
        if op == "respond" && self.control {
            *self.control_socket.borrow_mut() = report::control_socket(&invocation.report);
        }
        self.telemetry
            .borrow_mut()
            .push(invocation_telemetry(role, invocation));
    }
}

/// Fold one finished invocation into the record the engine aggregates: the timing
/// and session linkage for the candidate that ran, plus the identity of **every**
/// candidate oneharness attempted.
///
/// The measurements come from the ran candidate's own
/// [`ExecutionTelemetry`](oneharness_core::domain::report::ExecutionTelemetry) on
/// the run report, and fall back to whatever the result object itself supplied (a
/// producer standing in for oneharness may inline its timings instead). The
/// history file is still read, but only for `history_id` — the one signal the
/// report has no counterpart for.
fn invocation_telemetry(role: TelemetryRole, invocation: &Invocation) -> InvocationTelemetry {
    let report = &invocation.report;
    let attempts = history::read_attempts(report);
    let measured = invocation
        .result()
        .map(report::measured)
        .unwrap_or_default();
    let supplemental = &invocation.supplemental;
    let candidates = report
        .results
        .iter()
        .enumerate()
        .map(|(index, result)| CandidateAttempt {
            harness: result.harness.clone(),
            harness_id: result.harness_id.clone(),
            variant: result.variant.clone(),
            model: result.model.clone(),
            status: report::status_token(result.status).to_string(),
            available: result.available,
            ran: invocation.ran == Some(index),
            failure_kind: result
                .failure_kind
                .map(|kind| report::failure_token(kind).to_string()),
            failure_kind_source: result.failure_kind_source.clone(),
            exit_code: result.exit_code,
            duration_ms: report::millis(result.duration_ms),
            error: result.error.clone(),
            session_id: result.session_id.clone(),
            history_id: attempts
                .get(index)
                .map(|record| record.history_id.to_string()),
            usage: {
                let usage = report::usage(result);
                (!usage.is_empty()).then_some(usage)
            },
        })
        .collect();
    InvocationTelemetry {
        role: Some(role),
        model_ms: measured.model_ms.or(supplemental.model_ms),
        tool_ms: measured.tool_ms.or(supplemental.tool_ms),
        time_to_first_token_ms: measured
            .time_to_first_token_ms
            .or(supplemental.time_to_first_token_ms),
        usage: invocation.usage().unwrap_or_default(),
        session_id: invocation
            .result()
            .and_then(|result| result.session_id.clone()),
        started_at: measured
            .started_at
            .or_else(|| supplemental.started_at.clone()),
        finished_at: measured
            .finished_at
            .or_else(|| supplemental.finished_at.clone()),
        history_id: invocation
            .ran
            .and_then(|index| attempts.get(index))
            .map(|record| record.history_id.to_string())
            .or_else(|| supplemental.history_id.clone()),
        ran: invocation
            .result()
            .map(|result| result.harness_id.clone())
            .or_else(|| {
                report
                    .fallback
                    .as_ref()
                    .and_then(|fallback| fallback.ran.clone())
            }),
        fell_through: report
            .fallback
            .as_ref()
            .map(|fallback| {
                fallback
                    .fell_through
                    .iter()
                    .map(|fell| FellThrough {
                        harness: fell.harness.clone(),
                        reason: fell.reason.as_str().to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        candidates,
        history_file: report.history_file.clone(),
    }
}

/// Write the prompt to the child's stdin (`--prompt-file -`).
fn write_prompt(op: &str, stdin: Option<&mut impl Write>, prompt: &str) -> Result<()> {
    let stdin =
        stdin.ok_or_else(|| Error::provider(op.to_string(), "could not open oneharness stdin"))?;
    stdin
        .write_all(prompt.as_bytes())
        .map_err(|e| Error::provider(op.to_string(), format!("could not write prompt: {e}")))
}

/// The classified error for a `oneharness` process that exited non-zero.
fn exit_error(op: &str, status: &ExitStatus, stderr: &str) -> Error {
    Error::provider_classified(
        op.to_string(),
        format!("oneharness exited with {status}: {}", stderr.trim()),
        ProviderErrorKind::Protocol,
    )
}

/// Attach a streamed child's stderr to `err`, when it wrote any. The stream
/// violation stays the headline; oneharness's own words follow it.
fn with_stderr(err: Error, stderr: &str) -> Error {
    let stderr = stderr.trim();
    match err {
        Error::Provider {
            context,
            message,
            kind,
        } if !stderr.is_empty() => Error::Provider {
            context,
            message: format!("{message}; oneharness stderr: {stderr}"),
            kind,
        },
        other => other,
    }
}

/// Lift one parsed oneharness invocation into the engine's assistant turn.
fn assistant_turn(invocation: &Invocation) -> AssistantTurn {
    AssistantTurn {
        message: invocation.reply(),
        done: false,
        usage: invocation.usage(),
        events: invocation.events(),
    }
}

/// Describe a skill turn. Pure and total, so it is unit-tested directly.
///
/// No harness/model selection: the agent side relies on oneharness's own
/// discovered config (`oneharness.toml`) for it.
#[must_use]
fn respond_spec(
    instructions: &str,
    worktree: &str,
    session: Option<&str>,
    history_name: Option<&str>,
    stream: bool,
    control: bool,
    prompt: &str,
) -> TurnSpec {
    TurnSpec {
        system: Some(instructions.to_string()),
        cwd: Some(worktree.to_string()),
        config: None,
        // Folded in by `OneharnessProvider::mocked`, which is the one place that
        // knows whether this run is mocked at all.
        mock_harness: Vec::new(),
        // Always thread the caller-owned session name; the caller retries without
        // it if oneharness reports the harness cannot bind a session.
        session: session.map(str::to_string),
        history_name: history_name.map(str::to_string),
        events: true,
        stream,
        // Turn control is addressed by the `--session` name, which is why it only
        // ever rides alongside one and is dropped when the session is.
        control: control && session.is_some(),
        prompt: prompt.to_string(),
    }
}

/// Describe a judge or simulated-user turn (no system prompt, no events).
/// Harness/model selection comes from the judge config, not from a harness/model
/// selection onejudge makes.
#[must_use]
fn judge_side_spec(
    judge_config: Option<&Path>,
    session: Option<&str>,
    cwd: Option<&str>,
    prompt: &str,
) -> TurnSpec {
    TurnSpec {
        system: None,
        cwd: cwd.map(str::to_string),
        config: judge_config.map(Path::to_path_buf),
        // As on the agent side: `OneharnessProvider::mocked` folds it in.
        mock_harness: Vec::new(),
        session: session.map(str::to_string),
        history_name: None,
        events: false,
        // Streaming is about the long agent turn, not the short judgement calls.
        stream: false,
        control: false,
        prompt: prompt.to_string(),
    }
}

impl Provider for OneharnessProvider {
    fn reset_telemetry(&self) {
        self.telemetry.borrow_mut().clear();
        self.spawner.reset();
        // The control address describes one run's turn, not the provider, so a
        // second run must never inherit the first's — least of all an address
        // whose socket is gone.
        *self.control_outcome.borrow_mut() = ControlOutcome::NotRequested;
        *self.control_socket.borrow_mut() = None;
    }

    fn invocation_telemetry(&self) -> Vec<InvocationTelemetry> {
        self.telemetry.borrow().clone()
    }

    fn spawned_processes(&self) -> Vec<SpawnedProcess> {
        self.spawner.records()
    }

    fn control(&self) -> ControlOutcome {
        self.control_outcome.borrow().clone()
    }

    fn respond(
        &self,
        skill: &SkillRef<'_>,
        messages: &[Message],
        session: Option<&str>,
    ) -> Result<AssistantTurn> {
        self.run_respond(
            skill.instructions,
            skill.dir,
            messages,
            session,
            &mut |_| ControlFlow::Continue(()),
        )
    }

    fn respond_streaming(
        &self,
        skill: &SkillRef<'_>,
        messages: &[Message],
        session: Option<&str>,
        on_event: &mut dyn FnMut(&ToolEvent) -> ControlFlow<()>,
    ) -> Result<AssistantTurn> {
        self.run_respond(skill.instructions, skill.dir, messages, session, on_event)
    }

    fn simulate_user(
        &self,
        persona: &str,
        messages: &[Message],
        session: Option<&str>,
    ) -> Result<UserTurn> {
        let prompt = build_user_prompt(persona, messages);
        let result = self.run_judge_side("user", &prompt, session, None)?;
        Ok(UserTurn {
            message: result.reply(),
            stop: false,
            usage: result.usage(),
        })
    }

    fn supervise(
        &self,
        query: &SupervisorQuery<'_>,
        messages: &[Message],
        session: Option<&str>,
    ) -> Result<SupervisorTurn> {
        let base = build_supervisor_prompt(query, messages);
        supervise_with_reask(|attempt| {
            // The re-ask says what was unusable about the last answer; asking the
            // identical question again mostly buys the identical answer.
            let prompt = if attempt == 0 {
                base.clone()
            } else {
                format!("{base}{SUPERVISOR_REASK_NOTE}")
            };
            let result =
                self.run_judge_side("supervisor", &prompt, session, Some(query.worktree))?;
            Ok(SupervisorTurn {
                outcome: parse_supervisor("oneharness:supervisor", &result.reply())?,
                usage: result.usage(),
            })
        })
    }

    fn judge(&self, query: &JudgeQuery<'_>, messages: &[Message]) -> Result<JudgeVerdict> {
        // Judging is stateless — no session to continue.
        let prompt = build_judge_prompt(query, messages);
        let result = self.run_judge_side("judge", &prompt, None, None)?;
        let mut verdict = parse_verdict(query.kind, "oneharness:judge", &result.reply())?;
        verdict.usage = result.usage();
        Ok(verdict)
    }

    fn assess(&self, prompt: &str, messages: &[Message]) -> Result<Assessment> {
        let prompt = build_assessment_prompt(prompt, messages);
        let result = self.run_judge_side("assess", &prompt, None, None)?;
        let text = result.reply();
        if text.trim().is_empty() {
            return Err(Error::provider(
                "oneharness:assess",
                "judge returned an empty assessment",
            ));
        }
        Ok(Assessment {
            text,
            usage: result.usage(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::JudgeKind;
    use oneharness_core::errors::OneharnessError;

    /// The argv a spec renders on the spawning seam, for the assertions below that
    /// are about flags rather than about the spec.
    fn argv_of(spec: &TurnSpec) -> Vec<String> {
        turn::argv(spec)
    }

    #[test]
    fn builders_configure_bin_and_judge_config() {
        let provider = OneharnessProvider::default()
            .with_bin("my-oneharness")
            .with_judge_config("custom.judge.toml");
        // Naming a binary is the opt-in to the spawning seam.
        assert!(matches!(&provider.execution, Execution::Process(bin) if bin == "my-oneharness"));
        assert_eq!(
            provider.judge_config.as_deref(),
            Some(Path::new("custom.judge.toml"))
        );
        // The judge/user side passes the configured file via --config.
        let args = argv_of(&judge_side_spec(
            provider.judge_config.as_deref(),
            Some("s"),
            None,
            "p",
        ));
        assert!(args
            .windows(2)
            .any(|w| w == ["--config", "custom.judge.toml"]));
        assert!(args.windows(2).any(|w| w == ["--session", "s"]));
    }

    #[test]
    fn a_default_provider_runs_the_turn_in_process() {
        assert!(matches!(
            OneharnessProvider::new().execution,
            Execution::Library
        ));
    }

    #[test]
    fn installing_a_spawn_hook_selects_the_seam_that_has_a_process_to_offer_it() {
        // A hook offers a *process*; the in-process seam has none, so installing
        // one must not leave the embedder believing it owns an empty group.
        let hooked = OneharnessProvider::new().with_spawn_hook(std::sync::Arc::new(Inert));
        assert!(matches!(&hooked.execution, Execution::Process(bin) if bin == "oneharness"));
        // An explicitly named binary is not clobbered by installing a hook.
        let pinned = OneharnessProvider::new()
            .with_bin("/opt/oneharness")
            .with_spawn_hook(std::sync::Arc::new(Inert));
        assert!(matches!(&pinned.execution, Execution::Process(bin) if bin == "/opt/oneharness"));
    }

    #[test]
    fn a_mock_harness_rides_both_sides_of_the_turn_and_selects_the_spawning_seam() {
        let provider = OneharnessProvider::new().with_mock_harness("claude-code");
        // oneharness delivers the responder by re-executing its own binary, which an
        // in-process run does not have — so naming one selects the spawning seam
        // rather than silently running against a paid model.
        assert!(matches!(&provider.execution, Execution::Process(bin) if bin == "oneharness"));
        // An explicitly named binary is kept.
        let pinned = OneharnessProvider::new()
            .with_bin("/opt/oneharness")
            .with_mock_harness("codex");
        assert!(matches!(&pinned.execution, Execution::Process(bin) if bin == "/opt/oneharness"));

        // Both parties are mocked: either one alone still bills a model.
        let agent = argv_of(&provider.mocked(respond_spec(
            "do x",
            "/work",
            Some("s"),
            Some("s"),
            false,
            false,
            "p",
        )));
        assert!(agent
            .windows(2)
            .any(|w| w == ["--mock-harness", "claude-code"]));
        let judge = argv_of(&provider.mocked(judge_side_spec(
            provider.judge_config.as_deref(),
            Some("s"),
            None,
            "p",
        )));
        assert!(judge
            .windows(2)
            .any(|w| w == ["--mock-harness", "claude-code"]));

        // And an ordinary provider asks for no responder at all.
        let plain = OneharnessProvider::new();
        assert!(
            !argv_of(&plain.mocked(judge_side_spec(None, None, None, "p")))
                .iter()
                .any(|arg| arg == "--mock-harness")
        );
    }

    /// A hook that records nothing and refuses nothing — this is about which seam
    /// installing one selects, not about what a hook does.
    struct Inert;

    impl crate::spawn::SpawnHook for Inert {}

    #[test]
    fn default_judge_config_is_the_judge_toml() {
        let provider = OneharnessProvider::new();
        assert_eq!(
            provider.judge_config.as_deref(),
            Some(Path::new(DEFAULT_JUDGE_CONFIG))
        );
    }

    #[test]
    fn a_respond_spec_threads_the_session_and_selects_no_harness_or_model() {
        let args = argv_of(&respond_spec(
            "do x",
            "/work",
            Some("run-1-skill"),
            Some("run-1-skill"),
            false,
            false,
            "p",
        ));
        assert!(args.windows(2).any(|w| w == ["--session", "run-1-skill"]));
        assert!(args.iter().any(|a| a == "--events"));
        // Harness/model selection is oneharness's config's job now.
        assert!(!args.iter().any(|a| a == "--harness"));
        assert!(!args.iter().any(|a| a == "--model"));
        // `oneharness run` has no `--format` flag; passing it is a live-path bug.
        assert!(!args.iter().any(|a| a == "--format"));
        // A buffered provider never asks for the stream.
        assert!(!args.iter().any(|a| a == "--stream"));

        // No session supplied: no `--session`.
        let none = argv_of(&respond_spec(
            "do x", "/work", None, None, false, false, "p",
        ));
        assert!(!none.iter().any(|a| a == "--session"));
    }

    #[test]
    fn control_needs_a_session_to_be_addressed_by() {
        // Asked for without one, the spec drops it rather than asking for a socket
        // nothing can address.
        let spec = respond_spec("do x", "/work", None, None, false, true, "p");
        assert!(!spec.control);
        let spec = respond_spec("do x", "/work", Some("s"), Some("s"), false, true, "p");
        assert!(spec.control);
    }

    #[test]
    fn streaming_is_asked_for_on_the_agent_side_only() {
        let provider = OneharnessProvider::new().with_streaming(true);
        assert!(provider.stream);
        let agent = respond_spec(
            "do x",
            "/work",
            Some("s"),
            Some("s"),
            provider.stream,
            false,
            "p",
        );
        assert!(agent.stream);
        assert!(argv_of(&agent).iter().any(|a| a == "--stream"));
        // The judge / simulated-user side stays buffered: streaming is about the
        // long agent turn, not the short judgement calls.
        let judge = judge_side_spec(None, Some("s"), None, "p");
        assert!(!judge.stream);
        assert!(!argv_of(&judge).iter().any(|a| a == "--stream"));
    }

    #[test]
    fn a_judge_side_spec_selects_by_config_not_by_harness_or_model() {
        let args = argv_of(&judge_side_spec(
            Some(Path::new("oneharness.judge.toml")),
            None,
            None,
            "p",
        ));
        assert!(!args.iter().any(|a| a == "--system"));
        assert!(!args.iter().any(|a| a == "--events"));
        assert!(!args.iter().any(|a| a == "--harness"));
        assert!(!args.iter().any(|a| a == "--model"));
        assert!(args
            .windows(2)
            .any(|w| w == ["--config", "oneharness.judge.toml"]));
        // With no judge config, no `--config` is passed (oneharness discovers its
        // own default).
        let no_config = argv_of(&judge_side_spec(None, None, None, "p"));
        assert!(!no_config.iter().any(|a| a == "--config"));
    }

    #[test]
    fn is_session_unsupported_matches_oneharness_error() {
        let unsupported = Error::provider_classified(
            "respond",
            "oneharness exited with exit status: 1: harness `goose` does not support --session: \
             it exposes no session id headlessly",
            ProviderErrorKind::Protocol,
        );
        assert!(is_session_unsupported(&unsupported));
        let other = Error::provider_classified(
            "respond",
            "some other failure",
            ProviderErrorKind::Protocol,
        );
        assert!(!is_session_unsupported(&other));
    }

    #[test]
    fn session_unsupported_marker_tracks_oneharness() {
        // oneharness refuses `--session` before it can emit a report, so this one
        // recovery is driven off stderr text. Pin the substring against the error
        // oneharness itself renders: an upstream rewording then fails here instead
        // of silently turning the graceful retry into a failed run.
        let upstream = OneharnessError::SessionUnsupported {
            id: "goose".into(),
            supported: "claude-code".into(),
        }
        .to_string();
        assert!(
            upstream.contains(SESSION_UNSUPPORTED_MARKER),
            "oneharness now says: {upstream}"
        );
    }

    #[test]
    fn control_refusal_markers_track_oneharness() {
        // Every way `oneharness run --control` can refuse the ask, in oneharness's
        // own rendering. Each is a usage error before a report exists, so the
        // degradation is driven off stderr text — and an upstream rewording has to
        // fail here rather than turn a degraded run into a failed one.
        let refusals = [
            OneharnessError::ControlNeedsSession,
            OneharnessError::ControlUnsupported {
                id: "qwen".into(),
                supported: "claude-code, codex".into(),
            },
            OneharnessError::ControlSingleHarness {
                selected: "claude-code, codex".into(),
            },
            OneharnessError::ControlBatch,
            OneharnessError::ControlPlatform,
            OneharnessError::ControlStreamUnsupported {
                id: "opencode".into(),
            },
            OneharnessError::ControlSchema,
            OneharnessError::ControlModeUnsupported {
                id: "opencode".into(),
                mode: "edit",
            },
            OneharnessError::ControlOutputFormat {
                id: "claude-code".into(),
                required: "stream-json".into(),
                selected: "text".into(),
            },
            OneharnessError::ControlSocket {
                path: "/state/control/x.sock".into(),
                source: std::io::Error::other("in use"),
            },
            OneharnessError::MultiModelConflict {
                with: "--control",
                why: "control drives one live turn",
            },
            // A named handle whose stored conversation cannot be reopened over
            // the mechanism that would serve this turn (oneharness 0.12). It
            // belongs on this ladder rather than failing the run because
            // oneharness's own remedy is onejudge's next rung: drop `--control`
            // and the handle continues on the harness's ordinary headless run.
            // Degrading the other way — keeping the flag and taking a fresh
            // conversation — is the very failure the refusal exists to stop.
            OneharnessError::SessionControlNoResume {
                name: "run-42-skill".into(),
                id: "opencode".into(),
                mechanism: "opencode-http",
                supported: "claude-code, codex".into(),
            },
            // The socket address the run would open is past this platform's
            // `sun_path` budget (oneharness 0.12). The channel cannot exist, so
            // it is the ask that is refused, not the turn.
            OneharnessError::ControlSocketAddress {
                source: oneharness_core::domain::control::socket_path(
                    std::path::Path::new(&"/x".repeat(200)),
                    "run-42-skill",
                )
                .expect_err("an address that long cannot be bound anywhere"),
            },
        ];
        for refusal in refusals {
            let err = stderr_error(&format!("oneharness: error: {refusal}"));
            assert!(
                is_control_refused(&err),
                "onejudge would fail the run instead of degrading; oneharness now says: {refusal}"
            );
        }
        // And it must not swallow an ordinary turn failure as a control refusal.
        let unrelated = stderr_error("oneharness: error: harness `codex` is not installed");
        assert!(!is_control_refused(&unrelated));
    }

    #[test]
    fn a_platform_without_unix_sockets_degrades_with_a_stated_reason() {
        // The Windows answer, asserted wherever the gate runs: the ask is dropped
        // with a reason rather than sent to a oneharness that would refuse the run.
        let reason = control_platform_reason(false).expect("a non-unix host has no socket");
        assert!(reason.contains("unix domain socket"));
        assert!(control_platform_reason(true).is_none());
    }

    #[test]
    fn control_rides_the_agent_argv_only_alongside_a_session() {
        let with = argv_of(&respond_spec(
            "do x",
            "/work",
            Some("s"),
            Some("s"),
            false,
            true,
            "p",
        ));
        assert!(with.iter().any(|a| a == "--control"));
        // Off by default: the argv is byte-identical to a run that never asked.
        let without = argv_of(&respond_spec(
            "do x",
            "/work",
            Some("s"),
            Some("s"),
            false,
            false,
            "p",
        ));
        assert!(!without.iter().any(|a| a == "--control"));
        // `--control` is addressed by the session name, so dropping the session
        // drops it too rather than sending oneharness an unaddressable ask.
        let sessionless = argv_of(&respond_spec(
            "do x",
            "/work",
            None,
            Some("s"),
            false,
            true,
            "p",
        ));
        assert!(!sessionless.iter().any(|a| a == "--control"));
        // The judge side is never controlled.
        assert!(!argv_of(&judge_side_spec(None, Some("s"), None, "p"))
            .iter()
            .any(|a| a == "--control"));
    }

    #[test]
    fn a_provider_that_never_asked_reports_no_control() {
        assert_eq!(
            OneharnessProvider::new().control(),
            ControlOutcome::NotRequested
        );
        // Asking does not by itself claim an address: only a turn that got one does.
        assert_eq!(
            OneharnessProvider::new().with_control(true).control(),
            ControlOutcome::NotRequested
        );
    }

    #[test]
    fn a_controlled_report_with_no_socket_is_unavailable_not_open() {
        // oneharness refuses `--control` rather than honoring it silently, so this
        // is unreachable through it — but a stand-in producer could, and reporting
        // a lever that does not exist is the one failure this feature must not have.
        let provider = OneharnessProvider::new().with_control(true);
        provider.control_address("/work");
        assert!(provider
            .control()
            .unavailable_reason()
            .expect("a missing socket is a stated reason")
            .contains("named no control socket"));
    }

    /// The classified error a `oneharness` process that refused the call produces,
    /// carrying `stderr` — the shape [`is_control_refused`] reads.
    fn stderr_error(stderr: &str) -> Error {
        Error::provider_classified(
            "respond".to_string(),
            format!("oneharness exited with exit status: 2: {stderr}"),
            ProviderErrorKind::Protocol,
        )
    }

    #[test]
    fn respond_prompt_switches_on_continuing() {
        let mut messages = vec![Message::user("first")];
        messages.push(Message::assistant("reply"));
        messages.push(Message::user("second"));
        assert_eq!(latest_or_inline(&messages, true), "second");
        assert!(latest_or_inline(&messages, false).contains("first"));
    }

    #[test]
    fn spawn_failure_is_classified() {
        let provider = OneharnessProvider::new().with_bin("definitely-not-oneharness-xyz");
        let err = provider
            .judge(
                &JudgeQuery {
                    kind: JudgeKind::Boolean,
                    criterion: "x",
                    scale: None,
                },
                &[],
            )
            .unwrap_err();
        assert_eq!(err.kind(), Some(ProviderErrorKind::Spawn));
    }
}
