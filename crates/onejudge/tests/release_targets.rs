//! Drift gate for what this repository *releases*, and the contract of the probe
//! that answers what a registry currently serves for it.
//!
//! A consumer that sequences work across repositories holds a dependent task
//! until the artifact it depends on is released; a repository that declares no
//! release target releases nothing as far as that consumer is concerned, so the
//! hold quietly stops happening. `release-targets.toml` at the repository root is
//! this repository's declaration and `scripts/release-probe.sh` answers it.
//!
//! The declaration is written against the **canonical release-target schema**,
//! which is defined once — in `docs/contract.md` of
//! github.com/nickderobertis/onevcs — and read across six repositories by
//! machinery that knows none of them. This suite is the half of that contract this
//! repository owes: [`schema`] is the canonical shape as a validator, and the
//! document is held to it here so a required field dropped, an identifier
//! malformed, or a short name repeated fails the gate rather than reaching a
//! consumer that cannot read it.
//!
//! The declaration is also the thing that goes stale silently, so this suite never
//! trusts it: it derives the published set from the *real* release configuration
//! — the release workflows and the manifests they build — and fails in both
//! directions. A new artifact in the workflows fails here instead of passing
//! unnoticed, and a declared target the workflows no longer publish fails too.
//!
//! The probe's three answers are proven by driving the real script. The two that
//! need a public registry are `#[ignore]`-d, like every other network-touching
//! test here (`live.rs`, `docs/live-tier.md`): the gate stays offline and
//! deterministic. Run them with `just test-release-targets`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use schema::Declaration;

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
/// It lives in the test rather than in the library because it is not onejudge's
/// public surface: onejudge *publishes* the document, and this is the gate that
/// what it publishes can be read.
mod schema {
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

/// The workspace root: the checkout the declaration and the release configuration
/// both live in.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("onejudge package should be nested under the workspace root")
        .to_path_buf()
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|err| panic!("reading {}: {err}", path.display()))
}

/// Where this repository's declaration is: one TOML document, at the root, under
/// the one name a consumer reading across repositories can find without being told.
fn declaration_path() -> PathBuf {
    repo_root().join("release-targets.toml")
}

/// This repository's own declaration, held to the canonical schema.
fn declaration() -> Declaration {
    let path = declaration_path();
    schema::parse(&read(&path), &path.display().to_string()).unwrap_or_else(|failure| {
        panic!("{failure}");
    })
}

/// The identifiers this repository declares, in the document's publication order.
fn declared_targets() -> Vec<String> {
    declaration()
        .targets
        .into_iter()
        .map(|target| target.id)
        .collect()
}

/// A TOML manifest, parsed.
fn manifest(path: &Path) -> toml::Value {
    toml::from_str(&read(path)).unwrap_or_else(|err| panic!("parsing {}: {err}", path.display()))
}

/// A string at `table.key` of a parsed manifest.
fn field<'a>(document: &'a toml::Value, table: &str, key: &str) -> Option<&'a str> {
    document.get(table)?.get(key)?.as_str()
}

/// Files under `dir` named `file_name`, skipping build output and vendored trees.
fn find(dir: &Path, file_name: &str, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if matches!(name.as_ref(), "target" | ".git" | "node_modules" | ".venv") {
                continue;
            }
            find(&path, file_name, found);
        } else if name == file_name {
            found.push(path);
        }
    }
}

/// The `.github/workflows/*.yml` files, read.
fn workflows(root: &Path) -> Vec<(PathBuf, String)> {
    let dir = root.join(".github/workflows");
    let mut entries: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|err| panic!("reading {}: {err}", dir.display()))
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext == "yml" || ext == "yaml")
        })
        .collect();
    entries.sort();
    assert!(
        !entries.is_empty(),
        "no workflows in {} — the release configuration this gate reads is gone",
        dir.display()
    );
    entries
        .into_iter()
        .map(|path| {
            let text = read(&path);
            (path, text)
        })
        .collect()
}

