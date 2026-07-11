//! [`OneharnessProvider`]: the default [`Provider`], which runs each prompt on a
//! real harness through the [`oneharness`](https://github.com/nickderobertis/oneharness)
//! CLI (`oneharness run`, whose report is JSON by default) and parses its report.
//!
//! It targets **oneharness v0.3.13+** for the uniform `--session <name>` handle:
//! the engine threads one caller-owned name across turns and oneharness maps it to
//! the harness's native session in its on-disk store, so onejudge never extracts
//! or re-passes a native id. `--session` is honored only for the harnesses that
//! expose a session id headlessly (see [`session_capable`]); the rest re-read the
//! inlined transcript.
//!
//! The pure pieces — argument construction, report parsing, the session-capable
//! table — are separated from the one thin `spawn + wait` shell so they are
//! deterministically unit-tested; the whole path is proven end-to-end against a
//! fake `oneharness` binary in the e2e suite and against a real one in the live
//! tier (`docs/live-tier.md`).

use std::io::Write as _;
use std::process::{Command, Stdio};

use serde::Deserialize;

use crate::error::{Error, ProviderErrorKind, Result};
use crate::provider::{
    build_judge_prompt, build_user_prompt, latest_or_inline, parse_verdict, AssistantTurn,
    JudgeQuery, JudgeVerdict, Provider, SkillRef, UserTurn,
};
use crate::transcript::{Message, ToolEvent};
use crate::usage::Usage;

/// The harnesses whose native session id oneharness can bind a `--session` name
/// to (their `session_capable` in `oneharness list`). The rest emit no id
/// headlessly, so `--session` is a usage error there and they fall back to
/// re-prompting the inlined transcript.
const SESSION_CAPABLE: &[&str] = &["claude-code", "codex", "opencode", "cursor", "qwen"];

/// Whether `oneharness run --session <name>` faithfully continues a session on
/// `platform`.
#[must_use]
pub fn session_capable(platform: &str) -> bool {
    SESSION_CAPABLE.contains(&platform)
}

/// The default [`Provider`]: shells out to the `oneharness` CLI.
pub struct OneharnessProvider {
    bin: String,
    judge_harness: String,
}

impl Default for OneharnessProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl OneharnessProvider {
    /// A provider that invokes `oneharness` on `PATH`, running the judge and
    /// simulated user on the `claude-code` harness.
    #[must_use]
    pub fn new() -> Self {
        Self {
            bin: "oneharness".into(),
            judge_harness: "claude-code".into(),
        }
    }

    /// Override the `oneharness` binary path (e.g. a pinned install, or the fake
    /// binary the e2e suite drives).
    #[must_use]
    pub fn with_bin(mut self, bin: impl Into<String>) -> Self {
        self.bin = bin.into();
        self
    }

    /// Override the harness the judge and simulated user run on (default
    /// `claude-code`), independent of the harness under test.
    #[must_use]
    pub fn with_judge_harness(mut self, harness: impl Into<String>) -> Self {
        self.judge_harness = harness.into();
        self
    }

    /// Spawn `oneharness run` with `args` and `prompt`, and return the parsed
    /// single-result report. The prompt is passed via stdin (`--prompt-file -`)
    /// so an arbitrarily long transcript never trips the OS argv limit.
    fn run(&self, op: &str, args: &[String], prompt: &str) -> Result<OneharnessResult> {
        let mut child = Command::new(&self.bin)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                Error::provider_classified(
                    op.to_string(),
                    format!(
                        "could not run `{}`: {e}. Is oneharness installed and on PATH?",
                        self.bin
                    ),
                    ProviderErrorKind::Spawn,
                )
            })?;
        {
            let stdin = child.stdin.as_mut().ok_or_else(|| {
                Error::provider(op.to_string(), "could not open oneharness stdin")
            })?;
            stdin.write_all(prompt.as_bytes()).map_err(|e| {
                Error::provider(op.to_string(), format!("could not write prompt: {e}"))
            })?;
        }
        let output = child.wait_with_output().map_err(|e| {
            Error::provider(op.to_string(), format!("oneharness did not complete: {e}"))
        })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::provider_classified(
                op.to_string(),
                format!(
                    "oneharness exited with {}: {}",
                    output.status,
                    stderr.trim()
                ),
                ProviderErrorKind::Protocol,
            ));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_report(op, &stdout)
    }
}

