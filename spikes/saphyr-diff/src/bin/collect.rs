//! SPIKE ONLY: copy the real documents outside this repository that the harness
//! reads into fixtures/, read-only at the source. Re-run to refresh; the
//! committed fixtures are what the report's counts are from.
//! Run with `cargo run -q --manifest-path spikes/saphyr-diff/Cargo.toml --bin collect`.

use std::path::{Path, PathBuf};
use std::process::Command;

const HERE: &str = env!("CARGO_MANIFEST_DIR");

/// Every file named `name` under `dir`, skipping build, dependency and git trees.
fn find(dir: &Path, name: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        let skip = ["target", "node_modules", ".git"].iter().any(|s| p.file_name().is_some_and(|f| f == *s));
        match e.file_type() {
            Ok(t) if t.is_dir() && !skip => find(&p, name, out),
            Ok(t) if t.is_file() && p.file_name().is_some_and(|f| f == name) => out.push(p),
            _ => {}
        }
    }
}

/// The first 12 hex digits of the file's md5, as `md5sum | cut -c1-12` names it.
fn md5_prefix(path: &Path) -> String {
    let out = Command::new("md5sum").arg(path).output().expect("spawn md5sum");
    assert!(out.status.success(), "md5sum {}", path.display());
    String::from_utf8_lossy(&out.stdout)[..12].to_owned()
}

fn fresh(dir: &Path) {
    if dir.exists() {
        std::fs::remove_dir_all(dir).unwrap();
    }
    std::fs::create_dir_all(dir).unwrap();
}

fn main() {
    let here = Path::new(HERE);
    let home = PathBuf::from(std::env::var("HOME").expect("HOME"));
    let ws = home.join(".onevcs/workspaces");

    // Effective configs oneagentgraph wrote for real runs (serde_norway-rendered):
    // runs/<run>/…/<member>/onejudge.yaml, named <run>-<member>.yaml.
    let effective = here.join("fixtures/effective");
    fresh(&effective);
    let mut configs = Vec::new();
    find(&home.join(".local/state/oneagentgraph/runs"), "onejudge.yaml", &mut configs);
    configs.sort();
    for f in &configs {
        let member = f.parent().unwrap();
        let run = member.parent().unwrap().parent().unwrap();
        let name = format!(
            "{}-{}.yaml",
            run.file_name().unwrap().to_string_lossy(),
            member.file_name().unwrap().to_string_lossy()
        );
        std::fs::copy(f, effective.join(name)).unwrap();
    }

    // SKILL.md files from the user's own skills (not third-party ones).
    let skill = here.join("fixtures/skill");
    fresh(&skill);
    let mut skills = Vec::new();
    for e in std::fs::read_dir(&ws).into_iter().flatten().flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if n.starts_with("github.com-nickderobertis-skilltest-") || n.starts_with("github.com-nickderobertis-dero-skills-")
        {
            find(&e.path().join("pool/1/worktree"), "SKILL.md", &mut skills);
        }
    }
    for s in ["create-repo", "auto-remote-control"] {
        let p = home.join(".claude/skills").join(s).join("SKILL.md");
        if p.is_file() {
            skills.push(p);
        }
    }
    skills.sort();
    skills.dedup();
    for f in &skills {
        let dir = skill.join(md5_prefix(f));
        if dir.is_dir() {
            continue;
        }
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(f, dir.join("SKILL.md")).unwrap();
        let origin = f.strip_prefix(&home).unwrap_or(f);
        std::fs::write(dir.join("origin"), format!("{}\n", origin.display())).unwrap();
    }
    let mut copied = Vec::new();
    find(&skill, "SKILL.md", &mut copied);
    println!("effective: {}  skill: {}", configs.len(), copied.len());
}
