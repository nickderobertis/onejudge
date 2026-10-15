//! `codex app-server --listen unix://PATH` for the fake-harness double: the
//! shared, multi-client server the linked oneharness engine runs every
//! controlled Codex turn on, one per pool key (oneharness-core 0.28).
//!
//! Only as much of Codex's protocol as one turn needs: the WebSocket upgrade,
//! then JSON-RPC — `initialize`, `thread/start` / `thread/resume`, `turn/start`
//! (answered with the turn, then `turn/started`, one `item/agentMessage/delta`
//! carrying the reply and `turn/completed`), `thread/unsubscribe` and
//! `thread/loaded/list`. Anything else is answered with an empty result, never
//! left unanswered.
//!
//! The turn's prompt is what steers it, with the same markers as every other
//! invocation of the double:
//!
//! * `[[reply:TEXT]]` — the turn's text. Defaults to `ok`.
//! * `[[await-turns:N]]` — before completing, wait until this **process** has
//!   started `N` turns, bounded by the double's wait limit. Turns served by two
//!   different servers can never satisfy it, so a run that reaches the end of
//!   it on time ran its turns concurrently on one process.
//! * `[[record-harness:PATH]]` — append one JSON line per completed turn:
//!   `{"argv", "server_pid", "thread", "turns_started"}`, the last being how
//!   many turns this process had started when the turn completed.
//! * `[[app-server-exit]]` — exit as soon as the turn has started, as a server
//!   that crashes under a live turn does.
//!
//! The server exits on its own once it has served a connection and then held
//! none for [`IDLE_EXIT`]: the pool would stop it after its linger, but only a
//! process hosting oneharness's sweeper does that unprompted, and a test
//! executable is none.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// How long the server outlives its last connection.
const IDLE_EXIT: Duration = Duration::from_secs(3);

/// How long the server waits for a first connection before giving up, so a
/// launch nobody dials cannot leak this process onto a runner.
const UNUSED_EXIT: Duration = Duration::from_secs(60);

/// The longest path `sun_path` holds on every unix this suite runs on; a
/// longer address is bound elsewhere and linked, as Codex itself does.
const SUN_PATH_BUDGET: usize = 100;

struct Server {
    argv: Vec<String>,
    connections: AtomicUsize,
    served: AtomicUsize,
    turns_started: AtomicUsize,
    next_thread: AtomicUsize,
    wait_limit: Duration,
}

/// Serve `argv` (`app-server … --listen unix://PATH`) until idle; never returns.
pub fn run(argv: &[String], wait_limit: Duration) -> ! {
    let path = argv
        .windows(2)
        .find(|pair| pair[0] == "--listen")
        .and_then(|pair| pair[1].strip_prefix("unix://"))
        .map(PathBuf::from)
        .unwrap_or_else(|| super::fail("app-server --listen must name a unix:// path"));
    let listener = bind(&path);
    let server = Arc::new(Server {
        argv: argv.to_vec(),
        connections: AtomicUsize::new(0),
        served: AtomicUsize::new(0),
        turns_started: AtomicUsize::new(0),
        next_thread: AtomicUsize::new(1),
        wait_limit,
    });
    let watcher = Arc::clone(&server);
    std::thread::spawn(move || exit_when_idle(&watcher));
    for stream in listener.incoming().flatten() {
        let server = Arc::clone(&server);
        server.connections.fetch_add(1, Ordering::SeqCst);
        server.served.fetch_add(1, Ordering::SeqCst);
        std::thread::spawn(move || {
            serve(&server, stream);
            server.connections.fetch_sub(1, Ordering::SeqCst);
        });
    }
    std::process::exit(0);
}

fn exit_when_idle(server: &Server) {
    let started = Instant::now();
    let mut idle_since: Option<Instant> = None;
    loop {
        std::thread::sleep(Duration::from_millis(50));
        let served = server.served.load(Ordering::SeqCst) > 0;
        if !served && started.elapsed() >= UNUSED_EXIT {
            std::process::exit(0);
        }
        if !served || server.connections.load(Ordering::SeqCst) > 0 {
            idle_since = None;
            continue;
        }
        let since = *idle_since.get_or_insert_with(Instant::now);
        if since.elapsed() >= IDLE_EXIT {
            std::process::exit(0);
        }
    }
}

/// Bind `path`, or a short stand-in linked from it when `path` is past the
/// `sun_path` budget.
fn bind(path: &Path) -> UnixListener {
    let target = if path.as_os_str().len() >= SUN_PATH_BUDGET {
        let target = std::env::temp_dir().join(format!("oj-codex-{}.sock", std::process::id()));
        let target = if target.as_os_str().len() >= SUN_PATH_BUDGET {
            PathBuf::from(format!("/tmp/oj-codex-{}.sock", std::process::id()))
        } else {
            target
        };
        let _ = std::fs::remove_file(path);
        if let Err(e) = std::os::unix::fs::symlink(&target, path) {
            super::fail(&format!("cannot link {}: {e}", path.display()));
        }
        target
    } else {
        path.to_path_buf()
    };
    let _ = std::fs::remove_file(&target);
    UnixListener::bind(&target)
        .unwrap_or_else(|e| super::fail(&format!("cannot bind {}: {e}", target.display())))
}

type Writer = Arc<Mutex<UnixStream>>;

fn serve(server: &Server, stream: UnixStream) {
    let Ok(writer) = stream.try_clone() else {
        return;
    };
    let writer: Writer = Arc::new(Mutex::new(writer));
    let mut reader = BufReader::new(stream);
    let mut key = String::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        if line == "\r\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("sec-websocket-key") {
                key = value.trim().to_string();
            }
        }
    }
    let upgrade = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
        oneharness_core::io::websocket::accept_key(&key)
    );
    if lock(&writer).write_all(upgrade.as_bytes()).is_err() {
        return;
    }
    while let Some(message) = read_message(&mut reader) {
        let Ok(frame) = serde_json::from_slice::<Value>(&message) else {
            continue;
        };
        handle(server, &writer, &frame);
    }
}