/// Every Python distribution this repository *builds*: the `[project] name` of
/// each `pyproject.toml` in the tree, mapped to the manifest that declares it.
fn python_distributions(root: &Path) -> BTreeMap<String, PathBuf> {
    let mut manifests = Vec::new();
    find(root, "pyproject.toml", &mut manifests);
    let mut distributions = BTreeMap::new();
    for path in manifests {
        let document = manifest(&path);
        if let Some(name) = field(&document, "project", "name") {
            distributions.insert(name.to_owned(), path);
        }
    }
    distributions
}

/// The registry-qualified names this repository publishes, mapped to the manifest
/// each one's name and version come from — read out of the real release
/// configuration rather than an inventory transcribed into this test.
///
/// * crates.io — release-plz's `command: release` step publishes the workspace's
///   packages, so every member manifest that does not opt out is a crate target.
/// * PyPI — each `pypa/gh-action-pypi-publish` step publishes exactly one
///   distribution and names it (`- name: Publish <dist>`); that name has to be a
///   distribution the tree actually builds.
///
/// npm is not a registry this repository publishes to; that is its own assertion
/// below rather than a branch here, so it stays a fact the workflows prove.
fn published_targets() -> BTreeMap<String, PathBuf> {
    let root = repo_root();
    let mut published = BTreeMap::new();
    let workflows = workflows(&root);

    let release_plz = workflows
        .iter()
        .find(|(_, text)| text.contains("release-plz/action"))
        .map(|(_, text)| text)
        .expect("no release-plz workflow — the crates.io publisher this gate reads is gone");
    if release_plz
        .lines()
        .any(|line| line.trim() == "command: release")
    {
        let workspace = manifest(&root.join("Cargo.toml"));
        let members: Vec<&str> = workspace
            .get("workspace")
            .and_then(|table| table.get("members"))
            .and_then(toml::Value::as_array)
            .map(|members| members.iter().filter_map(toml::Value::as_str).collect())
            .unwrap_or_default();
        assert!(
            !members.is_empty(),
            "no `[workspace] members` in Cargo.toml"
        );
        for member in members {
            let path = root.join(member).join("Cargo.toml");
            let document = manifest(&path);
            if document
                .get("package")
                .and_then(|table| table.get("publish"))
                .and_then(toml::Value::as_bool)
                == Some(false)
            {
                continue;
            }
            let name = field(&document, "package", "name")
                .unwrap_or_else(|| panic!("no `[package] name` in {}", path.display()));
            published.insert(format!("crate:{name}"), path);
        }
    }

    let distributions = python_distributions(&root);
    for (path, text) in &workflows {
        let lines: Vec<&str> = text.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            if line.trim_start().starts_with('#') || !line.contains("pypa/gh-action-pypi-publish") {
                continue;
            }
            let step = lines[..index]
                .iter()
                .rev()
                .find_map(|line| line.trim().strip_prefix("- name: "))
                .unwrap_or_else(|| {
                    panic!(
                        "{}: a PyPI publish step with no `- name:` above it — this gate reads the \
                         published distribution out of that name",
                        path.display()
                    )
                });
            let distribution = step.trim().strip_prefix("Publish ").unwrap_or_else(|| {
                panic!(
                    "{}: PyPI publish step named {step:?} — name it `Publish <distribution>` so \
                     what it publishes is readable from the workflow",
                    path.display()
                )
            });
            let manifest_path = distributions.get(distribution).unwrap_or_else(|| {
                panic!(
                    "{}: publishes `{distribution}` to PyPI, but no pyproject.toml in the tree \
                     declares that distribution (found: {:?})",
                    path.display(),
                    distributions.keys().collect::<Vec<_>>()
                )
            });
            published.insert(format!("pypi:{distribution}"), manifest_path.clone());
        }
    }

    published
}

/// The document is one a reader with a standard TOML parser and no knowledge of
/// this repository can act on: it conforms to the canonical schema, and every
/// artifact this repository publishes is listed with the short name it is waited
/// on by.
#[test]
fn the_declaration_conforms_to_the_canonical_schema() {
    let declaration = declaration();
    assert_eq!(
        declaration.schema_version,
        schema::SCHEMA_VERSION,
        "this repository writes the schema this gate reads"
    );
    assert_eq!(
        declaration.probe.as_deref(),
        Some("scripts/release-probe.sh"),
        "the declaration names the script that answers what a registry serves for one id"
    );

    let listed: Vec<(String, String)> = declaration
        .targets
        .iter()
        .map(|target| (target.name.clone(), target.id.clone()))
        .collect();
    assert_eq!(
        listed,
        vec![
            ("crate".to_owned(), "crate:onejudge".to_owned()),
            ("cli".to_owned(), "pypi:onejudge-cli".to_owned()),
            ("sdk".to_owned(), "pypi:onejudge".to_owned()),
        ],
        "every artifact this repository publishes, and the short name each is waited on by"
    );
}

