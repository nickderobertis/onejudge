//! A run, published for another process to follow: `onejudge run` appends each
//! observation it emits, then its result, to a durable stream named after its
//! `--session`, and `onejudge watch <session>` replays that stream and follows it
//! to the result.
//!
//! The stream rides the same `onemessagebus` onejudge already carries notes over
//! (`note.rs`): its [`LocalTransport`], a durable, totally ordered log per queue
//! in a directory, with a small named document beside it. Each run appends to a
//! **queue of its own**, and the session's document names the queue of the run
//! that started last — so a new run under a session name starts that session's
//! stream afresh, and a watch following one run can never be handed another
//! run's records: it reads one queue, from its first record, and a queue only
//! ever holds one run.
//!
//! Where the streams live is `ONEJUDGE_WATCH_DIR`, else a per-user state
//! directory keyed by the working directory, so a `watch` started in the same
//! project finds the `run` beside it and two projects using the default session
//! name do not collide. See `docs/cli.md`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use onemessagebus::{Changed, DocumentName, LocalTransport, QueueName, Transport};
use serde::{Deserialize, Serialize};

use super::CliError;

/// The environment variable naming the directory the streams are kept in.
pub(crate) const WATCH_DIR_ENV: &str = "ONEJUDGE_WATCH_DIR";

/// How long a follower waits for the stream to move before looking again —
/// including at whether a newer run has replaced the one it follows.
const FOLLOW_POLL: Duration = Duration::from_millis(250);

/// How many runs this process has published, so each gets a queue of its own.
static RUNS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How many records a follower reads at once.
const READ_BATCH: usize = 256;

/// The session's document: which queue the run that started last writes.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Current {
    /// The session name, as the run was given it.
    session: String,
    /// The queue that run appends to.
    queue: String,
}

/// The directory the streams are kept in: `ONEJUDGE_WATCH_DIR`, else
/// `<state>/onejudge/watch/<working directory>` — `<state>` being
/// `$XDG_STATE_HOME`, `~/.local/state`, `%LOCALAPPDATA%`, or the temporary
/// directory, whichever is found first.
pub(crate) fn watch_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os(WATCH_DIR_ENV).filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    let state = std::env::var_os("XDG_STATE_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|d| !d.is_empty())
                .map(|home| Path::new(&home).join(".local").join("state"))
        })
        .or_else(|| std::env::var_os("LOCALAPPDATA").map(PathBuf::from))
        .unwrap_or_else(std::env::temp_dir);
    let cwd = std::env::current_dir()
        .and_then(|dir| dir.canonicalize())
        .unwrap_or_default();
    state
        .join("onejudge")
        .join("watch")
        .join(key("p", &cwd.display().to_string()))
}

/// A name the transport accepts that stands for `text`: `prefix`, a stable hash
/// of the whole of `text`, and a readable slug of its tail.
fn key(prefix: &str, text: &str) -> String {
    let slug: String = text
        .chars()
        .rev()
        .take(40)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("{prefix}{:016x}-{}", fnv1a(text), slug.trim_matches('-'))
        .trim_end_matches('-')
        .to_string()
}

