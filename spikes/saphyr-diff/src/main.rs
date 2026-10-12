//! SPIKE ONLY — differential harness: this repository's real YAML through
//! `serde_yaml_ng` 0.10 (today) and `serde-saphyr` 1.3 (+ `saphyr` 0.2 for the
//! schema-less walk), printing every difference in what is read, written and
//! what errors say. Run with `spikes/saphyr-diff/run.sh`.

mod workflows_json;
mod workflows_ng;

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::path::{Path, PathBuf};
use std::time::Instant;

use onejudge::cli::{CliError, Config};
use onejudge::Frontmatter;
use serde::de::DeserializeOwned;
use serde_json::Value as Json;
use serde_saphyr::{DuplicateKeyPolicy, MergeKeyPolicy, Options, SerializerOptions};

const HERE: &str = env!("CARGO_MANIFEST_DIR");

fn repo() -> PathBuf {
    Path::new(HERE).join("../..")
}

// ---------------------------------------------------------------- corpus ----

struct Doc {
    kind: &'static str,
    name: String,
    text: String,
}

fn read_dir_sorted(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == ext))
        .collect();
    v.sort();
    v
}

/// The first test that parsed a captured document, and which suite it came from.
fn origin(path: &Path) -> (String, String) {
    let text = std::fs::read_to_string(path.with_extension("origin")).unwrap_or_default();
    let mut suites = std::collections::BTreeSet::new();
    let mut first_test = None;
    for line in text.lines() {
        let (exe, thread) = line.split_once('\t').unwrap_or((line, ""));
        let suite = if exe == "onejudge" {
            "cli.rs (spawned binary)"
        } else if exe.starts_with("cli-") {
            "cli.rs (in-process)"
        } else if exe.starts_with("onejudge-") {
            "src unit tests"
        } else {
            "?"
        };
        suites.insert(suite);
        if thread != "main" && first_test.is_none() {
            first_test = Some(thread.to_owned());
        }
    }
    (
        suites.into_iter().collect::<Vec<_>>().join(" + "),
        first_test.unwrap_or_else(|| "(spawned binary)".into()),
    )
}

fn config_corpus() -> Vec<Doc> {
    let mut docs = Vec::new();
    for dir in ["single-judge", "single-judge-control"] {
        let p = repo().join(format!("crates/onejudge-cli-e2e/tests/golden/{dir}/config.yaml"));
        // The placeholders substituted as cli.rs's golden replays do (a JSON-encoded
        // binary path, plain store/record paths).
        let text = read(&p)
            .replace("{{ECHO}}", "\"/opt/doubles/onejudge-echo-provider\"")
            .replace("{{ONEHARNESS}}", "\"/opt/doubles/onejudge-fake-oneharness\"")
            .replace("{{AGENT_STORE}}", "/tmp/agent-store")
            .replace("{{JUDGE_STORE}}", "/tmp/judge-store")
            .replace("{{RECORD}}", "/tmp/record.jsonl");
        assert!(!text.contains("{{"), "{dir}: unsubstituted placeholder");
        docs.push(Doc { kind: "config/golden", name: format!("golden/{dir}/config.yaml"), text });
    }
    docs.push(Doc {
        kind: "config/starter",
        name: "cli/starter.yaml".into(),
        text: read(&repo().join("crates/onejudge/src/cli/starter.yaml")),
    });
    for p in read_dir_sorted(&Path::new(HERE).join("fixtures/captured/config"), "yaml") {
        let (suites, test) = origin(&p);
        let kind = if suites.contains("src unit tests") && !suites.contains("cli.rs") {
            "config/unit-tests"
        } else {
            "config/cli.rs"
        };
        docs.push(Doc {
            kind,
            name: format!("{} [{suites}; {test}]", p.file_name().unwrap().to_string_lossy()),
            text: read(&p),
        });
    }
    for p in read_dir_sorted(&Path::new(HERE).join("fixtures/captured-sdk/config"), "yaml") {
        docs.push(Doc {
            kind: "config/python-sdk-json",
            name: p.file_name().unwrap().to_string_lossy().into_owned(),
            text: read(&p),
        });
    }
    for p in read_dir_sorted(&Path::new(HERE).join("fixtures/effective"), "yaml") {
        docs.push(Doc {
            kind: "config/oneagentgraph-effective",
            name: p.file_name().unwrap().to_string_lossy().into_owned(),
            text: read(&p),
        });
    }
    for (name, text) in SYNTHETIC_CONFIGS {
        docs.push(Doc { kind: "config/edge+malformed", name: (*name).into(), text: (*text).into() });
    }
    docs
}

