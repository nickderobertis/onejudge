//! The reader the repository-level suites share: this repository's release-target
//! declaration (`release-targets.toml`), the canonical schema it is held to, and
//! the probe that answers what a registry serves for one of its targets.
//!
//! Two suites read it. `onejudge-repo`'s own `tests/release_targets.rs` is the
//! offline drift gate that runs in `just check`; `onejudge-release-targets` is the
//! network tier that reconciles the same schema and probe against the real
//! registries and the upstream definition, out of the gate. One reader, so the two
//! can never hold the document to different shapes. Never published.

use std::fs;
use std::path::{Path, PathBuf};

pub use schema::Declaration;

// llmlint: ignore-block[contracts_have_one_source_or_a_drift_gate] this restatement's drift gate is `onejudge-release-targets`' `the_restated_schema_matches_the_canonical_definition`, which reconciles every constant and key list below with onevcs's own implementation; it reads the network, so by the rule that external contact leaves the affected tier it runs on its schedule and workflow rather than in `check`.
/// The canonical release-target schema, as a reader that refuses.
///
/// A restatement of somebody else's contract is a thing that drifts, so this is
/// deliberately narrow: it is `schema_version = 1` exactly as
/// `nickderobertis/onevcs`'s `docs/contract.md` fixes it — the same keys, the same
/// alphabets, the same document-level refusals — and nothing beside it. Where this
/// repository would want a field the schema does not offer, the answer is to raise
/// it there, not to admit one here; the two artifacts that need one (the
/// per-platform wheels, the release archives) are comments in the document instead.
///
/// It lives in this `publish = false` crate rather than in the library because it
/// is not onejudge's public surface: onejudge *publishes* the document, and this
/// is the gate that what it publishes can be read.
pub mod schema {
    use std::collections::BTreeMap;

    use serde::Deserialize;

    /// The schema version this gate reads, and the oldest it accepts.
    pub const SCHEMA_VERSION: u32 = 1;
    /// How long one line of operator-written prose may be.
    pub const MAX_PROSE: usize = 400;
    /// How long a registry-qualified identifier may be.
    pub const MAX_IDENTIFIER: usize = 128;
    /// How long a target's short name may be.
    pub const MAX_TARGET_NAME: usize = 64;

    /// The keys `schema_version = 1` declares, by the table they belong to. Spelled
    /// out rather than derived from `deny_unknown_fields`, because a *later* schema's
    /// keys are read leniently and that attribute would refuse them too.
    pub const TOP_LEVEL_KEYS: [&str; 4] = ["schema_version", "probe", "target", "retired"];
    pub const TARGET_KEYS: [&str; 6] = ["id", "name", "what", "published_by", "manifest", "covers"];
    pub const RETIRED_KEYS: [&str; 2] = ["id", "why"];

    // llmlint: ignore-block[invalid_states_unrepresentable] these are the serde shapes of another repository's document (nickderobertis/onevcs's canonical release-target schema), moved verbatim from the test that owned them; nothing builds one except `parse`, which refuses every malformed id, name and prose field before it returns, so no unvalidated value reaches a caller.
    /// What one repository publishes, as its own `release-targets.toml` declares it.
    #[derive(Debug, Deserialize)]
    pub struct Declaration {
        /// The schema this document is written against.
        pub schema_version: u32,
        /// The script that answers what a registry currently serves for one
        /// [`DeclaredTarget::id`]. Optional.
        #[serde(default)]
        pub probe: Option<String>,
        /// The consumable artifacts this repository publishes, in publication order.
        #[serde(rename = "target", default)]
        pub targets: Vec<DeclaredTarget>,
        /// What this repository once published and does not any more.
        #[serde(rename = "retired", default)]
        pub retired: Vec<RetiredArtifact>,
    }

