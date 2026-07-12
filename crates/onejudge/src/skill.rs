//! Loading a **skill** from disk: a directory containing a `SKILL.md` whose YAML
//! frontmatter carries the skill's `name` / `description` and whose Markdown body
//! is the instruction text handed to the harness as a system prompt.
//!
//! This is onejudge's owned skill-execution primitive. A higher-level framework
//! (e.g. [`skilltest`](https://github.com/nickderobertis/skilltest)) resolves a
//! skill *path* and composes over [`load_skill`] rather than re-implementing the
//! SKILL.md format. Gated behind the `skill` feature so a bare `cargo add onejudge`
//! never pulls a YAML parser.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{Error, Result};

/// The YAML frontmatter at the top of a `SKILL.md`. `name` / `description` are
/// optional here (validating that they are present is a separate concern); unknown
/// keys are allowed so authors can carry extra metadata.
#[derive(Debug, Clone, Deserialize)]
pub struct Frontmatter {
    /// The skill's short name.
    pub name: Option<String>,
    /// A one-line description of what the skill does.
    pub description: Option<String>,
    /// An optional license identifier.
    #[serde(default)]
    pub license: Option<String>,
}

/// A skill loaded from disk: where it lives, its parsed frontmatter, and the
/// instruction body (everything after the frontmatter) that is handed to a
/// provider as the skill's system prompt.
#[derive(Debug, Clone)]
pub struct SkillDefinition {
    /// The directory the skill was loaded from.
    pub dir: PathBuf,
    /// The skill's name (from frontmatter; empty when absent).
    pub name: String,
    /// The skill's description (from frontmatter; empty when absent).
    pub description: String,
    /// The instruction body after the frontmatter — the skill's system prompt.
    pub instructions: String,
}

/// Split a `SKILL.md` into `(frontmatter_yaml, body)`. Returns `None` for the
/// frontmatter when the document does not open with a `---` fence (or opens one it
/// never closes).
fn split_frontmatter(text: &str) -> (Option<&str>, &str) {
    let rest = match text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
    {
        Some(rest) => rest,
        None => return (None, text),
    };
    // Find the closing fence at the start of a line.
    for sep in ["\n---\n", "\n---\r\n", "\r\n---\r\n"] {
        if let Some(idx) = rest.find(sep) {
            let fm = &rest[..idx];
            let body = &rest[idx + sep.len()..];
            return (Some(fm), body);
        }
    }
    // Opened a fence but never closed it.
    (None, text)
}

/// Load a skill definition from a directory containing `SKILL.md`.
///
/// # Errors
/// [`Error::Invalid`] if `SKILL.md` cannot be read, or if its frontmatter is not
/// valid YAML — the path and the underlying cause are in the message.
pub fn load_skill(dir: &Path) -> Result<SkillDefinition> {
    let skill_md = dir.join("SKILL.md");
    let text = std::fs::read_to_string(&skill_md).map_err(|source| {
        Error::Invalid(format!("could not read `{}`: {source}", skill_md.display()))
    })?;
    let (fm, body) = split_frontmatter(&text);
    let frontmatter: Frontmatter = match fm {
        Some(fm) => serde_yaml_ng::from_str(fm).map_err(|source| {
            Error::Invalid(format!(
                "invalid SKILL.md frontmatter in `{}`: {source}",
                skill_md.display()
            ))
        })?,
        None => Frontmatter {
            name: None,
            description: None,
            license: None,
        },
    };
    Ok(SkillDefinition {
        dir: dir.to_path_buf(),
        name: frontmatter.name.unwrap_or_default(),
        description: frontmatter.description.unwrap_or_default(),
        instructions: body.trim().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_frontmatter_and_body() {
        let text = "---\nname: greeter\ndescription: hi\n---\nBody here\n";
        let (fm, body) = split_frontmatter(text);
        assert_eq!(fm, Some("name: greeter\ndescription: hi"));
        assert_eq!(body, "Body here\n");
    }

    #[test]
    fn no_frontmatter_returns_none() {
        let (fm, body) = split_frontmatter("# Just a heading\n");
        assert!(fm.is_none());
        assert_eq!(body, "# Just a heading\n");
    }

    #[test]
    fn unclosed_fence_yields_no_frontmatter() {
        // Opening `---` but no closing fence falls back to "no frontmatter".
        let (fm, body) = split_frontmatter("---\nname: x\nstill going\n");
        assert!(fm.is_none());
        assert_eq!(body, "---\nname: x\nstill going\n");
    }

    /// Make a unique temp skill directory with a `SKILL.md` of `contents`.
    fn skill_dir(tag: &str, name: &str, contents: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "onejudge-skill-{}-{tag}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), contents).unwrap();
        dir
    }

    #[test]
    fn load_skill_reads_frontmatter_and_body() {
        let dir = skill_dir(
            "load",
            "greeter",
            "---\nname: greeter\ndescription: a friendly greeter\n---\nGreet warmly.\n",
        );
        let skill = load_skill(&dir).unwrap();
        assert_eq!(skill.name, "greeter");
        assert_eq!(skill.description, "a friendly greeter");
        assert_eq!(skill.instructions, "Greet warmly.");
        assert_eq!(skill.dir, dir);
    }

    #[test]
    fn load_skill_without_frontmatter_uses_defaults() {
        let dir = skill_dir("nofm", "bare", "# Just a body\nNo frontmatter here.\n");
        let skill = load_skill(&dir).unwrap();
        assert_eq!(skill.name, "");
        assert_eq!(skill.description, "");
        assert!(skill.instructions.contains("No frontmatter here."));
    }

    #[test]
    fn load_skill_missing_file_is_invalid() {
        let dir = std::env::temp_dir().join(format!("onejudge-missing-{}", std::process::id()));
        let err = load_skill(&dir).unwrap_err();
        assert!(matches!(err, Error::Invalid(m) if m.contains("could not read")));
    }

    #[test]
    fn load_skill_bad_frontmatter_is_invalid() {
        let dir = skill_dir("badyaml", "x", "---\nname: [unterminated\n---\nbody\n");
        let err = load_skill(&dir).unwrap_err();
        assert!(matches!(err, Error::Invalid(m) if m.contains("frontmatter")));
    }
}