/// Edge and malformed configs: each is a shape a real config could take, chosen
/// where YAML 1.1/1.2 resolution, serde coercion or the parsers' strictness differ.
const SYNTHETIC_CONFIGS: &[(&str, &str)] = &[
    ("unknown key", "task: x\ntsak: y\n"),
    ("unknown nested key", "task: x\nuser:\n  persnoa: p\n"),
    ("misplaced provider key", "provider:\n  kind: command\n  label: [a]\n"),
    ("max_turns word", "task: x\nuser:\n  max_turns: three\n"),
    ("max_turns negative", "user:\n  max_turns: -1\n"),
    ("max_turns float", "user:\n  max_turns: 1.5\n"),
    ("max_turns quoted", "user:\n  max_turns: \"3\"\n"),
    ("max_turns hex", "user:\n  max_turns: 0x10\n"),
    ("max_turns 0o octal", "user:\n  max_turns: 0o10\n"),
    ("max_turns leading zero", "user:\n  max_turns: 010\n"),
    ("max_turns underscore", "user:\n  max_turns: 1_000\n"),
    ("max_turns plus sign", "user:\n  max_turns: +4\n"),
    ("max_turns overflow u32", "user:\n  max_turns: 99999999999\n"),
    ("max_turns .nan", "user:\n  max_turns: .nan\n"),
    ("settle_on_noop yes", "user:\n  settle_on_noop: yes\n"),
    ("settle_on_noop on", "user:\n  settle_on_noop: on\n"),
    ("settle_on_noop True", "user:\n  settle_on_noop: True\n"),
    ("settle_on_noop TRUE", "user:\n  settle_on_noop: TRUE\n"),
    ("settle_on_noop quoted", "user:\n  settle_on_noop: \"true\"\n"),
    ("settle_on_noop 1", "user:\n  settle_on_noop: 1\n"),
    ("task yes", "task: yes\n"),
    ("task true", "task: true\n"),
    ("task number", "task: 123\n"),
    ("task float", "task: 1.10\n"),
    ("task null", "task: null\n"),
    ("task tilde", "task: ~\n"),
    ("task empty", "task:\n"),
    ("task date", "task: 2026-10-12\n"),
    ("session leading zero", "session: 0123\n"),
    ("persona .inf", "user:\n  persona: .inf\n"),
    ("kind invalid", "provider:\n  kind: bogus\n"),
    ("kind capitalized", "provider:\n  kind: Command\n"),
    ("kind number", "provider:\n  kind: 1\n"),
    ("scale three items", "evals:\n  - criterion: c\n    kind: numeric\n    scale: [0, 1, 2]\n"),
    ("scale ints", "evals:\n  - criterion: c\n    kind: numeric\n    scale: [0, 10]\n"),
    ("scale .inf", "evals:\n  - criterion: c\n    kind: numeric\n    scale: [0, .inf]\n"),
    ("scale strings", "evals:\n  - criterion: c\n    kind: numeric\n    scale: [a, b]\n"),
    ("scale quoted numbers", "evals:\n  - criterion: c\n    kind: numeric\n    scale: [\"0\", \"5\"]\n"),
    ("eval missing criterion", "evals:\n  - kind: boolean\n"),
    ("evals scalar", "evals: c\n"),
    ("evals null", "evals:\n"),
    ("user null", "user:\n"),
    ("user empty map", "user: {}\n"),
    ("provider null", "provider:\n"),
    ("command scalar", "provider:\n  kind: command\n  command: echo\n"),
    ("mock_harness scalar", "provider:\n  mock_harness: claude-code\n"),
    ("top-level list", "- task: x\n"),
    ("top-level scalar", "hello\n"),
    ("empty document", ""),
    ("only a comment", "# nothing here\n"),
    ("explicit document start", "---\ntask: x\n"),
    ("document end marker", "task: x\n...\n"),
    ("two documents", "task: x\n---\ntask: y\n"),
    ("duplicate key", "task: x\ntask: y\n"),
    ("duplicate nested key", "user:\n  persona: a\n  persona: b\n"),
    ("anchor and alias", "system_prompt: &p hello\ntask: *p\n"),
    ("merge key", "user:\n  <<: {persona: p}\n  max_turns: 2\n"),
    ("!!str tag", "task: !!str 123\n"),
    ("custom tag", "task: !foo bar\n"),
    ("tab indentation", "user:\n\tpersona: p\n"),
    ("bad indentation", "user:\n  persona: p\n persona2: q\n"),
    ("unterminated quote", "task: \"abc\n"),
    ("unclosed flow sequence", "provider:\n  command: [a, b\n"),
    ("colon in plain scalar", "task: a: b\n"),
    ("CRLF line endings", "task: x\r\nsession: s\r\n"),
    ("UTF-8 BOM", "\u{feff}task: x\n"),
    ("block scalars", "system_prompt: >\n  a\n  b\ntask: |+\n  x\n\n"),
    ("JSON object", "{\"task\": \"x\", \"user\": {\"max_turns\": 3}}"),
    // Exactly what the Python SDK's `json.dumps(parsed)` writes for a non-ASCII
    // task: ensure_ascii escapes, a surrogate pair for anything past the BMP.
    ("python json.dumps, non-ASCII task", "{\"task\": \"caf\\u00e9 \\ud83d\\ude00\"}"),
    ("python json.dumps, BMP-only non-ASCII task", "{\"task\": \"caf\\u00e9 \\u2014 na\\u00efve\"}"),
    ("folded block last, no final newline", "task: x\nsystem_prompt: >\n  a\n  b"),
    ("literal block last, no final newline", "task: x\nsystem_prompt: |\n  a\n  b"),
    ("literal keep last, no final newline", "task: x\nsystem_prompt: |+\n  a\n  b"),
    ("trailing comment", "task: x # comment\n"),
    ("split with judges", "provider:\n  kind: split\n  skill: {kind: command, command: [a]}\n  judges:\n    - {kind: command, command: [b], label: one}\n    - kind: llmlint\n      diff_base: origin/main\ntask: x\n"),
];

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// The same split `skill.rs::split_frontmatter` does (copied; it is private).
fn split_frontmatter(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("---\n").or_else(|| text.strip_prefix("---\r\n"))?;
    for sep in ["\n---\n", "\n---\r\n", "\r\n---\r\n"] {
        if let Some(idx) = rest.find(sep) {
            return Some(&rest[..idx]);
        }
    }
    None
}