    /// One consumable artifact: something a dependent names in order to depend on it.
    #[derive(Debug, Deserialize)]
    pub struct DeclaredTarget {
        /// `<registry>:<name>`, where `<name>` is exactly what that registry serves.
        pub id: String,
        /// The short name a host document and a consumer's plan wait on this by.
        pub name: String,
        /// One sentence saying what a dependent gets.
        pub what: String,
        /// The workflow and job that publish it, and the manifest it comes from.
        pub published_by: String,
        /// The manifest this target's version is read from.
        #[serde(default)]
        pub manifest: Option<String>,
        /// Identifiers this target's release also ships, which are not targets.
        #[serde(default)]
        pub covers: Vec<String>,
    }

    /// Something this repository once published and does not publish again.
    #[derive(Debug, Deserialize)]
    pub struct RetiredArtifact {
        /// The identifier that is no longer published.
        pub id: String,
        /// Why it is not published any more, and what replaced it if anything did.
        pub why: String,
    }
    // llmlint: ignore-end[invalid_states_unrepresentable]

    /// Read one declaration's text, or say what is wrong with it and where.
    ///
    /// `origin` is what the refusals name the document by, so a caller validating a
    /// fixture and a caller validating the repository's own file both get a message
    /// that points at what they handed over.
    pub fn parse(document: &str, origin: &str) -> Result<Declaration, String> {
        let value: toml::Value = toml::from_str(document).map_err(|failure| {
            format!("the release declaration at {origin} is not TOML: {failure}")
        })?;

        // The version is read, and refused, before the shape is: which keys a document
        // may carry is a fact about the schema it declares.
        let Some(declared) = value
            .get("schema_version")
            .and_then(toml::Value::as_integer)
        else {
            return Err(format!(
                "the release declaration at {origin} declares no schema_version; every \
                 declaration opens with `schema_version = {SCHEMA_VERSION}`, before any table"
            ));
        };
        if declared < i64::from(SCHEMA_VERSION) {
            return Err(format!(
                "the release declaration at {origin} declares schema_version {declared}; this \
                 gate reads schema_version {SCHEMA_VERSION} and newer"
            ));
        }
        // Only at the version this gate knows. A typo is the likeliest defect in a
        // hand-written document, and reading `manifset` as an absent `manifest` would
        // publish an answer nobody declared. A later schema's keys are ignored, which is
        // the leniency the document promises a consumer one release behind.
        if declared == i64::from(SCHEMA_VERSION) {
            refuse_unknown_keys(&value, origin)?;
        }

        let declaration: Declaration = toml::from_str(document).map_err(|failure| {
            format!(
                "the release declaration at {origin} is not the shape schema_version \
                 {SCHEMA_VERSION} declares: {failure}"
            )
        })?;
        validate(&declaration, origin)?;
        Ok(declaration)
    }

    /// Refuse a key this schema does not declare, naming it and the table it is in.
    fn refuse_unknown_keys(document: &toml::Value, origin: &str) -> Result<(), String> {
        let unknown = |table: &str, key: &str| {
            format!(
                "the release declaration at {origin} names {key:?} in {table}, which \
                 schema_version {SCHEMA_VERSION} does not declare; a misspelled key would \
                 otherwise be read as an absent one"
            )
        };
        let Some(top) = document.as_table() else {
            return Err(format!(
                "the release declaration at {origin} is not a table of keys; every declaration \
                 opens with `schema_version = {SCHEMA_VERSION}`, before any table"
            ));
        };
        for key in top.keys() {
            if !TOP_LEVEL_KEYS.contains(&key.as_str()) {
                return Err(unknown("the document", key));
            }
        }
        for (array, keys) in [("target", &TARGET_KEYS[..]), ("retired", &RETIRED_KEYS[..])] {
            let Some(entries) = top.get(array).and_then(toml::Value::as_array) else {
                continue;
            };
            for (index, entry) in entries.iter().enumerate() {
                let Some(table) = entry.as_table() else {
                    continue;
                };
                for key in table.keys() {
                    if !keys.contains(&key.as_str()) {
                        return Err(unknown(&format!("[[{array}]] {}", index + 1), key));
                    }
                }
            }
        }
        Ok(())
    }

