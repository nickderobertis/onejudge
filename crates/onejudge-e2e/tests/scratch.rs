//! Concurrent processes of one test binary never share a scratch file.
//!
//! The note journeys hold a turn open on a marker file and release it with the
//! file beside it, both under [`support::scratch_path`]. When that path was the
//! same for every process over one target dir, a second process running the same
//! journey found the first's marker and queued its note before its own turn had
//! opened, and each removed the other's files as an earlier run's. This drives two
//! real processes of this binary at once — each the [`hold_probe`] below — and
//! asserts neither can see, satisfy or remove the other's hold.

use onejudge_test_doubles::support;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The coordination directory and role a [`hold_probe`] child is handed.
const COORD: &str = "ONEJUDGE_SCRATCH_PROBE_COORD";
const ROLE: &str = "ONEJUDGE_SCRATCH_PROBE_ROLE";

fn release_of(marker: &Path) -> PathBuf {
    PathBuf::from(format!("{}.release", marker.display()))
}

/// Run as a child of [`two_processes_of_one_test_hold_and_release_their_own_scratch`]
/// only: alone it has no coordination directory to report to.
#[test]
#[ignore = "a child process of two_processes_of_one_test_hold_and_release_their_own_scratch"]
fn hold_probe() {
    let coord = PathBuf::from(std::env::var(COORD).expect("spawned with a coordination dir"));
    let role = std::env::var(ROLE).expect("spawned with a role");
    match role.as_str() {
        // Opens its hold and releases it, the way a journey does, then waits for
        // the other process to take its own scratch path of the same name.
        "first" => {
            let marker = support::scratch_path("scratch-probe.held");
            std::fs::write(&marker, b"held\n").unwrap();
            std::fs::write(release_of(&marker), b"go\n").unwrap();
            std::fs::write(coord.join("first.path"), marker.display().to_string()).unwrap();
            support::await_path(&coord.join("second.done"), "the second process never ran");
            assert!(
                marker.exists() && release_of(&marker).exists(),
                "another process's scratch_path removed this process's hold {}",
                marker.display()
            );
        }
        "second" => {
            let first = coord.join("first.path");
            support::await_path(&first, "the first process never opened its hold");
            let marker = support::scratch_path("scratch-probe.held");
            std::fs::write(coord.join("second.path"), marker.display().to_string()).unwrap();
            let held_elsewhere = !marker.exists();
            let released_elsewhere = !release_of(&marker).exists();
            std::fs::write(coord.join("second.done"), b"done\n").unwrap();
            assert!(
                held_elsewhere,
                "another process's hold marker already stands at {}, so this process's \
                 wait for its own turn would end before that turn opened",
                marker.display()
            );
            assert!(
                released_elsewhere,
                "another process's release already stands beside {}, so this process's \
                 hold would be skipped",
                marker.display()
            );
        }
        other => panic!("unknown probe role {other}"),
    }
}

#[test]
fn two_processes_of_one_test_hold_and_release_their_own_scratch() {
    let coord = support::scratch_path("scratch-probe-coord");
    let _ = std::fs::remove_dir_all(&coord);
    std::fs::create_dir_all(&coord).unwrap();
    let exe = std::env::current_exe().unwrap();
    let spawn = |role: &str| {
        Command::new(&exe)
            .args(["--exact", "hold_probe", "--ignored", "--nocapture"])
            .env(COORD, &coord)
            .env(ROLE, role)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the test binary re-executes itself")
    };
    let children = [("first", spawn("first")), ("second", spawn("second"))];
    for (role, child) in children {
        let out = child.wait_with_output().unwrap();
        let said = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "the {role} process failed:\n{said}");
        // libtest exits 0 when a filter matches nothing; the probe must have run.
        assert!(
            said.contains("1 passed"),
            "the {role} probe never ran:\n{said}"
        );
    }
    let path = |role: &str| std::fs::read_to_string(coord.join(format!("{role}.path"))).unwrap();
    assert_ne!(
        path("first"),
        path("second"),
        "two concurrent processes were handed one scratch path"
    );
}