fn skill_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<PathBuf> = rd.map(|e| e.unwrap().path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            if !p.file_name().is_some_and(|n| n == ".trash" || n == "node_modules" || n == ".git") {
                skill_files(&p, out);
            }
        } else if p.file_name().is_some_and(|n| n == "SKILL.md") {
            out.push(p);
        }
    }
}

fn frontmatter_corpus() -> Vec<Doc> {
    let mut docs = Vec::new();
    for p in read_dir_sorted(&Path::new(HERE).join("fixtures/captured/frontmatter"), "yaml") {
        let (suites, test) = origin(&p);
        docs.push(Doc {
            kind: "frontmatter/test-suites",
            name: format!("{} [{suites}; {test}]", p.file_name().unwrap().to_string_lossy()),
            text: read(&p),
        });
    }
    let mut files = Vec::new();
    skill_files(&Path::new(HERE).join("fixtures/skill"), &mut files);
    for p in files {
        // Proves the copied split is the one load_skill applies: load_skill must
        // succeed exactly when serde_yaml_ng accepts the split front matter.
        let text = read(&p);
        let Some(fm) = split_frontmatter(&text) else { continue };
        let loaded = onejudge::load_skill(p.parent().unwrap());
        assert_eq!(loaded.is_ok(), serde_yaml_ng::from_str::<Frontmatter>(fm).is_ok(), "{}", p.display());
        let origin = read(&p.with_file_name("origin"));
        docs.push(Doc { kind: "frontmatter/own-skills", name: origin.trim().to_owned(), text: fm.to_owned() });
    }
    let mut seen = std::collections::BTreeSet::new();
    for dir in std::env::var("SPIKE_EXTRA_SKILL_DIRS").unwrap_or_default().split(':').filter(|d| !d.is_empty()) {
        let mut files = Vec::new();
        skill_files(Path::new(dir), &mut files);
        for p in files {
            let text = read(&p);
            let Some(fm) = split_frontmatter(&text) else { continue };
            if !seen.insert(fm.to_owned()) {
                continue;
            }
            docs.push(Doc { kind: "frontmatter/extra-skills", name: p.display().to_string(), text: fm.to_owned() });
        }
    }
    for (name, text) in SYNTHETIC_FRONTMATTER {
        docs.push(Doc { kind: "frontmatter/edge+malformed", name: (*name).into(), text: (*text).into() });
    }
    docs
}

