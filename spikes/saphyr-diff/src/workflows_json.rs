//! SPIKE ONLY: the `workflows_ng.rs` checkers ported onto `serde_json::Value`
//! (`preserve_order`) read by serde-saphyr — what `crates/onejudge-repo/tests/workflows.rs`
//! becomes. Every edit from the verbatim copy is one of the substitutions listed in
//! `PORT_EDITS` in main.rs; nothing else changes.
#![allow(dead_code, clippy::all)]
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
type Mapping = serde_json::Map<String, Value>;

/// The status-check contexts branch protection requires, by exact name.
// llmlint: ignore[contracts_have_one_source_or_a_drift_gate] Branch protection is only readable through the GitHub API with admin credentials, which this offline gate never has; the live settings are reconciled against this list by the maintainer's governance step, and AGENTS.md's "All gating checks required" names the same set.
const REQUIRED_CONTEXTS: &[&str] = &[
    "check",
    "test-os (macos-latest)",
    "test-os (windows-latest)",
    "msrv",
    "package",
    "commitlint",
    "llmlint",
];

/// The job-level conditions a required job may carry: each is true for every
/// pull request event the workflow's default `pull_request` types deliver.
const PULL_REQUEST_CONDITIONS: &[&str] = &[
    "github.event_name == 'pull_request'",
    "github.event_name == 'pull_request' && github.event.action != 'edited'",
];

const NOTIGNORED_JOB: &str = "suppressions";
/// The gate jobs, and the recipe each hands the selected tier to.
const GATE_JOBS: &[(&str, &str)] = &[("check", "just check"), ("test-os", "just test-fast")];
const TIER_SCRIPT: &str = "node scripts/ci-tier.mjs >> \"$GITHUB_OUTPUT\"";
const LLMLINT_VALIDATE: &str = "just lint-llm-validate --diff-base origin/main";
const LLMLINT_CREDENTIAL: &str = "CLAUDE_CODE_OAUTH_TOKEN";

pub fn workflow_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.github/workflows")
}
fn workflow(name: &str) -> Value {
    let path = workflow_dir().join(name);
    let text =
        fs::read_to_string(&path).unwrap_or_else(|err| panic!("reading {}: {err}", path.display()));
    serde_saphyr::from_str(&text).unwrap_or_else(|err| panic!("parsing {}: {err}", path.display()))
}

fn workflows() -> Vec<(String, Value)> {
    let mut names: Vec<String> = fs::read_dir(workflow_dir())
        .expect("reading .github/workflows")
        .map(|entry| {
            let entry = entry.expect("reading a .github/workflows entry");
            entry.file_name().to_string_lossy().into_owned()
        })
        .filter(|name| name.ends_with(".yml") || name.ends_with(".yaml"))
        .collect();
    names.sort();
    names
        .into_iter()
        .map(|name| {
            let doc = workflow(&name);
            (name, doc)
        })
        .collect()
}

fn jobs(doc: &Value) -> &Mapping {
    doc.get("jobs")
        .and_then(Value::as_object)
        .expect("workflow has a `jobs` mapping")
}

fn text(value: Option<&Value>) -> Option<&str> {
    value.and_then(Value::as_str)
}

/// The job ids a job `needs`, whether written as one id or a list; any other
/// shape is a malformed workflow, refused rather than read as "needs nothing".
fn needs(job: &Value) -> Vec<String> {
    let id = |value: &Value| {
        value
            .as_str()
            .unwrap_or_else(|| panic!("`needs` entry {value:?} is not a job id"))
            .to_owned()
    };
    match job.get("needs") {
        None => Vec::new(),
        Some(Value::Array(many)) => many.iter().map(id).collect(),
        Some(one) => vec![id(one)],
    }
}

