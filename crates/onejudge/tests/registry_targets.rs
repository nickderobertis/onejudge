//! Drift gate for what this repository *releases*, and the contract of the probe
//! that answers what a registry currently serves for it.
//!
//! A consumer that sequences work across repositories holds a dependent task
//! until the artifact it depends on is released; a repository that declares no
//! release target releases nothing as far as that consumer is concerned, so the
//! hold quietly stops happening. `registry-targets.txt` is this repository's
//! declaration and `scripts/release-probe.sh` answers it.
//!
//! The declaration is the thing that goes stale silently, so this suite never
//! trusts it: it derives the published set from the *real* release configuration
//! — the release workflows and the manifests they build — and fails in both
//! directions. A new artifact in the workflows fails here instead of passing
//! unnoticed, and a declared target the workflows no longer publish fails too.
//!
//! The probe's three answers are proven by driving the real script. The two that
//! need a public registry are `#[ignore]`-d, like every other network-touching
//! test here (`live.rs`, `docs/live-tier.md`): the gate stays offline and
//! deterministic. Run them with `just test-release-probe`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

/// The workspace root: the checkout the release configuration lives in.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("onejudge package should be nested under the workspace root")
        .to_path_buf()
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|err| panic!("reading {}: {err}", path.display()))
}

/// The identifiers declared in `registry-targets.txt`, in file order. Blank lines
/// and `#` comments are documentation; everything else is a target.
fn declared_targets() -> Vec<String> {
    read(&repo_root().join("registry-targets.txt"))
        .lines()
        .map(|line| line.split('#').next().unwrap_or("").trim().to_string())
        .filter(|line| !line.is_empty())
        .collect()
}

/// The scalar on the right of `=`, unquoted, with any trailing comment dropped.
fn scalar(raw: &str) -> String {
    let raw = raw.trim();
    match raw.strip_prefix('"') {
        Some(rest) => rest.split('"').next().unwrap_or("").to_string(),
        None => raw.split('#').next().unwrap_or("").trim().to_string(),
    }
}

/// `key`'s value inside `[table]` of a TOML document. Enough of TOML for the two
/// keys this reads (`name`, `publish`), and no dependency for a test to carry.
fn toml_value(text: &str, table: &str, key: &str) -> Option<String> {
    let header = format!("[{table}]");
    let mut inside = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            inside = line == header;
            continue;
        }
        if !inside {
            continue;
        }
        if let Some(rest) = line.strip_prefix(key) {
            if let Some(value) = rest.trim_start().strip_prefix('=') {
                return Some(scalar(value));
            }
        }
    }
    None
}

/// Every quoted string of a (possibly multi-line) TOML array value.
fn toml_array(text: &str, table: &str, key: &str) -> Vec<String> {
    let header = format!("[{table}]");
    let mut inside = false;
    let mut collecting = false;
    let mut items = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if !collecting {
            if trimmed.starts_with('#') {
                continue;
            }
            if trimmed.starts_with('[') {
                inside = trimmed == header;
                continue;
            }
            if !inside {
                continue;
            }
            match trimmed.strip_prefix(key).map(str::trim_start) {
                Some(rest) if rest.starts_with('=') => collecting = true,
                _ => continue,
            }
        }
        let mut rest = trimmed;
        while let Some(open) = rest.find('"') {
            let after = &rest[open + 1..];
            let Some(close) = after.find('"') else { break };
            items.push(after[..close].to_string());
            rest = &after[close + 1..];
        }
        if trimmed.contains(']') {
            break;
        }
    }
    items
}

/// Files under `dir` named `file_name`, skipping build output and vendored trees.
fn find(dir: &Path, file_name: &str, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if matches!(name.as_ref(), "target" | ".git" | "node_modules" | ".venv") {
                continue;
            }
            find(&path, file_name, found);
        } else if name == file_name {
            found.push(path);
        }
    }
}

/// The `.github/workflows/*.yml` files, read.
fn workflows(root: &Path) -> Vec<(PathBuf, String)> {
    let dir = root.join(".github/workflows");
    let mut entries: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|err| panic!("reading {}: {err}", dir.display()))
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext == "yml" || ext == "yaml")
        })
        .collect();
    entries.sort();
    assert!(
        !entries.is_empty(),
        "no workflows in {} — the release configuration this gate reads is gone",
        dir.display()
    );
    entries
        .into_iter()
        .map(|path| {
            let text = read(&path);
            (path, text)
        })
        .collect()
}