const SYNTHETIC_FRONTMATTER: &[(&str, &str)] = &[
    ("known keys only", "name: a\ndescription: b"),
    ("unknown keys", "name: a\ndescription: b\nallowed-tools: [Bash, Read]\nmetadata:\n  version: 1\n"),
    ("unknown key, YAML 1.1 bool", "name: a\nenabled: yes"),
    ("unknown key, custom tag", "name: a\nx: !custom {a: 1}"),
    ("unknown key, anchor reused", "base: &b n\nname: *b"),
    ("unknown key duplicated", "name: a\nfoo: 1\nfoo: 2"),
    ("merge key", "<<: {name: merged}\ndescription: d"),
    ("known key duplicated", "name: a\nname: b"),
    ("license", "name: a\nlicense: MIT"),
    ("name number", "name: 123"),
    ("name yes", "name: yes"),
    ("name true", "name: true"),
    ("name null", "name:\ndescription: d"),
    // split_frontmatter cuts before the closing fence's newline, so the front
    // matter of every real SKILL.md ends without a line break.
    ("description folded, last key", "description: >\n  long\n  text"),
    ("description literal, last key", "description: |\n  long\n  text"),
    ("description folded strip, last key", "description: >-\n  long\n  text"),
    ("description folded, not last", "description: >\n  long\n  text\nname: a"),
    ("description plain multi-line, last key", "description: long\n  text"),
    ("description colon unquoted", "name: a\ndescription: Use when: x"),
    ("empty", ""),
    ("only a comment", "# c"),
    ("non-mapping", "- a"),
    ("bad indentation", "name: a\n  description: b"),
    ("unterminated quote", "name: \"a\ndescription: b"),
    ("CRLF", "name: a\r\ndescription: b"),
];

// --------------------------------------------------------------- readers ----

fn de_variants() -> Vec<(&'static str, Options)> {
    let mut v = vec![("default", Options::default())];
    let mut o = Options::default();
    o.strict_booleans = true;
    v.push(("strict_booleans", o));
    let mut o = Options::default();
    o.no_schema = true;
    v.push(("no_schema", o));
    let mut o = Options::default();
    o.duplicate_keys = DuplicateKeyPolicy::LastWins;
    v.push(("duplicate_keys=LastWins", o));
    let mut o = Options::default();
    o.duplicate_keys = DuplicateKeyPolicy::FirstWins;
    v.push(("duplicate_keys=FirstWins", o));
    let mut o = Options::default();
    o.legacy_octal_numbers = true;
    v.push(("legacy_octal_numbers", o));
    let mut o = Options::default();
    o.merge_keys = MergeKeyPolicy::AsOrdinary;
    v.push(("merge_keys=AsOrdinary", o));
    let mut o = Options::default();
    o.merge_keys = MergeKeyPolicy::Error;
    v.push(("merge_keys=Error", o));
    let mut o = Options::default();
    o.reject_unsupported_tags = true;
    v.push(("reject_unsupported_tags", o));
    let mut o = Options::default();
    o.strict_booleans = true;
    o.merge_keys = MergeKeyPolicy::AsOrdinary;
    v.push(("strict_booleans+merge_keys=AsOrdinary", o));
    let mut o = Options::default();
    o.strict_booleans = true;
    o.merge_keys = MergeKeyPolicy::AsOrdinary;
    o.duplicate_keys = DuplicateKeyPolicy::FirstWins;
    v.push(("strict_booleans+merge_keys=AsOrdinary+duplicate_keys=FirstWins", o));
    v
}

type Read = Result<String, String>;

fn ng<T: DeserializeOwned + Debug>(text: &str) -> Read {
    serde_yaml_ng::from_str::<T>(text).map(|v| format!("{v:?}")).map_err(|e| e.to_string())
}

fn sy<T: DeserializeOwned + Debug>(text: &str, o: Options) -> Read {
    serde_saphyr::from_str_with_options::<T>(text, o).map(|v| format!("{v:?}")).map_err(|e| e.to_string())
}

#[derive(Default)]
struct Tally {
    docs: usize,
    identical_ok: usize,
    identical_err_class: usize,
    read_diff: usize,
    err_text_diff: usize,
}