/// The contexts a job reports: its `name` (or id), or one per matrix value.
fn contexts(id: &str, job: &Value) -> Vec<String> {
    let base = text(job.get("name")).unwrap_or(id);
    let Some(matrix) = job.get("strategy").and_then(|s| s.get("matrix")) else {
        return vec![base.to_owned()];
    };
    let axes: Vec<(&String, &Value)> = matrix
        .as_object()
        .expect("matrix is a mapping")
        .iter()
        .filter(|(key, _)| !matches!(key.as_str(), "include" | "exclude"))
        .collect();
    assert_eq!(axes.len(), 1, "{id}: only a single-axis matrix is modelled");
    axes[0]
        .1
        .as_array()
        .expect("matrix axis is a list")
        .iter()
        .map(|value| format!("{base} ({})", value.as_str().expect("string matrix value")))
        .collect()
}

/// Why `doc` would leave a pull request without one of `required`, if it would.
fn required_context_violations(doc: &Value, required: &[&str]) -> Vec<String> {
    let mut violations = Vec::new();
    match doc.get("on").and_then(|on| on.get("pull_request")) {
        None => violations.push("ci.yml is not triggered by pull_request".to_owned()),
        Some(Value::Null) => {}
        Some(filter) => violations.push(format!(
            "ci.yml's pull_request trigger is filtered ({filter:?}), so a pull request can go unreported"
        )),
    }
    let jobs = jobs(doc);
    for context in required {
        let reporting: Vec<&str> = jobs
            .iter()
            .filter(|(id, job)| {
                contexts(id.as_str(), job)
                    .iter()
                    .any(|c| c == context)
            })
            .map(|(id, _)| id.as_str())
            .collect();
        let [id] = reporting[..] else {
            violations.push(format!(
                "required context `{context}` is reported by {} jobs, not exactly one",
                reporting.len()
            ));
            continue;
        };
        let mut pending = vec![id.to_owned()];
        let mut seen = Vec::new();
        while let Some(id) = pending.pop() {
            if seen.contains(&id) {
                continue;
            }
            let Some(job) = jobs.get(id.as_str()) else {
                violations.push(format!("`{context}` needs `{id}`, which is not a job"));
                continue;
            };
            match job.get("if") {
                None => {}
                Some(Value::String(condition))
                    if PULL_REQUEST_CONDITIONS.contains(&condition.as_str()) => {}
                Some(condition) => violations.push(format!(
                    "`{context}` depends on job `{id}` whose condition {condition:?} can skip a pull request"
                )),
            }
            pending.extend(needs(job));
            seen.push(id);
        }
    }
    violations
}

/// Why `doc` is not the notignored review-comment workflow, if it is not.
fn notignored_violations(doc: &Value) -> Vec<String> {
    let mut violations = Vec::new();
    let on = doc.get("on").and_then(Value::as_object);
    if on.is_none_or(|on| on.len() != 1 || !on.contains_key("pull_request")) {
        violations.push("notignored must run on pull_request and nothing else".to_owned());
    }
    let permissions = doc.get("permissions").and_then(Value::as_object);
    let expected: Mapping = serde_saphyr::from_str("contents: read\npull-requests: write")
        .expect("literal permissions parse");
    if permissions != Some(&expected) {
        violations.push(format!(
            "notignored permissions must be exactly {expected:?}, got {permissions:?}"
        ));
    }
    let Some(job) = jobs(doc).get(NOTIGNORED_JOB) else {
        violations.push(format!("notignored has no `{NOTIGNORED_JOB}` job"));
        return violations;
    };
    if text(job.get("if"))
        != Some("github.event.pull_request.head.repo.full_name == github.repository")
    {
        violations.push("notignored must skip pull requests from forks".to_owned());
    }
    let steps = job
        .get("steps")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let uses = |step: &Value| text(step.get("uses")).map(str::to_owned);
    let checkout = steps
        .iter()
        .position(|step| uses(step).is_some_and(|u| u.starts_with("actions/checkout@")));
    let action = steps
        .iter()
        .position(|step| uses(step).as_deref() == Some("nickderobertis/notignored@v0"));
    match (checkout, action) {
        (Some(checkout), Some(action)) if checkout < action => {
            let depth = steps[checkout]
                .get("with")
                .and_then(|w| w.get("fetch-depth"));
            if depth.and_then(Value::as_u64) != Some(0) {
                violations.push(
                    "notignored's checkout must fetch full history (fetch-depth: 0)".to_owned(),
                );
            }
        }
        _ => violations
            .push("notignored must check out and then run nickderobertis/notignored@v0".to_owned()),
    }
    violations
}

