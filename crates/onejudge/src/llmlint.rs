//! [`LlmlintProvider`]: a judge-side [`Provider`] whose whole verdict is one
//! [`llmlint`](https://github.com/nickderobertis/llmlint) run over the worker's
//! tree.
//!
//! A run with failing rules is a *not done* decision whose message to the worker
//! is llmlint's own evaluation output, verbatim; a run with no failing rules is
//! *done*; and a run that could not complete is an error of the turn that is
//! never mistaken for either. It is the deterministic-by-rule judge a repository's
//! lint tier already is, moved inside the loop — so a worker is pushed back on its
//! findings turn by turn instead of discovering them at the merge path, and so a
//! judge can be something other than a model's opinion.
//!
//! onejudge meets llmlint **at the process boundary only**: no llmlint crate is
//! linked, and the contract is llmlint's documented exit codes — `0` every rule
//! holds, `1` at least one violation, `2` the run could not complete. The
//! executable is probed (`<bin> --version`) when the provider is built, so an
//! absent `llmlint` is a loud error at the boundary and never a silent pass.
//!
//! The provider answers only the two judge-side operations a lint run *can*
//! answer — the per-turn `supervise` decision and a boolean `judge` — and refuses
//! every other with [`Error::Invalid`]: it runs no agent turn, plays no user,
//! scores no number and writes no prose. Composed into a
//! [`JudgePanel`](crate::JudgePanel) with the matching
//! [`JudgeAbilities`](crate::JudgeAbilities), the panel simply leaves it out of
//! those operations. `docs/judges.md` is the contract this module implements.

use std::cell::RefCell;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::time::Instant;

use crate::error::{Error, ProviderErrorKind, Result};
use crate::provider::{
    Assessment, AssistantTurn, EvidenceContext, JudgeKind, JudgeQuery, JudgeValue, JudgeVerdict,
    Provider, SkillRef, SupervisorOutcome, SupervisorQuery, SupervisorTurn, UserTurn,
};
use crate::spawn::{SharedSpawnHook, SpawnContext, SpawnedProcess, Spawner};
use crate::telemetry::{InvocationTelemetry, TelemetryRole};
use crate::transcript::{Message, ToolEvent};

/// The `llmlint` executable a provider built without naming one resolves on
/// `PATH`.
pub const DEFAULT_LLMLINT_BIN: &str = "llmlint";

/// How much of llmlint's stderr and stdout an error of the turn carries. The
/// message is read by an operator and lands on the failure report; a run that
/// died mid-way can have written a great deal, and none of it past the head is
/// what says why.
const OUTPUT_BOUND: usize = 4096;

/// The operation the construction-time `--version` probe is recorded under.
const PROBE_OP: &str = "probe";

/// A judge-side [`Provider`] whose verdict is one `llmlint lint` run over the
/// worker's tree. See the [module docs](self).
#[derive(Debug, Clone)]
pub struct LlmlintProvider {
    bin: String,
    config: Option<PathBuf>,
    diff_base: Option<String>,
    args: Vec<String>,
    spawner: Spawner,
    telemetry: RefCell<Vec<InvocationTelemetry>>,
}

/// What one `llmlint lint` run decided.
enum Lint {
    /// Exit 0: every rule holds. `summary` is the report's summary line.
    Clean { summary: String },
    /// Exit 1: at least one rule fails. `report` is llmlint's evaluation output,
    /// verbatim; `summary` its summary line.
    Failing { report: String, summary: String },
}

impl LlmlintProvider {
    /// Build a provider over the `llmlint` executable `bin` (a name resolved on
    /// `PATH`, or a path), probing it with `<bin> --version` before returning.
    ///
    /// # Errors
    /// [`Error::Invalid`] if `bin` is blank; [`Error::Provider`] with
    /// [`ProviderErrorKind::Spawn`], naming the binary and the `bin` field, if it
    /// cannot be run or does not answer `--version` — so an absent or wrong
    /// executable is refused where the provider is built, before any turn.
    pub fn new(bin: impl Into<String>) -> Result<Self> {
        let bin = bin.into();
        if bin.trim().is_empty() {
            return Err(Error::Invalid("llmlint `bin` is empty".into()));
        }
        let provider = Self {
            bin,
            config: None,
            diff_base: None,
            args: Vec::new(),
            spawner: Spawner::default(),
            telemetry: RefCell::new(Vec::new()),
        };
        provider.probe()?;
        Ok(provider)
    }

    /// Pass `config` to every run as `llmlint lint -c <config>`. Unset, llmlint
    /// discovers its own configuration from the worktree it lints.
    #[must_use]
    pub fn with_config(mut self, config: impl Into<PathBuf>) -> Self {
        self.config = Some(config.into());
        self
    }

