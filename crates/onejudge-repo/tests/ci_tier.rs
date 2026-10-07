//! The CI tier routing, driven for real: `scripts/ci-tier.mjs` run by `node` in a
//! scratch git repository, fed the GitHub event payload each kind of run carries.
//!
//! The workflows hand whatever this script prints to the gate recipe
//! (`tests/workflows.rs` holds that wiring), so what it selects here is what CI
//! runs: the broader tier on release-plz's release pull request — where onejudge's
//! batched releases are swept — and the affected tier, against a base it derived
//! with `git merge-base`, on every other pull request and every push to main — a
//! push never escalated to the sweep by a root file, because the pull request that
//! landed the change already gated it at that tier. A base it cannot derive falls
//! back to the broader tier rather than to nothing.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/ci-tier.mjs")
        .canonicalize()
        .expect("scripts/ci-tier.mjs exists")
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

/// A history shaped like a pull request's: `main` at `base`, then a branch two
/// commits on from it, while `main` itself moved on by one (`moved`).
struct Repo {
    dir: PathBuf,
    base: String,
    moved: String,
}

fn repo() -> Repo {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "ci-tier-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["config", "user.email", "ci-tier@example.invalid"]);
    git(&dir, &["config", "user.name", "ci-tier"]);
    let commit = |name: &str| {
        std::fs::write(dir.join(name), name).unwrap();
        git(&dir, &["add", name]);
        git(&dir, &["commit", "-q", "-m", name]);
        git(&dir, &["rev-parse", "HEAD"])
    };
    let base = commit("base");
    let moved = commit("main-moved");
    git(&dir, &["checkout", "-q", "-b", "feature", &base]);
    commit("feature-1");
    commit("feature-2");
    Repo { dir, base, moved }
}

/// `(tier, base, flags)`: the three `key=value` lines the workflows hand to the
/// gate recipe.
fn route(repo: &Repo, name: &str, event: &serde_json::Value) -> (String, String, String) {
    route_payload(repo, name, &event.to_string())
}

/// Takes the payload as raw text, so a journey can hand over one that is not JSON.
fn route_payload(repo: &Repo, name: &str, payload_text: &str) -> (String, String, String) {
    let payload = repo.dir.join(format!("{name}-event.json"));
    std::fs::write(&payload, payload_text).unwrap();
    let output = Command::new("node")
        .arg(script())
        .current_dir(&repo.dir)
        .env("GITHUB_EVENT_NAME", name)
        .env("GITHUB_EVENT_PATH", &payload)
        .output()
        .expect("node runs");
    assert!(
        output.status.success(),
        "ci-tier.mjs failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let value = |key: &str| {
        stdout
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{key}=")))
            .unwrap_or_else(|| panic!("no `{key}=` line in {stdout:?}"))
            .to_owned()
    };
    (value("tier"), value("base"), value("flags"))
}

fn pull_request(head_ref: &str, base_sha: &str) -> serde_json::Value {
    serde_json::json!({
        "pull_request": {
            "head": { "ref": head_ref },
            "base": { "ref": "main", "sha": base_sha },
        }
    })
}

#[test]
fn release_plz_s_release_pull_request_runs_the_broader_tier() {
    let repo = repo();
    let event = pull_request("release-plz-2026-10-06T12-00-00Z", &repo.moved);
    assert_eq!(
        route(&repo, "pull_request", &event),
        ("sweep".into(), String::new(), "--sweep".into())
    );
}

#[test]
fn an_ordinary_pull_request_runs_the_affected_tier_against_its_merge_base() {
    let repo = repo();
    // The base branch moved on after the branch forked: the base is where they
    // diverged, not the base branch's tip.
    let event = pull_request("nick/a-feature", &repo.moved);
    assert_eq!(
        route(&repo, "pull_request", &event),
        ("affected".into(), repo.base.clone(), String::new())
    );
}

#[test]
fn a_push_to_main_runs_the_affected_tier_against_the_tip_it_replaced() {
    let repo = repo();
    git(&repo.dir, &["checkout", "-q", "main"]);
    git(
        &repo.dir,
        &["merge", "-q", "--ff-only", "--no-edit", &repo.moved],
    );
    let event = serde_json::json!({ "ref": "refs/heads/main", "before": repo.base });
    assert_eq!(
        route(&repo, "push", &event),
        ("affected".into(), repo.base.clone(), "--no-escalate".into())
    );
}

