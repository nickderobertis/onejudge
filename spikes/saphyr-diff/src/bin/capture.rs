//! SPIKE ONLY: regenerate fixtures/captured{,-sdk}/ — every config and SKILL.md
//! front matter this repository's own suites actually parse. Applies
//! capture/capture.patch (an env-gated dump in Config::from_yaml and load_skill),
//! runs the src unit tests and onejudge-cli-e2e under nextest (one process per
//! test, so a capture names its test) and the Python SDK suite against the
//! patched binary, then reverts the patch. Run from a clean tree with
//! `cargo run -q --manifest-path spikes/saphyr-diff/Cargo.toml --bin capture`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const HERE: &str = env!("CARGO_MANIFEST_DIR");

fn run(root: &Path, program: &str, args: &[&str], env: &[(&str, &Path)], quiet: bool) -> bool {
    let mut c = Command::new(program);
    c.args(args).current_dir(root);
    for (k, v) in env {
        c.env(k, v);
    }
    if quiet {
        c.stdout(Stdio::null()).stderr(Stdio::null());
    }
    c.status()
        .unwrap_or_else(|e| panic!("spawn {program}: {e}"))
        .success()
}

/// Reverts the patch however `main` leaves, as the shell `trap … EXIT` did.
struct Revert(PathBuf);

impl Drop for Revert {
    fn drop(&mut self) {
        run(
            &self.0,
            "git",
            &["checkout", "--", "crates/onejudge/src"],
            &[],
            false,
        );
    }
}

fn yaml_count(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|d| {
            d.flatten()
                .filter(|e| e.file_name().to_string_lossy().contains("yaml"))
                .count()
        })
        .unwrap_or(0)
}

fn fresh(dir: &Path) {
    if dir.exists() {
        std::fs::remove_dir_all(dir).unwrap();
    }
    std::fs::create_dir_all(dir).unwrap();
}

fn main() {
    let here = PathBuf::from(HERE);
    let root = here.join("../..").canonicalize().unwrap();
    if !run(
        &root,
        "git",
        &["diff", "--quiet", "--", "crates/onejudge/src"],
        &[],
        false,
    ) {
        eprintln!("crates/onejudge/src is dirty; refusing");
        std::process::exit(1);
    }
    let patch = here.join("capture/capture.patch");
    assert!(
        run(
            &root,
            "git",
            &["apply", patch.to_str().unwrap()],
            &[],
            false
        ),
        "git apply capture.patch"
    );
    let _revert = Revert(root.clone());

    let cap = here.join("fixtures/captured");
    let sdk = here.join("fixtures/captured-sdk");
    fresh(&cap);
    fresh(&sdk);
    let var = "ONEJUDGE_SPIKE_CAPTURE";
    assert!(run(
        &root,
        "cargo",
        &["build", "-q", "-p", "onejudge-test-doubles"],
        &[],
        false
    ));
    assert!(run(
        &root,
        "cargo",
        &[
            "build",
            "-q",
            "-p",
            "onejudge",
            "--features",
            "cli",
            "--bin",
            "onejudge"
        ],
        &[],
        false
    ));
    let lib = [
        "nextest",
        "run",
        "-p",
        "onejudge",
        "--features",
        "cli,skill",
        "--lib",
    ];
    assert!(
        run(&root, "cargo", &lib, &[(var, &cap)], true),
        "the src unit tests failed under the capture patch"
    );
    let e2e = ["nextest", "run", "-p", "onejudge-cli-e2e"];
    assert!(
        run(&root, "cargo", &e2e, &[(var, &cap)], true),
        "onejudge-cli-e2e failed under the capture patch"
    );
    let pythonpath = root.join("python/onejudge-sdk/src");
    let sdk_ok = run(
        &root,
        "uv",
        &[
            "run",
            "--no-project",
            "--python",
            "3.9",
            "--with-requirements",
            "python/onejudge-sdk/requirements-dev.txt",
            "python",
            "-m",
            "unittest",
            "discover",
            "-s",
            "python/onejudge-sdk/test",
            "-p",
            "test_*.py",
        ],
        &[(var, &sdk), ("PYTHONPATH", &pythonpath)],
        true,
    );
    assert!(
        sdk_ok,
        "the Python SDK suite failed against the patched binary"
    );
    println!(
        "config: {}  frontmatter: {}  sdk: {}",
        yaml_count(&cap.join("config")),
        yaml_count(&cap.join("frontmatter")),
        yaml_count(&sdk.join("config"))
    );
}
