//! `onejudge-fake-llmlint` — a deterministic stand-in for the `llmlint` CLI, so
//! the `LlmlintProvider` path is exercised end to end (the real argv, a real
//! subprocess, the real exit-code and output reading) without llmlint installed
//! and without a model. The gate needs no llmlint: this double is what it runs
//! against.
//!
//! It answers `--version` with a version line and exit 0 — the probe the provider
//! runs when it is built — and treats every other invocation as a `lint` run.
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
//!
//! Built only under the `fake-provider` feature; never shipped to a consumer.
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
    let lint_runs_so_far = record(&args);

    if args.iter().any(|arg| arg == "--version") {
        println!("llmlint 0.4.1");
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
    let stderr = std::env::var("ONEJUDGE_FAKE_LLMLINT_STDERR")
        .unwrap_or_else(|_| default_stderr.to_string());

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

/// Append this invocation's argv to the record, if one is named, and return how
/// many `lint` runs were recorded before it.
fn record(args: &[String]) -> usize {
    let Ok(path) = std::env::var("ONEJUDGE_FAKE_LLMLINT_ARGV") else {
        return 0;
    };
    if path.is_empty() {
        return 0;
    }
    let so_far = std::fs::read_to_string(&path)
        .map(|recorded| {
            recorded
                .lines()
                .filter(|line| line.starts_with("[\"lint\""))
                .count()
        })
        .unwrap_or(0);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .expect("the argv record is writable");
    let line = serde_json::to_string(args).expect("argv serializes");
    writeln!(file, "{line}").expect("the argv record is writable");
    so_far
}

fn env_int(name: &str) -> Option<i32> {
    std::env::var(name).ok()?.trim().parse().ok()
}
