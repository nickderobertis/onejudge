//! The YAML readers held to what real documents read as.
//!
//! Every config and SKILL.md here is a real document — committed to this tree,
//! written by the Python SDK's own suite, written by oneagentgraph for a real run,
//! or the user's own skill — and each `.expected` beside it is the value the crate
//! read from it on `d4f2a15`, the last tree parsing YAML through `serde_yaml_ng`
//! (`tests/golden/README.md`). A reader change that moves what a user's file
//! means fails here, naming the file.

use std::path::{Path, PathBuf};

use onejudge::cli::{Config, STARTER_CONFIG};
use onejudge::load_skill;

fn golden() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

/// What `Config::from_yaml` reads from `text`, rendered whole.
fn read_config(text: &str, origin: &Path) -> String {
    let config = Config::from_yaml(text)
        .unwrap_or_else(|err| panic!("{} no longer reads: {err}", origin.display()));
    format!("{config:#?}\n")
}

fn assert_reads_as(actual: &str, expected: &Path) {
    let want = std::fs::read_to_string(expected)
        .unwrap_or_else(|err| panic!("reading {}: {err}", expected.display()));
    assert!(
        actual == want,
        "{} no longer holds what its document reads as\n--- expected\n{want}\n--- read\n{actual}",
        expected.display()
    );
}

/// Every `*.yaml` in `dir` beside its `.expected`, sorted, so a fixture without
/// an expectation (or the reverse) is a failure rather than a skip.
fn configs_in(dir: &Path) -> Vec<(PathBuf, PathBuf)> {
    let mut configs: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|err| panic!("reading {}: {err}", dir.display()))
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "yaml"))
        .map(|path| {
            let expected = path.with_extension("expected");
            (path, expected)
        })
        .collect();
    configs.sort();
    let expectations = std::fs::read_dir(dir)
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .path()
                .extension()
                .is_some_and(|ext| ext == "expected")
        })
        .count();
    assert_eq!(
        configs.len(),
        expectations,
        "{}: every config needs exactly one .expected",
        dir.display()
    );
    configs
}

fn assert_configs_in(dir: &Path, count: usize) {
    let configs = configs_in(dir);
    assert_eq!(configs.len(), count, "{}", dir.display());
    for (config, expected) in configs {
        let text = std::fs::read_to_string(&config).unwrap();
        assert_reads_as(&read_config(&text, &config), &expected);
    }
}

#[test]
fn committed_configs_read_as_they_did() {
    let readers = golden().join("yaml-readers/committed");
    for name in ["single-judge", "single-judge-control"] {
        let config = golden().join(name).join("config.yaml");
        // Filled the way `tests/cli.rs` fills these templates before a run.
        let text = std::fs::read_to_string(&config)
            .unwrap()
            .replace("{{ECHO}}", r#""/bin/onejudge-echo-provider""#)
            .replace("{{ONEHARNESS}}", r#""/bin/onejudge-fake-oneharness""#)
            .replace("{{AGENT_STORE}}", "/tmp/agent-store")
            .replace("{{JUDGE_STORE}}", "/tmp/judge-store")
            .replace("{{RECORD}}", "/tmp/record");
        assert_reads_as(
            &read_config(&text, &config),
            &readers.join(format!("{name}.expected")),
        );
    }
    // What `onejudge init` writes, and so every config a user started from.
    assert_reads_as(
        &read_config(STARTER_CONFIG, Path::new("cli/starter.yaml")),
        &readers.join("starter.expected"),
    );
}

/// The SDK writes its config as JSON and hands it over as YAML.
#[test]
fn python_sdk_configs_read_as_they_did() {
    assert_configs_in(&golden().join("yaml-readers/sdk"), 10);
}

/// The config each oneagentgraph member reads, the largest among them included.
#[test]
fn oneagentgraph_effective_configs_read_as_they_did() {
    let dir = golden().join("yaml-readers/effective");
    let largest = std::fs::metadata(dir.join("planner-worker.yaml"))
        .unwrap()
        .len();
    assert!(
        largest > 60_000,
        "the largest effective config is {largest} bytes"
    );
    assert_configs_in(&dir, 5);
}

/// The user's own skills: `create-repo` carries front-matter keys `Frontmatter`
/// does not declare, `auto-remote-control` only the declared ones.
#[test]
fn skills_read_as_they_did() {
    let readers = golden().join("yaml-readers/skill");
    for (name, undeclared) in [("auto-remote-control", false), ("create-repo", true)] {
        let dir = readers.join(name);
        let text = std::fs::read_to_string(dir.join("SKILL.md")).unwrap();
        assert_eq!(text.contains("\nmetadata:"), undeclared, "{name}");
        let skill = load_skill(&dir)
            .unwrap_or_else(|err| panic!("{} no longer loads: {err}", dir.display()));
        let read = format!(
            "{:#?}\n",
            (&skill.name, &skill.description, &skill.instructions)
        );
        assert_reads_as(&read, &readers.join(format!("{name}.expected")));
    }
}
