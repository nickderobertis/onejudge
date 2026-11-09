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

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

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

/// How many times a refused publish is tried before it is an error, and the wait
/// between tries. Only Windows refuses, and only for as long as another caller
/// holds the destination open or is itself replacing it.
const PUBLISH_ATTEMPTS: u32 = 20;
const PUBLISH_RETRY: Duration = Duration::from_millis(10);

/// Numbers this process's staging files, so no two calls share one.
static STAGING_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Where onejudge keeps [`JUDGE_DEFAULTS`] on disk, writing it the first time.
///
/// The directory is the invoking user's own — the runtime directory, else the
/// cache directory, else the temp directory — because a file every evaluator
/// call trusts for its mode must not sit where another user could replace it.
/// Each call that has to write stages into a sibling of its own and renames it
/// into place, so concurrent callers — the judges of one panel, first — never
/// touch each other's staging and a reader never sees the file half-written. An
/// existing file is rewritten only if its content is not exactly the defaults.
pub(crate) fn defaults_file() -> Result<PathBuf> {
    write_defaults(DEFAULTS_FILE, JUDGE_DEFAULTS)
}

fn write_defaults(file: &str, content: &str) -> Result<PathBuf> {
    publish_defaults(&defaults_dir().join("onejudge"), file, content)
}

fn publish_defaults(dir: &Path, file: &str, content: &str) -> Result<PathBuf> {
    let path = dir.join(file);
    if is_published(&path, content) {
        return Ok(path);
    }
    let unwritable = |e: io::Error| {
        Error::provider_classified(
            "judge-defaults",
            format!(
                "could not write onejudge's judge-side defaults to `{}`: {e}",
                path.display()
            ),
            ProviderErrorKind::Protocol,
        )
    };
    std::fs::create_dir_all(dir).map_err(unwritable)?;
    let (staging, mut staged) = open_staging(dir, file).map_err(unwritable)?;
    let written = staged.write_all(content.as_bytes());
    // Closed before the rename: Windows will not move a file that is open.
    drop(staged);
    let outcome = written.and_then(|()| {
        install(&staging, &path, content, |from, to| {
            std::fs::rename(from, to)
        })
    });
    // Already gone once it was renamed, and the name is this call's alone.
    let _ = std::fs::remove_file(&staging);
    outcome.map_err(unwritable)?;
    Ok(path)
}

/// Whether `path` already holds exactly `content`.
fn is_published(path: &Path, content: &str) -> bool {
    std::fs::read_to_string(path).is_ok_and(|text| text == content)
}

