//! Helpers shared by the integration suites that drive a **cancellation** across
//! the real subprocess boundary: the scratch paths the test doubles publish
//! through, the liveness check that asks a leaked harness stand-in from *outside*
//! the process tree whether it is still alive, and the POSIX half of the
//! embedder-owned grouping a [`onejudge::SpawnHook`] exists to give back.
//!
//! `e2e.rs` drives these against the engine, `cli.rs` against a
//! [`Plan`](onejudge::cli::Plan) — the same defect reached through the two entry
//! points a consumer has, so the helpers live here rather than being copied.
//!
//! `dead_code` is allowed because each test binary compiles this module
//! separately and uses the subset its own journeys need (and the Windows matrix
//! job compiles the portable half with the `unix` journeys cfg'd out).

#![allow(dead_code)]

/// A unique path under the integration-test tmp dir, removed if it survived an
/// earlier run.
pub fn scratch_path(name: &str) -> std::path::PathBuf {
    let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_file(&path);
    // …and the coverage marker published beside it, so an earlier run's marker can
    // never satisfy this run's `assert_profile_is_detached`.
    let _ = std::fs::remove_file(format!("{}.profile", path.display()));
    path
}

/// A private scratch directory whose `store_within` subdirectory can really
/// address a control socket on **this** platform, created empty and handed back
/// canonicalized.
///
/// A unix socket address is capped at `sockaddr_un.sun_path` — 108 bytes on
/// Linux, 104 on the macOS/BSD lineage — and the platform temp dir does not cost
/// the same everywhere. Linux's is `/tmp` (5 bytes); macOS's is
/// `/var/folders/<ab>/<30-char hash>/T` (49, and 56 once resolved through
/// `/private`), which can leave a store nested under it with no budget left to
/// spell a socket name in. So the root is **measured** rather than assumed, and
/// the first candidate that can carry a socket wins — on Linux that is always
/// the first one, leaving its behaviour byte-identical.
///
/// Two details make the measurement the same one the run will make. It is taken
/// **canonicalized**, because that is the address oneharness finally checks (its
/// own `socket_path` docs call out `/tmp` → `/private/tmp` as the case that
/// lengthens an address after it is built). And it asks for a name long enough to
/// force the digest fallback: a store that can address `<digest>.sock` can
/// address *every* session name, since `socket_file_name` abbreviates any longer
/// one to exactly that.
///
/// The pid keeps two checkouts running the same test apart.
#[cfg(unix)]
fn addressable_store_root(leaf: &str, store_within: &[&str]) -> std::path::PathBuf {
    // `/tmp` is the fallback rather than the default so that a host which points
    // `TMPDIR` somewhere deliberately (a sandbox, a tmpfs) keeps being honoured
    // whenever its budget genuinely stretches to a socket.
    let mut candidates = vec![std::env::temp_dir()];
    if !candidates
        .iter()
        .any(|root| root == std::path::Path::new("/tmp"))
    {
        candidates.push("/tmp".into());
    }

    let mut refusal = None;
    for root in candidates {
        let dir = root.join(leaf);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the session store is creatable");
        let canonical = dir
            .canonicalize()
            .expect("a directory this process just created resolves");
        let store = store_within
            .iter()
            .fold(canonical.clone(), |path, part| path.join(part));
        match oneharness_core::domain::control::socket_path(&store, &"n".repeat(256)) {
            Ok(_) => return canonical,
            Err(too_long) => {
                let _ = std::fs::remove_dir_all(&dir);
                refusal = Some(too_long);
            }
        }
    }
    panic!(
        "no temp root on this host can address a control socket for `{leaf}`: {}",
        refusal.expect("at least one candidate root is always measured")
    );
}

/// A unique, empty oneharness **session store** for a controlled run: the
/// directory its handle and its `control/<name>.sock` live under. Never the
/// platform default, which is the developer's own store.
///
/// Deliberately NOT under `CARGO_TARGET_TMPDIR` like every other scratch path:
/// a target dir nested under a worktree path blows the socket-address budget
/// before the socket name is even appended. See [`addressable_store_root`] for
/// the budget and how the root is chosen against it.
#[cfg(unix)]
pub fn control_store(name: &str) -> std::path::PathBuf {
    // The socket lands directly in this directory's `control/`, so the store is
    // the root itself.
    addressable_store_root(&format!("oj-{name}-{}", std::process::id()), &[])
}

