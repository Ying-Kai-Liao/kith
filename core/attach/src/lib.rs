//! The attach client (M1-6): the raw-mode terminal side of `kith attach`. It speaks the frames of
//! `core/runtime/src/terminal.rs` ("Attach frames") to the relay (M1-5) over the owner socket's attach endpoint, puts the
//! user's terminal in raw mode while it is attached, and gives it back on every way out.
//!
//! Reached through `kith kernel attach --endpoint <socket|pipe> --ticket-stdin [--observer] [--takeover] [--detach-key K]
//! [--tty]` (serve/host's main.rs, a thin call into this crate), the low-level command M1-7's `kith attach` / `kith AGENT --held` call. The ticket is one line on
//! stdin, never argv. With `--tty` the keys are read from the controlling terminal (`/dev/tty`, `CONIN$`) instead of
//! stdin, for a caller whose stdin is the pipe the ticket came on.
//!
//! Exit codes: the engine's code on EXIT (a signal is 128 + n), 75 for "busy" (no retry), 0 for a detach, and for the
//! relay ending the attach with DETACHED: 76 `slow` (the caller may attach again with a new ticket), 77 `taken_over`,
//! 78 `holder_lost`. 1 for everything else that is not a normal end (refused ticket, error frame, connection lost). The
//! stub's own codes are 70, 75, 126 and 127, so none of these collides with one (an engine that exits 76-78 itself is
//! indistinguishable, as with any exit code).
//!
//! Resize is polled (every [`RESIZE_POLL`]) rather than taken from SIGWINCH / console events: one code path on both
//! systems, and a storm of changes is coalesced to the size at each tick.

use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use kith_holder::platform::{self, Stream};
use kith_holder::wire;
use serde_json::{Value, json};

#[cfg(unix)]
mod term_unix;
#[cfg(unix)]
use term_unix as term;
#[cfg(windows)]
mod term_windows;
#[cfg(windows)]
use term_windows as term;

use kith_holder::attach as kind;

/// The stub's "busy" exit code (a `--direct` writer holds the session).
const BUSY: i32 = 75;
const RESIZE_POLL: Duration = Duration::from_millis(100);
/// Keys are sent in frames of at most this many bytes.
const INPUT_CHUNK: usize = 4096;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    Owner,
    Observer,
}

impl Role {
    fn as_str(self) -> &'static str {
        match self {
            Role::Owner => "owner",
            Role::Observer => "observer",
        }
    }
}

pub struct Opts {
    pub ticket: String,
    pub role: Role,
    pub takeover: bool,
    pub detach_key: u8,
    /// Read keys from the controlling terminal instead of stdin.
    pub tty_input: bool,
}

/// `ctrl-\`, `ctrl-a`, `^]`, `ctrl-_` ... to the byte it types. Enter, tab and newline are refused: they would end every
/// line you type.
pub fn parse_detach_key(s: &str) -> Result<u8, String> {
    let s = s.trim();
    let rest = ["ctrl-", "Ctrl-", "CTRL-", "^"]
        .iter()
        .find_map(|p| s.strip_prefix(p))
        .ok_or_else(|| {
            format!("detach key {s:?}: write it as ctrl-<char>, for example ctrl-\\ or ctrl-a")
        })?;
    let mut chars = rest.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return Err(format!("detach key {s:?}: one character after ctrl-"));
    };
    let byte = match c.to_ascii_lowercase() {
        l @ 'a'..='z' => l as u8 - b'a' + 1,
        '@' => 0,
        '\\' => 28,
        ']' => 29,
        '^' => 30,
        '_' => 31,
        _ => return Err(format!("detach key {s:?}: ctrl-{c} is not a control key")),
    };
    if matches!(byte, 0 | 9 | 10 | 13 | 27) {
        return Err(format!(
            "detach key {s:?}: that key is needed for typing (nul, tab, newline, enter, escape)"
        ));
    }
    Ok(byte)
}