/// A new staging file beside the defaults, named for this call. Created
/// exclusively, so a file a crashed process left under a reused pid is stepped
/// over rather than written through.
fn open_staging(dir: &Path, file: &str) -> io::Result<(PathBuf, File)> {
    loop {
        let sequence = STAGING_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let staging = dir.join(format!("{file}.{}.{sequence}.partial", std::process::id()));
        match File::options().write(true).create_new(true).open(&staging) {
            Ok(staged) => return Ok((staging, staged)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
}

/// Move the complete `staging` file to `path`, leaving it unmoved if a concurrent
/// caller published the same content first. A refused rename is only a failure
/// once it has outlasted the retries with the content still not there.
fn install(
    staging: &Path,
    path: &Path,
    content: &str,
    mut rename: impl FnMut(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    if is_published(path, content) {
        return Ok(());
    }
    let mut attempts = PUBLISH_ATTEMPTS;
    loop {
        let refused = match rename(staging, path) {
            Ok(()) => return Ok(()),
            Err(e) => e,
        };
        if is_published(path, content) {
            return Ok(());
        }
        attempts -= 1;
        if attempts == 0 {
            return Err(refused);
        }
        std::thread::sleep(PUBLISH_RETRY);
    }
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

/// History is enabled by default, beneath each side's configured preference.
pub(crate) fn history_configs(configs: &[PathBuf], worktree: &Path) -> Result<Vec<PathBuf>> {
    let mut layered = vec![write_defaults(
        "history-defaults-v1.toml",
        "history = true\n",
    )?];
    if configs.is_empty() {
        layered.extend(discovered(worktree)?);
    } else {
        layered.extend_from_slice(configs);
    }
    Ok(layered)
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

    /// How long a race may take before the test fails instead of waiting on it.
    const RACE_DEADLINE: Duration = Duration::from_secs(30);

    /// Run `calls` publishes of `content` into `dir` released together, returning
    /// each outcome, and fail the test if they do not all finish by the deadline.
    /// Detached threads, because a scoped one that never returned would hang the
    /// scope the failure has to unwind through.
    fn race(dir: &Path, calls: usize, content: &str) -> Vec<Result<PathBuf>> {
        let start = std::sync::Arc::new(std::sync::Barrier::new(calls));
        let (finished, outcomes) = std::sync::mpsc::channel();
        for _ in 0..calls {
            let (start, finished) = (start.clone(), finished.clone());
            let (dir, content) = (dir.to_path_buf(), content.to_string());
            std::thread::spawn(move || {
                start.wait();
                let _ = finished.send(publish_defaults(&dir, DEFAULTS_FILE, &content));
            });
        }
        let deadline = std::time::Instant::now() + RACE_DEADLINE;
        (0..calls)
            .map(|done| {
                let left = deadline.saturating_duration_since(std::time::Instant::now());
                outcomes
                    .recv_timeout(left)
                    .unwrap_or_else(|e| panic!("{done} of {calls} publishes finished: {e}"))
            })
            .collect()
    }

    fn entries(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect()
    }

    #[test]
    fn concurrent_initializations_all_succeed_and_publish_what_one_alone_writes() {
        let root = scratch("race");
        let alone = publish_defaults(&root.join("alone"), DEFAULTS_FILE, JUDGE_DEFAULTS).unwrap();
        let expected = std::fs::read(&alone).unwrap();
        for round in 0..50 {
            let dir = root.join(format!("round-{round}"));
            for outcome in race(&dir, 32, JUDGE_DEFAULTS) {
                assert_eq!(outcome.unwrap(), dir.join(DEFAULTS_FILE));
            }
            assert_eq!(std::fs::read(dir.join(DEFAULTS_FILE)).unwrap(), expected);
            assert_eq!(entries(&dir), [DEFAULTS_FILE], "round {round}");
        }
    }

    #[test]
    fn a_reader_during_concurrent_publishes_sees_no_file_or_the_complete_defaults() {
        // The real defaults, and a body large enough that a write that was not
        // atomic would be caught part-way.
        let large = "# padding\n".repeat(100_000) + JUDGE_DEFAULTS;
        let root = scratch("reader");
        for (name, content) in [("small", JUDGE_DEFAULTS), ("large", large.as_str())] {
            for round in 0..20 {
                let dir = root.join(format!("{name}-{round}"));
                let path = dir.join(DEFAULTS_FILE);
                let done = std::sync::atomic::AtomicBool::new(false);
                let deadline = std::time::Instant::now() + RACE_DEADLINE;
                std::thread::scope(|scope| {
                    let reader = scope.spawn(|| {
                        let mut complete = 0;
                        while !done.load(Ordering::Acquire) && std::time::Instant::now() < deadline
                        {
                            match std::fs::read(&path) {
                                Ok(bytes) => {
                                    assert!(
                                        bytes == content.as_bytes(),
                                        "read {} of {} bytes",
                                        bytes.len(),
                                        content.len()
                                    );
                                    complete += 1;
                                }
                                // Windows refuses an open while the file is replaced.
                                Err(e) => assert!(
                                    e.kind() == io::ErrorKind::NotFound
                                        || (cfg!(windows)
                                            && e.kind() == io::ErrorKind::PermissionDenied),
                                    "{e}"
                                ),
                            }
                        }
                        complete
                    });
                    // The reader is released before any outcome is judged, so a
                    // failed publish fails the test rather than leaving it spinning.
                    let outcomes = race(&dir, 8, content);
                    done.store(true, Ordering::Release);
                    reader.join().unwrap();
                    for outcome in outcomes {
                        outcome.unwrap();
                    }
                });
                assert_eq!(std::fs::read(&path).unwrap(), content.as_bytes());
            }
        }
    }

    #[test]
    fn equivalent_defaults_already_published_are_not_rewritten() {
        let dir = scratch("equivalent");
        let path = dir.join(DEFAULTS_FILE);
        std::fs::write(&path, JUDGE_DEFAULTS).unwrap();
        let old = std::time::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert_eq!(
            publish_defaults(&dir, DEFAULTS_FILE, JUDGE_DEFAULTS).unwrap(),
            path
        );
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), old);
        assert_eq!(entries(&dir), [DEFAULTS_FILE]);
    }

    #[test]
    fn different_content_under_the_name_is_replaced() {
        let dir = scratch("different");
        let path = dir.join(DEFAULTS_FILE);
        std::fs::write(&path, "mode = \"auto\"\n").unwrap();
        publish_defaults(&dir, DEFAULTS_FILE, JUDGE_DEFAULTS).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), JUDGE_DEFAULTS);
        assert_eq!(entries(&dir), [DEFAULTS_FILE]);
    }

    #[test]
    fn a_staging_name_left_by_a_crashed_process_is_stepped_over() {
        let dir = scratch("stale");
        let next = STAGING_SEQUENCE.load(Ordering::Relaxed);
        let stale: Vec<PathBuf> = (next..next + 32)
            .map(|n| {
                dir.join(format!(
                    "{DEFAULTS_FILE}.{}.{n}.partial",
                    std::process::id()
                ))
            })
            .collect();
        for path in &stale {
            std::fs::write(path, "left behind").unwrap();
        }
        publish_defaults(&dir, DEFAULTS_FILE, JUDGE_DEFAULTS).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join(DEFAULTS_FILE)).unwrap(),
            JUDGE_DEFAULTS
        );
        for path in &stale {
            assert_eq!(std::fs::read_to_string(path).unwrap(), "left behind");
        }
    }

    /// The refusal Windows gives a rename onto a file another caller has open;
    /// no other platform produces it, so these tests supply it.
    fn refused() -> io::Error {
        io::Error::from(io::ErrorKind::PermissionDenied)
    }

    #[test]
    fn a_refused_publish_is_success_once_a_concurrent_caller_has_published() {
        let dir = scratch("refused-published");
        let (staging, path) = (dir.join("staged"), dir.join(DEFAULTS_FILE));
        std::fs::write(&staging, JUDGE_DEFAULTS).unwrap();
        install(&staging, &path, JUDGE_DEFAULTS, |_, to| {
            std::fs::write(to, JUDGE_DEFAULTS)?;
            Err(refused())
        })
        .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), JUDGE_DEFAULTS);
    }

    #[test]
    fn a_transiently_refused_publish_is_tried_again() {
        let dir = scratch("refused-transient");
        let (staging, path) = (dir.join("staged"), dir.join(DEFAULTS_FILE));
        std::fs::write(&staging, JUDGE_DEFAULTS).unwrap();
        let mut tries = 0;
        install(&staging, &path, JUDGE_DEFAULTS, |from, to| {
            tries += 1;
            if tries < 3 {
                Err(refused())
            } else {
                std::fs::rename(from, to)
            }
        })
        .unwrap();
        assert_eq!(tries, 3);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), JUDGE_DEFAULTS);
    }

    #[test]
    fn a_publish_that_can_never_land_is_a_classified_error_and_strands_no_staging() {
        // A directory squatting on the name: the rename cannot replace it and
        // nothing equivalent ever appears.
        let dir = scratch("squatted");
        std::fs::create_dir_all(dir.join(DEFAULTS_FILE).join("occupied")).unwrap();
        let err = publish_defaults(&dir, DEFAULTS_FILE, JUDGE_DEFAULTS).unwrap_err();
        assert!(err.to_string().contains("could not write"), "{err}");
        assert_eq!(err.kind(), Some(ProviderErrorKind::Protocol));
        assert_eq!(entries(&dir), [DEFAULTS_FILE]);
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
