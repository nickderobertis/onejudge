//! The gate's tier and project selection, driven for real: `scripts/gate-plan.sh`
//! — the script `just check` evaluates — run with this repository's own Nx
//! install over a scratch workspace whose history this test writes.
//!
//! The workspace is four projects shaped like this repository's: a contract
//! (`lib`), an e2e suite of it (`lib-e2e`), an SDK over it (`sdk`) and an external
//! tier (`live`). Each journey makes one change and reads the plan the script
//! prints, so what is held here is what the gate would run: which base it keyed
//! off and why, which projects a change reaches, that a root file sweeps, that the
//! external tier is never run, and that an `NX_BASE` that is not a plain ref name
//! or commit SHA is refused before git ever sees it.
//!
//! Unix only: the gate scripts are bash, and the Windows job runs the Rust suites
//! through them under Git Bash rather than testing their selection.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the workspace root exists")
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn project(name: &str, tag: &str, deps: &[&str]) -> String {
    serde_json::json!({ "name": name, "tags": [tag], "implicitDependencies": deps }).to_string()
}

/// A committed scratch workspace carrying the real gate scripts and Nx, and the
/// SHA of its one commit.
fn workspace() -> (PathBuf, String) {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let root = repo_root();
    // The same heal every recipe runs, so a direct `cargo nextest` finds Nx too.
    let healed = Command::new("bash")
        .arg(root.join("scripts/node-modules.sh"))
        .status()
        .expect("bash runs");
    assert!(
        healed.success(),
        "scripts/node-modules.sh could not install Nx"
    );

    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "gate-plan-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    for file in [
        "scripts/gate-plan.sh",
        "scripts/nx",
        "scripts/node-modules.sh",
        "package.json",
        "bun.lock",
    ] {
        // Copied with their modes: the gate runs `./scripts/nx` as an executable.
        std::fs::create_dir_all(dir.join(file).parent().unwrap()).unwrap();
        std::fs::copy(root.join(file), dir.join(file)).unwrap();
    }
    std::os::unix::fs::symlink(root.join("node_modules"), dir.join("node_modules")).unwrap();
    write(&dir.join(".gitignore"), "/node_modules\n/.nx\n");
    write(
        &dir.join("nx.json"),
        r#"{"namedInputs":{"default":["{projectRoot}/**/*"]}}"#,
    );
    write(&dir.join("README.md"), "root\n");
    write(
        &dir.join("lib/project.json"),
        &project("lib", "type:contract", &[]),
    );
    write(&dir.join("lib/src.txt"), "lib\n");
    write(
        &dir.join("lib-e2e/project.json"),
        &project("lib-e2e", "type:e2e", &["lib"]),
    );
    write(
        &dir.join("sdk/project.json"),
        &project("sdk", "type:sdk", &["lib"]),
    );
    write(&dir.join("sdk/src.txt"), "sdk\n");
    write(
        &dir.join("live/project.json"),
        &project("live", "type:external", &["lib"]),
    );
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["config", "user.email", "gate-plan@example.invalid"]);
    git(&dir, &["config", "user.name", "gate-plan"]);
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "base"]);
    let base = git(&dir, &["rev-parse", "HEAD"]);
    (dir, base)
}

/// Commit a change to `file` on top of whatever is checked out.
fn change(dir: &Path, file: &str) {
    let path = dir.join(file);
    let mut text = std::fs::read_to_string(&path).unwrap_or_default();
    text.push_str("changed\n");
    write(&path, &text);
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", &format!("change {file}")]);
}

/// `gate-plan.sh --print-plan [flags]` with `NX_BASE` set to `base` (or unset).
fn plan(dir: &Path, base: Option<&str>, flags: &[&str]) -> Output {
    let mut command = Command::new("bash");
    command
        .current_dir(dir)
        .arg("scripts/gate-plan.sh")
        .arg("--print-plan")
        .args(flags)
        .env_remove("NX_BASE");
    if let Some(base) = base {
        command.env("NX_BASE", base);
    }
    command.output().expect("bash runs")
}