/// Every Python distribution this repository *builds*: the `[project] name` of
/// each `pyproject.toml` in the tree, mapped to the manifest that declares it.
fn python_distributions(root: &Path) -> BTreeMap<String, PathBuf> {
    let mut manifests = Vec::new();
    find(root, "pyproject.toml", &mut manifests);
    let mut distributions = BTreeMap::new();
    for manifest in manifests {
        if let Some(name) = toml_value(&read(&manifest), "project", "name") {
            distributions.insert(name, manifest);
        }
    }
    distributions
}

/// The registry-qualified names this repository publishes, read out of the real
/// release configuration rather than an inventory transcribed into this test.
///
/// * crates.io — release-plz's `command: release` step publishes the workspace's
///   packages, so every member manifest that does not opt out is a crate target.
/// * PyPI — each `pypa/gh-action-pypi-publish` step publishes exactly one
///   distribution and names it (`- name: Publish <dist>`); that name has to be a
///   distribution the tree actually builds.
///
/// npm is not a registry this repository publishes to; that is its own assertion
/// below rather than a branch here, so it stays a fact the workflows prove.
fn published_targets() -> BTreeSet<String> {
    let root = repo_root();
    let mut published = BTreeSet::new();
    let workflows = workflows(&root);

    let release_plz = workflows
        .iter()
        .find(|(_, text)| text.contains("release-plz/action"))
        .map(|(_, text)| text)
        .expect("no release-plz workflow — the crates.io publisher this gate reads is gone");
    if release_plz
        .lines()
        .any(|line| line.trim() == "command: release")
    {
        let workspace = read(&root.join("Cargo.toml"));
        let members = toml_array(&workspace, "workspace", "members");
        assert!(
            !members.is_empty(),
            "no `[workspace] members` in Cargo.toml"
        );
        for member in members {
            let manifest = root.join(&member).join("Cargo.toml");
            let text = read(&manifest);
            if toml_value(&text, "package", "publish").as_deref() == Some("false") {
                continue;
            }
            let name = toml_value(&text, "package", "name")
                .unwrap_or_else(|| panic!("no `[package] name` in {}", manifest.display()));
            published.insert(format!("crate:{name}"));
        }
    }

    let distributions = python_distributions(&root);
    for (path, text) in &workflows {
        let lines: Vec<&str> = text.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            if line.trim_start().starts_with('#') || !line.contains("pypa/gh-action-pypi-publish") {
                continue;
            }
            let step = lines[..index]
                .iter()
                .rev()
                .find_map(|line| line.trim().strip_prefix("- name: "))
                .unwrap_or_else(|| {
                    panic!(
                        "{}: a PyPI publish step with no `- name:` above it — this gate reads the \
                         published distribution out of that name",
                        path.display()
                    )
                });
            let distribution = step.trim().strip_prefix("Publish ").unwrap_or_else(|| {
                panic!(
                    "{}: PyPI publish step named {step:?} — name it `Publish <distribution>` so \
                     what it publishes is readable from the workflow",
                    path.display()
                )
            });
            assert!(
                distributions.contains_key(distribution),
                "{}: publishes `{distribution}` to PyPI, but no pyproject.toml in the tree \
                 declares that distribution (found: {:?})",
                path.display(),
                distributions.keys().collect::<Vec<_>>()
            );
            published.insert(format!("pypi:{distribution}"));
        }
    }

    published
}

/// The whole point: the declaration and the release configuration agree, in both
/// directions. A published name no target covers is a consumer that never holds;
/// a declared target nothing publishes is a consumer that holds forever.
#[test]
fn the_declaration_matches_the_real_release_configuration() {
    let declared: BTreeSet<String> = declared_targets().into_iter().collect();
    let published = published_targets();

    let undeclared: Vec<&String> = published.difference(&declared).collect();
    assert!(
        undeclared.is_empty(),
        "the release configuration publishes {undeclared:?}, which registry-targets.txt does not \
         declare — declare it, or account for it there as a per-platform build of a target that \
         is already declared"
    );

    let unpublished: Vec<&String> = declared.difference(&published).collect();
    assert!(
        unpublished.is_empty(),
        "registry-targets.txt declares {unpublished:?}, which no release workflow publishes — a \
         consumer would hold on it forever. Published: {published:?}"
    );
}

