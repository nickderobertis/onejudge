//! One oneharness turn as data, and the two renderings of it.
//!
//! onejudge executes a turn either **in process** — `oneharness_core::io::run::run`
//! taking a [`RunRequest`] — or by **spawning** the `oneharness` CLI with an argv.
//! Both are renderings of the same turn, so both derive from one [`TurnSpec`]
//! rather than being written twice and drifting: [`request`] and [`argv`].
//!
//! That is also what makes the mapping checkable. `MAPPED_FLAGS` (in this module's
//! tests) pairs every flag [`argv`] can emit with the [`RunRequest`] field
//! [`request`] sets for it, and the drift gate renders one fully-populated spec
//! both ways and asserts the pairing holds — so a flag added to one rendering and
//! not the other fails the gate instead of silently changing what a turn means
//! depending on which seam ran it.

use std::path::PathBuf;

use oneharness_core::domain::mode::PermissionMode;
use oneharness_core::io::run::RunRequest;

/// Everything one `oneharness run` invocation needs, independent of *how* it runs.
///
/// The agent side and the judge/simulated-user side are the same shape with
/// different fields set — the agent carries `system` and asks for `events`, the
/// judge side carries `config` — so one type describes both and neither can grow a
/// flag the other's rendering has never seen.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TurnSpec {
    /// The system prompt (the skill's instructions). Agent side only.
    pub(crate) system: Option<String>,
    /// The working directory the harness runs in, and where oneharness starts its
    /// own project-config discovery.
    pub(crate) cwd: Option<String>,
    /// The oneharness config file this side's harness/model selection lives in.
    /// Judge side only — the agent side uses oneharness's discovered default.
    pub(crate) config: Option<PathBuf>,
    /// Harness ids whose provider process oneharness replaces with its own
    /// deterministic `MOCK_*`-scripted responder. Empty for an ordinary turn; see
    /// [`OneharnessProvider::with_mock_harness`](crate::OneharnessProvider::with_mock_harness).
    pub(crate) mock_harness: Vec<String>,
    /// The caller-owned session name threaded across turns.
    pub(crate) session: Option<String>,
    /// The human-meaningful name for the history session.
    pub(crate) history_name: Option<String>,
    /// Ask for normalized tool events. Agent side only.
    pub(crate) events: bool,
    /// Publish those events as they occur rather than only on the finished report.
    pub(crate) stream: bool,
    /// Ask for the out-of-band turn-control socket.
    pub(crate) control: bool,
    /// Normalized harness permission mode.
    pub(crate) mode: Option<PermissionMode>,
    /// The prompt this turn sends.
    pub(crate) prompt: String,
}

/// Render `spec` as the in-process request.
///
/// Every field oneharness does not need for a onejudge turn is left at its
/// default, so the request says exactly what the argv says and nothing more.
#[must_use]
pub(crate) fn request(spec: &TurnSpec) -> RunRequest {
    RunRequest {
        events: spec.events,
        mock_harness: spec.mock_harness.clone(),
        // Always on, exactly as `--history` is: the per-candidate record is where
        // `history_id` comes from, and it is the one signal the report has no
        // counterpart for.
        history: Some(true),
        history_name: spec.history_name.clone(),
        system: spec.system.clone(),
        cwd: spec.cwd.as_ref().map(PathBuf::from),
        config: spec.config.clone(),
        session: spec.session.clone(),
        stream: Some(spec.stream),
        control: spec.control,
        mode: spec.mode,
        // An owned value, so the `--prompt-file -` hop that exists only to keep a
        // long transcript off the OS argv disappears; oneharness moves a large
        // prompt off the *harness's* argv itself (`LARGE_INPUT_THRESHOLD`).
        prompt: vec![spec.prompt.clone()],
        ..RunRequest::default()
    }
}

