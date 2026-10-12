//! SPIKE ONLY: (1) the dependency closures of serde-saphyr and saphyr, checked for
//! the YAML crates the plan bans; (2) cargo-deny with the repository's deny.toml
//! over a post-move approximation, against the same run over the repository today
//! (the workspace without this harness), so a new duplicate version or refusal is
//! attributable. The approximation is a manifest generated into a temporary
//! directory for the run — the published crate with no YAML feature (so no
//! serde_yaml_ng), clap as the `cli` feature pulls it, and serde-saphyr + saphyr —
//! resolved against the repository's own Cargo.lock, then removed.
//! Run with `cargo run -q --manifest-path spikes/saphyr-diff/Cargo.toml --bin deny_probe`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

const HERE: &str = env!("CARGO_MANIFEST_DIR");
const BANNED: [&str; 7] = [
    "serde_yaml",
    "serde_yaml_ng",
    "serde_norway",
    "serde_yml",
    "unsafe-libyaml",
    "unsafe-libyaml-norway",
    "libyml",
];

/// Runs `cargo <args>` and returns stdout + stderr; a failed run is still read,
/// because cargo-deny exits non-zero on the very findings this probe reports.
fn cargo(args: &[&str]) -> String {
    let out = Command::new("cargo")
        .args(args)
        .output()
        .expect("spawn cargo");
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    s
}

fn tree(manifest: &Path, package: Option<&str>) -> BTreeSet<String> {
    let m = manifest.to_str().unwrap();
    let mut args = vec![
        "tree",
        "-q",
        "--manifest-path",
        m,
        "-e",
        "normal,build",
        "--prefix",
        "none",
    ];
    if let Some(p) = package {
        args.extend(["-p", p]);
    }
    cargo(&args)
        .lines()
        .map(|l| l.replace(" (*)", "").replace(" (proc-macro)", ""))
        .filter(|l| !l.is_empty())
        .collect()
}

fn banned_in<'a>(lines: impl IntoIterator<Item = &'a String>) -> Vec<String> {
    let names: BTreeSet<String> = lines
        .into_iter()
        .filter_map(|l| l.split_whitespace().next())
        .map(str::to_owned)
        .collect();
    names
        .into_iter()
        .filter(|n| BANNED.contains(&n.as_str()))
        .collect()
}

fn deny(manifest: &Path, config: &Path, extra: &[&str]) -> String {
    let mut args = vec!["deny", "--manifest-path", manifest.to_str().unwrap()];
    args.extend_from_slice(extra);
    args.extend(["check", "--config", config.to_str().unwrap()]);
    cargo(&args)
}

fn deny_summary(out: &str) {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for l in out.lines() {
        let t = l.trim_start();
        let summary = ["advisories", "bans", "licenses", "sources"]
            .iter()
            .any(|k| t.starts_with(&format!("{k} ok")) || t.starts_with(&format!("{k} FAILED")));
        let diag = (l.starts_with("error[") || l.starts_with("warning[")) && l.contains("]");
        if summary || diag {
            *counts.entry(l).or_default() += 1;
        }
    }
    for (l, n) in counts {
        println!("{n:>7} {l}");
    }
}

fn duplicates(out: &str) -> BTreeSet<String> {
    let mut v = BTreeSet::new();
    for l in out.lines() {
        let mut rest = l;
        while let Some(i) = rest.find("duplicate entries for crate '") {
            let tail = &rest[i..];
            let start = "duplicate entries for crate '".len();
            if let Some(end) = tail[start..].find('\'') {
                v.insert(tail[..start + end + 1].to_owned());
                rest = &tail[start + end + 1..];
            } else {
                break;
            }
        }
    }
    v
}