/// Build the `oneharness run` args (before the trailing `--prompt-file -`) for a
/// skill turn. Pure and total, so it is unit-tested directly.
#[must_use]
fn respond_args(
    platform: &str,
    model: &str,
    instructions: &str,
    session: Option<&str>,
) -> Vec<String> {
    // `oneharness run` emits a JSON report by default; `--compact` makes it a
    // single line. There is no `--format` flag on `run`.
    let mut args = vec![
        "run".into(),
        "--harness".into(),
        platform.into(),
        "--compact".into(),
        "--events".into(),
        "--system".into(),
        instructions.into(),
        "--prompt-file".into(),
        "-".into(),
    ];
    // Omit --model when unspecified so the harness uses its own default/env model.
    if !model.is_empty() {
        args.push("--model".into());
        args.push(model.into());
    }
    // Thread the caller-owned session name only where oneharness can bind it.
    if let Some(name) = session.filter(|_| session_capable(platform)) {
        args.push("--session".into());
        args.push(name.into());
    }
    args
}

/// Build the `oneharness run` args for a judge or simulated-user turn on the
/// judge harness (no `--system`, no `--events`).
#[must_use]
fn judge_side_args(harness: &str, model: &str, session: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "run".into(),
        "--harness".into(),
        harness.into(),
        "--compact".into(),
        "--prompt-file".into(),
        "-".into(),
    ];
    if !model.is_empty() {
        args.push("--model".into());
        args.push(model.into());
    }
    if let Some(name) = session.filter(|_| session_capable(harness)) {
        args.push("--session".into());
        args.push(name.into());
    }
    args
}

// --- Report model (the fields onejudge reads from oneharness's JSON) --------

#[derive(Deserialize)]
struct OneharnessReport {
    #[serde(default)]
    results: Vec<OneharnessResult>,
}

#[derive(Debug, Deserialize, Default)]
struct OneharnessResult {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    usage: OneharnessUsage,
    #[serde(default)]
    events: Option<Vec<ToolEvent>>,
    #[serde(default)]
    failure_kind: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    stdout: String,
}

/// The usage signals onejudge reads from oneharness's report. oneharness reports
/// a few more (e.g. `usage_source`) that serde ignores; the token/cost fields —
/// including the prompt-cache reads/writes — map straight through by name.
#[derive(Debug, Deserialize, Default)]
struct OneharnessUsage {
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
    #[serde(default)]
    cache_read_tokens: Option<u64>,
    #[serde(default)]
    cache_write_tokens: Option<u64>,
    #[serde(default)]
    cost_usd: Option<f64>,
}

impl OneharnessResult {
    /// The reply text, falling back to raw stdout for the (contractually rare)
    /// case where a harness produced output but oneharness left `text` null.
    fn reply(&self) -> String {
        self.text
            .clone()
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| self.stdout.clone())
    }

    fn usage(&self) -> Option<Usage> {
        let usage = Usage {
            input_tokens: self.usage.input_tokens,
            output_tokens: self.usage.output_tokens,
            cache_read_tokens: self.usage.cache_read_tokens,
            cache_write_tokens: self.usage.cache_write_tokens,
            cost_usd: self.usage.cost_usd,
        };
        (!usage.is_empty()).then_some(usage)
    }
}

/// Parse a oneharness JSON report into its single result, turning a normalized
/// `failure_kind` into a classified [`Error::Provider`].
fn parse_report(op: &str, stdout: &str) -> Result<OneharnessResult> {
    let report: OneharnessReport = serde_json::from_str(stdout.trim()).map_err(|e| {
        Error::provider_classified(
            op.to_string(),
            format!(
                "oneharness report was not valid JSON: {e}; got: {}",
                stdout.trim()
            ),
            ProviderErrorKind::Protocol,
        )
    })?;
    let result = report.results.into_iter().next().ok_or_else(|| {
        Error::provider_classified(
            op.to_string(),
            "oneharness report carried no results",
            ProviderErrorKind::Protocol,
        )
    })?;
    if let Some(kind) = &result.failure_kind {
        let message = result
            .error
            .clone()
            .unwrap_or_else(|| format!("harness failed ({kind})"));
        return Err(Error::provider_classified(
            op.to_string(),
            message,
            ProviderErrorKind::classify(kind),
        ));
    }
    Ok(result)
}

impl Provider for OneharnessProvider {
    fn respond(
        &self,
        platform: &str,
        model: &str,
        skill: &SkillRef<'_>,
        messages: &[Message],
        session: Option<&str>,
    ) -> Result<AssistantTurn> {
        let args = respond_args(platform, model, skill.instructions, session);
        // A continued session only needs the latest user turn; a fresh/fallback
        // call inlines the whole conversation so the stateless harness sees it.
        let prompt = latest_or_inline(messages, session_capable(platform) && session.is_some());
        let result = self.run("respond", &args, &prompt)?;
        Ok(AssistantTurn {
            message: result.reply(),
            done: false,
            usage: result.usage(),
            events: result.events.clone().unwrap_or_default(),
        })
    }

    fn simulate_user(
        &self,
        model: &str,
        persona: &str,
        messages: &[Message],
        session: Option<&str>,
    ) -> Result<UserTurn> {
        let args = judge_side_args(&self.judge_harness, model, session);
        let prompt = build_user_prompt(persona, messages);
        let result = self.run("user", &args, &prompt)?;
        Ok(UserTurn {
            message: result.reply(),
            stop: false,
            usage: result.usage(),
        })
    }

