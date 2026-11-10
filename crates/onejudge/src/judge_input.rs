//! The material a **judge-only** run judges in place of a worker's turn: a
//! document's text, handed to the judge side once against the run's bar with no
//! agent side run at all (`Engine::judge_only`, `Plan::with_judge_input` under the
//! `cli` feature, `onejudge run --judge-only`). See `docs/cli.md`.
//!
//! A model judge reads the text from its prompt, where it stands as the reply to
//! the run's task. A judge whose verdict is a run over files — an `llmlint` judge
//! — needs the text *on disk* and inside the directory it lints from, because a
//! file outside that root matches none of its globs and would pass vacuously. So
//! the engine places it: a file under the worktree that still holds the text read
//! from it is judged where it lies, and anything else — that text included, once
//! the file has changed — is written to a scratch root of its own for the length
//! of the judgement, so every judge judges the same captured material. [`JudgedFile`] is what a judge is told
//! ([`Provider::set_judged_file`](crate::Provider::set_judged_file)).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::{Error, Result};

/// The file name an in-memory judge input is written under when it names none —
/// Markdown, because a document under review is the case this mode exists for
/// and a linter's globs most often select it.
pub const DEFAULT_JUDGE_INPUT_NAME: &str = "judge-input.md";

/// The material a judge-only run judges: its text, and — when it was read from a
/// file — where, so a judge that reads files judges that file where it lies.
///
/// Build one with [`JudgeInput::text`] or [`JudgeInput::from_path`]; the fields
/// are readable, and the type is non-exhaustive so a later field is not a break.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct JudgeInput {
    /// The material, verbatim. This is what a model judge is handed as the reply
    /// under judgement.
    pub text: String,
    /// The file name the material is judged under when it has to be written out
    /// (an in-memory input): [`DEFAULT_JUDGE_INPUT_NAME`] unless one was given
    /// with [`JudgeInput::named`]. A linter selects files by name, so naming the
    /// material for the globs it is meant to meet is how an in-memory input
    /// reaches the same rules a file would.
    pub name: Option<String>,
    /// The file the material was read from, when it was.
    pub path: Option<PathBuf>,
}

impl JudgeInput {
    /// In-memory material, judged under [`DEFAULT_JUDGE_INPUT_NAME`] unless
    /// [`named`](Self::named).
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            name: None,
            path: None,
        }
    }

    /// The material held in the file at `path`, read now.
    ///
    /// # Errors
    /// The read's own error when the file cannot be read as UTF-8 text.
    pub fn from_path(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        let text = std::fs::read_to_string(&path)?;
        Ok(Self {
            text,
            name: None,
            path: Some(path),
        })
    }

    /// Judge in-memory material under the file name `name` (builder style). A
    /// name is a bare file name — it is refused when the run starts if it holds a
    /// path separator or is empty.
    #[must_use]
    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// The file name the material is written under when it has to be.
    fn file_name(&self) -> Result<String> {
        let name = match (&self.name, &self.path) {
            (Some(name), _) => name.clone(),
            (None, Some(path)) => path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| DEFAULT_JUDGE_INPUT_NAME.to_string()),
            (None, None) => DEFAULT_JUDGE_INPUT_NAME.to_string(),
        };
        let bare = Path::new(&name)
            .file_name()
            .is_some_and(|file| file == name.as_str());
        if name.trim().is_empty() || !bare {
            return Err(Error::Invalid(format!(
                "the judge input's name `{name}` is not a bare file name"
            )));
        }
        Ok(name)
    }
}

/// Where a judge that reads files finds the material of a judge-only run: the
/// `file` holding it, and the `root` to judge it from — the directory it lies
/// under, which a linter's globs are rooted at.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct JudgedFile {
    /// The file holding the material, as an absolute path.
    pub file: PathBuf,
    /// The directory to judge `file` from: the run's worktree when the material
    /// is a file under it, else a scratch directory holding only that file.
    pub root: PathBuf,
    /// Whether `root` is a scratch directory onejudge made for this judgement —
    /// so it holds none of the worktree's own configuration, and a judge that
    /// discovers its configuration from where it runs has to be given one.
    pub scratch: bool,
}

/// The material placed for the length of one judgement; a scratch root is
/// removed when this is dropped.
pub(crate) struct Placed {
    pub(crate) file: JudgedFile,
}

impl Drop for Placed {
    fn drop(&mut self) {
        if self.file.scratch {
            let _ = std::fs::remove_dir_all(&self.file.root);
        }
    }
}

/// `path` canonicalized, without the `\\?\` verbatim prefix Windows puts on a
/// canonical path: a consumer handed the path — a linter, a prompt — reads the
/// ordinary spelling.
fn canonical(path: &Path) -> std::io::Result<PathBuf> {
    let path = std::fs::canonicalize(path)?;
    let text = path.to_string_lossy();
    Ok(match text.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => path,
    })
}

