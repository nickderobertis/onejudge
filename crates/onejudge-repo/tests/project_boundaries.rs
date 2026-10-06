//! The module-boundary rule, driven for real: `scripts/check-project-boundaries.mjs`
//! — what every project's `lint` runs — over a scratch workspace with this
//! repository's own Nx install, a real Cargo workspace, and `nx.json`'s
//! `boundaries.allow` shaped like this repository's.
//!
//! Each journey draws one edge (or breaks one declaration) and reads what the
//! check says: an allowed graph passes, and every refusal names the edge or the
//! project at fault — an edge a tag forbids whether Nx or Cargo drew it, a Cargo
//! edge the Nx graph does not know about, a project with no `type:` tag or one
//! `boundaries.allow` does not declare, a crate with no project beside it.
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

fn project(dir: &Path, name: &str, tags: &[&str], deps: &[&str]) {
    let definition =
        serde_json::json!({ "name": name, "tags": tags, "implicitDependencies": deps });
    write(
        &dir.join(name).join("project.json"),
        &definition.to_string(),
    );
}

fn krate(dir: &Path, name: &str, path_deps: &[&str]) {
    let deps: String = path_deps
        .iter()
        .map(|dep| format!("{dep} = {{ path = \"../{dep}\" }}\n"))
        .collect();
    write(
        &dir.join(name).join("Cargo.toml"),
        &format!(
            "[package]\nname = \"{name}\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[dependencies]\n{deps}"
        ),
    );
    write(&dir.join(name).join("src/lib.rs"), "");
}

/// A workspace whose graph the rule allows: an e2e crate over a contract crate
/// (Cargo edge and Nx edge both), and an SDK over the contract.
fn workspace() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let root = repo_root();
    let healed = Command::new("bash")
        .arg(root.join("scripts/node-modules.sh"))
        .status()
        .expect("bash runs");
    assert!(
        healed.success(),
        "scripts/node-modules.sh could not install Nx"
    );

    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "project-boundaries-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    for file in ["scripts/check-project-boundaries.mjs", "package.json"] {
        std::fs::create_dir_all(dir.join(file).parent().unwrap()).unwrap();
        std::fs::copy(root.join(file), dir.join(file)).unwrap();
    }
    std::os::unix::fs::symlink(root.join("node_modules"), dir.join("node_modules")).unwrap();
    write(&dir.join(".gitignore"), "/node_modules\n/.nx\n/target\n");
    allow(&dir, true);
    write(
        &dir.join("Cargo.toml"),
        "[workspace]\nresolver = \"2\"\nmembers = [\"lib\", \"lib-e2e\"]\n",
    );
    krate(&dir, "lib", &[]);
    krate(&dir, "lib-e2e", &["lib"]);
    project(&dir, "lib", &["type:contract"], &[]);
    project(&dir, "lib-e2e", &["type:e2e"], &["lib"]);
    project(&dir, "sdk", &["type:sdk"], &["lib"]);
    let git = Command::new("git")
        .current_dir(&dir)
        .args(["init", "-q"])
        .status()
        .expect("git runs");
    assert!(git.success());
    dir
}

/// `nx.json`, with this repository's boundary shape or none at all.
fn allow(dir: &Path, with_boundaries: bool) {
    let boundaries = serde_json::json!({
        "allow": {
            "type:contract": [],
            "type:e2e": ["type:contract"],
            "type:sdk": ["type:contract"],
        }
    });
    let mut nx = serde_json::json!({ "namedInputs": { "default": ["{projectRoot}/**/*"] } });
    if with_boundaries {
        nx["boundaries"] = boundaries;
    }
    write(&dir.join("nx.json"), &nx.to_string());
}

fn check(dir: &Path, projects: &[&str]) -> Output {
    Command::new("node")
        .current_dir(dir)
        .arg("scripts/check-project-boundaries.mjs")
        .args(projects)
        .env("NX_DAEMON", "false")
        .output()
        .expect("node runs")
}