struct Out {
    tallies: BTreeMap<&'static str, Tally>,
    /// (kind, the difference class, how often) for the summary.
    classes: BTreeMap<(String, String), usize>,
    /// (Options variant, kind) -> the documents whose accept/refuse outcome or
    /// read value differs from today's under that variant.
    scores: BTreeMap<(&'static str, &'static str), Vec<String>>,
}

fn indent(s: &str) -> String {
    s.lines().map(|l| format!("      | {l}")).collect::<Vec<_>>().join("\n")
}

/// Compare one document read into `T`; `wrap` turns a parser message into the
/// message this repository surfaces (config: `CliError::Config`, front matter:
/// load_skill's `invalid SKILL.md frontmatter in ...`).
fn compare_read<T: DeserializeOwned + Debug>(out: &mut Out, doc: &Doc, wrap: &dyn Fn(String) -> String) {
    out.tallies.entry(doc.kind).or_default().docs += 1;
    let before = ng::<T>(&doc.text);
    let variants = de_variants();
    for (vname, o) in &variants {
        let v = sy::<T>(&doc.text, o.clone());
        let same = match (&before, &v) {
            (Ok(a), Ok(b)) => a == b,
            (Err(_), Err(_)) => true,
            _ => false,
        };
        let entry = out.scores.entry((vname, doc.kind)).or_default();
        if !same {
            entry.push(doc.name.clone());
        }
    }
    let tally = out.tallies.entry(doc.kind).or_default();
    let after = sy::<T>(&doc.text, variants[0].1.clone());
    match (&before, &after) {
        (Ok(a), Ok(b)) if a == b => tally.identical_ok += 1,
        (Err(a), Err(b)) => {
            tally.identical_err_class += 1;
            tally.err_text_diff += 1;
            *out.classes.entry((doc.kind.into(), "error text differs (both refuse)".into())).or_default() += 1;
            println!("  - [{}] {}: ERROR TEXT DIFFERS (both refuse)", doc.kind, doc.name);
            println!("      before (serde_yaml_ng):\n{}", indent(&wrap(a.clone())));
            println!("      after (serde-saphyr):\n{}", indent(&wrap(b.clone())));
            let mut plain = Options::default();
            plain.with_snippet = false;
            if let Err(c) = sy::<T>(&doc.text, plain) {
                println!("      after (serde-saphyr, with_snippet=false):\n{}", indent(&wrap(c)));
            }
        }
        _ => {
            tally.read_diff += 1;
            let class = match (&before, &after) {
                (Ok(_), Ok(_)) => "read differs (both accept)",
                (Ok(_), Err(_)) => "serde-saphyr refuses what serde_yaml_ng accepts",
                _ => "serde-saphyr accepts what serde_yaml_ng refuses",
            };
            *out.classes.entry((doc.kind.into(), class.into())).or_default() += 1;
            let reproduced: Vec<&str> = variants[1..]
                .iter()
                .filter(|(_, o)| {
                    let v = sy::<T>(&doc.text, o.clone());
                    match (&before, &v) {
                        (Ok(a), Ok(b)) => a == b,
                        (Err(_), Err(_)) => true,
                        _ => false,
                    }
                })
                .map(|(n, _)| *n)
                .collect();
            println!("  - [{}] {}: {}", doc.kind, doc.name, class.to_uppercase());
            println!("      input:\n{}", indent(&doc.text));
            let show = |r: &Read| match r {
                Ok(v) => format!("Ok {v}"),
                Err(e) => format!("Err {}", wrap(e.clone())),
            };
            println!("      before (serde_yaml_ng):\n{}", indent(&show(&before)));
            println!("      after (serde-saphyr default Options):\n{}", indent(&show(&after)));
            if reproduced.is_empty() {
                println!("      reproduced by Options: NONE");
            } else {
                println!("      reproduced by Options: {}", reproduced.join(", "));
            }
        }
    }
}

// ------------------------------------------------------------- workflows ----

fn json_diff(a: &Json, b: &Json, path: &str, out: &mut Vec<String>) {
    match (a, b) {
        (Json::Object(x), Json::Object(y)) => {
            let kx: Vec<&String> = x.keys().collect();
            let ky: Vec<&String> = y.keys().collect();
            if kx != ky {
                out.push(format!("{path}: keys {kx:?} vs {ky:?}"));
            }
            for (k, v) in x {
                if let Some(w) = y.get(k) {
                    json_diff(v, w, &format!("{path}.{k}"), out);
                }
            }
        }
        (Json::Array(x), Json::Array(y)) if x.len() == y.len() => {
            for (i, (v, w)) in x.iter().zip(y).enumerate() {
                json_diff(v, w, &format!("{path}[{i}]"), out);
            }
        }
        _ if a == b => {}
        _ => out.push(format!("{path}: {a} vs {b}")),
    }
}