/// Render `spec` as the `oneharness run` argv, prompt excluded — it rides stdin
/// via the trailing `--prompt-file -`, which is what keeps an arbitrarily long
/// transcript under the OS argument ceiling.
#[must_use]
pub(crate) fn argv(spec: &TurnSpec) -> Vec<String> {
    // `oneharness run` emits a JSON report by default; `--compact` makes it a
    // single line. There is no `--format` flag on `run`.
    let mut args = vec!["run".into(), "--compact".into()];
    if spec.events {
        args.push("--events".into());
    }
    args.push("--history".into());
    if let Some(system) = &spec.system {
        args.push("--system".into());
        args.push(system.clone());
    }
    if let Some(config) = &spec.config {
        args.push("--config".into());
        args.push(config.display().to_string());
    }
    // Repeatable: one flag per harness id whose provider process is replaced with
    // oneharness's deterministic responder.
    for id in &spec.mock_harness {
        args.push("--mock-harness".into());
        args.push(id.clone());
    }
    if let Some(cwd) = &spec.cwd {
        args.push("--cwd".into());
        args.push(cwd.clone());
    }
    args.push("--prompt-file".into());
    args.push("-".into());
    // `--stream` republishes the same normalized events as they occur and ends with
    // a terminal result line; it implies `--events`' format selection upstream.
    if spec.stream {
        args.push("--stream".into());
    }
    if let Some(name) = &spec.history_name {
        args.push("--history-name".into());
        args.push(name.clone());
    }
    if let Some(name) = &spec.session {
        args.push("--session".into());
        args.push(name.clone());
    }
    // `--control` is addressed by the `--session` name, which is why the caller
    // only ever sets it alongside one.
    if spec.control {
        args.push("--control".into());
    }
    if let Some(mode) = spec.mode {
        args.push("--mode".into());
        args.push(mode.as_str().into());
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every flag [`argv`] can emit, paired with the [`RunRequest`] field
    /// [`request`] sets for it — the one source for the mapping
    /// `docs/oneharness-library.md` renders.
    ///
    /// The predicate is what makes this a gate rather than prose: it is compiled
    /// against `RunRequest`, so a field oneharness renames or drops fails the build
    /// here, and it asserts the field is actually *set* for a spec that emits the
    /// flag — not merely that a field of that name exists.
    ///
    /// `None` is the one flag with no field, and deliberately so: `RunRequest`'s
    /// own docs exclude `--compact` because it is about how the CLI *prints* a
    /// report, not how the engine produces one, and an in-process caller is handed
    /// the report as a value.
    type FieldSet = fn(&RunRequest) -> bool;
    const MAPPED_FLAGS: &[(&str, Option<(&str, FieldSet)>)] = &[
        ("--compact", None),
        ("--events", Some(("events", |r| r.events))),
        ("--history", Some(("history", |r| r.history == Some(true)))),
        (
            "--history-name",
            Some(("history_name", |r| r.history_name.is_some())),
        ),
        (
            "--mock-harness",
            Some(("mock_harness", |r| !r.mock_harness.is_empty())),
        ),
        ("--system", Some(("system", |r| r.system.is_some()))),
        ("--cwd", Some(("cwd", |r| r.cwd.is_some()))),
        ("--config", Some(("config", |r| r.config.is_some()))),
        ("--session", Some(("session", |r| r.session.is_some()))),
        ("--stream", Some(("stream", |r| r.stream == Some(true)))),
        ("--control", Some(("control", |r| r.control))),
        (
            "--mode",
            Some(("mode", |r| r.mode == Some(PermissionMode::ReadOnly))),
        ),
        (
            "--prompt-file",
            Some(("prompt", |r| {
                !r.prompt.is_empty() && r.prompt_file.is_empty()
            })),
        ),
    ];

    /// A spec with every field set, so one rendering cannot pass by omission.
    fn populated() -> TurnSpec {
        TurnSpec {
            system: Some("do x".into()),
            cwd: Some("/work".into()),
            config: Some(PathBuf::from("oneharness.judge.toml")),
            mock_harness: vec!["claude-code".into()],
            session: Some("sess".into()),
            history_name: Some("hist".into()),
            events: true,
            stream: true,
            control: true,
            mode: Some(PermissionMode::ReadOnly),
            prompt: "the whole transcript".into(),
        }
    }

    #[test]
    fn every_argv_flag_sets_the_run_request_field_the_mapping_pairs_it_with() {
        let spec = populated();
        let request = request(&spec);
        let argv = argv(&spec);

        // Neither rendering may grow a flag the mapping has never seen.
        for flag in argv.iter().filter(|a| a.starts_with("--")) {
            assert!(
                MAPPED_FLAGS.iter().any(|(name, _)| name == flag),
                "`{flag}` has no entry in the RunRequest mapping"
            );
        }
        // The load-bearing half: the flag the CLI seam emits and the field the
        // library seam sets describe the same turn.
        for (flag, field) in MAPPED_FLAGS {
            assert!(
                argv.iter().any(|a| a == flag),
                "`{flag}` is in the mapping but `argv` no longer emits it"
            );
            if let Some((name, is_set)) = field {
                assert!(
                    is_set(&request),
                    "`{flag}` is emitted on the argv but `RunRequest::{name}` is unset"
                );
            }
        }
    }

    #[test]
    fn an_empty_spec_asks_for_nothing_it_was_not_given() {
        let request = request(&TurnSpec::default());
        let argv = argv(&TurnSpec::default());
        for (flag, field) in MAPPED_FLAGS {
            // `--compact`, `--history` and `--prompt-file` ride every turn; the
            // rest are opt-in on both renderings or on neither.
            if matches!(*flag, "--compact" | "--history" | "--prompt-file") {
                continue;
            }
            assert!(
                !argv.iter().any(|a| a == flag),
                "an empty spec emitted `{flag}`"
            );
            if let Some((name, is_set)) = field {
                assert!(!is_set(&request), "an empty spec set `RunRequest::{name}`");
            }
        }
    }

    #[test]
    fn the_documented_mapping_matches_the_one_source() {
        // `docs/oneharness-library.md` renders the same mapping for a reader, so
        // both columns are reconciled against the const rather than left as a
        // second copy: the flag *and* the field it pairs with, matched as one row,
        // so neither `--history` can be satisfied by `--history-name` nor a field
        // name drift between the two.
        let doc = include_str!("../../../../docs/oneharness-library.md");
        for (flag, field) in MAPPED_FLAGS {
            let row = match field {
                Some((name, _)) => format!("| `{flag}` | `{name}"),
                // The fieldless one has to say so where the mapping is read, or it
                // looks like an omission rather than a decision.
                None => format!("| `{flag}` | **none"),
            };
            assert!(
                doc.contains(&row),
                "docs/oneharness-library.md's mapping table has no row `{row}…`"
            );
        }
    }

    #[test]
    fn the_prompt_is_an_owned_value_on_the_request_and_stdin_on_the_argv() {
        let spec = populated();
        // In process there is no argv ceiling to dodge, so the transcript is a
        // value rather than a file handle.
        assert_eq!(request(&spec).prompt, vec!["the whole transcript"]);
        assert!(request(&spec).prompt_file.is_empty());
        // Spawned, it still rides stdin.
        let argv = argv(&spec);
        assert!(argv.windows(2).any(|w| w == ["--prompt-file", "-"]));
        assert!(!argv.iter().any(|a| a == "the whole transcript"));
    }
}
