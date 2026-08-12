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
    path
}

/// A unique, empty oneharness **session store** for a controlled run: the
/// directory its handle and its `control/<name>.sock` live under. Never the
/// platform default, which is the developer's own store.
///
/// Deliberately NOT under `CARGO_TARGET_TMPDIR` like every other scratch path: a
/// unix socket address is capped at ~100 bytes (`SUN_LEN`), and a target dir
/// nested under a worktree path blows that before the socket name is even
/// appended. The pid keeps two checkouts running the same test apart.
pub fn control_store(name: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("oj-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("the session store is creatable");
    path
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
