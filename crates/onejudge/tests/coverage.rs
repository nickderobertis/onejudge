//! The gate's coverage step, driven with the artifact that blocked a release.
//!
//! `just test` merges one `.profraw` per process the suite ran — around six
//! hundred of them — and this suite kills instrumented processes on purpose:
//! `spawn.rs`'s hook-failure unit test tears down a re-exec of its own test
//! binary, and the cancellation journeys in `e2e.rs` kill harness stand-ins. A
//! child killed while the profiling runtime is still flushing leaves a *truncated*
//! profile behind, and `llvm-profdata merge` defaults to rejecting the entire
//! merge over a single one of those. That is how `release-pypi` for v0.5.0 died
//! with 310 of 310 tests green, publishing nothing:
//!
//! ```text
//! warning: .../onejudge-7416-8909306454109023302_0.profraw: invalid
//!          instrumentation profile data (file header is corrupt)
//! error: no profile can be merged
//! ```
//!
//! So this plants exactly that artifact, on every coverage run, in the directory
//! the recipe merges from — and the recipe is the assertion. `just test` has to
//! merge the surviving profiles, report over them, and still enforce
//! `--fail-under-lines`. Take `--failure-mode all` back out of the justfile and
//! the recipe fails again, which is the regression this exists to hold.

use std::path::PathBuf;
use std::process::Command;
use std::{env, fs};

/// `INSTR_PROF_RAW_MAGIC_64` little-endian and not one byte more: as far into its
/// header as a killed child got. Racing the real thing instead yields profiles cut
/// off at a page boundary, but LLVM rejects both with the same "file header is
/// corrupt" — and unlike a race, eight fixed bytes are the same every run.
const TRUNCATED_HEADER: [u8; 8] = [0x81, b'r', b'f', b'o', b'r', b'p', b'l', 0xff];

#[test]
fn plants_a_truncated_profile_the_coverage_step_must_survive() {
    let Some(dir) = profile_directory() else {
        // Uninstrumented: `just test-fast` and the non-Linux CI jobs run this same
        // suite with no profiles to merge, so there is no merge to survive.
        return;
    };
    let planted = dir.join("truncated-by-a-killed-child_0.profraw");
    fs::write(&planted, TRUNCATED_HEADER)
        .unwrap_or_else(|e| panic!("could not plant {}: {e}", planted.display()));

    // A fixture the merge would happily accept proves nothing, so hold it against
    // the very tool the recipe merges with.
    let profdata = llvm_profdata().unwrap_or_else(|| {
        panic!(
            "coverage is instrumented but llvm-profdata is neither in $LLVM_PROFDATA \
             nor beside the rustc target libdir — run `rustup component add llvm-tools`"
        )
    });
    let shown = Command::new(&profdata)
        .arg("show")
        .arg(&planted)
        .output()
        .unwrap_or_else(|e| panic!("could not run {}: {e}", profdata.display()));
    assert!(
        !shown.status.success(),
        "the planted profile is readable, so it is not the failure condition the \
         coverage step has to survive: {}",
        planted.display()
    );
    let complaint = String::from_utf8_lossy(&shown.stderr);
    assert!(
        complaint.contains("file header is corrupt"),
        "the planted profile fails for the wrong reason — an empty profile is \
         skipped silently and proves nothing. llvm-profdata said: {}",
        complaint.trim()
    );
}

/// The directory `cargo llvm-cov` globs `*.profraw` out of, read from the pattern
/// it points instrumented processes at. `None` when the run is not instrumented.
fn profile_directory() -> Option<PathBuf> {
    let pattern = PathBuf::from(env::var_os("LLVM_PROFILE_FILE")?);
    let dir = pattern.parent()?;
    dir.is_dir().then(|| dir.to_path_buf())
}

/// `llvm-profdata`, found the way `cargo llvm-cov` finds it: an explicit override
/// first, then the llvm-tools shipped beside the active toolchain's target libdir.
fn llvm_profdata() -> Option<PathBuf> {
    if let Some(explicit) = env::var_os("LLVM_PROFDATA") {
        return Some(PathBuf::from(explicit));
    }
    let printed = Command::new("rustc")
        .args(["--print", "target-libdir"])
        .output()
        .ok()?;
    if !printed.status.success() {
        return None;
    }
    let libdir = String::from_utf8(printed.stdout).ok()?;
    let bin = PathBuf::from(libdir.trim()).parent()?.join("bin");
    ["llvm-profdata", "llvm-profdata.exe"]
        .into_iter()
        .map(|name| bin.join(name))
        .find(|candidate| candidate.is_file())
}