    /// A registry-qualified identifier: exactly one colon, both halves present, and a
    /// name spelled in the alphabet every registry serves. The registry half is an
    /// open vocabulary — what is closed is the shape.
    fn registry_id(value: &str) -> Result<(), String> {
        if value.len() > MAX_IDENTIFIER {
            return Err(format!(
                "the identifier {value:?} is longer than {MAX_IDENTIFIER} characters"
            ));
        }
        let Some((registry, name)) = value.split_once(':') else {
            return Err(format!(
                "the identifier {value:?} names no registry; spell every identifier as \
                 <registry>:<name>, e.g. crate:onejudge, because one name published to two \
                 registries is two artifacts"
            ));
        };
        if registry.is_empty()
            || !registry
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(format!(
                "the identifier {value:?} names the registry {registry:?}, which is not one word \
                 of lowercase letters, digits, and '-'"
            ));
        }
        if name.is_empty()
            || !name.starts_with(|c: char| c.is_ascii_alphanumeric())
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '@' | '/'))
        {
            return Err(format!(
                "the identifier {value:?} names {name:?}, which is not a name a registry serves; \
                 spell the name exactly as its registry does"
            ));
        }
        Ok(())
    }

    /// The short name a host document and a consumer's plan wait on a target by.
    fn target_name(value: &str) -> Result<(), String> {
        if value.is_empty() {
            return Err("a release target's name cannot be empty".to_owned());
        }
        if value.len() > MAX_TARGET_NAME {
            return Err(format!(
                "the release target name {value:?} is longer than {MAX_TARGET_NAME} characters"
            ));
        }
        if !value.starts_with(|c: char| c.is_ascii_alphanumeric()) {
            return Err(format!(
                "the release target name {value:?} must start with a letter or a digit"
            ));
        }
        if !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            return Err(format!(
                "the release target name {value:?} may hold only letters, digits, '-', '_', \
                 and '.'"
            ));
        }
        Ok(())
    }

    /// One line of operator-written text, rendered beside the entry it describes.
    fn prose(value: &str) -> Result<(), String> {
        if value.trim().is_empty() {
            return Err(
                "a release declaration's `what`, `published_by`, and `why` are each what a \
                 reader learns from the entry they describe, so none of them may be blank"
                    .to_owned(),
            );
        }
        if value.len() > MAX_PROSE {
            return Err(format!(
                "the prose {value:?} is longer than {MAX_PROSE} characters; it is rendered on one \
                 line beside the entry it describes, and the reasoning behind it belongs in a \
                 comment"
            ));
        }
        if value.chars().any(char::is_control) {
            return Err(format!(
                "the prose {value:?} carries a control character; it is rendered on one line, so \
                 it must be one"
            ));
        }
        Ok(())
    }

    /// A path to something a *checkout* of this repository carries.
    ///
    /// Decided on how the path is spelled, never on what the reader's own platform
    /// makes of it: six repositories share one document and a consumer resolves it on
    /// whichever machine it runs on, so both separators are separators, a leading
    /// separator is absolute everywhere, and a leading drive letter names a location
    /// on whoever resolves it.
    fn repository_path(value: &str) -> Result<(), String> {
        const SEPARATORS: [char; 2] = ['/', '\\'];
        if value.is_empty() {
            return Err("a release declaration names an empty path".to_owned());
        }
        if value.starts_with(SEPARATORS) {
            return Err(format!(
                "the path {value:?} is absolute; it is a path relative to the repository root, \
                 because it names something the repository being released carries"
            ));
        }
        let mut characters = value.chars();
        if matches!(
            (characters.next(), characters.next()),
            (Some(drive), Some(':')) if drive.is_ascii_alphabetic()
        ) {
            return Err(format!(
                "the path {value:?} names a drive on the reader's own machine; it is a path \
                 relative to the repository root"
            ));
        }
        if value.split(SEPARATORS).any(|component| component == "..") {
            return Err(format!(
                "the path {value:?} leaves the repository root; it names something the \
                 repository being released carries"
            ));
        }
        Ok(())
    }

    /// Everything a whole document can be wrong about, and every field that is wrong
    /// on its own. Each refusal names the entry it is about by position and identifier.
    fn validate(declaration: &Declaration, origin: &str) -> Result<(), String> {
        let at = |kind: &str, index: usize, id: &str| format!("[[{kind}]] {} ({id:?})", index + 1);
        let fail = |where_: &str, failure: String| {
            format!("the release declaration at {origin} has {where_}: {failure}")
        };

        if let Some(probe) = &declaration.probe {
            repository_path(probe).map_err(|failure| fail("a `probe`", failure))?;
        }
        if declaration.targets.is_empty() {
            return Err(format!(
                "the release declaration at {origin} declares no [[target]]; a declaration that \
                 names nothing says less than no declaration at all, because a consumer reading \
                 it cannot tell whether this repository publishes nothing or nobody has said \
                 what it publishes"
            ));
        }

        let mut names: BTreeMap<&str, usize> = BTreeMap::new();
        let mut ids: BTreeMap<&str, usize> = BTreeMap::new();
        let mut covered: BTreeMap<&str, usize> = BTreeMap::new();
        for (index, target) in declaration.targets.iter().enumerate() {
            let here = at("target", index, &target.id);
            registry_id(&target.id).map_err(|failure| fail(&here, failure))?;
            target_name(&target.name).map_err(|failure| fail(&here, failure))?;
            prose(&target.what).map_err(|failure| fail(&here, failure))?;
            prose(&target.published_by).map_err(|failure| fail(&here, failure))?;
            if let Some(manifest) = &target.manifest {
                repository_path(manifest).map_err(|failure| fail(&here, failure))?;
            }
            if let Some(earlier) = names.insert(&target.name, index) {
                return Err(fail(
                    &here,
                    format!(
                        "it takes the short name {name:?}, which [[target]] {} already takes; the \
                         short name is what a host document and a consumer's plan name this \
                         target by, so two of them are two answers to one question",
                        earlier + 1,
                        name = target.name
                    ),
                ));
            }
            if let Some(earlier) = ids.insert(&target.id, index) {
                return Err(fail(
                    &here,
                    format!(
                        "it declares the identifier [[target]] {} already declares; one artifact \
                         is one target",
                        earlier + 1
                    ),
                ));
            }
            for id in &target.covers {
                registry_id(id).map_err(|failure| fail(&here, failure))?;
                if *id == target.id {
                    return Err(fail(
                        &here,
                        "it covers its own identifier; `covers` names what a target's \
                         release also ships and that is not a target of its own"
                            .to_owned(),
                    ));
                }
                if let Some(earlier) = covered.insert(id, index) {
                    return Err(fail(
                        &here,
                        format!(
                            "it covers {id:?}, which [[target]] {} already covers; one artifact \
                             is shipped by one release",
                            earlier + 1
                        ),
                    ));
                }
            }
        }
        // Covering something another target declares is only knowable once every
        // target has been read, so it is asked after the pass above rather than during.
        for (index, target) in declaration.targets.iter().enumerate() {
            for id in &target.covers {
                if let Some(other) = ids.get(id.as_str()) {
                    return Err(fail(
                        &at("target", index, &target.id),
                        format!(
                            "it covers {id:?}, which [[target]] {} declares as a target of its \
                             own; an artifact is one or the other, because a consumer waits on a \
                             target by name and never waits on something covered",
                            other + 1
                        ),
                    ));
                }
            }
        }

        let mut retired: BTreeMap<&str, usize> = BTreeMap::new();
        for (index, entry) in declaration.retired.iter().enumerate() {
            let here = at("retired", index, &entry.id);
            registry_id(&entry.id).map_err(|failure| fail(&here, failure))?;
            prose(&entry.why).map_err(|failure| fail(&here, failure))?;
            if let Some(target) = ids.get(entry.id.as_str()) {
                return Err(fail(
                    &here,
                    format!(
                        "it retires what [[target]] {} publishes; a retired artifact is one this \
                         repository does not publish any more",
                        target + 1
                    ),
                ));
            }
            if let Some(earlier) = retired.insert(&entry.id, index) {
                return Err(fail(
                    &here,
                    format!(
                        "it repeats what [[retired]] {} already records",
                        earlier + 1
                    ),
                ));
            }
        }
        Ok(())
    }
}
// llmlint: ignore-end[contracts_have_one_source_or_a_drift_gate]