/// The plan's `# key: value` lines, as (tier line, projects, external tiers).
fn read(output: &Output) -> (String, String, String) {
    assert!(
        output.status.success(),
        "gate-plan.sh failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let line = |key: &str| {
        stdout
            .lines()
            .find_map(|line| line.strip_prefix(&format!("# {key}:")))
            .unwrap_or_else(|| panic!("no `# {key}:` line in {stdout:?}"))
            .trim()
            .to_owned()
    };
    (
        line("tier"),
        line("projects"),
        line("external tiers, static targets only"),
    )
}

#[test]
fn a_contract_change_selects_its_dependents_and_never_runs_the_external_tier() {
    let (dir, base) = workspace();
    change(&dir, "lib/src.txt");
    let (tier, projects, external) = read(&plan(&dir, Some(&base), &[]));
    assert_eq!(tier, format!("affected (base {base}, from NX_BASE={base})"));
    assert_eq!(projects, "lib lib-e2e sdk");
    assert_eq!(external, "live");
}

#[test]
fn a_change_confined_to_one_project_selects_only_it() {
    let (dir, base) = workspace();
    change(&dir, "sdk/src.txt");
    let (tier, projects, external) = read(&plan(&dir, Some(&base), &[]));
    assert!(tier.starts_with("affected"), "{tier}");
    assert_eq!(projects, "sdk");
    assert_eq!(external, "");
}

#[test]
fn a_workspace_root_file_escalates_to_the_broader_tier() {
    let (dir, base) = workspace();
    change(&dir, "README.md");
    let (tier, projects, external) = read(&plan(&dir, Some(&base), &[]));
    assert_eq!(
        tier,
        format!(
            "sweep (escalated from the affected tier against base {base}, from NX_BASE={base}: \
             README.md is a workspace-root file)"
        )
    );
    assert_eq!(projects, "lib lib-e2e sdk");
    assert_eq!(external, "live");
    let (tier, ..) = read(&plan(&dir, None, &["--sweep"]));
    assert_eq!(tier, "sweep");
}

#[test]
fn without_nx_base_the_merge_base_with_origin_main_is_the_base() {
    let (dir, base) = workspace();
    git(&dir, &["update-ref", "refs/remotes/origin/main", &base]);
    change(&dir, "sdk/src.txt");
    let (tier, projects, _) = read(&plan(&dir, None, &[]));
    assert_eq!(
        tier,
        format!("affected (base {base}, from the merge base with origin/main)")
    );
    assert_eq!(projects, "sdk");
    // A plain ref name is accepted as NX_BASE, and resolved to its commit.
    let (tier, ..) = read(&plan(&dir, Some("origin/main"), &[]));
    assert_eq!(
        tier,
        format!("affected (base {base}, from NX_BASE=origin/main)")
    );
}

#[test]
fn with_no_merge_base_to_derive_the_broader_tier_runs() {
    let (dir, _) = workspace();
    change(&dir, "sdk/src.txt");
    let (tier, projects, _) = read(&plan(&dir, None, &[]));
    assert_eq!(
        tier,
        "sweep (escalated from the affected tier: no merge base with origin/main)"
    );
    assert_eq!(projects, "lib lib-e2e sdk");
}

#[test]
fn an_nx_base_that_is_not_a_plain_ref_or_sha_fails_closed_naming_it() {
    let (dir, _) = workspace();
    for refused in [
        "HEAD~1",
        "main^",
        "$(touch pwned)",
        "--all",
        "a..b",
        "@{-1}",
    ] {
        let output = plan(&dir, Some(refused), &[]);
        assert_eq!(output.status.code(), Some(2), "{refused}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(&format!(
                "NX_BASE='{refused}' is not a plain ref name or commit SHA"
            )),
            "{refused}: {stderr}"
        );
        assert!(output.stdout.is_empty(), "{refused} printed a plan");
    }
    assert!(!dir.join("pwned").exists(), "NX_BASE reached a shell");
    // A well-formed name that names nothing is refused too, and says so.
    let output = plan(&dir, Some("no-such-branch"), &[]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("names no commit"));
}