fn saphyr_to_json(y: &saphyr::Yaml) -> Result<Json, String> {
    use saphyr::{Scalar, Yaml};
    Ok(match y {
        Yaml::Value(Scalar::Null) => Json::Null,
        Yaml::Value(Scalar::Boolean(b)) => Json::Bool(*b),
        Yaml::Value(Scalar::Integer(i)) => Json::from(*i),
        Yaml::Value(Scalar::FloatingPoint(f)) => Json::from(f.into_inner()),
        Yaml::Value(Scalar::String(s)) => Json::String(s.to_string()),
        Yaml::Sequence(s) => Json::Array(s.iter().map(saphyr_to_json).collect::<Result<_, _>>()?),
        Yaml::Mapping(m) => {
            let mut map = serde_json::Map::new();
            for (k, v) in m {
                let Yaml::Value(Scalar::String(k)) = k else {
                    return Err(format!("non-string key {k:?}"));
                };
                map.insert(k.to_string(), saphyr_to_json(v)?);
            }
            Json::Object(map)
        }
        other => return Err(format!("unmodelled node {other:?}")),
    })
}

fn ser_variants() -> Vec<(&'static str, SerializerOptions)> {
    let mut v = vec![("default", SerializerOptions::default())];
    let mut o = SerializerOptions::default();
    o.compact_list_indent = false;
    v.push(("compact_list_indent=false", o));
    let mut o = SerializerOptions::default();
    o.prefer_block_scalars = false;
    v.push(("prefer_block_scalars=false", o));
    let mut o = SerializerOptions::default();
    o.yaml_12 = true;
    v.push(("yaml_12", o));
    let mut o = SerializerOptions::default();
    o.empty_as_braces = false;
    v.push(("empty_as_braces=false", o));
    let mut o = SerializerOptions::default();
    o.prefer_block_scalars = false;
    o.compact_list_indent = false;
    v.push(("prefer_block_scalars=false+compact_list_indent=false", o));
    v
}

