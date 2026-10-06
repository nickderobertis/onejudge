//! The release-target network tier: the answers about what this repository
//! releases that need a network, held out of the offline gate.
//!
//! `onejudge-repo`'s `tests/release_targets.rs` holds `release-targets.toml` to the
//! canonical release-target schema and to the real release configuration, offline,
//! in `just check`. What it cannot reach offline is here: whether the schema it
//! restates still matches the one implementation that defines it
//! (`nickderobertis/onevcs`), and what the public registries serve for every
//! declared target right now. Every test is `#[ignore]`-d; run them with
//! `just test-release-targets`, which `.github/workflows/release-targets.yml` does
//! on a schedule and on a change to what they read.

use onejudge_repo::schema;

/// Network tier: the canonical schema this suite restates has not moved.
///
/// [`schema`] is a restatement of a contract this repository does not own, so it is
/// the one thing here that can drift *silently*: upstream tightens a limit or adds
/// a required key, this gate keeps passing, and the defect is met by a consumer
/// whose reader refuses a document this repository published. There is no offline
/// way to close that — the definition is in another repository — so it is
/// reconciled the way every other answer needing a network is reached here: an
/// `#[ignore]`-d tier, out of the deterministic gate, run by
/// `just test-release-targets`.
///
/// It reconciles against the *implementation* that defines the schema rather than
/// against `docs/contract.md`'s prose beside it, because the implementation is what
/// a consumer's reader actually enforces — its constants, its key lists, and the
/// expressions its rules are made of. A refusal here is never "fix this test": it
/// is the canonical schema having changed, and what this repository publishes has
/// to be reread against it.
///
/// `.github/workflows/release-targets.yml` runs it, on a schedule as well as on a
/// change here, because drift upstream is silent and does not wait for a change in
/// this repository to become a document a consumer refuses.
#[test]
#[ignore = "network: reads nickderobertis/onevcs; run via `just test-release-targets`"]
fn the_restated_schema_matches_the_canonical_definition() {
    /// Where the one implementation of the canonical schema lives.
    const CANONICAL: &str = "https://raw.githubusercontent.com/nickderobertis/onevcs/HEAD";

    /// One upstream file, fetched with the tool the probe uses for the same reason:
    /// no credential, a public read, and a bound well inside the suite's own.
    fn upstream(path: &str) -> String {
        let url = format!("{CANONICAL}/{path}");
        let output = std::process::Command::new("curl")
            .args([
                "--silent",
                "--show-error",
                "--fail",
                "--location",
                "--max-time",
                "30",
                &url,
            ])
            .output()
            .unwrap_or_else(|err| panic!("curl is needed to read {url}: {err}"));
        assert!(
            output.status.success(),
            "could not read the canonical schema at {url}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("the canonical schema is UTF-8")
    }

    /// The value of `const <name>` in a Rust source file, up to its `;`.
    fn constant<'a>(source: &'a str, origin: &str, name: &str) -> &'a str {
        let declaration = format!("const {name}:");
        source
            .lines()
            .map(str::trim)
            .find(|line| {
                line.starts_with(&declaration) || line.starts_with(&format!("pub {declaration}"))
            })
            .and_then(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
            .map(str::trim)
            .unwrap_or_else(|| {
                panic!(
                    "the canonical schema at {origin} no longer declares `{name}` on one line; \
                     the definition has moved or been renamed, and what this repository \
                     publishes has to be reread against it"
                )
            })
    }

    /// Every quoted string of a Rust array literal.
    fn strings(literal: &str) -> Vec<String> {
        let mut items = Vec::new();
        let mut rest = literal;
        while let Some(open) = rest.find('"') {
            let after = &rest[open + 1..];
            let Some(close) = after.find('"') else { break };
            items.push(after[..close].to_owned());
            rest = &after[close + 1..];
        }
        items
    }

    let declaration = upstream("crates/onevcs/src/declaration.rs");
    let releases = upstream("crates/onevcs/src/releases.rs");
    let origin = "crates/onevcs/src/declaration.rs";

    for (name, restated) in [
        ("SCHEMA_VERSION", u64::from(schema::SCHEMA_VERSION)),
        ("MAX_PROSE", schema::MAX_PROSE as u64),
        ("MAX_IDENTIFIER", schema::MAX_IDENTIFIER as u64),
    ] {
        let canonical: u64 = constant(&declaration, origin, name)
            .parse()
            .unwrap_or_else(|err| panic!("the canonical `{name}` is not a number: {err}"));
        assert_eq!(
            restated, canonical,
            "the canonical schema declares {name} = {canonical}; this suite restates {restated}"
        );
    }
    let canonical: u64 = constant(
        &releases,
        "crates/onevcs/src/releases.rs",
        "MAX_TARGET_NAME",
    )
    .parse()
    .expect("the canonical MAX_TARGET_NAME is a number");
    assert_eq!(
        schema::MAX_TARGET_NAME as u64,
        canonical,
        "the canonical schema declares MAX_TARGET_NAME = {canonical}"
    );

    // The rules themselves, not only the numbers they are bounded by. Each of
    // these is the expression one canonical check is made of, and `schema` restates
    // it verbatim; a rule tightened upstream changes the expression, and this is
    // where that is heard rather than at a consumer whose reader refuses a document
    // this gate passed.
    for (rule, source, origin, expression) in [
        (
            "an identifier's registry half",
            &declaration,
            origin,
            "c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'",
        ),
        (
            "an identifier's name half",
            &declaration,
            origin,
            "c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '@' | '/')",
        ),
        (
            "a short name's alphabet",
            &releases,
            "crates/onevcs/src/releases.rs",
            "c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')",
        ),
        (
            "blank prose",
            &declaration,
            origin,
            "value.trim().is_empty()",
        ),
        (
            "prose carrying a control character",
            &declaration,
            origin,
            "value.chars().any(char::is_control)",
        ),
        (
            "a path's separators",
            &declaration,
            origin,
            "const SEPARATORS: [char; 2] = ['/', '\\\\'];",
        ),
        (
            "a path that leaves the repository root",
            &declaration,
            origin,
            "component == \"..\"",
        ),
        (
            "a drive-qualified path",
            &declaration,
            origin,
            "(Some(drive), Some(':')) if drive.is_ascii_alphabetic()",
        ),
    ] {
        assert!(
            source.contains(expression),
            "the canonical schema at {origin} no longer decides {rule} with `{expression}`; the \
             rule has changed, and `schema` restates the one it replaced"
        );
    }

    for (name, restated) in [
        ("TOP_LEVEL_KEYS", &schema::TOP_LEVEL_KEYS[..]),
        ("TARGET_KEYS", &schema::TARGET_KEYS[..]),
        ("RETIRED_KEYS", &schema::RETIRED_KEYS[..]),
    ] {
        let canonical = strings(constant(&declaration, origin, name));
        assert_eq!(
            restated, canonical,
            "the canonical schema declares {name} = {canonical:?}; this suite restates \
             {restated:?}, so a document this gate passes is one a consumer's reader may refuse"
        );
    }
}