/// The check's refusal, which must fail and name `expected`, with a next step.
fn refuses(output: &Output, expected: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(1),
        "expected a refusal: {stderr}"
    );
    assert!(stderr.contains(expected), "{expected:?} not in: {stderr}");
    assert!(stderr.contains("ACTION:"), "no next step in: {stderr}");
}

#[test]
fn an_allowed_graph_passes() {
    let dir = workspace();
    let output = check(&dir, &[]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "check-project-boundaries: 3 projects: every edge allowed"
    );
}

#[test]
fn an_edge_a_tag_forbids_is_refused_naming_it() {
    let dir = workspace();
    // An unrelated project reaching an e2e suite.
    project(&dir, "sdk", &["type:sdk"], &["lib", "lib-e2e"]);
    refuses(
        &check(&dir, &["sdk"]),
        "sdk (type:sdk) -> lib-e2e (type:e2e) is not allowed: type:sdk may depend only on [type:contract]",
    );
    // The contract reaching one of its consumers.
    project(&dir, "sdk", &["type:sdk"], &["lib"]);
    project(&dir, "lib", &["type:contract"], &["sdk"]);
    refuses(
        &check(&dir, &["lib"]),
        "lib (type:contract) -> sdk (type:sdk) is not allowed: type:contract may depend only on []",
    );
    // Only the named project's own edges are judged.
    assert!(check(&dir, &["lib-e2e"]).status.success());
}

#[test]
fn a_cargo_edge_is_judged_too_and_must_be_in_the_nx_graph() {
    let dir = workspace();
    // The contract crate drawing a Cargo edge to its e2e suite, undeclared in Nx.
    krate(&dir, "lib", &["lib-e2e"]);
    krate(&dir, "lib-e2e", &[]);
    let output = check(&dir, &["lib"]);
    refuses(
        &output,
        "Cargo edge lib -> lib-e2e is missing from the Nx graph, so a change to lib-e2e would not select lib",
    );
    refuses(
        &output,
        "lib (type:contract) -> lib-e2e (type:e2e) is not allowed",
    );
}

#[test]
fn every_project_carries_exactly_one_declared_type() {
    let dir = workspace();
    project(&dir, "sdk", &[], &["lib"]);
    refuses(
        &check(&dir, &[]),
        "sdk carries 0 type: tags (none); give it exactly one",
    );
    project(&dir, "sdk", &["type:weird"], &["lib"]);
    refuses(
        &check(&dir, &[]),
        "sdk is tagged type:weird, which nx.json \"boundaries.allow\" does not declare",
    );
}

#[test]
fn a_crate_without_a_project_beside_it_is_refused() {
    let dir = workspace();
    write(
        &dir.join("Cargo.toml"),
        "[workspace]\nresolver = \"2\"\nmembers = [\"lib\", \"lib-e2e\", \"orphan\"]\n",
    );
    krate(&dir, "orphan", &["lib"]);
    refuses(
        &check(&dir, &[]),
        "Cargo edge orphan -> lib joins a crate with no project.json beside its Cargo.toml",
    );
}

#[test]
fn a_missing_table_or_an_unknown_project_is_refused_with_a_next_step() {
    let dir = workspace();
    refuses(
        &check(&dir, &["nope"]),
        "no project named nope in the Nx graph",
    );
    allow(&dir, false);
    refuses(
        &check(&dir, &[]),
        "nx.json has no well-formed \"boundaries.allow\" table to enforce",
    );
    // A table whose entry is not a list of declared types is no table at all: a
    // string would otherwise be matched by substring.
    write(
        &dir.join("nx.json"),
        r#"{"boundaries":{"allow":{"type:contract":[],"type:e2e":"type:contract","type:sdk":[]}}}"#,
    );
    refuses(
        &check(&dir, &[]),
        "nx.json has no well-formed \"boundaries.allow\" table to enforce",
    );
    write(&dir.join("nx.json"), "{ not json");
    refuses(&check(&dir, &[]), "nx.json could not be read");
}
