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

/// How one scripted run ends: llmlint's own exit code, or death by signal.
#[derive(Clone, Copy)]
enum Outcome {
    Exit(i32),
    Signal,
}

impl Outcome {
    /// One entry of `ONEJUDGE_FAKE_LLMLINT_EXIT`: an exit code, or `signal`.
    fn parse(entry: &str) -> Self {
        if entry == "signal" {
            return Self::Signal;
        }
        entry.parse().map(Self::Exit).unwrap_or_else(|e| {
            fail(&format!(
                "ONEJUDGE_FAKE_LLMLINT_EXIT entry `{entry}` is neither an exit code nor `signal`: {e}"
            ))
        })
    }
}

/// Whether the run writes llmlint's results pointer to stderr.
enum Pointer {
    /// Exactly when llmlint itself does: on a run that completed (exit 0 or 1).
    Default,
    Off,
    Always,
}

impl Pointer {
    fn from_env() -> Self {
        match std::env::var("ONEJUDGE_FAKE_LLMLINT_POINTER").as_deref() {
            Err(std::env::VarError::NotPresent) | Ok("") => Self::Default,
            Ok("off") => Self::Off,
            Ok("always") => Self::Always,
            Ok(other) => fail(&format!(
                "ONEJUDGE_FAKE_LLMLINT_POINTER is `{other}`, not `off` or `always`"
            )),
            Err(e) => fail(&format!("ONEJUDGE_FAKE_LLMLINT_POINTER is unreadable: {e}")),
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // The two invocations the provider makes: the `--version` probe, and
    // `lint …` (whose trailing arguments are the caller's own, passed through).
    let probe = args == ["--version"];
    if !probe && args.first().map(String::as_str) != Some("lint") {
        fail(&format!(
            "expected `--version` or `lint …`, as the provider invokes llmlint, not {args:?}"
        ));
    }
    let earlier = record(&args);
    let lint_runs_so_far = earlier.len();

    if probe {
        let version =
            std::env::var("ONEJUDGE_FAKE_LLMLINT_VERSION").unwrap_or_else(|_| "0.4.3".to_string());
        println!("llmlint {version}");
        std::process::exit(env_int("ONEJUDGE_FAKE_LLMLINT_VERSION_EXIT").unwrap_or(0));
    }

    let scripted = std::env::var("ONEJUDGE_FAKE_LLMLINT_EXIT")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "0".to_string());
    let outcomes: Vec<Outcome> = scripted
        .split(',')
        .map(str::trim)
        .map(Outcome::parse)
        .collect();
    let outcome = outcomes
        .get(lint_runs_so_far)
        .or_else(|| outcomes.last())
        .copied()
        .unwrap_or(Outcome::Exit(0));
    let completed = matches!(outcome, Outcome::Exit(0 | 1));

    let default_stdout = match outcome {
        Outcome::Exit(0) => CLEAN_REPORT,
        Outcome::Exit(1) => FAILING_REPORT,
        _ => "",
    };
    let default_stderr = if completed { "" } else { INCOMPLETE_STDERR };
    let stdout = match std::env::var("ONEJUDGE_FAKE_LLMLINT_STDOUT") {
        Ok(text) if text == "none" => String::new(),
        Ok(text) => text,
        Err(_) => default_stdout.to_string(),
    };
    let mut stderr = std::env::var("ONEJUDGE_FAKE_LLMLINT_STDERR")
        .unwrap_or_else(|_| default_stderr.to_string());
    let pointer = match Pointer::from_env() {
        Pointer::Off => false,
        Pointer::Always => true,
        Pointer::Default => completed,
    };
    if pointer {
        if !stderr.is_empty() && !stderr.ends_with('\n') {
            stderr.push('\n');
        }
        stderr.push_str(&results_pointer(&args, &earlier));
    }

    let mut out = std::io::stdout().lock();
    out.write_all(stdout.as_bytes())
        .and_then(|()| out.flush())
        .unwrap_or_else(|e| fail(&format!("could not write the report to stdout: {e}")));
    let mut err = std::io::stderr().lock();
    err.write_all(stderr.as_bytes())
        .and_then(|()| err.flush())
        .unwrap_or_else(|e| fail(&format!("could not write to stderr: {e}")));

    match outcome {
        Outcome::Signal => std::process::abort(),
        Outcome::Exit(code) => std::process::exit(code),
    }
}

/// Report a scripting or I/O error and exit 2 — llmlint's own "could not run"
/// code, which the provider classifies as an incomplete run, never a pass.
fn fail(message: &str) -> ! {
    eprintln!("onejudge-fake-llmlint: {message}");
    std::process::exit(2);
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
            Some(args.next().map_or("", String::as_str))
        } else {
            arg.strip_prefix("--label=")
        };
        if let Some(pair) = pair {
            let Some((key, value)) = pair.split_once('=').filter(|(key, _)| !key.is_empty()) else {
                fail(&format!(
                    "`--label {pair}` is not KEY=VALUE, as llmlint requires"
                ));
            };
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
    let recorded = match std::fs::read_to_string(&path) {
        Ok(recorded) => recorded,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => fail(&format!("could not read the argv record {path}: {e}")),
    };
    let so_far = recorded
        .lines()
        .filter(|line| line.starts_with("[\"lint\""))
        .map(|line| {
            serde_json::from_str(line).unwrap_or_else(|e| {
                fail(&format!(
                    "the argv record {path} holds a corrupt line `{line}`: {e}"
                ))
            })
        })
        .collect();
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .unwrap_or_else(|e| fail(&format!("could not open the argv record {path}: {e}")));
    // One `write_all` of the whole line, newline included. The judges of a panel
    // run at the same time, so two doubles append to this record at once; append
    // mode keeps each single write whole, but `writeln!` issues the text and its
    // newline as two, which interleave into two arrays on one line.
    let line = serde_json::Value::from(args.to_vec()).to_string() + "\n";
    file.write_all(line.as_bytes())
        .unwrap_or_else(|e| fail(&format!("could not write the argv record {path}: {e}")));
    so_far
}

/// The integer `name` scripts, if it is set: one that is set but not an integer is
/// a scripting error, not the default.
fn env_int(name: &str) -> Option<i32> {
    let value = std::env::var(name).ok()?;
    Some(
        value
            .trim()
            .parse()
            .unwrap_or_else(|e| fail(&format!("{name} is `{value}`, not an integer: {e}"))),
    )
}