/// Point this test process's **session store** at a private directory, and hand
/// back the store root a controlled run will now use.
///
/// Named for the mutation rather than the value: the load-bearing half is the
/// `XDG_STATE_HOME` it sets, and one caller wants only that.
///
/// The in-process seam gives a caller no way to choose the store — oneharness
/// resolves it from the platform state directory — so the only lever is that
/// directory itself, and `XDG_STATE_HOME` is what resolves it on unix. Safe to
/// set here because the suite runs under `cargo nextest`, which gives every test
/// its own process; and necessary, because the alternative is the developer's own
/// store, where a `control/<session>.sock` keyed only by session name would
/// collide between two checkouts running the same journey.
///
/// Rooted outside `CARGO_TARGET_TMPDIR`, and measured, for the reason
/// [`addressable_store_root`] gives — doubly so here, because oneharness nests
/// its store a further `oneharness/sessions` under the state home, and that
/// nesting is what pushed a macOS runner's address to 120 bytes against a
/// 104-byte budget.
#[cfg(unix)]
pub fn use_private_session_store(name: &str) -> std::path::PathBuf {
    let store_within = ["oneharness", "sessions"];
    let home = addressable_store_root(
        &format!("oj-state-{name}-{}", std::process::id()),
        &store_within,
    );
    std::env::set_var("XDG_STATE_HOME", &home);
    store_within.iter().fold(home, |path, part| path.join(part))
}

/// The `<pid> <port>` the double's harness stand-in published once it was live.
pub fn descendant_handle(path: &std::path::Path) -> (u32, u16) {
    let raw = std::fs::read_to_string(path).expect("the harness stand-in published its handle");
    let (pid, port) = raw
        .trim()
        .split_once(' ')
        .expect("handle is `<pid> <port>`");
    (
        pid.parse().expect("a pid"),
        port.parse().expect("a liveness port"),
    )
}

/// Whether the harness stand-in is still answering on its liveness port — asked
/// from outside the process tree, so it holds however the process died.
pub fn descendant_is_running(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        std::time::Duration::from_millis(200),
    )
    .is_ok()
}

/// Block until `path` exists, failing loudly rather than hanging.
pub fn await_path(path: &std::path::Path, why: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !path.exists() {
        assert!(std::time::Instant::now() < deadline, "{why}");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Assert the detached process that published `artifact` was started with its
/// coverage profile redirected **out of this run's merge set**.
///
/// Every such process records the `LLVM_PROFILE_FILE` it really inherited beside
/// the handle, socket, or sink it publishes (`<artifact>.profile`), written before
/// that artifact so reading it is never a race. A spawn site that forgot the
/// redirect therefore fails here — instead of corrupting a release gate's profile
/// merge weeks later, on a run where every test passed.
pub fn assert_profile_is_detached(artifact: &std::path::Path) {
    let marker = std::path::PathBuf::from(format!("{}.profile", artifact.display()));
    await_path(
        &marker,
        "the detached process never recorded the coverage profile it inherited",
    );
    let recorded = std::fs::read_to_string(&marker).expect("the profile marker is readable");
    let recorded = std::path::PathBuf::from(recorded.trim());
    assert!(
        recorded.parent() == Some(std::env::temp_dir().as_path()),
        "the detached process inherited `{}`, not the temp path it must write to: \
         its spawn site is missing `detached_profile()`",
        recorded.display()
    );
    // Under `cargo llvm-cov` the test process names the merge set itself, so the
    // check can be made against the real directory rather than only the intent.
    if let Ok(ours) = std::env::var("LLVM_PROFILE_FILE") {
        let merge_set = std::path::PathBuf::from(&ours);
        assert_ne!(
            recorded.parent(),
            merge_set.parent(),
            "the detached process writes its profile into the very set `cargo \
             llvm-cov` merges ({ours}), which is the race that fails releases"
        );
    }
}

/// An embedder-owned group per spawned process: onejudge's child becomes its own
/// POSIX process-group leader, so its pid *is* the group id, and everything it
/// goes on to spawn inherits the group. This is the POSIX half of what a Windows
/// embedder does with a job object.
#[cfg(unix)]
#[derive(Default)]
pub struct OwnedProcessGroups {
    /// The group id of every process the hook placed, in spawn order.
    pub groups: std::sync::Mutex<Vec<u32>>,
}

#[cfg(unix)]
impl onejudge::SpawnHook for OwnedProcessGroups {
    fn spawning(
        &self,
        command: &mut std::process::Command,
        _context: &onejudge::SpawnContext<'_>,
    ) -> std::io::Result<()> {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
        Ok(())
    }

    fn spawned(
        &self,
        child: &std::process::Child,
        _context: &onejudge::SpawnContext<'_>,
    ) -> std::io::Result<Option<String>> {
        self.groups.lock().unwrap().push(child.id());
        Ok(Some(format!("pgid:{}", child.id())))
    }
}

/// Terminate an embedder-owned process group the way a `cancel --kill` does:
/// unconditionally, to every member, including descendants the group leader is no
/// longer around to reap.
#[cfg(unix)]
pub fn kill_group(pgid: u32) {
    let pid = i32::try_from(pgid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
        .expect("a real pid");
    let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
}

/// Whether `pid` still names a live (or unreaped) process.
#[cfg(unix)]
pub fn process_exists(pid: u32) -> bool {
    i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
        .is_some_and(|pid| rustix::process::test_kill_process(pid).is_ok())
}
