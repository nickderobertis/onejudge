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
//! repository owes: [`schema`] (this crate's library, shared with the network
//! tier) is the canonical shape as a validator, and the
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
//! need a public registry, and the reconciliation of [`schema`] with its upstream
//! definition, are the `onejudge-release-targets` network tier, out of the gate
//! like every other network-touching suite here (`docs/live-tier.md`): this one
//! stays offline and deterministic. Run that tier with `just test-release-targets`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use onejudge_repo::{declaration, declaration_path, declared_targets, read, repo_root, schema};

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
            // `python/dist` is where the SDK's packer stages a stamped copy of its
            // manifest — build output, like `target`, that the gate's own wheel
            // journey writes while this suite may be reading the tree.
            if matches!(
                name.as_ref(),
                "target" | ".git" | "node_modules" | ".venv" | ".nx" | "dist"
            ) {
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
/// each `pyproject.toml` in the tree that is a package, mapped to the manifest
/// that declares it. A manifest that says it is not one (`[tool.uv] package =
/// false`, a project of the graph that is never built) is left out.
fn python_distributions(root: &Path) -> BTreeMap<String, PathBuf> {
    let mut manifests = Vec::new();
    find(root, "pyproject.toml", &mut manifests);
    let mut distributions = BTreeMap::new();
    for path in manifests {
        let document = manifest(&path);
        let packaged = document
            .get("tool")
            .and_then(|tool| tool.get("uv"))
            .and_then(|uv| uv.get("package"))
            .and_then(toml::Value::as_bool)
            != Some(false);
        if let Some(name) = field(&document, "project", "name").filter(|_| packaged) {
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
///
/// Both ways a second one arrives are asked. A nested `release-targets.toml` is
/// the likelier of the two now — the name is canonical, so a subdirectory that
/// grows one is a document a reader could reasonably find — and the two legacy
/// names are what this repository and its siblings declared targets in before.
#[test]
fn the_declaration_is_the_only_one() {
    let root = repo_root();

    let mut found = Vec::new();
    find(&root, "release-targets.toml", &mut found);
    assert_eq!(
        found,
        vec![declaration_path()],
        "the declaration is one document, at the repository root; a second one is a second \
         answer to the one question a consumer asks"
    );

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
/// answers that must come from the true public registries are the
/// `onejudge-release-targets` network tier.
///
/// Unix only: the contract is a direct spawn with no shell interposed, and
/// Windows cannot execute a `#!` script without one.
#[cfg(unix)]
mod probe {
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::{env, fs};

    use super::declared_targets;
    use onejudge_repo::probe::{assert_no_release_yet, assert_not_answered, probe_on_path};

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
}
