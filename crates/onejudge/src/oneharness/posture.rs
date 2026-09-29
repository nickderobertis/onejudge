//! The posture an **evaluator** judge-side call runs under — a default the
//! judge's own configuration can override, never an override that silently beats
//! it.
//!
//! An evaluator call is a judge-side call made with the worker's tree in hand: a
//! supervisor decision, a verdict or an assessment. onejudge wants those
//! read-only unless told otherwise, and it used to say so with `--mode read-only`
//! (`RunRequest::mode` in process). oneharness takes a request's mode before any
//! config's, so a `mode` in the judge's own `judge_config` was silently ignored
//! and no judge could ever be granted a shell.
//!
//! Now the read-only default is a **config file**, [`JUDGE_DEFAULTS`], and it is
//! layered *first*: the call passes `[<defaults>, <judge_config>]` (or, with no
//! judge config, the defaults followed by the user and project files oneharness's
//! own discovery resolves), with no mode on the request. oneharness folds each
//! later file over the ones before it and its `ONEHARNESS_*` overrides over all
//! of them, so the judge's config, the environment or a discovered file can each
//! choose another mode through the normal precedence. With no mode configured
//! anywhere the effective mode is still read-only — the file's.
//!
//! [`resolve`] reads that same list back through the linked core's own loader
//! (`load_layers` + `explain`, environment included), so the mode onejudge
//! records, and frames the judge's prompt by, is the one `oneharness config`
//! attributes to the same file. A spawned `oneharness` of another version could
//! in principle resolve differently; the list it is handed is the same one, which
//! is what keeps the two from drifting in practice.

use std::path::{Path, PathBuf};

use oneharness_core::domain::config::{explain, DEFAULT_SOURCE};
use oneharness_core::domain::mode::PermissionMode;
use oneharness_core::io::config::load_layers;

use crate::error::{Error, ProviderErrorKind, Result};
use crate::telemetry::JudgePosture;

/// onejudge's judge-side defaults: a oneharness config holding the read-only
/// mode and nothing else, layered under every evaluator call's own config.
pub(crate) const JUDGE_DEFAULTS: &str = "mode = \"read-only\"\n";

/// The defaults file's name, versioned by its content so a future default is a
/// new file rather than a rewrite of one another process may be reading.
const DEFAULTS_FILE: &str = "judge-defaults-v1.toml";

/// Where onejudge keeps [`JUDGE_DEFAULTS`] on disk, writing it the first time.
///
/// The directory is the invoking user's own — the runtime directory, else the
/// cache directory, else the temp directory — because a file every evaluator
/// call trusts for its mode must not sit where another user could replace it.
/// The file is written to a sibling and renamed into place, so a concurrent
/// reader never sees it half-written, and an existing file is rewritten only if
/// its content is not exactly the defaults.
pub(crate) fn defaults_file() -> Result<PathBuf> {
    let dir = defaults_dir().join("onejudge");
    let path = dir.join(DEFAULTS_FILE);
    if std::fs::read_to_string(&path).is_ok_and(|text| text == JUDGE_DEFAULTS) {
        return Ok(path);
    }
    let unwritable = |e: std::io::Error| {
        Error::provider_classified(
            "judge-defaults",
            format!(
                "could not write onejudge's judge-side defaults to `{}`: {e}",
                path.display()
            ),
            ProviderErrorKind::Protocol,
        )
    };
    std::fs::create_dir_all(&dir).map_err(unwritable)?;
    let staging = dir.join(format!("{DEFAULTS_FILE}.{}.partial", std::process::id()));
    std::fs::write(&staging, JUDGE_DEFAULTS).map_err(unwritable)?;
    std::fs::rename(&staging, &path).map_err(unwritable)?;
    Ok(path)
}