// llmlint: ignore-block[no_panics_on_recoverable_errors] test support for the two release-target suites, and nothing else links it: each panic here is the failed assertion the calling test reports, naming what was missing, exactly as an `assert!` in the test would.
/// The workspace root: the checkout the declaration and the release configuration
/// both live in.
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the onejudge-repo package should be nested under the workspace root")
        .to_path_buf()
}

pub fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|err| panic!("reading {}: {err}", path.display()))
}

/// Where this repository's declaration is: one TOML document, at the root, under
/// the one name a consumer reading across repositories can find without being told.
pub fn declaration_path() -> PathBuf {
    repo_root().join("release-targets.toml")
}

/// This repository's own declaration, held to the canonical schema.
pub fn declaration() -> Declaration {
    let path = declaration_path();
    schema::parse(&read(&path), &path.display().to_string()).unwrap_or_else(|failure| {
        panic!("{failure}");
    })
}

/// The identifiers this repository declares, in the document's publication order.
pub fn declared_targets() -> Vec<String> {
    declaration()
        .targets
        .into_iter()
        .map(|target| target.id)
        .collect()
}

/// The probe's contract, as the real script over a real subprocess: the bound,
/// the spawn a consumer makes, and the two answers that are not a version.
///
/// Unix only: the contract is a direct spawn with no shell interposed, and
/// Windows cannot execute a `#!` script without one.
#[cfg(unix)]
pub mod probe {
    use std::env;
    use std::process::{Command, Output};
    use std::time::{Duration, Instant};