    /// Review only what the worktree changed against the git revision `base`:
    /// every run adds `--diff --diff-base <base>`.
    ///
    /// onejudge does no base auto-detection — a host wiring a stack passes its own
    /// comparison base here.
    #[must_use]
    pub fn with_diff_base(mut self, base: impl Into<String>) -> Self {
        self.diff_base = Some(base.into());
        self
    }

    /// Append `args` to every `llmlint lint` invocation, after the arguments
    /// onejudge composes.
    #[must_use]
    pub fn with_args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    /// Offer every process this provider spawns to `hook` before it starts work,
    /// so an in-process embedder can place it in a group it owns and can later
    /// terminate — a running `llmlint` included. See [`SpawnHook`](crate::SpawnHook).
    #[must_use]
    pub fn with_spawn_hook(mut self, hook: SharedSpawnHook) -> Self {
        self.spawner.install(hook);
        self
    }

    /// The executable this provider runs.
    #[must_use]
    pub fn bin(&self) -> &str {
        &self.bin
    }

    /// The config file every run is handed, if one was set.
    #[must_use]
    pub fn config(&self) -> Option<&Path> {
        self.config.as_deref()
    }

    /// The comparison base every run diffs against, if one was set.
    #[must_use]
    pub fn diff_base(&self) -> Option<&str> {
        self.diff_base.as_deref()
    }

    /// The extra arguments appended to every run.
    #[must_use]
    pub fn args(&self) -> &[String] {
        &self.args
    }

    /// `<bin> --version`: the construction-time proof that the executable is
    /// there and is runnable. Its process record is discarded once it has
    /// answered — it is a check on the environment, not a process of any run.
    fn probe(&self) -> Result<()> {
        let mut command = Command::new(&self.bin);
        command.arg("--version");
        let output = self.run(&mut command, PROBE_OP)?;
        self.spawner.reset();
        if !output.status.success() {
            return Err(Error::provider_classified(
                PROBE_OP,
                format!(
                    "llmlint `{}` (the provider's `bin`) could not answer `--version` ({}): {}",
                    self.bin,
                    describe(output.status),
                    bounded(&output.stderr, &output.stdout)
                ),
                ProviderErrorKind::Spawn,
            ));
        }
        Ok(())
    }

    /// Spawn `command` under the spawn hook — stdin closed, stdout and stderr
    /// captured, the environment inherited — and wait for it.
    fn run(&self, command: &mut Command, op: &str) -> Result<Output> {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = self.spawner.spawn(
            command,
            &SpawnContext {
                role: TelemetryRole::Judge,
                op,
                program: &self.bin,
            },
            |e| {
                Error::provider_classified(
                    op.to_string(),
                    format!(
                        "could not run llmlint `{}` (the provider's `bin`): {e}. Is llmlint \
                         installed and on PATH?",
                        self.bin
                    ),
                    ProviderErrorKind::Spawn,
                )
            },
        )?;
        Running::new(child).finish().map_err(|e| {
            Error::provider(
                op.to_string(),
                format!("llmlint `{}` did not complete: {e}", self.bin),
            )
        })
    }