fn workflows_section() {
    println!("\n## workflows: the CI contract gate (crates/onejudge-repo/tests/workflows.rs)\n");
    let dir = workflows_ng::workflow_dir();
    let files = read_dir_sorted(&dir, "yml");
    println!("  {} workflow files read\n", files.len());
    println!("### read: serde_yaml_ng::Value vs serde-saphyr -> serde_json::Value (preserve_order) vs saphyr::Yaml\n");
    let mut read_diffs = 0;
    for f in &files {
        let text = read(f);
        let name = f.file_name().unwrap().to_string_lossy();
        let ngv: serde_yaml_ng::Value = serde_yaml_ng::from_str(&text).unwrap();
        let ng_on = ngv.as_mapping().unwrap().keys().find(|k| k.as_str() == Some("on")).map(|k| format!("{k:?}"));
        let ng_json = serde_json::to_value(&ngv).map_err(|e| e.to_string());
        let sy_json = serde_saphyr::from_str::<Json>(&text).map_err(|e| e.to_string());
        let tree = <saphyr::Yaml as saphyr::LoadableYamlNode>::load_from_str(&text).map_err(|e| e.to_string());
        let sy_on = sy_json.as_ref().ok().and_then(|j| j.as_object()).map(|m| m.contains_key("on"));
        let (tree_on, tree_json) = match &tree {
            Ok(docs) => {
                let root = &docs[0];
                let on = root
                    .as_mapping()
                    .map(|m| m.keys().find(|k| format!("{k:?}").contains("\"on\"")).map(|k| format!("{k:?}")));
                (on.flatten(), saphyr_to_json(root))
            }
            Err(e) => (None, Err(e.clone())),
        };
        println!(
            "  - {name}: `on` key — serde_yaml_ng {}; serde-saphyr->json has string key \"on\": {}; saphyr tree key {}",
            ng_on.as_deref().unwrap_or("MISSING"),
            sy_on.map_or("PARSE ERROR".into(), |b| b.to_string()),
            tree_on.as_deref().unwrap_or("MISSING"),
        );
        match (&ng_json, &sy_json) {
            (Ok(a), Ok(b)) => {
                let mut d = Vec::new();
                json_diff(a, b, "$", &mut d);
                if d.is_empty() {
                    println!("      serde-saphyr->json: identical to serde_yaml_ng (values and key order)");
                } else {
                    read_diffs += d.len();
                    for x in d {
                        println!("      serde-saphyr->json DIFFERS: {x}");
                    }
                }
            }
            (a, b) => {
                read_diffs += 1;
                println!("      DIFFERS: serde_yaml_ng->json {a:?}, serde-saphyr {b:?}");
            }
        }
        match (&ng_json, &tree_json) {
            (Ok(a), Ok(b)) => {
                let mut d = Vec::new();
                json_diff(a, b, "$", &mut d);
                if d.is_empty() {
                    println!("      saphyr tree: identical to serde_yaml_ng (values and key order)");
                } else {
                    for x in d {
                        println!("      saphyr tree DIFFERS: {x}");
                    }
                }
            }
            (_, b) => println!("      saphyr tree: {b:?}"),
        }
    }
    println!("  read differences: {read_diffs}");

    println!("\n### checkers: each workflows.rs test body, on serde_yaml_ng::Value vs the serde_json port\n");
    let a = workflows_ng::cases();
    let b = workflows_json::cases();
    let mut checker_diffs = 0;
    for ((case, va), (_, vb)) in a.iter().zip(&b) {
        if va == vb {
            println!("  - {case}: identical ({} violation(s))", va.len());
        } else {
            checker_diffs += 1;
            println!("  - {case}: VIOLATION TEXT DIFFERS");
            for v in va {
                println!("      before: {v}");
            }
            for v in vb {
                println!("      after:  {v}");
            }
        }
    }
    println!("  checker differences: {checker_diffs} of {} cases", a.len());

    println!("\n### write: serde_yaml_ng::to_string(step) vs serde_saphyr::to_string(json step), every step of every workflow\n");
    let steps_ng = workflows_ng::all_steps();
    let variants = ser_variants();
    let mut by_variant: BTreeMap<&str, usize> = BTreeMap::new();
    let mut secrets_disagree = 0;
    for (label, step) in &steps_ng {
        let before = serde_yaml_ng::to_string(step).unwrap();
        let json: Json = serde_json::to_value(step).unwrap();
        let mut matched = Vec::new();
        for (vname, o) in &variants {
            let after = serde_saphyr::to_string_with_options(&json, o.clone()).unwrap();
            if after == before {
                matched.push(*vname);
                *by_variant.entry(vname).or_default() += 1;
            }
        }
        let after = serde_saphyr::to_string(&json).unwrap();
        if before.contains("secrets.") != after.contains("secrets.") {
            secrets_disagree += 1;
        }
        if after != before {
            println!("  - {label}: BYTES DIFFER (default SerializerOptions)");
            println!("      before (serde_yaml_ng):\n{}", indent(&before));
            println!("      after (serde-saphyr):\n{}", indent(&after));
            for (i, (x, y)) in before.lines().zip(after.lines()).enumerate() {
                if x != y {
                    println!("      line {}: {x:?} -> {y:?}", i + 1);
                }
            }
            println!(
                "      reproduced by SerializerOptions: {}",
                if matched.is_empty() { "NONE".to_owned() } else { matched.join(", ") }
            );
        }
    }
    println!("\n  {} steps rendered; byte-identical per SerializerOptions variant:", steps_ng.len());
    for (vname, _) in &variants {
        println!("    {vname}: {}/{}", by_variant.get(vname).copied().unwrap_or(0), steps_ng.len());
    }
    println!("  steps where `rendered.contains(\"secrets.\")` disagrees: {secrets_disagree}");
    let v = workflows_ng::validate_step();
    println!("\n  the step the gate renders (llmlint validate step):");
    println!("      before (serde_yaml_ng):\n{}", indent(&serde_yaml_ng::to_string(&v).unwrap()));
    println!(
        "      after (serde-saphyr):\n{}",
        indent(&serde_saphyr::to_string(&serde_json::to_value(&v).unwrap()).unwrap())
    );
}

// ---------------------------------------------------------------- timing ----

fn median_micros(iters: usize, f: &mut dyn FnMut()) -> f64 {
    let mut t: Vec<f64> = (0..iters)
        .map(|_| {
            let s = Instant::now();
            f();
            s.elapsed().as_secs_f64() * 1e6
        })
        .collect();
    t.sort_by(|a, b| a.partial_cmp(b).unwrap());
    t[iters / 2]
}