/// The invoking user's own directory for the defaults file.
fn defaults_dir() -> PathBuf {
    let var = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
    if cfg!(windows) {
        return var("LOCALAPPDATA").map_or_else(std::env::temp_dir, PathBuf::from);
    }
    var("XDG_RUNTIME_DIR")
        .or_else(|| var("XDG_CACHE_HOME"))
        .map(PathBuf::from)
        .or_else(|| var("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(std::env::temp_dir)
}

/// The config list an evaluator call passes, in layering order: onejudge's
/// defaults first, then the judge's own config — or, with none, the user and
/// project files oneharness's discovery resolves from `worktree`, so a
/// discovered config still shapes the judge exactly as it did without the
/// defaults in front of it.
pub(crate) fn evaluator_configs(
    judge_config: Option<&Path>,
    worktree: &Path,
) -> Result<Vec<PathBuf>> {
    let mut configs = vec![defaults_file()?];
    match judge_config {
        Some(config) => configs.push(config.to_path_buf()),
        None => configs.extend(discovered(worktree)?),
    }
    Ok(configs)
}

/// The files oneharness's own discovery loads from `worktree`: the user file,
/// then the project file — each a top-level file, not an `extends` parent, which
/// oneharness follows again from the file that names it.
fn discovered(worktree: &Path) -> Result<Vec<PathBuf>> {
    let layers = load_layers(&[], false, worktree).map_err(unreadable)?;
    let mut files = Vec::new();
    for (index, (path, _)) in layers.iter().enumerate() {
        if path == oneharness_core::domain::config::ENV_SOURCE {
            continue;
        }
        // A file's `extends` chain precedes it, deepest first, so a layer is a
        // parent exactly when the next layer names it.
        let parent = layers.get(index + 1).is_some_and(|(child, config)| {
            config.extends.as_ref().is_some_and(|extends| {
                Path::new(child)
                    .parent()
                    .unwrap_or_else(|| Path::new(""))
                    .join(extends.as_str())
                    .display()
                    .to_string()
                    == *path
            })
        });
        if !parent {
            // Absolute, so the list — and the source a posture names — says which
            // file it is wherever it is read; oneharness resolves it to the same
            // file it discovered.
            files.push(std::path::absolute(path).unwrap_or_else(|_| PathBuf::from(path)));
        }
    }
    Ok(files)
}

/// The posture `configs` resolve to from `worktree`: the effective mode, the
/// layer that set it, and every layer read — through the linked core's loader,
/// `ONEHARNESS_*` overrides included, exactly as `oneharness config` reports it.
pub(crate) fn resolve(configs: &[PathBuf], worktree: &Path) -> Result<JudgePosture> {
    let layers = load_layers(configs, false, worktree).map_err(unreadable)?;
    let report = explain(&layers);
    // oneharness's own precedence: a `mode` from any layer, else the legacy
    // `bypass` boolean, else the built-in `default`.
    let (mode, source) = match (report.mode.value, report.mode.source) {
        (Some(mode), Some(source)) => (mode, source),
        _ => match (report.bypass.value, report.bypass.source) {
            (Some(bypass), Some(source)) if source != DEFAULT_SOURCE => {
                (PermissionMode::from_bypass(bypass), source)
            }
            _ => (PermissionMode::Default, DEFAULT_SOURCE.to_string()),
        },
    };
    Ok(JudgePosture {
        mode: mode.as_str().to_string(),
        source,
        config_files: report.config_files,
    })
}

/// A config oneharness would refuse is refused here first, naming what it said.
fn unreadable(e: oneharness_core::errors::OneharnessError) -> Error {
    Error::provider_classified(
        "judge-config",
        format!("the judge's oneharness configuration could not be read: {e}"),
        ProviderErrorKind::Protocol,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("onejudge-posture-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_defaults_file_holds_the_read_only_mode_and_nothing_else() {
        let path = defaults_file().unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "mode = \"read-only\"\n"
        );
        // Written once, then reused as it stands.
        assert_eq!(defaults_file().unwrap(), path);
    }

    #[test]
    fn a_judge_config_that_sets_no_mode_leaves_the_defaults_in_force() {
        let dir = scratch("no-mode");
        let judge = dir.join("judge.toml");
        std::fs::write(&judge, "timeout = 30\n").unwrap();
        let configs = evaluator_configs(Some(&judge), &dir).unwrap();
        assert_eq!(configs, [defaults_file().unwrap(), judge.clone()]);
        let posture = resolve(&configs, &dir).unwrap();
        assert_eq!(posture.mode, "read-only");
        assert_eq!(
            posture.source,
            defaults_file().unwrap().display().to_string()
        );
        assert!(!posture.is_writable());
    }

    #[test]
    fn a_judge_config_that_sets_a_mode_overrides_the_defaults() {
        let dir = scratch("auto");
        let judge = dir.join("judge.toml");
        std::fs::write(&judge, "mode = \"auto\"\n").unwrap();
        let posture = resolve(&evaluator_configs(Some(&judge), &dir).unwrap(), &dir).unwrap();
        assert_eq!(posture.mode, "auto");
        assert_eq!(posture.source, judge.display().to_string());
        assert!(posture.is_writable());
    }

    #[test]
    fn a_legacy_bypass_under_the_defaults_mode_does_not_win() {
        // oneharness takes a `mode` from any layer before a `bypass` from any
        // layer, so the defaults' mode stands beside a legacy boolean — the same
        // answer `run` gives.
        let dir = scratch("bypass");
        let judge = dir.join("judge.toml");
        std::fs::write(&judge, "bypass = true\n").unwrap();
        let posture = resolve(&evaluator_configs(Some(&judge), &dir).unwrap(), &dir).unwrap();
        assert_eq!(posture.mode, "read-only");
        // With no defaults in front of it, the boolean decides, attributed to its
        // file.
        let alone = resolve(std::slice::from_ref(&judge), &dir).unwrap();
        assert_eq!(alone.mode, "bypass");
        assert_eq!(alone.source, judge.display().to_string());
    }

    #[test]
    fn nothing_setting_a_mode_is_the_built_in_default() {
        let dir = scratch("built-in");
        let judge = dir.join("judge.toml");
        std::fs::write(&judge, "timeout = 30\n").unwrap();
        let posture = resolve(std::slice::from_ref(&judge), &dir).unwrap();
        assert_eq!(
            (posture.mode.as_str(), posture.source.as_str()),
            ("default", "default")
        );
    }

    #[test]
    fn an_unreadable_judge_config_is_a_classified_error_naming_it() {
        let dir = scratch("missing");
        let missing = dir.join("absent.toml");
        let err = resolve(&evaluator_configs(Some(&missing), &dir).unwrap(), &dir).unwrap_err();
        assert!(err.to_string().contains("absent.toml"), "{err}");
        assert_eq!(err.kind(), Some(ProviderErrorKind::Protocol));
    }

    #[test]
    fn with_no_judge_config_the_discovered_project_file_follows_the_defaults() {
        // The project file's `extends` parent is not listed on its own: oneharness
        // follows it again from the file that names it.
        let dir = scratch("discovered");
        std::fs::write(dir.join("base.toml"), "timeout = 30\n").unwrap();
        std::fs::write(
            dir.join("oneharness.toml"),
            "extends = \"base.toml\"\nmode = \"auto\"\n",
        )
        .unwrap();
        let configs = evaluator_configs(None, &dir).unwrap();
        assert_eq!(configs[0], defaults_file().unwrap());
        assert_eq!(configs.last().unwrap(), &dir.join("oneharness.toml"));
        assert!(
            !configs.iter().any(|c| c.ends_with("base.toml")),
            "{configs:?}"
        );
        let posture = resolve(&configs, &dir).unwrap();
        assert_eq!(posture.mode, "auto");
        assert_eq!(
            posture.source,
            dir.join("oneharness.toml").display().to_string()
        );
    }
}
