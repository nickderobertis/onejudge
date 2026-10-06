//! `onejudge-fake-llmlint` — a deterministic stand-in for the `llmlint` CLI, so
//! the `LlmlintProvider` path is exercised end to end (the real argv, a real
//! subprocess, the real exit-code and output reading) without llmlint installed
//! and without a model. The gate needs no llmlint: this double is what it runs
//! against.
//!
//! It answers `--version` with a version line and exit 0 — the probe the provider
//! runs when it is built — and treats every other invocation as a `lint` run,
//! accepting any argument (`--label` included). Like llmlint, a run that finished
//! ends its stderr with the results pointer (contract C3): ``See full results with
//! `llmlint history <id>` ``, suffixed ` (labels: k=v, …)` — sorted by key —
//! when the run was passed a `--label`.
//! Unlike the other doubles it takes no prompt, so there is nothing to carry a
//! `[[marker]]`; it is scripted **through the environment** it inherits from the
//! test process instead (the suite runs under `cargo nextest`, one process per
//! test, so a test's variables are its own):
//!
//! * `ONEJUDGE_FAKE_LLMLINT_ARGV` — a path; every invocation appends the argv it
//!   was given (after the program name) as one JSON array line, so a test asserts
//!   the exact arguments the provider composed rather than a re-derivation.
//! * `ONEJUDGE_FAKE_LLMLINT_EXIT` — the outcome of each `lint` run in order,
//!   comma-separated, the last one repeating: `0` (every rule holds), `1` (a rule
//!   fails), `2` (could not complete), any other integer, or `signal` (die by
//!   signal — `abort`, so it is an uncatchable death rather than an exit code on
//!   unix). Default `0`. Which run this is counts the `lint` lines already
//!   recorded at `ONEJUDGE_FAKE_LLMLINT_ARGV`, so sequencing needs that set.
//! * `ONEJUDGE_FAKE_LLMLINT_STDOUT` — what a `lint` run writes to stdout in place
//!   of the canned report for its outcome; `none` writes nothing at all (the
//!   protocol-violation case the provider must refuse).
//! * `ONEJUDGE_FAKE_LLMLINT_STDERR` — what a `lint` run writes to stderr.
//! * `ONEJUDGE_FAKE_LLMLINT_VERSION_EXIT` — the exit code `--version` answers with
//!   (default `0`): an executable that is there but is not llmlint.
//! * `ONEJUDGE_FAKE_LLMLINT_VERSION` — the version `--version` reports (default
//!   `0.4.3`, the provider's floor): an llmlint too old to take `--label`.
//! * `ONEJUDGE_FAKE_LLMLINT_POINTER` — when the results pointer is written:
//!   unset, after a run that exited `0` or `1` (llmlint records a run it
//!   finished); `always`, after every run, an incomplete one included; `off`,
//!   never (history disabled). The id is `fake-<judge>-<n>` for a run labelled
//!   `judge=<judge>`, `n` counting that judge's lint runs from 1 — the judges of
//!   a panel run at once, and each judge's own runs are serial, so the id is
//!   deterministic — and `fake-<n>` over every lint run for an unlabelled one.
//!   Both counts read the argv record, so a stable id needs it set.
//!
//! Lives in the `publish = false` `onejudge-test-doubles` crate; never shipped to a
//! consumer.
#![allow(missing_docs)]

use std::io::Write as _;

/// The report a failing run writes when the test scripts none: llmlint's human
/// format in shape — the failing rules with the locations pinned, then the one
/// summary line the provider reads as the reason.
const FAILING_REPORT: &str = "\
FAIL  scripts_are_quiet_on_success
  scripts/release-probe.sh:12: prints a banner on every successful run
  rationale: a script that succeeds should print one line or nothing
1 failed, 3 passed, 0 skipped, 0 not relevant
";

/// The summary line a clean run writes when the test scripts none.
const CLEAN_REPORT: &str = "0 failed, 4 passed, 0 skipped, 0 not relevant\n";