fn timing_section(configs: &[Doc]) {
    println!("\n## parse time of the largest real document\n");
    let mut real: Vec<(String, String)> = configs
        .iter()
        .filter(|d| d.kind != "config/edge+malformed")
        .map(|d| (format!("[{}] {}", d.kind, d.name), d.text.clone()))
        .collect();
    for f in read_dir_sorted(&workflows_ng::workflow_dir(), "yml") {
        real.push((f.display().to_string(), read(&f)));
    }
    let (name, text) = real.iter().max_by_key(|(_, t)| t.len()).unwrap();
    println!("  largest: {name} ({} bytes)", text.len());
    let n = 200;
    let rows: Vec<(&str, f64)> = vec![
        ("serde_yaml_ng -> Config", median_micros(n, &mut || {
            std::hint::black_box(serde_yaml_ng::from_str::<Config>(text).unwrap());
        })),
        ("serde-saphyr -> Config", median_micros(n, &mut || {
            std::hint::black_box(serde_saphyr::from_str::<Config>(text).unwrap());
        })),
        ("serde_yaml_ng -> serde_yaml_ng::Value", median_micros(n, &mut || {
            std::hint::black_box(serde_yaml_ng::from_str::<serde_yaml_ng::Value>(text).unwrap());
        })),
        ("serde-saphyr -> serde_json::Value", median_micros(n, &mut || {
            std::hint::black_box(serde_saphyr::from_str::<Json>(text).unwrap());
        })),
        ("saphyr -> Yaml tree", median_micros(n, &mut || {
            std::hint::black_box(<saphyr::Yaml as saphyr::LoadableYamlNode>::load_from_str(text).unwrap());
        })),
    ];
    for (what, us) in rows {
        println!("    {what}: median {us:.0} µs over {n} parses");
    }
}

// ------------------------------------------------------------------ main ----

fn main() {
    println!("# serde_yaml_ng 0.10.0 vs serde-saphyr 1.3.0 (saphyr 0.2.1 for the schema-less walk)");
    println!("# 'before' = serde_yaml_ng (today), 'after' = serde-saphyr with default Options unless named.\n");
    let mut out = Out { tallies: BTreeMap::new(), classes: BTreeMap::new(), scores: BTreeMap::new() };

    println!("## config: Config::from_yaml (serde_yaml_ng) vs serde_saphyr::from_str::<Config>\n");
    let configs = config_corpus();
    let wrap_config = |e: String| CliError::Config(format!("could not parse config: {e}")).to_string();
    for doc in &configs {
        compare_read::<Config>(&mut out, doc, &wrap_config);
    }

    println!("\n## frontmatter: SKILL.md front matter into onejudge::Frontmatter\n");
    let fms = frontmatter_corpus();
    let wrap_fm = |e: String| format!("invalid SKILL.md frontmatter in `<dir>/SKILL.md`: {e}");
    for doc in &fms {
        compare_read::<Frontmatter>(&mut out, doc, &wrap_fm);
    }

    workflows_section();
    timing_section(&configs);

    println!("\n## summary per document kind\n");
    println!("  {:<34} {:>5} {:>10} {:>10} {:>10} {:>14}", "kind", "docs", "same-read", "read-diff", "both-err", "err-text-diff");
    for (kind, t) in &out.tallies {
        println!(
            "  {kind:<34} {:>5} {:>10} {:>10} {:>10} {:>14}",
            t.docs, t.identical_ok, t.read_diff, t.identical_err_class, t.err_text_diff
        );
    }
    println!("\n  difference classes:");
    for ((kind, class), n) in &out.classes {
        println!("    {kind}: {class}: {n}");
    }

    println!("\n## Options scoreboard: documents whose outcome (accept/refuse, and the value read) differs from today\n");
    for (vname, _) in de_variants() {
        let rows: Vec<String> = out
            .scores
            .iter()
            .filter(|((v, _), names)| *v == vname && !names.is_empty())
            .map(|((_, kind), names)| format!("{kind}: {}", names.join(" | ")))
            .collect();
        let total: usize = out.scores.iter().filter(|((v, _), _)| *v == vname).map(|(_, n)| n.len()).sum();
        println!("  {vname}: {total} document(s)");
        for r in rows {
            println!("      {r}");
        }
    }
}