/// Every `manifest` a target names is a file this checkout actually carries, and
/// it is the manifest the release configuration publishes that target from. A
/// pointer at a file that is not there is worse than no pointer: a consumer
/// resolves it against a checkout and gets nothing.
#[test]
fn every_declared_manifest_is_the_one_that_publishes_the_target() {
    let root = repo_root();
    let published = published_targets();
    for target in declaration().targets {
        let declared = target
            .manifest
            .unwrap_or_else(|| panic!("`{}` names no manifest", target.id));
        let path = root.join(&declared);
        assert!(
            path.is_file(),
            "`{}` names the manifest {declared}, which this checkout does not carry",
            target.id
        );
        assert_eq!(
            Some(&path),
            published.get(&target.id),
            "`{}` names the manifest {declared}, which is not the one the release configuration \
             publishes it from",
            target.id
        );
    }
    assert!(
        declaration_path().is_file(),
        "the declaration is at the repository root, under the one name a consumer can find"
    );
}

/// Where a declaration and the release configuration disagree, in both
/// directions. Empty is agreement.
///
/// The comparison lives here rather than inside the assertion below, because the
/// assertion below can only ever be driven over a declaration that *agrees*: this
/// repository's own. What it does when the two disagree is the half that matters —
/// a published name no target declares is a consumer that never holds, and a
/// declared target nothing publishes is a consumer that holds forever — and
/// [`the_drift_check_fails_a_declaration_that_disagrees_with_the_workflows`]
/// drives this same function over real documents that really disagree.
fn drift(declared: &BTreeSet<String>, published: &BTreeSet<String>) -> Vec<String> {
    let mut disagreements = Vec::new();
    for undeclared in published.difference(declared) {
        disagreements.push(format!(
            "the release configuration publishes {undeclared:?}, which the declaration does not \
             declare — declare it, or account for it there as a per-platform build of a target \
             that is already declared"
        ));
    }
    for unpublished in declared.difference(published) {
        disagreements.push(format!(
            "the declaration declares {unpublished:?}, which no release workflow publishes — a \
             consumer would hold on it forever"
        ));
    }
    disagreements
}

/// The identifiers one declaration document declares, read by the real reader.
fn declared_in(document: &str, origin: &str) -> BTreeSet<String> {
    schema::parse(document, origin)
        .unwrap_or_else(|failure| panic!("{failure}"))
        .targets
        .into_iter()
        .map(|target| target.id)
        .collect()
}

/// The declaration with one `[[target]]` cut out of it — a document that really
/// says something different, for the reader to read back.
fn without_target(document: &str, id: &str) -> String {
    const HEADER: &str = "\n[[target]]\n";
    let declared = format!("id = \"{id}\"");
    let mut parts = document.split(HEADER);
    let mut kept = parts
        .next()
        .expect("split always yields the text before the first target")
        .to_owned();
    let mut dropped = false;
    for part in parts {
        if part.lines().any(|line| line.trim() == declared) {
            dropped = true;
            continue;
        }
        kept.push_str(HEADER);
        kept.push_str(part);
    }
    assert!(dropped, "the document declares no `{id}` to cut out");
    kept
}

/// The whole point: this repository's declaration and its release configuration
/// agree.
#[test]
fn the_declaration_matches_the_real_release_configuration() {
    let declared: BTreeSet<String> = declared_targets().into_iter().collect();
    let published: BTreeSet<String> = published_targets().into_keys().collect();
    let disagreements = drift(&declared, &published);
    assert!(
        disagreements.is_empty(),
        "{}\nDeclared: {declared:?}\nPublished: {published:?}",
        disagreements.join("\n")
    );
}