    /// One `llmlint lint` run over `worktree`, under `op` (`supervise` or
    /// `judge`), recorded on this provider's telemetry and process records.
    ///
    /// The exact argv is the contract in `docs/judges.md`:
    /// `<bin> lint --cwd <worktree> --format human --color never --progress never
    /// [-c <config>] [--diff --diff-base <ref>] [<args>…]`.
    fn lint(&self, op: &str, worktree: &str) -> Result<Lint> {
        let mut command = Command::new(&self.bin);
        command.arg("lint").arg("--cwd").arg(worktree).args([
            "--format",
            "human",
            "--color",
            "never",
            "--progress",
            "never",
        ]);
        if let Some(config) = &self.config {
            command.arg("-c").arg(config);
        }
        if let Some(base) = &self.diff_base {
            command.arg("--diff").arg("--diff-base").arg(base);
        }
        command.args(&self.args);

        let started = Instant::now();
        let output = self.run(&mut command, op)?;
        // One judge-side invocation per run: its wall time is the tool time (there
        // is no model call of onejudge's to attribute anything else to), and no
        // candidate identity — llmlint's own harness selection is llmlint's.
        self.telemetry.borrow_mut().push(InvocationTelemetry {
            role: Some(TelemetryRole::Judge),
            tool_ms: Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)),
            ..InvocationTelemetry::default()
        });

        let stdout = String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_string();
        match output.status.code() {
            Some(0) => {
                let summary = summary_line(&stdout).ok_or_else(|| {
                    Error::provider_classified(
                        op.to_string(),
                        "llmlint exited 0 but wrote no report (expected its summary line on \
                         stdout)",
                        ProviderErrorKind::Protocol,
                    )
                })?;
                Ok(Lint::Clean { summary })
            }
            Some(1) => {
                let summary = summary_line(&stdout).ok_or_else(|| {
                    Error::provider_classified(
                        op.to_string(),
                        "llmlint exited 1 (at least one rule fails) but wrote no report: \
                         there is nothing to hand the worker",
                        ProviderErrorKind::Protocol,
                    )
                })?;
                Ok(Lint::Failing {
                    report: stdout,
                    summary,
                })
            }
            // Exit 2 is llmlint's "could not complete"; any other code, and death
            // by signal, is the same fact told less politely. None of them is a
            // verdict: never `Completed`, never `Continue`.
            _ => Err(Error::provider_classified(
                op.to_string(),
                format!(
                    "llmlint could not complete ({}): {}",
                    describe(output.status),
                    bounded(&output.stderr, &output.stdout)
                ),
                ProviderErrorKind::Other,
            )),
        }
    }

    /// The refusal for an operation a lint run cannot answer.
    fn cannot<T>(what: &str) -> Result<T> {
        Err(Error::Invalid(format!(
            "an llmlint judge {what}; it takes part only in the per-turn supervisor decision and \
             boolean judgements, as one judge of a `SplitProvider` panel"
        )))
    }
}

/// A spawned child whose output is being collected. Dropped before it was
/// reaped — the collecting call unwound — it is killed and waited for, so no
/// `llmlint` outlives the provider call that started it.
struct Running {
    child: Child,
    reaped: bool,
}

impl Running {
    fn new(child: Child) -> Self {
        Self {
            child,
            reaped: false,
        }
    }