/// This repository publishes nothing to npm, so no target names that registry.
/// The day a workflow does, the published set grows a kind this gate does not
/// derive — so it fails here, where the fix is to derive it and declare it,
/// rather than shipping an artifact no consumer can wait on.
#[test]
fn nothing_here_publishes_to_npm() {
    for (path, text) in workflows(&repo_root()) {
        let publishes = text
            .lines()
            .any(|line| !line.trim_start().starts_with('#') && line.contains("npm publish"));
        assert!(
            !publishes,
            "{} runs `npm publish`: teach published_targets() to derive npm names and declare \
             them in registry-targets.txt",
            path.display()
        );
    }
}

/// Every declared identifier is registry-qualified and unique. `onejudge` alone
/// names both the crate and the SDK distribution, so an unqualified identifier
/// names two different artifacts. That the probe can actually *answer* for each
/// one is derived from the probe itself, in `probe::the_probe_recognises_every_
/// declared_target`, rather than restated as an allow-list here.
#[test]
fn every_declared_identifier_is_registry_qualified() {
    let declared = declared_targets();
    assert!(
        !declared.is_empty(),
        "registry-targets.txt declares nothing"
    );

    let mut seen = BTreeSet::new();
    for target in &declared {
        let (registry, name) = target
            .split_once(':')
            .unwrap_or_else(|| panic!("`{target}` is not a `<registry>:<name>` identifier"));
        assert!(
            !registry.is_empty(),
            "`{target}` names no registry, and `onejudge` alone names two different artifacts"
        );
        assert!(
            !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')),
            "`{target}` does not name a registry artifact"
        );
        assert!(seen.insert(target), "`{target}` is declared twice");
    }
}

/// The probe's contract, driven as the real script over a real subprocess.
///
/// The registry is faked the way the rest of this suite fakes the model: with a
/// **real** binary — a `curl` stand-in first on `PATH`, which is the probe's only
/// view of a registry — so "the registry failed" and "the registry serves nothing"
/// are deterministic offline journeys rather than untestable branches. The two
/// answers that must come from the true public registries are `#[ignore]`-d.
///
/// Unix only: the contract is a direct spawn with no shell interposed, and
/// Windows cannot execute a `#!` script without one.
#[cfg(unix)]
mod probe {
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::process::{Command, Output};
    use std::time::{Duration, Instant};
    use std::{env, fs};

    use super::{declared_targets, repo_root};

    /// The contract's bound: an answer well inside sixty seconds.
    const BOUND: Duration = Duration::from_secs(60);

    /// Spawn the probe exactly as a consumer does: directly, from the repository
    /// root, with an environment carrying only PATH and HOME — no credential, and
    /// no variable the caller happened to be holding.
    fn probe_on_path(path: &str, args: &[&str]) -> (Output, Duration) {
        let root = repo_root();
        let mut command = Command::new(root.join("scripts/release-probe.sh"));
        command
            .current_dir(&root)
            .args(args)
            .env_clear()
            .env("PATH", path);
        if let Ok(home) = env::var("HOME") {
            command.env("HOME", home);
        }
        let started = Instant::now();
        let output = command.output().expect("the probe should be executable");
        (output, started.elapsed())
    }

    fn probe(args: &[&str]) -> (Output, Duration) {
        probe_on_path(&env::var("PATH").expect("PATH"), args)
    }

    /// A directory of this test's own, emptied first so an earlier run's stand-in
    /// can never answer for this one.
    fn stub_dir(case: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join("release-probe")
            .join(case);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("the stub directory is creatable");
        dir
    }

    /// Where `bash` really is: the probe's shebang resolves its interpreter through
    /// PATH like anything else, so a PATH under test still has to carry it.
    fn interpreter() -> PathBuf {
        env::var("PATH")
            .unwrap_or_default()
            .split(':')
            .map(|dir| PathBuf::from(dir).join("bash"))
            .find(|candidate| candidate.is_file())
            .unwrap_or_else(|| PathBuf::from("/bin/bash"))
    }

