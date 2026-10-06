//! The deterministic test doubles every onejudge suite drives as real
//! subprocesses, and the one way those suites find them.
//!
//! The four doubles are this crate's `[[bin]]`s (`src/bin/`). A suite in another
//! package cannot name them through `env!("CARGO_BIN_EXE_…")` — Cargo sets those
//! only for the bins of the package that owns the test — so [`echo_provider`]
//! and its siblings resolve the freshly built executable instead: from
//! `ONEJUDGE_TEST_DOUBLES_DIR` when the gate names the directory it built them
//! into, and otherwise from the profile directory the running test binary itself
//! was built into, which is where `cargo build -p onejudge-test-doubles` (the
//! `onejudge-test-doubles:build` target every consuming `test` depends on) puts
//! them.
//!
//! [`support`] holds the helpers more than one suite needs. Never published.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

pub mod support;

/// The environment variable naming the directory the doubles were built into.
/// The coverage run sets it: its suites are built into the instrumented target
/// directory, while the doubles stay the uninstrumented ones
/// `onejudge-test-doubles:build` produced.
pub const DOUBLES_DIR_ENV: &str = "ONEJUDGE_TEST_DOUBLES_DIR";

/// `onejudge-echo-provider`: speaks the `CommandProvider` JSON-lines protocol.
pub fn echo_provider() -> &'static str {
    double("onejudge-echo-provider")
}

/// `onejudge-fake-oneharness`: emulates `oneharness run --format json`.
pub fn fake_oneharness() -> &'static str {
    double("onejudge-fake-oneharness")
}

/// `onejudge-fake-harness`: a harness CLI for the in-process engine to spawn.
pub fn fake_harness() -> &'static str {
    double("onejudge-fake-harness")
}

/// `onejudge-fake-llmlint`: stands in for the `llmlint` CLI.
pub fn fake_llmlint() -> &'static str {
    double("onejudge-fake-llmlint")
}

/// The `onejudge` CLI, built into the same profile directory as the running
/// test binary: by `onejudge:build`, or instrumented beside the suite under
/// coverage, so the lines it runs count toward the library's floor.
pub fn onejudge_cli() -> &'static str {
    resolve(&profile_dir(), "onejudge")
}

fn double(name: &str) -> &'static str {
    let dir = std::env::var_os(DOUBLES_DIR_ENV).map_or_else(profile_dir, PathBuf::from);
    resolve(&dir, name)
}

/// `<target>/<profile>`: the running test binary lives in its `deps/`.
fn profile_dir() -> PathBuf {
    let exe = std::env::current_exe().expect("the running test binary has a path");
    exe.parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| panic!("{} is not under <target>/<profile>/deps", exe.display()))
        .to_path_buf()
}

/// The built executable `name` under `dir`, as the `&'static str` the
/// `env!("CARGO_BIN_EXE_…")` it replaces was. Resolved once per name and kept,
/// so the few hundred journeys that ask share one allocation each.
///
/// # Panics
///
/// When it has not been built: a stale or missing binary would make a journey
/// assert against the wrong program, so it fails naming the build instead.
fn resolve(dir: &Path, name: &str) -> &'static str {
    static RESOLVED: OnceLock<Mutex<HashMap<PathBuf, &'static str>>> = OnceLock::new();
    let path = dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    let mut resolved = RESOLVED.get_or_init(Mutex::default).lock().unwrap();
    if let Some(found) = resolved.get(&path) {
        return found;
    }
    assert!(
        path.is_file(),
        "{} is not built — run the suite through its Nx `test` target (`just test-e2e`, \
         `just check`), which builds it first, or build it with \
         `cargo build -p onejudge-test-doubles` / `cargo build -p onejudge --features cli --bin onejudge`",
        path.display()
    );
    let found: &'static str = Box::leak(path.display().to_string().into_boxed_str());
    resolved.insert(path, found);
    found
}
