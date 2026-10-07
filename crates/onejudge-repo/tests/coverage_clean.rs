//! The coverage aggregate's fresh start, driven through the real recipe:
//! `just _coverage-clean` — what every instrumented suite's `test` depends on —
//! run against a scratch Cargo target directory seeded with a stale profile.
//!
//! A profile an earlier run left behind would be merged into this run's number,
//! so the recipe must clear every `.profraw` from the directory the aggregate
//! merges, and nothing else; and when coverage is off (`ONEJUDGE_COVERAGE=0`, the
//! macOS and Windows runs) it must leave the directory alone, because nothing is
//! measured there.
//!
//! Unix only: the recipe runs under the justfile's bash.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A scratch target directory holding one stale profile and one other file.
fn seeded() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let target = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "coverage-clean-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&target);
    let profiles = target.join("llvm-cov-target");
    std::fs::create_dir_all(&profiles).unwrap();
    std::fs::write(profiles.join("worktree-123-456_0.profraw"), b"stale").unwrap();
    std::fs::write(profiles.join("kept.txt"), b"not a profile").unwrap();
    target
}

/// `just _coverage-clean` from the workspace root, building into `target`.
fn clean(target: &Path, coverage: Option<&str>) {
    let mut command = Command::new("just");
    command
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .arg("_coverage-clean")
        .env("CARGO_TARGET_DIR", target)
        .env_remove("ONEJUDGE_COVERAGE");
    if let Some(value) = coverage {
        command.env("ONEJUDGE_COVERAGE", value);
    }
    let output = command.output().expect("just runs");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn a_stale_profile_is_cleared_before_a_measured_run_and_nothing_else_is() {
    let target = seeded();
    clean(&target, None);
    let profiles = target.join("llvm-cov-target");
    assert!(!profiles.join("worktree-123-456_0.profraw").exists());
    assert!(profiles.join("kept.txt").exists());
}

#[test]
fn an_unmeasured_run_leaves_the_profiles_alone() {
    let target = seeded();
    clean(&target, Some("0"));
    assert!(target
        .join("llvm-cov-target/worktree-123-456_0.profraw")
        .exists());
}
