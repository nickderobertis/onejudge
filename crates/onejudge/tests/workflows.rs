//! Contract gate for the CI wiring a pull request depends on.
//!
//! Branch protection names status-check contexts by string, so a renamed job, a
//! path filter, or a `needs` edge onto a job that skips leaves a required context
//! unreported and every pull request blocked — and nothing in the workflow files
//! says so. Three properties are held here, each by a checker that returns its
//! violations, and each checker is also driven against a mutated workflow to show
//! it refuses the break it exists for:
//!
//! - every required context is defined by a job that reports on every pull request;
//! - `notignored.yml` is the review-comment workflow (pull_request trigger, least
//!   privilege, fork guard, full history) and nothing required waits on it;
//! - the `llmlint` job validates its config with no credential *before* the step
//!   that calls the model, and that step still requires its credential.

use std::fs;
use std::path::{Path, PathBuf};

use serde_yaml_ng::{Mapping, Value};

/// The status-check contexts branch protection requires, by exact name.
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
const LLMLINT_VALIDATE: &str = "just lint-llm-validate --diff-base origin/main";
const LLMLINT_CREDENTIAL: &str = "CLAUDE_CODE_OAUTH_TOKEN";

fn workflow_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("onejudge package should be nested under the workspace root")
        .join(".github/workflows")
}

fn workflow(name: &str) -> Value {
    let path = workflow_dir().join(name);
    let text =
        fs::read_to_string(&path).unwrap_or_else(|err| panic!("reading {}: {err}", path.display()));
    serde_yaml_ng::from_str(&text).unwrap_or_else(|err| panic!("parsing {}: {err}", path.display()))
}

/// Every committed workflow, by file name.
fn workflows() -> Vec<(String, Value)> {
    let mut names: Vec<String> = fs::read_dir(workflow_dir())
        .expect("reading .github/workflows")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
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
        .and_then(Value::as_mapping)
        .expect("workflow has a `jobs` mapping")
}

fn text(value: Option<&Value>) -> Option<&str> {
    value.and_then(Value::as_str)
}