fn licences(manifest: &Path) {
    let raw = cargo(&[
        "metadata",
        "-q",
        "--manifest-path",
        manifest.to_str().unwrap(),
        "--format-version",
        "1",
    ]);
    let m: serde_json::Value = serde_json::from_str(
        raw.lines()
            .find(|l| l.starts_with('{'))
            .expect("metadata JSON"),
    )
    .expect("parse metadata");
    let packages: BTreeMap<&str, &serde_json::Value> = m["packages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["id"].as_str().unwrap(), p))
        .collect();
    let nodes: BTreeMap<&str, &serde_json::Value> = m["resolve"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| (n["id"].as_str().unwrap(), n))
        .collect();
    for root in ["serde-saphyr", "saphyr"] {
        let start = packages
            .iter()
            .find(|(_, p)| p["name"] == root)
            .map(|(i, _)| *i)
            .unwrap();
        let (mut seen, mut todo) = (BTreeSet::new(), vec![start]);
        while let Some(i) = todo.pop() {
            if !seen.insert(i) {
                continue;
            }
            for d in nodes[i]["deps"].as_array().unwrap() {
                let normal_or_build = d["dep_kinds"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|k| k["kind"].is_null() || k["kind"] == "build");
                if normal_or_build {
                    todo.push(d["pkg"].as_str().unwrap());
                }
            }
        }
        let lic: BTreeSet<&str> = seen
            .iter()
            .map(|i| packages[i]["license"].as_str().unwrap_or("NONE"))
            .collect();
        println!(
            "   {root}: {} crates; licence expressions: {lic:?}",
            seen.len()
        );
    }
}

/// Writes the post-move approximation into a fresh temporary directory: a manifest
/// whose dependencies are the probe (nothing ever uses them), resolved against a
/// copy of the repository's Cargo.lock so its versions are the workspace's.
fn generate_probe(root: &Path) -> PathBuf {
    let probe = std::env::temp_dir().join(format!("saphyr-deny-probe-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&probe);
    std::fs::create_dir_all(probe.join("src")).expect("create the probe directory");
    let onejudge = root
        .join("crates/onejudge")
        .canonicalize()
        .expect("crates/onejudge exists");
    let manifest = format!(
        "[package]\nname = \"saphyr-deny-probe\"\nversion = \"0.0.0\"\nedition = \"2021\"\npublish = false\nlicense = \"MIT\"\n\n\
         [workspace]\n\n\
         [dependencies]\nonejudge = {{ path = {:?}, default-features = false }}\n\
         clap = {{ version = \"4\", features = [\"derive\"] }}\n\
         serde-saphyr = \"=1.3.0\"\nsaphyr = \"=0.2.1\"\n",
        onejudge.to_str().expect("a UTF-8 path"),
    );
    std::fs::write(probe.join("Cargo.toml"), manifest).expect("write the probe manifest");
    std::fs::write(probe.join("src/main.rs"), "fn main() {}\n").expect("write the probe source");
    std::fs::copy(root.join("Cargo.lock"), probe.join("Cargo.lock"))
        .expect("copy the repository's Cargo.lock");
    probe
}

fn main() {
    let here = PathBuf::from(HERE);
    let root = here.join("../..");
    let probe = generate_probe(&root);
    let probe_manifest = probe.join("Cargo.toml");
    let root_manifest = root.join("Cargo.toml");
    let config = root.join("deny.toml");

    for krate in ["serde-saphyr", "saphyr"] {
        let closure = tree(&probe_manifest, Some(krate));
        println!("== {krate} dependency closure ({} crates):", closure.len());
        for l in &closure {
            println!("   {l}");
        }
        let hits = banned_in(&closure);
        println!(
            "   banned YAML crates in closure: {}",
            if hits.is_empty() {
                "none".into()
            } else {
                hits.join(" ")
            }
        );
    }
    println!("== banned YAML crates anywhere in the post-move approximation:");
    let hits = banned_in(&tree(&probe_manifest, None));
    if hits.is_empty() {
        println!("   none");
    }
    for h in hits {
        println!("{h}");
    }

    let probe_deny = deny(&probe_manifest, &config, &[]);
    let root_deny = deny(&root_manifest, &config, &["--exclude", "saphyr-diff"]);
    println!("== cargo deny (repository deny.toml) over the post-move approximation:");
    deny_summary(&probe_deny);
    println!(
        "== cargo deny (repository deny.toml) over the repository today (without this harness):"
    );
    deny_summary(&root_deny);
    println!(
        "== duplicate-version warnings in the approximation that today's repository does not have:"
    );
    for d in duplicates(&probe_deny).difference(&duplicates(&root_deny)) {
        println!("   {d}");
    }
    println!("== licenses of serde-saphyr / saphyr closures:");
    licences(&probe_manifest);
    std::fs::remove_dir_all(&probe).ok();
}