/// The drift check fails in *both* directions, driven end to end: a real document,
/// edited to really disagree with this repository's real release configuration,
/// read back by the same reader and compared by the same function the assertion
/// above is made of.
#[test]
fn the_drift_check_fails_a_declaration_that_disagrees_with_the_workflows() {
    let published: BTreeSet<String> = published_targets().into_keys().collect();
    let real = read(&declaration_path());

    // A name this repository publishes without declaring: the SDK's target, cut.
    let without_sdk = without_target(&real, "pypi:onejudge");
    let declared = declared_in(&without_sdk, "the declaration with its SDK target cut");
    let disagreements = drift(&declared, &published);
    assert_eq!(
        disagreements.len(),
        1,
        "one artifact was undeclared: {disagreements:?}"
    );
    assert!(
        disagreements[0].contains("publishes \"pypi:onejudge\"")
            && disagreements[0].contains("does not declare"),
        "an undeclared published artifact must be reported as one: {disagreements:?}"
    );

    // A name declared without publishing: a target nothing here releases.
    let with_phantom = format!(
        "{real}\n[[target]]\nid = \"npm:onejudge\"\nname = \"npm\"\nwhat = \"A launcher \
         nothing here publishes.\"\npublished_by = \"Nothing: no workflow in this repository \
         publishes to npm.\"\n"
    );
    let declared = declared_in(&with_phantom, "the declaration with a phantom npm target");
    let disagreements = drift(&declared, &published);
    assert_eq!(
        disagreements.len(),
        1,
        "one target was unpublished: {disagreements:?}"
    );
    assert!(
        disagreements[0].contains("declares \"npm:onejudge\"")
            && disagreements[0].contains("hold on it forever"),
        "a declared target no workflow publishes must be reported as one: {disagreements:?}"
    );
}

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

/// This repository publishes nothing to npm, so no target names that registry.
/// The day a workflow does, the published set grows a kind this gate does not
/// derive — so it fails here, where the fix is to derive it and declare it,
/// rather than shipping an artifact no consumer can wait on.
#[test]
fn nothing_here_publishes_to_npm() {
    for (path, text) in workflows(&repo_root()) {
        let publishes = text
            .lines()
            .any(|line| !line.trim_start().starts_with('#') && line.contains("npm publish"));
        assert!(
            !publishes,
            "{} runs `npm publish`: teach published_targets() to derive npm names and declare \
             them in release-targets.toml",
            path.display()
        );
    }
}

/// No other declaration of this repository's release targets survives anywhere in
/// the tree. Two documents answering "what does this repository publish?" is the
/// defect this document exists to end: a consumer reading the stale one waits on
/// an artifact nobody releases, or fails to wait on one somebody does.
#[test]
fn the_declaration_is_the_only_one() {
    let root = repo_root();
    for stale in ["registry-targets.txt", "release-targets.txt"] {
        let mut found = Vec::new();
        find(&root, stale, &mut found);
        assert!(
            found.is_empty(),
            "{found:?} declares release targets beside release-targets.toml; one repository \
             answers what it publishes once"
        );
    }
}