    /// A registry stand-in, and the `PATH` that reaches it: a real `curl` that
    /// answers the one call the probe makes with a canned status and body, or
    /// refuses to connect at all (`exit_code` non-zero, as curl does at 7).
    /// Prepended to the real PATH, so everything else the probe needs is still
    /// found and only the registry is faked.
    fn registry_on_path(case: &str, status: &str, body: &str, exit_code: i32) -> String {
        assert!(!body.contains('\''), "the stand-in quotes the body with '");
        let dir = stub_dir(case);
        let curl = dir.join("curl");
        fs::write(
            &curl,
            format!(
                "#!/usr/bin/env bash\n\
                 # Registry stand-in for tests/registry_targets.rs.\n\
                 set -eu\n\
                 out=\n\
                 prev=\n\
                 for arg in \"$@\"; do\n\
                 \x20   if [ \"$prev\" = --output ]; then out=$arg; fi\n\
                 \x20   prev=$arg\n\
                 done\n\
                 if [ -n \"$out\" ]; then printf '%s' '{body}' > \"$out\"; fi\n\
                 if [ {exit_code} -ne 0 ]; then\n\
                 \x20   echo 'curl: ({exit_code}) stand-in refused to connect' >&2\n\
                 \x20   exit {exit_code}\n\
                 fi\n\
                 printf '%s' '{status}'\n"
            ),
        )
        .expect("the registry stand-in is writable");
        fs::set_permissions(&curl, fs::Permissions::from_mode(0o755))
            .expect("the registry stand-in is executable");
        format!("{}:{}", dir.display(), env::var("PATH").unwrap_or_default())
    }