/// Why the CI `llmlint` job does not validate before it spends a model call.
fn llmlint_job_violations(doc: &Value) -> Vec<String> {
    let mut violations = Vec::new();
    let steps = jobs(doc)
        .get("llmlint")
        .and_then(|job| job.get("steps"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let find = |needle: &str| {
        steps
            .iter()
            .position(|step| text(step.get("run")).is_some_and(|run| run.contains(needle)))
    };
    let (Some(claude), Some(install), Some(validate), Some(model)) = (
        find("@anthropic-ai/claude-code"),
        find("setup-llmlint.sh"),
        find(LLMLINT_VALIDATE),
        find("just lint-llm-diff"),
    ) else {
        violations.push(
            "llmlint job must install claude-code and llmlint, validate, and run lint-llm-diff"
                .to_owned(),
        );
        return violations;
    };
    if !(claude < model && install < validate && validate < model) {
        violations.push(format!(
            "llmlint step order must be toolchain install < validate < model step, got \
             claude={claude} install={install} validate={validate} model={model}"
        ));
    }
    let validate_step = &steps[validate];
    let rendered = serde_saphyr::to_string(validate_step).expect("step serializes");
    if validate_step.get("env").is_some() || rendered.contains("secrets.") {
        violations.push("the llmlint validate step must receive no credential".to_owned());
    }
    let credential = steps[model]
        .get("env")
        .and_then(|env| env.get(LLMLINT_CREDENTIAL));
    if text(credential) != Some("${{ secrets.CLAUDE_CODE_OAUTH_TOKEN }}") {
        violations.push(format!(
            "the llmlint model step must require {LLMLINT_CREDENTIAL}"
        ));
    }
    violations
}

/// Why a gate job would not run the tier `scripts/ci-tier.mjs` selects, if it would
/// not: it must check out full history (the merge base the affected tier is keyed
/// off), record the script's output under a step id, and then run its recipe —
/// unconditionally — with that step's `base` as `NX_BASE` and its `flags` as the
/// recipe's arguments.
fn tier_routing_violations(doc: &Value) -> Vec<String> {
    let mut violations = Vec::new();
    for (id, recipe) in GATE_JOBS {
        let steps = jobs(doc)
            .get(*id)
            .and_then(|job| job.get("steps"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let position = |pred: &dyn Fn(&Value) -> bool| steps.iter().position(pred);
        let checkout = position(&|step| {
            text(step.get("uses")).is_some_and(|u| u.starts_with("actions/checkout@"))
        });
        if checkout
            .and_then(|at| steps[at].get("with"))
            .and_then(|with| with.get("fetch-depth"))
            .and_then(Value::as_u64)
            != Some(0)
        {
            violations.push(format!(
                "`{id}` must check out full history (fetch-depth: 0)"
            ));
        }
        let select = position(&|step| text(step.get("run")) == Some(TIER_SCRIPT));
        let Some(select) = select else {
            violations.push(format!("`{id}` never runs `{TIER_SCRIPT}`"));
            continue;
        };
        let Some(step_id) = text(steps[select].get("id")) else {
            violations.push(format!(
                "`{id}`'s tier step has no `id` to read its output by"
            ));
            continue;
        };
        let run = position(&|step| text(step.get("run")).is_some_and(|run| run.contains(recipe)));
        let Some(run) = run else {
            violations.push(format!("`{id}` never runs `{recipe}`"));
            continue;
        };
        if run < select {
            violations.push(format!("`{id}` runs `{recipe}` before it selects the tier"));
        }
        let env = |key: &str| {
            steps[run]
                .get("env")
                .and_then(|env| env.get(key))
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        if steps[run].get("if").is_some() {
            violations.push(format!(
                "`{id}` must run `{recipe}` on every run, at the selected tier"
            ));
        }
        for (key, output) in [("NX_BASE", "base"), ("FLAGS", "flags")] {
            let expected = format!("${{{{ steps.{step_id}.outputs.{output} }}}}");
            if env(key).as_deref() != Some(expected.as_str()) {
                violations.push(format!("`{id}` must pass {key}: {expected} to `{recipe}`"));
            }
        }
        if !text(steps[run].get("run"))
            .is_some_and(|run| run.contains(&format!("{recipe} ${{FLAGS:+\"$FLAGS\"}}")))
        {
            violations.push(format!(
                "`{id}` must run `{recipe}` with the selected flags"
            ));
        }
    }
    violations
}

fn set(doc: &mut Value, path: &[&str], value: Value) {
    let (last, parents) = path.split_last().expect("non-empty path");
    let mut node = doc;
    for key in parents {
        node = node.get_mut(*key).unwrap_or_else(|| panic!("no `{key}`"));
    }
    node.as_object_mut()
        .expect("mapping")
        .insert((*last).to_owned(), value);
}

fn llmlint_steps(doc: &mut Value) -> &mut Vec<Value> {
    doc.get_mut("jobs")
        .and_then(|jobs| jobs.get_mut("llmlint"))
        .and_then(|job| job.get_mut("steps"))
        .and_then(Value::as_array_mut)
        .expect("llmlint steps")
}


pub fn parse(text: &str) -> Value {
    serde_saphyr::from_str(text).unwrap()
}

/// Every test body of workflows.rs, as (case, the violations its checker returned).
pub fn cases() -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    let ci = workflow("ci.yml");
    out.push(("required/real ci.yml".into(), required_context_violations(&ci, REQUIRED_CONTEXTS)));

    let mut gated = ci.clone();
    set(&mut gated, &["jobs", "package", "needs"], Value::from("cli-binary"));
    set(&mut gated, &["jobs", "cli-binary", "if"], Value::from("github.event_name == 'push'"));
    out.push(("required/gated".into(), required_context_violations(&gated, REQUIRED_CONTEXTS)));

    let mut filtered = ci.clone();
    set(&mut filtered, &["on", "pull_request"], parse("paths: [src/**]"));
    out.push(("required/filtered".into(), required_context_violations(&filtered, REQUIRED_CONTEXTS)));

    let mut disabled = ci.clone();
    set(&mut disabled, &["jobs", "msrv", "if"], Value::Bool(false));
    out.push(("required/disabled".into(), required_context_violations(&disabled, REQUIRED_CONTEXTS)));

    out.push(("required/notignored context".into(), required_context_violations(&ci, &["notignored"])));

    out.push(("notignored/real".into(), notignored_violations(&workflow("notignored.yml"))));
    let mut needing = Vec::new();
    for (file, doc) in workflows() {
        for (id, job) in jobs(&doc) {
            let id = id.as_str().to_owned();
            if needs(job).iter().any(|need| need == NOTIGNORED_JOB) {
                needing.push(format!("{file}: {id}"));
            }
        }
    }
    out.push(("notignored/jobs needing it, all workflows".into(), needing));

    let doc = workflow("notignored.yml");
    let job = |path: &[&str]| -> Vec<String> {
        [&["jobs", NOTIGNORED_JOB][..], path].concat().iter().map(|s| (*s).to_owned()).collect()
    };
    let cases: Vec<(&str, Vec<String>, Value)> = vec![
        ("on", vec!["on".into()], parse("push: {}")),
        ("permissions", vec!["permissions".into()], parse("contents: write\npull-requests: write")),
        ("if", job(&["if"]), Value::from("true")),
        ("shallow", job(&["steps"]), parse("[{uses: actions/checkout@v4}, {uses: nickderobertis/notignored@v0}]")),
        ("no action", job(&["steps"]), parse("[{uses: actions/checkout@v4, with: {fetch-depth: 0}}]")),
    ];
    for (name, path, value) in cases {
        let mut broken = doc.clone();
        let path: Vec<&str> = path.iter().map(String::as_str).collect();
        set(&mut broken, &path, value);
        out.push((format!("notignored/{name}"), notignored_violations(&broken)));
    }

    out.push(("llmlint/real".into(), llmlint_job_violations(&ci)));
    let position = |steps: &[Value], needle: &str| {
        steps.iter().position(|step| text(step.get("run")).is_some_and(|run| run.contains(needle))).expect("step present")
    };
    let mut late = ci.clone();
    let steps = llmlint_steps(&mut late);
    let validate = steps.remove(position(steps, LLMLINT_VALIDATE));
    steps.push(validate);
    out.push(("llmlint/late".into(), llmlint_job_violations(&late)));

    let mut credentialed = ci.clone();
    let steps = llmlint_steps(&mut credentialed);
    let validate = position(steps, LLMLINT_VALIDATE);
    set(&mut steps[validate], &["env"], parse("CLAUDE_CODE_OAUTH_TOKEN: ${{ secrets.CLAUDE_CODE_OAUTH_TOKEN }}"));
    out.push(("llmlint/credentialed".into(), llmlint_job_violations(&credentialed)));

    let mut uncredentialed = ci.clone();
    let steps = llmlint_steps(&mut uncredentialed);
    let model = position(steps, "just lint-llm-diff");
    steps[model].as_object_mut().unwrap().remove("env");
    out.push(("llmlint/uncredentialed".into(), llmlint_job_violations(&uncredentialed)));

    out.push(("tier/real".into(), tier_routing_violations(&ci)));
    let steps = |doc: &Value, id: &str| -> Vec<Value> {
        doc.get("jobs").and_then(|jobs| jobs.get(id)).and_then(|job| job.get("steps")).and_then(Value::as_array).cloned().expect("steps")
    };
    let rewrite = |id: &str, edit: &dyn Fn(&mut Vec<Value>)| {
        let mut doc = ci.clone();
        let mut list = steps(&doc, id);
        edit(&mut list);
        set(&mut doc, &["jobs", id, "steps"], Value::Array(list));
        tier_routing_violations(&doc)
    };
    let find = |list: &[Value], needle: &str| {
        list.iter().position(|step| {
            text(step.get("run")).is_some_and(|run| run.contains(needle)) || text(step.get("uses")).is_some_and(|uses| uses.contains(needle))
        }).expect("step present")
    };
    out.push(("tier/shallow".into(), rewrite("check", &|list| { let at = find(list, "actions/checkout@"); list[at].as_object_mut().unwrap().remove("with"); })));
    out.push(("tier/unrouted".into(), rewrite("test-os", &|list| { let at = find(list, "scripts/ci-tier.mjs"); list.remove(at); })));
    out.push(("tier/unbased".into(), rewrite("check", &|list| { let at = find(list, "just check"); list[at].as_object_mut().unwrap().remove("env"); })));
    out.push(("tier/late".into(), rewrite("check", &|list| { let at = find(list, "scripts/ci-tier.mjs"); let step = list.remove(at); list.push(step); })));
    out.push(("tier/skipped".into(), rewrite("check", &|list| { let at = find(list, "just check"); set(&mut list[at], &["if"], Value::from("github.event_name == 'pull_request'")); })));
    out.push(("tier/flagless".into(), rewrite("test-os", &|list| { let at = find(list, "just test-fast"); set(&mut list[at], &["run"], Value::from("just test-fast")); })));
    out
}

/// Every step of every job of every workflow, as (label, step).
pub fn all_steps() -> Vec<(String, Value)> {
    let mut out = Vec::new();
    for (file, doc) in workflows() {
        for (id, job) in jobs(&doc) {
            let id = id.as_str();
            for (n, step) in job.get("steps").and_then(Value::as_array).cloned().unwrap_or_default().into_iter().enumerate() {
                out.push((format!("{file} jobs.{id}.steps[{n}]"), step));
            }
        }
    }
    out
}

/// The step the gate renders: the llmlint job's validate step.
pub fn validate_step() -> Value {
    let ci = workflow("ci.yml");
    let steps = jobs(&ci).get("llmlint").and_then(|j| j.get("steps")).and_then(Value::as_array).cloned().unwrap();
    steps.into_iter().find(|s| text(s.get("run")).is_some_and(|r| r.contains(LLMLINT_VALIDATE))).unwrap()
}
