//! The coverage aggregate's fresh start, driven through the real recipe:
//! `just _coverage-clean` — what every instrumented suite's `test` depends on —
//! run against a scratch Cargo target directory seeded with a stale profile.
//!
//! A profile an earlier run left behind would be merged into this run's number,
//! and so would a workspace crate's instrumented binary an earlier build left
//! under another hash: `cargo llvm-cov report` reads every object it finds, so a
//! stale one reports source the tree no longer has. The recipe must clear both
//! from the directory the aggregate merges, and keep the dependencies' artifacts
//! and anything else; and when coverage is off (`ONEJUDGE_COVERAGE=0`, the macOS
//! and Windows runs) it must leave the directory alone, because nothing is
//! measured there.
//!
//! Unix only: the recipe runs under the justfile's bash.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A scratch target directory holding a stale profile and a stale instrumented
/// suite binary, beside a dependency's artifact and a file that is neither.
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
    let deps = profiles.join("debug/deps");
    std::fs::create_dir_all(&deps).unwrap();
    std::fs::write(deps.join("e2e-0123456789abcdef"), b"stale suite").unwrap();
    std::fs::write(deps.join("libserde-0123456789abcdef.rlib"), b"dependency").unwrap();
    target
}

/// `coverage` of `None` unsets `ONEJUDGE_COVERAGE` rather than inheriting it, so
/// the caller's own environment cannot pick the branch under test.
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
fn stale_profiles_and_suite_binaries_are_cleared_before_a_measured_run_and_nothing_else_is() {
    let target = seeded();
    clean(&target, None);
    let profiles = target.join("llvm-cov-target");
    assert!(!profiles.join("worktree-123-456_0.profraw").exists());
    assert!(!profiles.join("debug/deps/e2e-0123456789abcdef").exists());
    assert!(profiles
        .join("debug/deps/libserde-0123456789abcdef.rlib")
        .exists());
    assert!(profiles.join("kept.txt").exists());
}

#[test]
fn an_unmeasured_run_leaves_the_profiles_alone() {
    let target = seeded();
    clean(&target, Some("0"));
    assert!(target
        .join("llvm-cov-target/worktree-123-456_0.profraw")
        .exists());
    assert!(target
        .join("llvm-cov-target/debug/deps/e2e-0123456789abcdef")
        .exists());
}