    /// Read stdout and stderr to their ends (concurrently, so neither pipe can
    /// fill and stall the child) and wait for the exit status.
    fn finish(mut self) -> io::Result<Output> {
        let mut stdout = self.child.stdout.take().expect("stdout is piped");
        let mut stderr = self.child.stderr.take().expect("stderr is piped");
        let (out, err) = std::thread::scope(|scope| {
            let err = scope.spawn(move || {
                let mut buf = Vec::new();
                stderr.read_to_end(&mut buf).map(|_| buf)
            });
            let mut buf = Vec::new();
            let out = stdout.read_to_end(&mut buf).map(|_| buf);
            let err = err
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
            (out, err)
        });
        let status = self.child.wait()?;
        self.reaped = true;
        Ok(Output {
            status,
            stdout: out?,
            stderr: err?,
        })
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// The report's summary line: the last non-empty line of stdout.
fn summary_line(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

/// `exit <code>` for a process that exited, and the status's own description
/// (`signal: 9 (SIGKILL)`) for one that did not.
fn describe(status: ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exit {code}"),
        None => status.to_string(),
    }
}

/// stderr, then stdout, trimmed, joined by a newline and cut to
/// [`OUTPUT_BOUND`] characters.
fn bounded(stderr: &[u8], stdout: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    let stdout = String::from_utf8_lossy(stdout);
    let mut text = [stderr.trim(), stdout.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if text.is_empty() {
        text.push_str("(no output)");
    }
    if let Some((cut, _)) = text.char_indices().nth(OUTPUT_BOUND) {
        text.truncate(cut);
        text.push_str(" […]");
    }
    text
}

impl Provider for LlmlintProvider {
    fn reset_telemetry(&self) {
        self.spawner.reset();
        self.telemetry.borrow_mut().clear();
    }

    fn invocation_telemetry(&self) -> Vec<InvocationTelemetry> {
        self.telemetry.borrow().clone()
    }

    fn spawned_processes(&self) -> Vec<SpawnedProcess> {
        self.spawner.records()
    }

    fn respond(
        &self,
        _skill: &SkillRef<'_>,
        _messages: &[Message],
        _session: Option<&str>,
    ) -> Result<AssistantTurn> {
        Self::cannot("runs no agent turn")
    }

    fn respond_streaming(
        &self,
        _skill: &SkillRef<'_>,
        _messages: &[Message],
        _session: Option<&str>,
        _on_event: &mut dyn FnMut(&ToolEvent) -> std::ops::ControlFlow<()>,
    ) -> Result<AssistantTurn> {
        Self::cannot("runs no agent turn")
    }

    fn simulate_user(
        &self,
        _persona: &str,
        _messages: &[Message],
        _session: Option<&str>,
    ) -> Result<UserTurn> {
        Self::cannot("cannot play the simulated user")
    }

    /// The per-turn decision: one lint run over the worktree the query names.
    /// Failing rules continue the run with llmlint's report as the worker's next
    /// user turn, verbatim; a clean run completes it with the summary line as the
    /// reason. The transcript and session are not consulted — the tree is the
    /// evidence.
    fn supervise(
        &self,
        query: &SupervisorQuery<'_>,
        _messages: &[Message],
        _session: Option<&str>,
    ) -> Result<SupervisorTurn> {
        let outcome = match self.lint("supervise", query.worktree)? {
            Lint::Clean { summary } => SupervisorOutcome::Completed { reason: summary },
            Lint::Failing { report, summary } => SupervisorOutcome::Continue {
                message: report,
                reason: summary,
            },
        };
        Ok(SupervisorTurn {
            outcome,
            usage: None,
        })
    }

    /// A judgement with no worktree to lint is refused: the evidence-carrying
    /// [`Provider::judge_with_evidence`] is the call an llmlint judge answers.
    fn judge(&self, query: &JudgeQuery<'_>, messages: &[Message]) -> Result<JudgeVerdict> {
        self.judge_with_evidence(query, messages, EvidenceContext::default())
    }

    /// A boolean verdict: `true` when every rule holds over `evidence.worktree`,
    /// `false` when at least one fails, the summary line as the reason either
    /// way. A numeric query, or one with no worktree, is [`Error::Invalid`].
    fn judge_with_evidence(
        &self,
        query: &JudgeQuery<'_>,
        _messages: &[Message],
        evidence: EvidenceContext<'_>,
    ) -> Result<JudgeVerdict> {
        if query.kind == JudgeKind::Numeric {
            return Self::cannot("scores no number");
        }
        let Some(worktree) = evidence.worktree else {
            return Err(Error::Invalid(
                "an llmlint judgement needs the worktree to lint (`EvidenceContext::worktree`), \
                 and none was supplied"
                    .into(),
            ));
        };
        let (value, reason) = match self.lint("judge", worktree)? {
            Lint::Clean { summary } => (true, summary),
            Lint::Failing { summary, .. } => (false, summary),
        };
        Ok(JudgeVerdict {
            value: JudgeValue::Bool(value),
            reason,
            usage: None,
        })
    }

    fn assess(&self, _prompt: &str, _messages: &[Message]) -> Result<Assessment> {
        Self::cannot("writes no assessment")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blank_bin_is_invalid() {
        let err = LlmlintProvider::new("  ").unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "{err}");
    }

    #[test]
    fn an_absent_executable_is_a_classified_spawn_error_naming_the_bin() {
        let err = LlmlintProvider::new("definitely-not-llmlint-xyz").unwrap_err();
        assert_eq!(err.kind(), Some(ProviderErrorKind::Spawn));
        let text = err.to_string();
        assert!(text.contains("definitely-not-llmlint-xyz"), "{text}");
        assert!(text.contains("`bin`"), "{text}");
    }

    #[test]
    fn the_summary_line_is_the_last_non_empty_line() {
        assert_eq!(
            summary_line("FAIL x\n  at a:1\n\n1 failed, 2 passed\n\n").as_deref(),
            Some("1 failed, 2 passed")
        );
        assert_eq!(summary_line("\n  \n"), None);
        assert_eq!(summary_line(""), None);
    }

    #[test]
    fn an_error_message_carries_stderr_then_stdout_within_the_bound() {
        assert_eq!(
            bounded(b"boom\n", b"partial report"),
            "boom\npartial report"
        );
        assert_eq!(bounded(b"", b""), "(no output)");
        let long = "x".repeat(OUTPUT_BOUND + 100);
        let cut = bounded(long.as_bytes(), b"");
        assert_eq!(cut.chars().count(), OUTPUT_BOUND + " […]".chars().count());
        assert!(cut.ends_with(" […]"));
    }

    #[cfg(unix)]
    #[test]
    fn an_exit_code_and_a_signal_are_described_differently() {
        use std::os::unix::process::ExitStatusExt as _;
        // A raw wait status: the exit code in the high byte, a signal in the low.
        assert_eq!(describe(ExitStatus::from_raw(2 << 8)), "exit 2");
        let killed = describe(ExitStatus::from_raw(9));
        assert!(killed.contains("signal"), "{killed}");
    }

    #[cfg(windows)]
    #[test]
    fn an_exit_code_is_described_by_its_number() {
        use std::os::windows::process::ExitStatusExt as _;
        assert_eq!(describe(ExitStatus::from_raw(2)), "exit 2");
    }
}