#[test]
fn an_underivable_base_falls_back_to_the_broader_tier() {
    let repo = repo();
    let missing = "0123456789abcdef0123456789abcdef01234567";
    for (name, event) in [
        // A base commit the checkout does not have (a shallow clone).
        ("pull_request", pull_request("nick/a-feature", missing)),
        // A first push to a branch: GitHub sends an all-zero `before`.
        (
            "push",
            serde_json::json!({ "ref": "refs/heads/main", "before": "0".repeat(40) }),
        ),
        // A payload field that is not a commit name at all.
        (
            "push",
            serde_json::json!({ "ref": "refs/heads/main", "before": "HEAD~1; rm -rf /" }),
        ),
    ] {
        assert_eq!(
            route(&repo, name, &event),
            ("sweep".into(), String::new(), "--sweep".into()),
            "{name}: {event}"
        );
    }
}

#[test]
fn any_other_event_runs_the_broader_tier() {
    let repo = repo();
    for (name, event) in [
        ("workflow_dispatch", serde_json::json!({})),
        ("schedule", serde_json::json!({})),
        (
            "push",
            serde_json::json!({ "ref": "refs/tags/v1.0.0", "before": repo.base }),
        ),
    ] {
        assert_eq!(
            route(&repo, name, &event),
            ("sweep".into(), String::new(), "--sweep".into()),
            "{name}"
        );
    }
}

#[test]
fn a_payload_that_is_not_an_event_object_runs_the_broader_tier() {
    let repo = repo();
    let sweep = ("sweep".to_owned(), String::new(), "--sweep".to_owned());
    for payload in ["{ not json", "null", "[]", "\"pull_request\""] {
        assert_eq!(
            route_payload(&repo, "pull_request", payload),
            sweep,
            "{payload}"
        );
    }
    // And one the runner never wrote at all.
    let output = Command::new("node")
        .arg(script())
        .current_dir(&repo.dir)
        .env("GITHUB_EVENT_NAME", "pull_request")
        .env("GITHUB_EVENT_PATH", repo.dir.join("no-such-event.json"))
        .output()
        .expect("node runs");
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("tier=sweep"));
}

#[test]
fn the_release_branch_is_the_one_release_plzs_workflow_merges() {
    // The prefix the script routes on is read from release-plz.yml's auto-merge
    // step, so a pull request from any other prefix is an ordinary one.
    let workflow = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.github/workflows/release-plz.yml"),
    )
    .unwrap();
    assert!(
        workflow.contains("startswith(\"release-plz-\")"),
        "release-plz.yml no longer names the release branch the way this suite drives it"
    );
    let repo = repo();
    let ordinary = pull_request("release-please--branches--main", &repo.moved);
    assert_eq!(route(&repo, "pull_request", &ordinary).0, "affected");
}

#[test]
fn with_no_release_branch_to_read_every_pull_request_is_swept() {
    // The script reads release-plz's branch prefix from the release-plz.yml beside
    // it; a copy beside a workflow that names none cannot scope any pull request.
    let repo = repo();
    let copy = repo.dir.join("scripts/ci-tier.mjs");
    std::fs::create_dir_all(copy.parent().unwrap()).unwrap();
    std::fs::copy(script(), &copy).unwrap();
    let workflows = repo.dir.join(".github/workflows");
    std::fs::create_dir_all(&workflows).unwrap();
    for workflow in [None, Some("name: release-plz\non: push\n")] {
        let path = workflows.join("release-plz.yml");
        match workflow {
            Some(text) => std::fs::write(&path, text).unwrap(),
            None => {
                let _ = std::fs::remove_file(&path);
            }
        }
        let payload = repo.dir.join("event.json");
        std::fs::write(
            &payload,
            pull_request("nick/a-feature", &repo.moved).to_string(),
        )
        .unwrap();
        let output = Command::new("node")
            .arg(&copy)
            .current_dir(&repo.dir)
            .env("GITHUB_EVENT_NAME", "pull_request")
            .env("GITHUB_EVENT_PATH", &payload)
            .output()
            .expect("node runs");
        assert!(output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("tier=sweep"),
            "{workflow:?}"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("names no release branch prefix"),
            "{workflow:?}"
        );
    }
}