    use super::repo_root;

    /// The longest the contract lets one probe answer take; every assertion here
    /// holds the probe to it.
    pub const BOUND: Duration = Duration::from_secs(60);

    /// Spawn the probe exactly as a consumer does: directly, from the repository
    /// root, with an environment carrying only PATH and HOME — no credential, and
    /// no variable the caller happened to be holding.
    pub fn probe_on_path(path: &str, args: &[&str]) -> (Output, Duration) {
        let root = repo_root();
        let mut command = Command::new(root.join("scripts/release-probe.sh"));
        command
            .current_dir(&root)
            .args(args)
            .env_clear()
            .env("PATH", path);
        if let Ok(home) = env::var("HOME") {
            command.env("HOME", home);
        }
        let started = Instant::now();
        let output = command.output().expect("the probe should be executable");
        (output, started.elapsed())
    }

    pub fn probe(args: &[&str]) -> (Output, Duration) {
        probe_on_path(&env::var("PATH").expect("PATH"), args)
    }

    /// Not answered: a reason on stderr, nothing on stdout, non-zero exit.
    pub fn assert_not_answered(path: &str, args: &[&str]) {
        let (output, elapsed) = probe_on_path(path, args);
        assert!(
            !output.status.success(),
            "{args:?} should not be answered, but the probe exited 0 with {:?}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(
            output.stdout.is_empty(),
            "not-answered must leave stdout empty, or a caller reads it as a version: {:?}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("release-probe:"),
            "not-answered must carry its reason on stderr, got {:?}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(elapsed < BOUND, "{args:?} took {elapsed:?}");
    }

    /// No release yet: exit 0 and *nothing at all* on stdout.
    pub fn assert_no_release_yet(path: &str, args: &[&str]) {
        let (output, elapsed) = probe_on_path(path, args);
        assert!(
            output.status.success(),
            "{args:?} has no release, which is an answer, not a failure: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.stdout.is_empty(),
            "{args:?} has no release, so the probe must say nothing at all, got {:?}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(elapsed < BOUND, "{args:?} took {elapsed:?}");
    }
}
// llmlint: ignore-end[no_panics_on_recoverable_errors]
