//! Keeping the doubles' **detached** children out of the coverage merge set.
//!
//! `cargo llvm-cov` merges every `.profraw` under the run's profile directory
//! once the suite ends. A process that deliberately outlives the test that
//! started it can still be writing its own profile at that moment, and a
//! half-written file fails the merge — `invalid instrumentation profile data
//! (file header is corrupt)` — on a run where every test passed. That failed the
//! v0.3.8 *and* v0.3.9 release gates. `src/bin/` is excluded from coverage
//! anyway, so those profiles have nothing to contribute: sending them elsewhere
//! removes the race rather than making it rarer.
//!
//! This lives in one file that every double reaches by `#[path]`, because the
//! processes that need it are spawned from more than one binary — and a copy per
//! binary is exactly how three of the five spawn sites came to be missing it.

/// The environment a detached child must be spawned with: its coverage profile
/// goes to a temp path, outside the set `cargo llvm-cov` merges.
///
/// Apply this at **every** spawn of a process that can outlive its parent.
pub fn detached_profile() -> [(String, String); 1] {
    let path = std::env::temp_dir().join("onejudge-detached-%p.profraw");
    [("LLVM_PROFILE_FILE".to_string(), path.display().to_string())]
}

/// Record the profile path this process actually inherited, as
/// `<artifact>.profile`, beside the handle/socket/sink `artifact` it is about to
/// publish.
///
/// Written by the detached child rather than claimed by its parent, and written
/// *before* the artifact a test waits on, so reading it is never a race. A
/// parent's claim would survive the very mistake this guards — a spawn site that
/// forgot [`detached_profile`] — and the child's report cannot.
///
/// Best-effort: a marker that cannot be written must never fail the double.
pub fn publish_profile(artifact: &str) {
    let inherited = std::env::var("LLVM_PROFILE_FILE").unwrap_or_default();
    let staging = format!("{artifact}.profile.staging");
    if std::fs::write(&staging, inherited).is_ok() {
        let _ = std::fs::rename(&staging, format!("{artifact}.profile"));
    }
}