/// The refusals, driven end to end: each of these is a whole document handed to
/// the same reader this repository's own file goes through, and each must be
/// refused with a message naming what is wrong.
///
/// A checker that only ever sees a conforming document proves nothing, and these
/// are the three defects the canonical schema exists to catch — a required field
/// dropped, an identifier malformed, a short name repeated — plus the ones that
/// let a hand-written document say something no repository can mean.
#[test]
fn a_document_that_does_not_conform_is_refused() {
    let conforming = r#"
schema_version = 1
probe = "scripts/release-probe.sh"

[[target]]
id = "crate:onejudge"
name = "crate"
what = "The library."
published_by = "release-plz.yml, from crates/onejudge/Cargo.toml."
manifest = "crates/onejudge/Cargo.toml"
"#;
    schema::parse(conforming, "the fixture")
        .expect("the fixture these refusals are edits of must itself conform");

    for (document, expected) in [
        // A required field dropped.
        (
            conforming.replace("what = \"The library.\"\n", ""),
            "missing field `what`",
        ),
        (
            conforming.replace("id = \"crate:onejudge\"\n", ""),
            "missing field `id`",
        ),
        (
            conforming.replace("schema_version = 1\n", ""),
            "declares no schema_version",
        ),
        // An identifier that is not registry-qualified, or not a name a registry
        // serves. `onejudge` alone names both the crate and the SDK distribution.
        (
            conforming.replace("crate:onejudge\"", "onejudge\""),
            "names no registry",
        ),
        (
            conforming.replace("crate:onejudge\"", "crate:not a name\""),
            "is not a name a registry serves",
        ),
        // A short name repeated: two answers to the one question a host document
        // and a consumer's plan ask.
        (
            format!(
                "{conforming}\n{}",
                conforming
                    .replace(
                        "schema_version = 1\nprobe = \"scripts/release-probe.sh\"\n",
                        ""
                    )
                    .replace("crate:onejudge", "pypi:onejudge")
            ),
            "already takes",
        ),
        // A key this schema does not declare: read as an absent one, it would
        // publish an answer nobody wrote.
        (
            conforming.replace("manifest =", "manifset ="),
            "which schema_version 1 does not declare",
        ),
        // Prose a reader learns nothing from.
        (
            conforming.replace("\"The library.\"", "\"   \""),
            "none of them may be blank",
        ),
        // A path that names a place outside a checkout of this repository.
        (
            conforming.replace("\"scripts/release-probe.sh\"", "\"../elsewhere/probe.sh\""),
            "leaves the repository root",
        ),
        (
            conforming.replace("\"scripts/release-probe.sh\"", "\"/usr/bin/probe\""),
            "is absolute",
        ),
        (
            conforming.replace("\"crates/onejudge/Cargo.toml\"", "\"C:\\\\Cargo.toml\""),
            "names a drive",
        ),
        // A declaration that names nothing says less than no declaration at all.
        ("schema_version = 1\n".to_owned(), "declares no [[target]]"),
        // `covers` names what a target's release also ships and is not a target.
        (
            format!("{conforming}covers = [\"crate:onejudge\"]\n"),
            "covers its own identifier",
        ),
        // A retired artifact is one this repository does not publish any more.
        (
            format!("{conforming}\n[[retired]]\nid = \"crate:onejudge\"\nwhy = \"Gone.\"\n"),
            "retires what [[target]] 1 publishes",
        ),
    ] {
        let failure = schema::parse(&document, "the fixture").expect_err(&format!(
            "this document must be refused, expecting {expected:?}:\n{document}"
        ));
        assert!(
            failure.contains(expected),
            "the refusal must name what is wrong; expected {expected:?}, got {failure:?}"
        );
    }
}

/// A declaration written against a *later* schema is read as this shape, with
/// whatever it names beyond it ignored — so a consumer one release behind still
/// learns what a repository one release ahead publishes.
#[test]
fn a_later_schema_is_read_leniently_and_an_older_one_is_refused() {
    let later = r#"
schema_version = 2
[[target]]
id = "crate:onejudge"
name = "crate"
what = "The library."
published_by = "release-plz.yml, from crates/onejudge/Cargo.toml."
something_schema_2_adds = "ignored by a reader that does not know it"
"#;
    let declaration =
        schema::parse(later, "the fixture").expect("a later schema is read leniently");
    assert_eq!(declaration.targets.len(), 1);

    let older = later.replace("schema_version = 2", "schema_version = 0");
    let failure = schema::parse(&older, "the fixture").expect_err("schema 0 is not this shape");
    assert!(
        failure.contains("declares schema_version 0"),
        "got {failure:?}"
    );
}

/// The probe's contract, driven as the real script over a real subprocess.
///
/// The registry is faked the way the rest of this suite fakes the model: with a
/// **real** binary — a `curl` stand-in first on `PATH`, which is the probe's only
/// view of a registry — so "the registry failed" and "the registry serves nothing"
/// are deterministic offline journeys rather than untestable branches. The two
/// answers that must come from the true public registries are `#[ignore]`-d.
///
/// Unix only: the contract is a direct spawn with no shell interposed, and
/// Windows cannot execute a `#!` script without one.
#[cfg(unix)]
mod probe {
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::process::{Command, Output};
    use std::time::{Duration, Instant};
    use std::{env, fs};