/// FNV-1a over `text`'s bytes: stable across builds and platforms, unlike the
/// standard library's hasher, so a `watch` built apart from the `run` it follows
/// still finds its stream.
fn fnv1a(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// The session's document name.
fn document(session: &str) -> Result<(QueueName, DocumentName), CliError> {
    let key = key("s", session);
    let queue = key
        .parse::<QueueName>()
        .map_err(|e| CliError::Config(format!("cannot name the watch stream: {e}")))?;
    let document = format!("{key}.json")
        .parse::<DocumentName>()
        .map_err(|e| CliError::Config(format!("cannot name the watch stream: {e}")))?;
    Ok((queue, document))
}

/// Where a run's observations and result are published.
pub(crate) struct Publisher {
    transport: LocalTransport,
    queue: QueueName,
    /// Set once a publish has failed and been reported, so a broken stream is
    /// said once rather than on every observation.
    failed: bool,
}

impl Publisher {
    /// Start `session`'s stream afresh in `dir`: a new queue for this run, named
    /// by the session's document, and the previous run's queue removed.
    ///
    /// # Errors
    /// The directory cannot be opened or the document written.
    pub(crate) fn start(dir: &Path, session: &str) -> Result<Self, CliError> {
        let transport = LocalTransport::open(dir).map_err(bus_error)?;
        let (session_queue, name) = document(session)?;
        // Unique per run: the clock, the process, and a count within it — two
        // runs one process starts in the same millisecond are still two runs.
        let run = format!(
            "{session_queue}-r{}-{}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |since| since.as_millis()),
            std::process::id(),
            RUNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let queue = run
            .parse::<QueueName>()
            .map_err(|e| CliError::Config(format!("cannot name the watch stream: {e}")))?;
        let previous = transport
            .document(&session_queue, &name)
            .ok()
            .flatten()
            .and_then(|bytes| serde_json::from_slice::<Current>(&bytes).ok());
        let current = Current {
            session: session.to_string(),
            queue: queue.to_string(),
        };
        let bytes = serde_json::to_vec(&current)
            .map_err(|e| CliError::Config(format!("cannot write the watch stream: {e}")))?;
        transport
            .replace_document(&session_queue, &name, &bytes)
            .map_err(bus_error)?;
        // The session's earlier run is over as far as this session name is
        // concerned; its log goes with it so the directory stays one run per
        // session. A follower still on it sees the document move and stops.
        if let Some(previous) = previous.and_then(|p| p.queue.parse::<QueueName>().ok()) {
            if previous != queue {
                let _ = std::fs::remove_file(transport.records_path(&previous));
            }
        }
        Ok(Self {
            transport,
            queue,
            failed: false,
        })
    }

    /// Append one record — one JSON line. A stream that cannot be written is a
    /// side channel lost, never a run taken down: it is said once on stderr.
    pub(crate) fn publish(&mut self, line: &str) {
        if self.failed {
            return;
        }
        if let Err(e) = self.transport.append(&self.queue, line.as_bytes()) {
            self.failed = true;
            eprintln!(
                "onejudge: warning — could not publish this run for `onejudge watch` ({e}); \
                 the run continues"
            );
        }
    }
}

fn bus_error(e: onemessagebus::TransportError) -> CliError {
    CliError::Config(format!("cannot open the watch stream: {e}"))
}

/// Follow `session`'s stream in `dir` from its first record: hand each record to
/// `on_record`, which returns `Some(code)` for the record that ends the run.
/// Waits for a run to start when there is none yet.
///
/// # Errors
/// The directory cannot be read, or a newer run replaced the one being
/// followed before it finished.
pub(crate) fn follow(
    dir: &Path,
    session: &str,
    on_record: &mut dyn FnMut(&str) -> Result<Option<i32>, CliError>,
) -> Result<i32, CliError> {
    let transport = LocalTransport::open(dir).map_err(bus_error)?;
    let (session_queue, name) = document(session)?;
    let current = |transport: &LocalTransport| -> Option<Current> {
        transport
            .document(&session_queue, &name)
            .ok()
            .flatten()
            .and_then(|bytes| serde_json::from_slice::<Current>(&bytes).ok())
    };
    let following = loop {
        if let Some(current) = current(&transport) {
            break current;
        }
        std::thread::sleep(FOLLOW_POLL);
    };
    let queue = following
        .queue
        .parse::<QueueName>()
        .map_err(|e| CliError::Config(format!("the watch stream names no queue: {e}")))?;
    let replaced = |transport: &LocalTransport| {
        current(transport).is_some_and(|now| now.queue != following.queue)
    };
    let newer = || {
        CliError::Config(format!(
            "a newer run of session `{session}` started before the one being watched \
             finished; run `onejudge watch {session}` again to follow it"
        ))
    };
    let mut position = None;
    loop {
        let batch = match transport.read(&queue, position.as_ref(), READ_BATCH) {
            Ok(batch) => batch,
            // A newer run removes the log it replaced.
            Err(_) if replaced(&transport) => return Err(newer()),
            Err(e) => return Err(bus_error(e)),
        };
        let read = batch.records.len();
        for record in batch.records {
            position = Some(record.after);
            let line = String::from_utf8_lossy(&record.bytes);
            if let Some(code) = on_record(&line)? {
                return Ok(code);
            }
        }
        if read > 0 {
            continue;
        }
        if replaced(&transport) {
            return Err(newer());
        }
        let seen = transport.fingerprint(&queue).map_err(bus_error)?;
        if let Changed::Unchanged(_) = transport
            .wait_for_change(&queue, &seen, FOLLOW_POLL)
            .map_err(bus_error)?
        {
            continue;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("onejudge-watch-unit-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_session_name_of_any_spelling_becomes_a_name_the_transport_accepts() {
        for session in ["onejudge", "my run/1", "ü", "-", ""] {
            let (queue, document) = document(session).unwrap();
            assert!(queue.as_str().starts_with('s'), "{session:?}: {queue}");
            assert!(document.as_str().ends_with(".json"), "{session:?}");
        }
        assert_ne!(
            key("s", "a/b"),
            key("s", "a-b"),
            "the hash keeps them apart"
        );
    }

    #[test]
    fn a_follower_reads_one_run_from_its_start_and_stops_at_its_end() {
        let dir = scratch("one-run");
        let mut first = Publisher::start(&dir, "s").unwrap();
        first.publish(r#"{"n":1}"#);
        first.publish(r#"{"end":0}"#);
        let mut second = Publisher::start(&dir, "s").unwrap();
        second.publish(r#"{"n":2}"#);
        second.publish(r#"{"end":3}"#);
        let mut seen = Vec::new();
        let code = follow(&dir, "s", &mut |line| {
            seen.push(line.to_string());
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            Ok(value["end"].as_i64().map(|c| c as i32))
        })
        .unwrap();
        assert_eq!(code, 3);
        assert_eq!(seen, [r#"{"n":2}"#, r#"{"end":3}"#]);
        // The earlier run's log went with it.
        let logs = std::fs::read_dir(&dir)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|x| x == "jsonl")
            })
            .count();
        assert_eq!(logs, 1);
    }

    #[test]
    fn a_follower_of_a_run_a_newer_one_replaced_says_so() {
        let dir = scratch("replaced");
        let mut first = Publisher::start(&dir, "s").unwrap();
        first.publish(r#"{"n":1}"#);
        let mut replaced = false;
        let err = follow(&dir, "s", &mut |_| {
            if !replaced {
                replaced = true;
                Publisher::start(&dir, "s").unwrap();
            }
            Ok(None)
        })
        .unwrap_err();
        assert!(
            err.to_string().contains("a newer run of session `s`"),
            "{err}"
        );
    }
}