    fn judge(
        &self,
        model: &str,
        query: &JudgeQuery<'_>,
        messages: &[Message],
    ) -> Result<JudgeVerdict> {
        let args = judge_side_args(&self.judge_harness, model, None);
        let prompt = build_judge_prompt(query, messages);
        let result = self.run("judge", &args, &prompt)?;
        let mut verdict = parse_verdict(query.kind, "oneharness:judge", &result.reply())?;
        verdict.usage = result.usage();
        Ok(verdict)
    }

    fn session_capable(&self, platform: &str) -> bool {
        session_capable(platform)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::JudgeKind;

    #[test]
    fn builders_configure_bin_and_judge_harness() {
        let provider = OneharnessProvider::default()
            .with_bin("my-oneharness")
            .with_judge_harness("codex");
        assert_eq!(provider.bin, "my-oneharness");
        assert_eq!(provider.judge_harness, "codex");
        // The judge/user side uses the configured harness.
        let args = judge_side_args("codex", "m", Some("s"));
        assert!(args.windows(2).any(|w| w == ["--session", "s"]));
    }

    #[test]
    fn session_capable_table_matches_oneharness() {
        for yes in ["claude-code", "codex", "opencode", "cursor", "qwen"] {
            assert!(session_capable(yes), "{yes} should be session-capable");
        }
        for no in ["goose", "crush", "copilot", "unknown"] {
            assert!(!session_capable(no), "{no} should not be session-capable");
        }
    }

    #[test]
    fn respond_args_thread_session_only_when_capable() {
        let capable = respond_args("claude-code", "sonnet", "do x", Some("run-1-skill"));
        assert!(capable
            .windows(2)
            .any(|w| w == ["--session", "run-1-skill"]));
        assert!(capable.windows(2).any(|w| w == ["--model", "sonnet"]));
        assert!(capable.iter().any(|a| a == "--events"));
        // `oneharness run` has no `--format` flag; passing it is a live-path bug.
        assert!(!capable.iter().any(|a| a == "--format"));

        // goose is not session-capable: the name is dropped even if supplied.
        let incapable = respond_args("goose", "sonnet", "do x", Some("run-1-skill"));
        assert!(!incapable.iter().any(|a| a == "--session"));
    }

    #[test]
    fn respond_args_omit_model_when_empty() {
        let args = respond_args("claude-code", "", "do x", None);
        assert!(!args.iter().any(|a| a == "--model"));
    }

    #[test]
    fn judge_side_args_have_no_system_or_events() {
        let args = judge_side_args("claude-code", "opus", None);
        assert!(!args.iter().any(|a| a == "--system"));
        assert!(!args.iter().any(|a| a == "--events"));
        assert!(args.windows(2).any(|w| w == ["--harness", "claude-code"]));
    }

    #[test]
    fn parse_report_reads_text_usage_events() {
        let json = r#"{"results":[{"status":"ok","text":"hi","usage":{"input_tokens":3,"output_tokens":1,"cache_read_tokens":9,"cache_write_tokens":4},"events":[{"kind":"tool_call","name":"bash","input":{"command":"ls"},"index":0}]}]}"#;
        let result = parse_report("respond", json).unwrap();
        assert_eq!(result.reply(), "hi");
        let usage = result.usage().unwrap();
        assert_eq!(usage.input_tokens, Some(3));
        // The prompt-cache reads/writes oneharness reports flow straight through.
        assert_eq!(usage.cache_read_tokens, Some(9));
        assert_eq!(usage.cache_write_tokens, Some(4));
        assert_eq!(result.events.unwrap().len(), 1);
    }

    #[test]
    fn parse_report_falls_back_to_stdout_when_text_null() {
        let json = r#"{"results":[{"status":"ok","text":null,"stdout":"raw reply"}]}"#;
        assert_eq!(parse_report("respond", json).unwrap().reply(), "raw reply");
    }

    #[test]
    fn parse_report_classifies_failure_kind() {
        let json = r#"{"results":[{"status":"error","failure_kind":"auth","error":"no key"}]}"#;
        let err = parse_report("respond", json).unwrap_err();
        assert_eq!(err.kind(), Some(ProviderErrorKind::Auth));
        assert!(err.to_string().contains("no key"));
    }

    #[test]
    fn parse_report_rejects_bad_json_and_empty_results() {
        assert_eq!(
            parse_report("respond", "not json").unwrap_err().kind(),
            Some(ProviderErrorKind::Protocol)
        );
        assert_eq!(
            parse_report("respond", r#"{"results":[]}"#)
                .unwrap_err()
                .kind(),
            Some(ProviderErrorKind::Protocol)
        );
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
                "opus",
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