/// What the double writes to stderr when it could not complete and the test
/// scripts nothing — the shape of llmlint's own operational error.
const INCOMPLETE_STDERR: &str = "error: could not resolve harness `claude-code`: not installed\n";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let earlier = record(&args);
    let lint_runs_so_far = earlier.len();

    if args.iter().any(|arg| arg == "--version") {
        let version =
            std::env::var("ONEJUDGE_FAKE_LLMLINT_VERSION").unwrap_or_else(|_| "0.4.3".to_string());
        println!("llmlint {version}");
        std::process::exit(env_int("ONEJUDGE_FAKE_LLMLINT_VERSION_EXIT").unwrap_or(0));
    }

    let outcome = std::env::var("ONEJUDGE_FAKE_LLMLINT_EXIT")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "0".to_string());
    let outcomes: Vec<&str> = outcome.split(',').map(str::trim).collect();
    let outcome = outcomes
        .get(lint_runs_so_far)
        .or_else(|| outcomes.last())
        .copied()
        .unwrap_or("0");

    let default_stdout = match outcome {
        "0" => CLEAN_REPORT,
        "1" => FAILING_REPORT,
        _ => "",
    };
    let default_stderr = match outcome {
        "0" | "1" => "",
        _ => INCOMPLETE_STDERR,
    };
    let stdout = match std::env::var("ONEJUDGE_FAKE_LLMLINT_STDOUT") {
        Ok(text) if text == "none" => String::new(),
        Ok(text) => text,
        Err(_) => default_stdout.to_string(),
    };
    let mut stderr = std::env::var("ONEJUDGE_FAKE_LLMLINT_STDERR")
        .unwrap_or_else(|_| default_stderr.to_string());
    let pointer = match std::env::var("ONEJUDGE_FAKE_LLMLINT_POINTER").as_deref() {
        Ok("off") => false,
        Ok("always") => true,
        _ => matches!(outcome, "0" | "1"),
    };
    if pointer {
        if !stderr.is_empty() && !stderr.ends_with('\n') {
            stderr.push('\n');
        }
        stderr.push_str(&results_pointer(&args, &earlier));
    }

    let mut out = std::io::stdout().lock();
    let _ = out.write_all(stdout.as_bytes());
    let _ = out.flush();
    let mut err = std::io::stderr().lock();
    let _ = err.write_all(stderr.as_bytes());
    let _ = err.flush();

    if outcome == "signal" {
        std::process::abort();
    }
    std::process::exit(outcome.parse().unwrap_or(2));
}

/// llmlint's results pointer for this run, given the `lint` runs recorded before
/// it: the unlabelled form, or the labelled one listing every `--label` the run
/// was passed, sorted by key, the later of a repeated key winning.
fn results_pointer(args: &[String], earlier: &[Vec<String>]) -> String {
    let passed = labels(args);
    let id = match passed.get("judge") {
        Some(judge) => {
            let n = earlier
                .iter()
                .filter(|run| labels(run).get("judge") == Some(judge))
                .count();
            format!("fake-{judge}-{}", n + 1)
        }
        None => format!("fake-{}", earlier.len() + 1),
    };
    let mut line = format!("See full results with `llmlint history {id}`");
    if !passed.is_empty() {
        let listed: Vec<String> = passed.iter().map(|(k, v)| format!("{k}={v}")).collect();
        line.push_str(&format!(" (labels: {})", listed.join(", ")));
    }
    line + "\n"
}

/// Every `--label KEY=VALUE` / `--label=KEY=VALUE` in `args`, the later of a
/// repeated key winning.
fn labels(args: &[String]) -> std::collections::BTreeMap<String, String> {
    let mut labels = std::collections::BTreeMap::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let pair = if arg == "--label" {
            args.next().map(String::as_str)
        } else {
            arg.strip_prefix("--label=")
        };
        if let Some((key, value)) = pair.and_then(|pair| pair.split_once('=')) {
            labels.insert(key.to_string(), value.to_string());
        }
    }
    labels
}

/// Append this invocation's argv to the record, if one is named, and return the
/// `lint` runs recorded before it.
fn record(args: &[String]) -> Vec<Vec<String>> {
    let Ok(path) = std::env::var("ONEJUDGE_FAKE_LLMLINT_ARGV") else {
        return Vec::new();
    };
    if path.is_empty() {
        return Vec::new();
    }
    let so_far = std::fs::read_to_string(&path)
        .map(|recorded| {
            recorded
                .lines()
                .filter(|line| line.starts_with("[\"lint\""))
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect()
        })
        .unwrap_or_default();
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .expect("the argv record is writable");
    // One `write_all` of the whole line, newline included. The judges of a panel
    // run at the same time, so two doubles append to this record at once; append
    // mode keeps each single write whole, but `writeln!` issues the text and its
    // newline as two, which interleave into two arrays on one line.
    let line = serde_json::to_string(args).expect("argv serializes") + "\n";
    file.write_all(line.as_bytes())
        .expect("the argv record is writable");
    so_far
}

fn env_int(name: &str) -> Option<i32> {
    std::env::var(name).ok()?.trim().parse().ok()
}