fn lock(writer: &Writer) -> std::sync::MutexGuard<'_, UnixStream> {
    writer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn send(writer: &Writer, frame: &Value) {
    let payload = frame.to_string().into_bytes();
    let mut bytes = vec![0x81u8];
    match payload.len() {
        len if len < 126 => bytes.push(len as u8),
        len if len <= usize::from(u16::MAX) => {
            bytes.push(126);
            bytes.extend_from_slice(&(len as u16).to_be_bytes());
        }
        len => {
            bytes.push(127);
            bytes.extend_from_slice(&(len as u64).to_be_bytes());
        }
    }
    bytes.extend_from_slice(&payload);
    let _ = lock(writer).write_all(&bytes);
}

/// The next whole text message from the client, unmasked; `None` once it is
/// gone or closes.
fn read_message(stream: &mut impl Read) -> Option<Vec<u8>> {
    let mut message = Vec::new();
    loop {
        let mut head = [0u8; 2];
        stream.read_exact(&mut head).ok()?;
        let len = match head[1] & 0x7f {
            126 => {
                let mut ext = [0u8; 2];
                stream.read_exact(&mut ext).ok()?;
                u64::from(u16::from_be_bytes(ext))
            }
            127 => {
                let mut ext = [0u8; 8];
                stream.read_exact(&mut ext).ok()?;
                u64::from_be_bytes(ext)
            }
            short => u64::from(short),
        };
        let mut mask = [0u8; 4];
        if head[1] & 0x80 != 0 {
            stream.read_exact(&mut mask).ok()?;
        }
        let mut payload = vec![0u8; usize::try_from(len).ok()?];
        stream.read_exact(&mut payload).ok()?;
        for (index, byte) in payload.iter_mut().enumerate() {
            *byte ^= mask[index % 4];
        }
        match head[0] & 0x0f {
            0x8 => return None,
            0x9 | 0xA => continue,
            _ => message.extend_from_slice(&payload),
        }
        if head[0] & 0x80 != 0 {
            return Some(message);
        }
    }
}

fn handle(server: &Server, writer: &Writer, frame: &Value) {
    let Some(method) = frame.get("method").and_then(Value::as_str) else {
        return;
    };
    // A notification (`initialized`) wants no answer.
    let Some(id) = frame.get("id").cloned() else {
        return;
    };
    let params = frame.get("params").cloned().unwrap_or(Value::Null);
    let reply = |result: Value| {
        send(
            writer,
            &json!({"jsonrpc": "2.0", "id": id, "result": result}),
        )
    };
    match method {
        "initialize" => reply(json!({"userAgent": "onejudge-fake-codex-app-server"})),
        "thread/start" | "thread/resume" => {
            let thread = params.get("threadId").and_then(Value::as_str).map_or_else(
                || {
                    format!(
                        "fake-thread-{}-{}",
                        std::process::id(),
                        server.next_thread.fetch_add(1, Ordering::SeqCst)
                    )
                },
                str::to_string,
            );
            let mut result = json!({"thread": {"id": thread}});
            if let Some(model) = params.get("model") {
                result["model"] = model.clone();
            }
            reply(result);
        }
        "turn/start" => turn(server, writer, &params, reply),
        "thread/unsubscribe" => reply(json!({"status": "unsubscribed"})),
        "thread/loaded/list" => reply(json!({"data": [], "nextCursor": null})),
        _ => reply(json!({})),
    }
}

fn turn(server: &Server, writer: &Writer, params: &Value, reply: impl FnOnce(Value)) {
    let thread = params
        .get("threadId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let prompt: String = params
        .get("input")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    let started = server.turns_started.fetch_add(1, Ordering::SeqCst) + 1;
    let turn_id = format!("fake-turn-{started}");
    reply(json!({"turn": {"id": turn_id, "status": "inProgress"}}));
    send(
        writer,
        &json!({"jsonrpc": "2.0", "method": "turn/started",
            "params": {"threadId": thread, "turn": {"id": turn_id}}}),
    );
    if prompt.contains("[[app-server-exit]]") {
        std::process::exit(1);
    }
    if let Some(wanted) = super::marker(&prompt, "await-turns") {
        let wanted: usize = wanted.parse().unwrap_or_else(|e| {
            super::fail(&format!("`[[await-turns:{wanted}]]` is no turn count: {e}"))
        });
        let until = Instant::now() + server.wait_limit;
        while server.turns_started.load(Ordering::SeqCst) < wanted && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    let text = super::marker(&prompt, "reply").unwrap_or_else(|| "ok".to_string());
    if let Some(path) = super::marker(&prompt, "record-harness") {
        let line = json!({
            "argv": server.argv,
            "server_pid": std::process::id(),
            "thread": thread,
            "turns_started": server.turns_started.load(Ordering::SeqCst),
        });
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap_or_else(|e| super::fail(&format!("could not open {path}: {e}")));
        let _ = file.write_all(format!("{line}\n").as_bytes());
    }
    send(
        writer,
        &json!({"jsonrpc": "2.0", "method": "item/agentMessage/delta",
            "params": {"threadId": thread, "turnId": turn_id, "itemId": "item-0", "delta": text}}),
    );
    send(
        writer,
        &json!({"jsonrpc": "2.0", "method": "turn/completed",
            "params": {"threadId": thread, "turn": {"id": turn_id, "status": "completed"}}}),
    );
}