/// The job ids a job `needs`, whether written as one id or a list.
fn needs(job: &Value) -> Vec<String> {
    match job.get("needs") {
        Some(Value::String(one)) => vec![one.clone()],
        Some(Value::Sequence(many)) => many
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

/// The contexts a job reports: its `name` (or id), or one per matrix value.
fn contexts(id: &str, job: &Value) -> Vec<String> {
    let base = text(job.get("name")).unwrap_or(id);
    let Some(matrix) = job.get("strategy").and_then(|s| s.get("matrix")) else {
        return vec![base.to_owned()];
    };
    let axes: Vec<(&Value, &Value)> = matrix
        .as_mapping()
        .expect("matrix is a mapping")
        .iter()
        .filter(|(key, _)| !matches!(key.as_str(), Some("include" | "exclude")))
        .collect();
    assert_eq!(axes.len(), 1, "{id}: only a single-axis matrix is modelled");
    axes[0]
        .1
        .as_sequence()
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
                contexts(id.as_str().unwrap_or_default(), job)
                    .iter()
                    .any(|c| c == context)
            })
            .map(|(id, _)| id.as_str().unwrap_or_default())
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
            if let Some(condition) =
                text(job.get("if")).filter(|c| !PULL_REQUEST_CONDITIONS.contains(c))
            {
                violations.push(format!(
                    "`{context}` depends on job `{id}` whose condition `{condition}` can skip a pull request"
                ));
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
    let on = doc.get("on").and_then(Value::as_mapping);
    if on.is_none_or(|on| on.len() != 1 || !on.contains_key("pull_request")) {
        violations.push("notignored must run on pull_request and nothing else".to_owned());
    }
    let permissions = doc.get("permissions").and_then(Value::as_mapping);
    let expected: Mapping = serde_yaml_ng::from_str("contents: read\npull-requests: write")
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
        .and_then(Value::as_sequence)
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
        .and_then(Value::as_sequence)
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
    let rendered = serde_yaml_ng::to_string(validate_step).expect("step serializes");
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

fn set(doc: &mut Value, path: &[&str], value: Value) {
    let (last, parents) = path.split_last().expect("non-empty path");
    let mut node = doc;
    for key in parents {
        node = node.get_mut(*key).unwrap_or_else(|| panic!("no `{key}`"));
    }
    node.as_mapping_mut()
        .expect("mapping")
        .insert(Value::from(*last), value);
}

fn llmlint_steps(doc: &mut Value) -> &mut Vec<Value> {
    doc.get_mut("jobs")
        .and_then(|jobs| jobs.get_mut("llmlint"))
        .and_then(|job| job.get_mut("steps"))
        .and_then(Value::as_sequence_mut)
        .expect("llmlint steps")
}

#[test]
fn every_required_context_reports_on_every_pull_request() {
    let violations = required_context_violations(&workflow("ci.yml"), REQUIRED_CONTEXTS);
    assert!(violations.is_empty(), "{violations:#?}");
}

#[test]
fn the_required_context_check_refuses_a_skipped_or_missing_context() {
    let ci = workflow("ci.yml");

    let mut gated = ci.clone();
    set(
        &mut gated,
        &["jobs", "package", "needs"],
        Value::from("python-sdk"),
    );
    set(
        &mut gated,
        &["jobs", "python-sdk", "if"],
        Value::from("github.event_name == 'push'"),
    );
    let violations = required_context_violations(&gated, REQUIRED_CONTEXTS);
    assert!(
        violations
            .iter()
            .any(|v| v.contains("`package` depends on job `python-sdk`")),
        "{violations:#?}"
    );

    let mut filtered = ci.clone();
    set(
        &mut filtered,
        &["on", "pull_request"],
        serde_yaml_ng::from_str("paths: [src/**]").unwrap(),
    );
    let violations = required_context_violations(&filtered, REQUIRED_CONTEXTS);
    assert!(
        violations.iter().any(|v| v.contains("trigger is filtered")),
        "{violations:#?}"
    );

    let violations = required_context_violations(&ci, &["notignored"]);
    assert!(
        violations.iter().any(|v| v.contains("reported by 0 jobs")),
        "{violations:#?}"
    );
}

#[test]
fn notignored_is_an_unrequired_review_comment_workflow() {
    let violations = notignored_violations(&workflow("notignored.yml"));
    assert!(violations.is_empty(), "{violations:#?}");
    for context in REQUIRED_CONTEXTS {
        assert!(!context.starts_with(NOTIGNORED_JOB) && *context != "notignored");
    }
    for (file, doc) in workflows() {
        for (id, job) in jobs(&doc) {
            assert!(
                !needs(job).iter().any(|need| need == NOTIGNORED_JOB) || file == "notignored.yml" && id.as_str() == Some(NOTIGNORED_JOB),
                "{file}: job {id:?} needs the notignored job, so a fork pull request would leave it unreported"
            );
        }
    }
}

#[test]
fn the_notignored_check_refuses_each_broken_property() {
    let doc = workflow("notignored.yml");
    let job = |path: &[&str]| -> Vec<String> {
        [&["jobs", NOTIGNORED_JOB][..], path]
            .concat()
            .iter()
            .map(|s| (*s).to_owned())
            .collect()
    };
    let cases: Vec<(Vec<String>, Value, &str)> = vec![
        (
            vec!["on".into()],
            serde_yaml_ng::from_str("push: {}").unwrap(),
            "run on pull_request",
        ),
        (
            vec!["permissions".into()],
            serde_yaml_ng::from_str("contents: write\npull-requests: write").unwrap(),
            "permissions must be exactly",
        ),
        (
            job(&["if"]),
            Value::from("true"),
            "skip pull requests from forks",
        ),
        (
            job(&["steps"]),
            serde_yaml_ng::from_str(
                "[{uses: actions/checkout@v4}, {uses: nickderobertis/notignored@v0}]",
            )
            .unwrap(),
            "fetch-depth: 0",
        ),
        (
            job(&["steps"]),
            serde_yaml_ng::from_str("[{uses: actions/checkout@v4, with: {fetch-depth: 0}}]")
                .unwrap(),
            "nickderobertis/notignored@v0",
        ),
    ];
    for (path, value, expected) in cases {
        let mut broken = doc.clone();
        let path: Vec<&str> = path.iter().map(String::as_str).collect();
        set(&mut broken, &path, value);
        let violations = notignored_violations(&broken);
        assert!(
            violations.iter().any(|v| v.contains(expected)),
            "{path:?}: {violations:#?}"
        );
    }
}

#[test]
fn llmlint_validates_without_a_credential_before_the_model_step() {
    let violations = llmlint_job_violations(&workflow("ci.yml"));
    assert!(violations.is_empty(), "{violations:#?}");
}

#[test]
fn the_llmlint_check_refuses_a_late_or_credentialed_validate_step() {
    let ci = workflow("ci.yml");
    let position = |steps: &[Value], needle: &str| {
        steps
            .iter()
            .position(|step| text(step.get("run")).is_some_and(|run| run.contains(needle)))
            .expect("step present")
    };

    let mut late = ci.clone();
    let steps = llmlint_steps(&mut late);
    let validate = steps.remove(position(steps, LLMLINT_VALIDATE));
    steps.push(validate);
    let violations = llmlint_job_violations(&late);
    assert!(
        violations.iter().any(|v| v.contains("step order")),
        "{violations:#?}"
    );

    let mut credentialed = ci.clone();
    let steps = llmlint_steps(&mut credentialed);
    let validate = position(steps, LLMLINT_VALIDATE);
    set(
        &mut steps[validate],
        &["env"],
        serde_yaml_ng::from_str("CLAUDE_CODE_OAUTH_TOKEN: ${{ secrets.CLAUDE_CODE_OAUTH_TOKEN }}")
            .unwrap(),
    );
    let violations = llmlint_job_violations(&credentialed);
    assert!(
        violations.iter().any(|v| v.contains("no credential")),
        "{violations:#?}"
    );

    let mut uncredentialed = ci.clone();
    let steps = llmlint_steps(&mut uncredentialed);
    let model = position(steps, "just lint-llm-diff");
    steps[model].as_mapping_mut().unwrap().remove("env");
    let violations = llmlint_job_violations(&uncredentialed);
    assert!(
        violations.iter().any(|v| v.contains("must require")),
        "{violations:#?}"
    );
}