/// What the session needs from the outside, so tests can stand a fake terminal in.
pub(crate) struct Io {
    pub input: Box<dyn Read + Send>,
    pub output: Box<dyn Write + Send>,
    /// The one-line messages, written after the terminal is restored.
    pub notes: Box<dyn Write + Send>,
    /// The terminal's size, when it is a terminal.
    pub size: Arc<dyn Fn() -> Option<(u16, u16)> + Send + Sync>,
    /// Puts the terminal in raw mode (called once, after ATTACHED); the returned guard restores it when dropped.
    pub raw: Box<dyn FnOnce() -> Option<Box<dyn Send>> + Send>,
}

/// Connects to the relay and runs the session. Returns the exit code.
pub fn run(endpoint: &Path, opts: &Opts) -> i32 {
    let stream = match platform::connect(endpoint) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "cannot reach the terminal relay at {}: {e}",
                endpoint.display()
            );
            return 1;
        }
    };
    let (input, tty) = match term::open_input(opts.tty_input) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("cannot open the terminal for keys: {e}");
            return 1;
        }
    };
    let size = tty.size_fn();
    let io = Io {
        input,
        output: Box::new(io::stdout()),
        notes: Box::new(io::stderr()),
        size,
        raw: Box::new(move || tty.enter_raw()),
    };
    session(stream, io, opts)
}

fn detached_note(why: &str) -> (i32, String) {
    match why {
        "detach" => (0, "detached; the agent keeps running".into()),
        "taken_over" => (
            77,
            "detached: another client took over this terminal".into(),
        ),
        "slow" => (
            76,
            "detached: this client could not keep up with the output; attach again".into(),
        ),
        "holder_lost" => (78, "detached: the process holder is gone".into()),
        "refused" => (1, "detached: the relay refused this attach".into()),
        other => (1, format!("detached: {other}")),
    }
}

fn exit_code(v: &Value) -> i32 {
    if let Some(code) = v.get("code").and_then(Value::as_i64) {
        return i32::try_from(code).unwrap_or(1);
    }
    match v.get("signal").and_then(Value::as_i64) {
        Some(sig) => 128 + i32::try_from(sig).unwrap_or(0),
        None => 1,
    }
}

type Writer = Arc<Mutex<Stream>>;

fn send(w: &Writer, kind: u8, payload: &[u8]) -> io::Result<()> {
    let mut g = w.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    wire::write_frame(&mut *g, kind, 0, payload)
}

pub(crate) fn session(stream: Stream, io: Io, opts: &Opts) -> i32 {
    let Io {
        input,
        mut output,
        mut notes,
        size,
        raw,
    } = io;
    let Ok(mut read_half) = stream.try_clone() else {
        let _ = writeln!(notes, "cannot use the connection to the terminal relay");
        return 1;
    };
    let writer: Writer = Arc::new(Mutex::new(stream));

    let start = size();
    let mut hello = json!({"v": kind::VERSION, "ticket": opts.ticket, "role": opts.role.as_str()});
    if let Some((cols, rows)) = start {
        hello["cols"] = json!(cols);
        hello["rows"] = json!(rows);
    }
    if opts.takeover {
        hello["takeover"] = json!(true);
    }
    if send(&writer, kind::ATTACH, hello.to_string().as_bytes()).is_err() {
        let _ = writeln!(notes, "the terminal relay closed the connection");
        return 1;
    }

    let detached = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let mut input = Some(input);
    let mut raw = Some(raw);
    let mut guard: Option<Box<dyn Send>> = None;
    let mut attached = false;
    let mut saw_output = false;

    let (code, note): (i32, Option<String>) = loop {
        let Ok(Some((k, _, payload))) = wire::read(&mut read_half) else {
            break if detached.load(Ordering::SeqCst) {
                (0, Some("detached; the agent keeps running".into()))
            } else if attached {
                (1, Some("the connection to the terminal closed".into()))
            } else {
                (
                    1,
                    Some("the terminal relay closed the connection without an answer".into()),
                )
            };
        };
        match k {
            kind::ATTACHED if !attached => {
                attached = true;
                if let Some(enter) = raw.take() {
                    guard = enter();
                }
                if let Some(input) = input.take() {
                    spawn_input(
                        input,
                        writer.clone(),
                        read_half.try_clone().ok(),
                        opts,
                        detached.clone(),
                        guard.is_some(),
                    );
                }
                if opts.role == Role::Owner {
                    spawn_resize(size.clone(), start, writer.clone(), stop.clone());
                }
            }
            kind::OUTPUT => {
                if !attached {
                    break (
                        1,
                        Some("the terminal relay sent output before it accepted the attach".into()),
                    );
                }
                saw_output = true;
                if output
                    .write_all(&payload)
                    .and_then(|()| output.flush())
                    .is_err()
                {
                    let _ = send(&writer, kind::DETACH, b"");
                    break (1, Some("cannot write to the terminal".into()));
                }
            }
            kind::EXIT => {
                let v: Value = serde_json::from_slice(&payload).unwrap_or(Value::Null);
                let code = exit_code(&v);
                if attached && !saw_output && code == BUSY {
                    break (
                        BUSY,
                        Some("busy: another writer has this conversation".into()),
                    );
                }
                break (
                    code,
                    v.get("error")
                        .and_then(Value::as_str)
                        .map(|e| format!("error: {e}")),
                );
            }
            kind::DETACHED => {
                let v: Value = serde_json::from_slice(&payload).unwrap_or(Value::Null);
                let (code, note) =
                    detached_note(v.get("why").and_then(Value::as_str).unwrap_or("unknown"));
                break (code, Some(note));
            }
            kind::ERROR => {
                let v: Value = serde_json::from_slice(&payload).unwrap_or(Value::Null);
                let e = v.get("error").and_then(Value::as_str).unwrap_or("unknown");
                break (1, Some(format!("error: {e}")));
            }
            // Kinds of 100 and up that this client does not know are later additions; a repeated ATTACHED is noise.
            _ => {}
        }
    };

    stop.store(true, Ordering::SeqCst);
    // Raw mode off before anything is printed, so the line lands on a sane terminal.
    drop(guard);
    if let Some(note) = note {
        let _ = writeln!(notes, "{note}");
    }
    code
}