    /// Not answered: a reason on stderr, nothing on stdout, non-zero exit.
    fn assert_not_answered(path: &str, args: &[&str]) {
        let (output, elapsed) = probe_on_path(path, args);
        assert!(
            !output.status.success(),
            "{args:?} should not be answered, but the probe exited 0 with {:?}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(
            output.stdout.is_empty(),
            "not-answered must leave stdout empty, or a caller reads it as a version: {:?}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("release-probe:"),
            "not-answered must carry its reason on stderr, got {:?}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(elapsed < BOUND, "{args:?} took {elapsed:?}");
    }

    /// No release yet: exit 0 and *nothing at all* on stdout.
    fn assert_no_release_yet(path: &str, args: &[&str]) {
        let (output, elapsed) = probe_on_path(path, args);
        assert!(
            output.status.success(),
            "{args:?} has no release, which is an answer, not a failure: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.stdout.is_empty(),
            "{args:?} has no release, so the probe must say nothing at all, got {:?}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(elapsed < BOUND, "{args:?} took {elapsed:?}");
    }

    /// The failure that matters most: an identifier the probe does not recognise
    /// must be *not answered*, never the empty output that means "no release yet".
    /// A consumer reading the second launches work whose dependency never landed.
    #[test]
    fn an_unrecognised_identifier_is_not_answered_rather_than_no_release_yet() {
        let path = env::var("PATH").expect("PATH");
        // Unqualified: `onejudge` alone names both the crate and the SDK wheel.
        assert_not_answered(&path, &["onejudge"]);
        // A registry this repository publishes nothing to.
        assert_not_answered(&path, &["npm:onejudge"]);
        // Qualified, but no artifact name at all.
        assert_not_answered(&path, &["crate:"]);
        // A name no registry could serve.
        assert_not_answered(&path, &["pypi:not a name"]);
    }

    /// Exactly one argument — no argument, and no second one to be ignored.
    #[test]
    fn the_probe_takes_exactly_one_identifier() {
        let path = env::var("PATH").expect("PATH");
        assert_not_answered(&path, &[]);
        assert_not_answered(&path, &["crate:onejudge", "pypi:onejudge"]);
    }

    /// Which registries the probe can answer for is the probe's own fact, so it is
    /// read off the probe: under a stand-in that serves nothing, a *recognised*
    /// identifier answers no-release-yet, and an unrecognised one does not. Every
    /// declared target has to be one the probe recognises, or the target is a hold
    /// that never resolves.
    #[test]
    fn the_probe_recognises_every_declared_target() {
        let path = registry_on_path("recognises", "404", "", 0);
        for target in declared_targets() {
            assert_no_release_yet(&path, &[&target]);
        }
    }

    /// A registry that could not be read is NOT a registry that has nothing to
    /// serve. Each of these is a way the lookup can fail after the identifier is
    /// recognised, and every one of them must stay on the not-answered side.
    #[test]
    fn a_registry_that_cannot_be_read_is_not_answered() {
        // Unreachable: curl itself fails (7 is its connect error).
        let unreachable = registry_on_path("unreachable", "", "", 7);
        assert_not_answered(&unreachable, &["pypi:onejudge"]);

        // Reached, but answering something neither served nor absent.
        let broken = registry_on_path("server-error", "500", "upstream is down", 0);
        assert_not_answered(&broken, &["pypi:onejudge"]);

        // Served, but with a payload no version can be read out of.
        let garbled = registry_on_path("garbled", "200", "<html>maintenance</html>", 0);
        assert_not_answered(&garbled, &["pypi:onejudge"]);

        // Served, well-formed, and empty where the version belongs.
        let empty = registry_on_path("empty-version", "200", r#"{"info": {"version": ""}}"#, 0);
        assert_not_answered(&empty, &["pypi:onejudge"]);
    }

    /// The probe assumes only PATH and HOME, so a PATH that cannot reach what it
    /// looks things up with is not-answered — never a silent no-release-yet.
    ///
    /// The PATH still carries `bash`, because a shebang that cannot resolve its
    /// interpreter never starts the probe at all: that would prove the harness,
    /// not the contract.
    #[test]
    fn a_lookup_tool_the_path_cannot_reach_is_not_answered() {
        let dir = stub_dir("no-tools");
        std::os::unix::fs::symlink(interpreter(), dir.join("bash"))
            .expect("the interpreter is linkable");
        assert_not_answered(&dir.display().to_string(), &["pypi:onejudge"]);
    }

    /// The two remaining answers, against a registry stand-in: a version it serves,
    /// and nothing for an artifact it has never served. The same two are proven
    /// against the true registries in the network tier below.
    #[test]
    fn a_stand_in_registry_answers_the_version_it_serves() {
        let pypi = registry_on_path("pypi-served", "200", r#"{"info": {"version": "1.2.3"}}"#, 0);
        let (output, _) = probe_on_path(&pypi, &["pypi:onejudge"]);
        assert!(output.status.success(), "the stand-in served a version");
        assert_eq!(String::from_utf8_lossy(&output.stdout), "1.2.3\n");

        let crates = registry_on_path(
            "crate-served",
            "200",
            r#"{"crate": {"max_stable_version": "1.2.3", "newest_version": "2.0.0-rc.1"}}"#,
            0,
        );
        let (output, _) = probe_on_path(&crates, &["crate:onejudge"]);
        assert!(output.status.success(), "the stand-in served a version");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "1.2.3\n",
            "a prerelease is not what the registry serves to a dependent"
        );

        assert_no_release_yet(
            &registry_on_path("never-served", "404", "", 0),
            &["crate:onejudge"],
        );
    }

    /// Network tier: what crates.io and PyPI serve for every declared target right
    /// now. `#[ignore]`-d like the rest of this repository's network-touching
    /// verification — the gate is offline. Run with `just test-release-probe`.
    #[test]
    #[ignore = "network: reads the public registries; run via `just test-release-probe`"]
    fn every_declared_target_reports_the_version_its_registry_serves() {
        for target in declared_targets() {
            let (output, elapsed) = probe(&[&target]);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                output.status.success(),
                "the probe did not answer for `{target}`: {stderr}"
            );
            let stdout = String::from_utf8_lossy(&output.stdout);
            let version = stdout.trim_end_matches('\n');
            assert!(
                !version.is_empty(),
                "`{target}` is declared but the registry serves no release of it"
            );
            assert!(
                !version.contains('\n'),
                "`{target}` answered more than one line: {stdout:?}"
            );
            assert!(
                version.starts_with(|c: char| c.is_ascii_digit()) && version.contains('.'),
                "`{target}` answered {version:?}, which is not a version"
            );
            assert!(elapsed < BOUND, "`{target}` took {elapsed:?}");
        }
    }

    /// The third answer against the real thing: a registry that has never served
    /// the artifact reports no release yet — exit 0, empty, distinct from a
    /// failure to answer.
    #[test]
    #[ignore = "network: reads the public registries; run via `just test-release-probe`"]
    fn an_artifact_no_registry_serves_answers_no_release_yet() {
        let path = env::var("PATH").expect("PATH");
        for target in [
            "crate:onejudge-no-such-crate-6bd41f",
            "pypi:onejudge-no-such-distribution-6bd41f",
        ] {
            assert_no_release_yet(&path, &[target]);
        }
    }
}