/// The probe against the true public registries.
#[cfg(unix)]
mod probe {
    use std::env;

    use onejudge_repo::declared_targets;
    use onejudge_repo::probe::{assert_no_release_yet, probe, BOUND};

    /// Network tier: what crates.io and PyPI serve for every declared target right
    /// now. `#[ignore]`-d like the rest of this repository's network-touching
    /// verification — the gate is offline. Run with `just test-release-targets`.
    #[test]
    #[ignore = "network: reads the public registries; run via `just test-release-targets`"]
    fn every_declared_target_reports_the_version_its_registry_serves() {
        for target in declared_targets() {
            let (output, elapsed) = probe(&[&target]);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                output.status.success(),
                "the probe did not answer for `{target}`: {stderr}"
            );
            let stdout = String::from_utf8_lossy(&output.stdout);
            let version = stdout.trim_end_matches('\n');
            assert!(
                !version.is_empty(),
                "`{target}` is declared but the registry serves no release of it"
            );
            assert!(
                !version.contains('\n'),
                "`{target}` answered more than one line: {stdout:?}"
            );
            assert!(
                version.starts_with(|c: char| c.is_ascii_digit()) && version.contains('.'),
                "`{target}` answered {version:?}, which is not a version"
            );
            assert!(elapsed < BOUND, "`{target}` took {elapsed:?}");
        }
    }

    /// The third answer against the real thing: a registry that has never served
    /// the artifact reports no release yet — exit 0, empty, distinct from a
    /// failure to answer.
    #[test]
    #[ignore = "network: reads the public registries; run via `just test-release-targets`"]
    fn an_artifact_no_registry_serves_answers_no_release_yet() {
        let path = env::var("PATH").expect("PATH");
        for target in [
            "crate:onejudge-no-such-crate-6bd41f",
            "pypi:onejudge-no-such-distribution-6bd41f",
        ] {
            assert_no_release_yet(&path, &[target]);
        }
    }
}