/// Keys to the relay. The detach key sends DETACH, is not forwarded, and ends the attach (the thread stops, so a second
/// press never arrives). An observer's keys are read only for the detach key. When the keys stop (end of input, a read
/// error, a failed send) on a raw terminal there is no way left to type, so that detaches too; a headless pipe that ends
/// just stops sending and the output keeps coming.
fn spawn_input(
    mut input: Box<dyn Read + Send>,
    writer: Writer,
    wake: Option<Stream>,
    opts: &Opts,
    detached: Arc<AtomicBool>,
    on_terminal: bool,
) {
    let owner = opts.role == Role::Owner;
    let key = opts.detach_key;
    thread::spawn(move || {
        let leave = |wake: Option<Stream>| {
            // The flag first: a relay that closes without answering DETACH still reads as a clean detach.
            detached.store(true, Ordering::SeqCst);
            let _ = send(&writer, kind::DETACH, b"");
            // Wakes the main thread out of its read.
            if let Some(w) = wake {
                let _ = w.shutdown(std::net::Shutdown::Both);
            }
        };
        let mut buf = [0u8; INPUT_CHUNK];
        loop {
            let n = match input.read(&mut buf) {
                Ok(0) | Err(_) => {
                    if on_terminal {
                        leave(wake);
                    }
                    return;
                }
                Ok(n) => n,
            };
            let data = &buf[..n];
            let hit = data.iter().position(|b| *b == key);
            let keys = hit.map_or(data, |i| &data[..i]);
            if owner && !keys.is_empty() && send(&writer, kind::INPUT, keys).is_err() {
                if on_terminal {
                    leave(wake);
                }
                return;
            }
            if hit.is_some() {
                leave(wake);
                return;
            }
        }
    });
}

fn spawn_resize(
    size: Arc<dyn Fn() -> Option<(u16, u16)> + Send + Sync>,
    mut last: Option<(u16, u16)>,
    writer: Writer,
    stop: Arc<AtomicBool>,
) {
    // Not a terminal: nothing to follow.
    if last.is_none() && size().is_none() {
        return;
    }
    thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            thread::sleep(RESIZE_POLL);
            let now = size();
            if let Some((cols, rows)) = now
                && now != last
            {
                last = now;
                let body = json!({"cols": cols, "rows": rows}).to_string();
                if send(&writer, kind::RESIZE, body.as_bytes()).is_err() {
                    return;
                }
            }
        }
    });
}

#[cfg(all(test, unix))]
mod tests;