/// Place `input` for judging against `worktree`: in place when it is a file under
/// the worktree that still holds exactly the captured text, otherwise that text
/// written into a fresh scratch root of its own under the same file name.
pub(crate) fn place(input: &JudgeInput, worktree: &str) -> Result<Placed> {
    let name = input.file_name()?;
    let worktree = canonical(Path::new(worktree)).ok();
    if let (Some(path), Some(worktree)) = (&input.path, &worktree) {
        if let Ok(file) = canonical(path) {
            // Judged where it lies only while it still holds the text that was
            // captured: every judge must judge the same material, and a model
            // judge is handed `input.text`, so a file that changed since is
            // judged from a snapshot of the captured text instead.
            let unchanged = std::fs::read(&file).is_ok_and(|now| now == input.text.as_bytes());
            if file.starts_with(worktree) && unchanged {
                return Ok(Placed {
                    file: JudgedFile {
                        file,
                        root: worktree.clone(),
                        scratch: false,
                    },
                });
            }
        }
    }
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let root = std::env::temp_dir().join(format!(
        "onejudge-judge-input-{}-{}-{nanos}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let written = (|| {
        std::fs::create_dir_all(&root)?;
        let root = canonical(&root)?;
        let file = root.join(&name);
        std::fs::write(&file, &input.text)?;
        Ok::<_, std::io::Error>(JudgedFile {
            file,
            root,
            scratch: true,
        })
    })();
    match written {
        Ok(file) => Ok(Placed { file }),
        Err(e) => {
            let _ = std::fs::remove_dir_all(&root);
            Err(Error::Invalid(format!(
                "could not write the judge input to a scratch directory under {}: {e}",
                std::env::temp_dir().display()
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "onejudge-judge-input-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_file_under_the_worktree_is_judged_where_it_lies() {
        let worktree = scratch("in-place");
        std::fs::create_dir_all(worktree.join("docs")).unwrap();
        std::fs::write(worktree.join("docs/plan.md"), "the plan").unwrap();
        let input = JudgeInput::from_path(worktree.join("docs/plan.md")).unwrap();
        let placed = place(&input, worktree.to_str().unwrap()).unwrap();
        let root = canonical(&worktree).unwrap();
        assert_eq!(placed.file.root, root);
        assert_eq!(placed.file.file, root.join("docs/plan.md"));
        assert!(!placed.file.scratch);
        drop(placed);
        assert!(worktree.join("docs/plan.md").exists(), "never removed");
    }

    #[test]
    fn a_file_changed_since_it_was_read_is_judged_from_a_snapshot_of_what_was_read() {
        let worktree = scratch("changed");
        std::fs::write(worktree.join("plan.md"), "the captured plan").unwrap();
        let input = JudgeInput::from_path(worktree.join("plan.md")).unwrap();
        std::fs::write(worktree.join("plan.md"), "an edited plan").unwrap();
        let placed = place(&input, worktree.to_str().unwrap()).unwrap();
        assert!(placed.file.scratch);
        assert_eq!(placed.file.file.file_name().unwrap(), "plan.md");
        assert_eq!(
            std::fs::read_to_string(&placed.file.file).unwrap(),
            "the captured plan"
        );
    }

    #[test]
    fn in_memory_material_gets_a_scratch_root_removed_after() {
        let worktree = scratch("in-memory");
        let input = JudgeInput::text("a plan").named("plan.txt");
        let placed = place(&input, worktree.to_str().unwrap()).unwrap();
        assert!(placed.file.scratch);
        assert_eq!(placed.file.file.file_name().unwrap(), "plan.txt");
        assert!(placed.file.file.starts_with(&placed.file.root));
        assert_eq!(
            std::fs::read_to_string(&placed.file.file).unwrap(),
            "a plan"
        );
        let root = placed.file.root.clone();
        drop(placed);
        assert!(!root.exists());
    }

    #[test]
    fn a_file_outside_the_worktree_is_copied_under_its_own_name() {
        let worktree = scratch("outside-wt");
        let elsewhere = scratch("outside-src");
        std::fs::write(elsewhere.join("review.md"), "words").unwrap();
        let input = JudgeInput::from_path(elsewhere.join("review.md")).unwrap();
        let placed = place(&input, worktree.to_str().unwrap()).unwrap();
        assert!(placed.file.scratch);
        assert_eq!(placed.file.file.file_name().unwrap(), "review.md");
        assert_eq!(input.text, "words");
    }

    #[test]
    fn a_name_that_is_a_path_is_refused() {
        let worktree = scratch("bad-name");
        for bad in ["../escape.md", "a/b.md", " "] {
            let err = place(
                &JudgeInput::text("x").named(bad),
                worktree.to_str().unwrap(),
            )
            .err()
            .unwrap();
            assert!(err.to_string().contains("bare file name"), "{bad}: {err}");
        }
        assert_eq!(
            JudgeInput::text("x").file_name().unwrap(),
            DEFAULT_JUDGE_INPUT_NAME
        );
    }
}