    use super::{declared_targets, repo_root};

    /// The contract's bound: an answer well inside sixty seconds.
    const BOUND: Duration = Duration::from_secs(60);

    /// Spawn the probe exactly as a consumer does: directly, from the repository
    /// root, with an environment carrying only PATH and HOME — no credential, and
    /// no variable the caller happened to be holding.
    fn probe_on_path(path: &str, args: &[&str]) -> (Output, Duration) {
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

    fn probe(args: &[&str]) -> (Output, Duration) {
        probe_on_path(&env::var("PATH").expect("PATH"), args)
    }

    /// A directory of this test's own, emptied first so an earlier run's stand-in
    /// can never answer for this one.
    fn stub_dir(case: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join("release-probe")
            .join(case);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("the stub directory is creatable");
        dir
    }

    /// Where `bash` really is: the probe's shebang resolves its interpreter through
    /// PATH like anything else, so a PATH under test still has to carry it.
    fn interpreter() -> PathBuf {
        env::var("PATH")
            .unwrap_or_default()
            .split(':')
            .map(|dir| PathBuf::from(dir).join("bash"))
            .find(|candidate| candidate.is_file())
            .unwrap_or_else(|| PathBuf::from("/bin/bash"))
    }

    /// A registry stand-in, and the `PATH` that reaches it: a real `curl` that
    /// answers the one call the probe makes with a canned status and body, or
    /// refuses to connect at all (`exit_code` non-zero, as curl does at 7).
    /// Prepended to the real PATH, so everything else the probe needs is still
    /// found and only the registry is faked.
    fn registry_on_path(case: &str, status: &str, body: &str, exit_code: i32) -> String {
        assert!(!body.contains('\''), "the stand-in quotes the body with '");
        let dir = stub_dir(case);
        let curl = dir.join("curl");
        fs::write(
            &curl,
            format!(
                "#!/usr/bin/env bash\n\
                 # Registry stand-in for tests/release_targets.rs.\n\
                 set -eu\n\
                 out=\n\
                 prev=\n\
                 for arg in \"$@\"; do\n\
                 \x20   if [ \"$prev\" = --output ]; then out=$arg; fi\n\
                 \x20   prev=$arg\n\
                 done\n\
                 if [ -n \"$out\" ]; then printf '%s' '{body}' > \"$out\"; fi\n\
                 if [ {exit_code} -ne 0 ]; then\n\
                 \x20   echo 'curl: ({exit_code}) stand-in refused to connect' >&2\n\
                 \x20   exit {exit_code}\n\
                 fi\n\
                 printf '%s' '{status}'\n"
            ),
        )
        .expect("the registry stand-in is writable");
        fs::set_permissions(&curl, fs::Permissions::from_mode(0o755))
            .expect("the registry stand-in is executable");
        format!("{}:{}", dir.display(), env::var("PATH").unwrap_or_default())
    }

    /// Not answered: a reason on stderr, nothing on stdout, non-zero exit.
    fn assert_not_answered(path: &str, args: &[&str]) {
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
    fn assert_no_release_yet(path: &str, args: &[&str]) {
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

    /// The failure that matters most: an identifier the probe does not recognise
    /// must be *not answered*, never the empty output that means "no release yet".
    /// A consumer reading the second launches work whose dependency never landed.
    #[test]
    fn an_unrecognised_identifier_is_not_answered_rather_than_no_release_yet() {
        let path = env::var("PATH").expect("PATH");
        // Unqualified: `onejudge` alone names both the crate and the SDK wheel.
        assert_not_answered(&path, &["onejudge"]);
        // A registry this repository publishes nothing to.
        assert_not_answered(&path, &["npm:onejudge"]);
        // Qualified, but no artifact name at all.
        assert_not_answered(&path, &["crate:"]);
        // A name no registry could serve.
        assert_not_answered(&path, &["pypi:not a name"]);
    }

    /// Exactly one argument — no argument, and no second one to be ignored.
    #[test]
    fn the_probe_takes_exactly_one_identifier() {
        let path = env::var("PATH").expect("PATH");
        assert_not_answered(&path, &[]);
        assert_not_answered(&path, &["crate:onejudge", "pypi:onejudge"]);
    }

    /// Which registries the probe can answer for is the probe's own fact, so it is
    /// read off the probe: under a stand-in that serves nothing, a *recognised*
    /// identifier answers no-release-yet, and an unrecognised one does not. Every
    /// declared target has to be one the probe recognises, or the target is a hold
    /// that never resolves.
    #[test]
    fn the_probe_recognises_every_declared_target() {
        let path = registry_on_path("recognises", "404", "", 0);
        for target in declared_targets() {
            assert_no_release_yet(&path, &[&target]);
        }
    }

    /// A registry that could not be read is NOT a registry that has nothing to
    /// serve. Each of these is a way the lookup can fail after the identifier is
    /// recognised, and every one of them must stay on the not-answered side.
    #[test]
    fn a_registry_that_cannot_be_read_is_not_answered() {
        // Unreachable: curl itself fails (7 is its connect error).
        let unreachable = registry_on_path("unreachable", "", "", 7);
        assert_not_answered(&unreachable, &["pypi:onejudge"]);

        // Reached, but answering something neither served nor absent.
        let broken = registry_on_path("server-error", "500", "upstream is down", 0);
        assert_not_answered(&broken, &["pypi:onejudge"]);

        // Served, but with a payload no version can be read out of.
        let garbled = registry_on_path("garbled", "200", "<html>maintenance</html>", 0);
        assert_not_answered(&garbled, &["pypi:onejudge"]);

        // Served, well-formed, and empty where the version belongs.
        let empty = registry_on_path("empty-version", "200", r#"{"info": {"version": ""}}"#, 0);
        assert_not_answered(&empty, &["pypi:onejudge"]);
    }

    /// The probe assumes only PATH and HOME, so a PATH that cannot reach what it
    /// looks things up with is not-answered — never a silent no-release-yet.
    ///
    /// The PATH still carries `bash`, because a shebang that cannot resolve its
    /// interpreter never starts the probe at all: that would prove the harness,
    /// not the contract.
    #[test]
    fn a_lookup_tool_the_path_cannot_reach_is_not_answered() {
        let dir = stub_dir("no-tools");
        std::os::unix::fs::symlink(interpreter(), dir.join("bash"))
            .expect("the interpreter is linkable");
        assert_not_answered(&dir.display().to_string(), &["pypi:onejudge"]);
    }

    /// The two remaining answers, against a registry stand-in: a version it serves,
    /// and nothing for an artifact it has never served. The same two are proven
    /// against the true registries in the network tier below.
    #[test]
    fn a_stand_in_registry_answers_the_version_it_serves() {
        let pypi = registry_on_path("pypi-served", "200", r#"{"info": {"version": "1.2.3"}}"#, 0);
        let (output, _) = probe_on_path(&pypi, &["pypi:onejudge"]);
        assert!(output.status.success(), "the stand-in served a version");
        assert_eq!(String::from_utf8_lossy(&output.stdout), "1.2.3\n");

        let crates = registry_on_path(
            "crate-served",
            "200",
            r#"{"crate": {"max_stable_version": "1.2.3", "newest_version": "2.0.0-rc.1"}}"#,
            0,
        );
        let (output, _) = probe_on_path(&crates, &["crate:onejudge"]);
        assert!(output.status.success(), "the stand-in served a version");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "1.2.3\n",
            "a prerelease is not what the registry serves to a dependent"
        );

        // A crate whose only releases are prereleases serves `max_stable_version:
        // null`. Something IS published, so the prerelease is the answer: saying
        // nothing would read as "no release yet" and hold a consumer on a release
        // that already happened.
        let prerelease_only = registry_on_path(
            "crate-prerelease-only",
            "200",
            r#"{"crate": {"max_stable_version": null, "newest_version": "2.0.0-rc.1"}}"#,
            0,
        );
        let (output, _) = probe_on_path(&prerelease_only, &["crate:onejudge"]);
        assert!(output.status.success(), "the stand-in served a prerelease");
        assert_eq!(String::from_utf8_lossy(&output.stdout), "2.0.0-rc.1\n");

        assert_no_release_yet(
            &registry_on_path("never-served", "404", "", 0),
            &["crate:onejudge"],
        );
    }

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
