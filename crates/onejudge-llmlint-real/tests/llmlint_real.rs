//! The real-llmlint tier: drive the built `onejudge run` CLI with a panel whose
//! llmlint judge is the **released `llmlint`** — not the `onejudge-fake-llmlint`
//! double — and read each decision back out of llmlint's own history store.
//!
//! The rest of the suite holds the argv, the labels and the pointer parse to the
//! contract through the double. This is the one place that contract is checked
//! against the executable that defines it: that the labels onejudge passes are
//! ones the release accepts and records, and that the id onejudge parses from its
//! stderr pointer is the id `llmlint history` answers with.
//!
//! No model is called and no credential is needed: the llmlint config's only rule
//! matches no file in the worktree, so llmlint judges nothing, and still records
//! the run. It needs llmlint at or above [`onejudge::LLMLINT_MIN_VERSION`] on
//! `PATH` (`scripts/setup-llmlint.sh` installs it), so it is `#[ignore]`-d out of
//! the offline gate and runs via `just test-llmlint` — in CI, the
//! `llmlint-real` job, which installs llmlint first. An absent llmlint fails it;
//! nothing here skips.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use onejudge_test_doubles as doubles;

const LLMLINT: &str = "llmlint";

/// The base session the run is named after, and every llmlint run labelled with.
const SESSION: &str = "real-llmlint-journey";

/// Emptied first, so a record or config an earlier run left can never answer for
/// this one.
fn fresh_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Only the fields this tier checks; the rest of llmlint's record is ignored, so a
/// field a later llmlint adds does not break the journey.
#[derive(Debug, serde::Deserialize)]
struct HistoryRun {
    id: String,
    #[serde(default)]
    labels: BTreeMap<String, String>,
}

fn history(history: &Path, extra: &[&str]) -> Vec<HistoryRun> {
    let output = Command::new(LLMLINT)
        .args(["history", "--format", "json", "--label"])
        .arg(format!("session={SESSION}"))
        .args(extra)
        .env("LLMLINT_HISTORY_DIR", history)
        .output()
        .expect("llmlint history runs");
    assert!(
        output.status.success(),
        "llmlint history failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("llmlint history --format json is a run list")
}

#[test]
#[ignore = "real llmlint: needs the released llmlint on PATH; run via `just test-llmlint`"]
fn a_panels_llmlint_judge_runs_are_labelled_and_each_decision_names_its_history_record() {
    let version = Command::new(LLMLINT)
        .arg("--version")
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "this tier drives the real llmlint and it could not be run ({e}); install it with \
             scripts/setup-llmlint.sh (llmlint {} or newer)",
                onejudge::LLMLINT_MIN_VERSION
            )
        });
    assert!(version.status.success(), "llmlint --version failed");

    // The worker's tree, holding llmlint's config: one rule, matching no file, so
    // the run judges nothing and calls no model — and is still recorded.
    let worktree = fresh_dir("llmlint-real-worktree");
    std::fs::write(worktree.join("README.md"), "nothing to lint here\n").unwrap();
    std::fs::write(
        worktree.join("llmlint.yml"),
        "rules:\n  - name: never_matches\n    description: Nothing in this tree matches this \
         rule.\n    files:\n      include: [\"**/*.nomatch\"]\n",
    )
    .unwrap();
    let history_dir = fresh_dir("llmlint-real-history");

    // Two judges: the echo reviewer continues until the worker has echoed its
    // "next step" question back (two supervisor turns), and the real llmlint,
    // clean on every run.
    let echo = serde_json::to_string(doubles::echo_provider()).unwrap();
    let config = worktree.join("run.yaml");
    std::fs::write(
        &config,
        format!(
            "provider:\n  kind: split\n  skill:\n    kind: command\n    command: [{echo}]\n  \
             judges:\n    - kind: command\n      label: reviewer\n      command: [{echo}]\n    \
             - kind: llmlint\n      label: lint\n      bin: {LLMLINT}\n      config: llmlint.yml\n\
             task: please commit\nsystem_prompt: Commit it.\nuser:\n  persona: A tester.\n  \
             done_when: next step\n  max_turns: 4\n"
        ),
    )
    .unwrap();
    let output = Command::new(doubles::onejudge_cli())
        .args(["run", config.to_str().unwrap(), "--format", "json"])
        .args(["--session", SESSION])
        .current_dir(&worktree)
        .env("LLMLINT_HISTORY_DIR", &history_dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: onejudge::Report = serde_json::from_slice(&output.stdout).unwrap();

    let decided: Vec<(usize, &onejudge::JudgeDecision)> = report
        .judge_decisions
        .iter()
        .flat_map(|turn| {
            turn.decisions
                .iter()
                .filter(|d| d.kind == "llmlint")
                .map(move |d| (turn.turn, d))
        })
        .collect();
    assert_eq!(
        decided.iter().map(|(turn, _)| *turn).collect::<Vec<_>>(),
        [1, 2],
        "{:#?}",
        report.judge_decisions
    );

    // Every llmlint run of the session, found by llmlint's own label filter: the
    // two supervisor decisions and the final `done_when` re-judge (on turn 2),
    // each labelled with the judge's panel label and the turn it decided on.
    let runs = history(&history_dir, &[]);
    let mut labelled: Vec<(String, String)> = runs
        .iter()
        .map(|run| (run.labels["judge"].clone(), run.labels["turn"].clone()))
        .collect();
    labelled.sort();
    assert_eq!(
        labelled,
        [
            ("lint".to_string(), "1".to_string()),
            ("lint".to_string(), "2".to_string()),
            ("lint".to_string(), "2".to_string()),
        ],
        "{runs:#?}"
    );

    for (turn, decision) in decided {
        let run_id = decision
            .run_id
            .as_deref()
            .unwrap_or_else(|| panic!("turn {turn}'s decision names no run: {decision:#?}"));
        let expected: BTreeMap<String, String> = [
            ("session", SESSION.to_string()),
            ("judge", decision.judge.clone()),
            ("turn", turn.to_string()),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect();
        assert_eq!(decision.labels, expected);

        // The id is one llmlint's history returns for this very turn…
        let this_turn = history(&history_dir, &["--label", &format!("turn={turn}")]);
        assert!(
            this_turn.iter().any(|run| run.id == run_id),
            "turn {turn}'s run_id {run_id} is not among llmlint's runs for it: {this_turn:#?}"
        );
        // …and the record behind it carries exactly the labels the decision says
        // the run was passed.
        let shown = Command::new(LLMLINT)
            .args(["history", run_id, "--format", "json"])
            .env("LLMLINT_HISTORY_DIR", &history_dir)
            .output()
            .unwrap();
        assert!(shown.status.success(), "llmlint history {run_id} failed");
        let record: HistoryRun = serde_json::from_slice(&shown.stdout).unwrap();
        assert_eq!(record.id, run_id);
        assert_eq!(record.labels, decision.labels);
    }
    // Turn 1 had exactly one run, so its decision names that one.
    let turn_one = history(&history_dir, &["--label", "turn=1"]);
    assert_eq!(turn_one.len(), 1);
    assert_eq!(
        report.judge_decisions[0].decisions[1].run_id.as_deref(),
        Some(turn_one[0].id.as_str())
    );
}
