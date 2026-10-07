//! Every Nx target command runs under Windows' default shell, driven for real:
//! `scripts/check-target-commands.mjs` — what every project's `lint` runs — over
//! this repository's own resolved project graph, and over a scratch workspace
//! with this repository's Nx install whose one target is written each way.
//!
//! The defect it holds closed: `onejudge-repo:test` once ran
//! `ONEJUDGE_COVERAGE=0 just _rust-test onejudge-repo ''`, which `/bin/sh` runs
//! and cmd.exe answers with `'ONEJUDGE_COVERAGE' is not recognized as an internal
//! or external command`, failing `test-os (windows-latest)`.
//!
//! Unix only: the scratch workspace links this checkout's `node_modules`.
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

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn heal_nx(root: &Path) {
    let healed = Command::new("bash")
        .arg(root.join("scripts/node-modules.sh"))
        .status()
        .expect("bash runs");
    assert!(
        healed.success(),
        "scripts/node-modules.sh could not install Nx"
    );
}

fn check(dir: &Path, projects: &[&str]) -> Output {
    Command::new("node")
        .current_dir(dir)
        .arg("scripts/check-target-commands.mjs")
        .args(projects)
        .env("NX_DAEMON", "false")
        .output()
        .expect("node runs")
}

/// A scratch workspace with one project, `app`, whose `targets` are `targets`.
fn workspace(targets: serde_json::Value) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let root = repo_root();
    heal_nx(&root);
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "target-commands-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    for file in ["scripts/check-target-commands.mjs", "package.json"] {
        std::fs::create_dir_all(dir.join(file).parent().unwrap()).unwrap();
        std::fs::copy(root.join(file), dir.join(file)).unwrap();
    }
    std::os::unix::fs::symlink(root.join("node_modules"), dir.join("node_modules")).unwrap();
    write(&dir.join(".gitignore"), "/node_modules\n/.nx\n");
    write(
        &dir.join("nx.json"),
        r#"{ "namedInputs": { "default": ["{projectRoot}/**/*"] } }"#,
    );
    let project = serde_json::json!({ "name": "app", "targets": targets });
    write(&dir.join("app/project.json"), &project.to_string());
    let git = Command::new("git")
        .current_dir(&dir)
        .args(["init", "-q"])
        .status()
        .expect("git runs");
    assert!(git.success());
    dir
}

fn passes(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "expected a pass: {stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
}

fn refuses(output: &Output, expected: &[&str]) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(1),
        "expected a refusal: {stderr}"
    );
    for line in expected {
        assert!(stderr.contains(line), "{line:?} not in: {stderr}");
    }
    assert!(stderr.contains("ACTION:"), "no next step in: {stderr}");
}

#[test]
fn every_target_in_this_repository_runs_on_every_platforms_shell() {
    let root = repo_root();
    heal_nx(&root);
    let stdout = passes(&check(&root, &[]));
    assert!(
        stdout.contains("commands run on every platform's shell"),
        "{stdout}"
    );
}

#[test]
fn the_target_that_failed_on_windows_is_refused_and_its_portable_form_passes() {
    let old = workspace(serde_json::json!({
        "test": { "command": "ONEJUDGE_COVERAGE=0 just _rust-test onejudge-repo ''" }
    }));
    refuses(
        &check(&old, &[]),
        &[
            "app:test runs `ONEJUDGE_COVERAGE=0 just _rust-test onejudge-repo ''`, which carries a leading environment assignment (declare it in the target's options.env)",
            "app:test runs `ONEJUDGE_COVERAGE=0 just _rust-test onejudge-repo ''`, which carries a single-quoted argument",
        ],
    );

    let portable = workspace(serde_json::json!({
        "test": {
            "command": "just _rust-test onejudge-repo \"\"",
            "options": { "env": { "ONEJUDGE_COVERAGE": "0" } }
        }
    }));
    let stdout = passes(&check(&portable, &["app"]));
    assert!(
        stdout.contains("app: 1 commands run on every platform's shell"),
        "{stdout}"
    );
}

#[test]
fn every_command_of_a_multi_command_target_is_read() {
    let dir = workspace(serde_json::json!({
        "lint": {
            "executor": "nx:run-commands",
            "options": {
                "commands": [
                    "node scripts/ok.mjs",
                    { "command": "PATH=\"$HOME/.local/bin:$PATH\" just _lint" }
                ]
            }
        }
    }));
    refuses(
        &check(&dir, &["app"]),
        &[
            "app:lint runs `PATH=\"$HOME/.local/bin:$PATH\" just _lint`, which carries a leading environment assignment",
            "which carries a `$` expansion",
        ],
    );
}

#[test]
fn an_assignment_after_a_command_separator_is_still_leading() {
    let dir = workspace(serde_json::json!({
        "build": { "command": "just _a && RUSTFLAGS=-Dwarnings just _b" }
    }));
    refuses(
        &check(&dir, &[]),
        &["app:build runs `just _a && RUSTFLAGS=-Dwarnings just _b`, which carries a leading environment assignment"],
    );
}

#[test]
fn an_unknown_project_is_named() {
    let dir = workspace(serde_json::json!({ "build": { "command": "just _a" } }));
    refuses(&check(&dir, &["nope"]), &["no project named nope"]);
}

#[test]
fn a_graph_nx_cannot_compute_is_refused_with_what_nx_said() {
    let dir = workspace(serde_json::json!({ "build": { "command": "just _a" } }));
    // Nx skips an unreadable project.json, so it is `nx.json` that is broken.
    write(&dir.join("nx.json"), "{ \"namedInputs\": ");
    let output = check(&dir, &[]);
    refuses(
        &output,
        &["computing the Nx project graph (`nx graph`) failed: "],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("failed: no output"), "{stderr}");
    assert!(
        stderr.contains("ValueExpected") && stderr.contains("nx.json at "),
        "{stderr}"
    );
}
