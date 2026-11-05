//! The resident control channel: `kith-stub --resident` serves a warm engine's writer lock one turn at a time.
//!
//! A warm engine outlives its turns, so its stub cannot take the lock at start (the one-shot mode) and hold it to
//! the engine's exit: the conversation would be locked while idle. Instead the holder, which owns the claim,
//! asks the stub over a control channel to take the conversation's lock when a turn starts running and to release
//! it when the turn ends. The channel is bidirectional lines of ASCII, `\n` terminated (a trailing `\r` is
//! ignored), at most 1024 bytes per line:
//!
//! ```text
//! holder -> stub                    stub -> holder
//! take <conv-key> <claim-id>        locked <claim-id> | busy <claim-id> | error <claim-id> <reason>
//! release <conv-key> <claim-id>     released <claim-id> | not-held <claim-id> | error <claim-id> <reason>
//! anything else                     error - <reason>
//! ```
//!
//! `conv-key` follows the lock key rules in the crate docs; `claim-id` is 1 to 128 bytes of `A-Za-z0-9._-`. One
//! answer per line, in order. Rules:
//! - `take` is non-blocking and takes the same lock file `take()` does (the one-shot stub and `kith --direct`
//!   use it), so a lock held by any of them answers `busy`. Taking the lock already held for the same key and
//!   claim answers `locked` again (a retried line). The handle is close-on-exec / non-inheritable, so the
//!   engine and its children never get it.
//! - The stub serves one conversation at a time, because a warm engine is one process with one conversation. A
//!   `take` while the lock is held for another claim, or for another key, answers `busy`.
//! - `release` frees the lock only when the held claim id (and key) match: `released`; otherwise `not-held` and
//!   the lock stays. A repeated `release` after `released` is `not-held`.
//! - Nothing is ever released because the channel closed. If the holder dies, the stub keeps every held lock
//!   until the engine exits and then exits itself; a line cut off by the close is not acted on.
//! - The channel must be owner-only, and the stub checks it: on Unix `--ctl-fd` is a socket whose peer is our uid; on
//!   Windows `--ctl-pipe` is `\\.\pipe\...` and its server process is the stub's parent.
//! - A garbled, unknown or over-long line is answered `error` and serving goes on. Lock files are never deleted.

//!
//! What the holder (M3-4) must do, because this design relies on it:
//! - Keep no copy of the stub's end of the socketpair (close it after spawning), and give the stub no copy of its
//!   own end. Otherwise a dead holder never produces EOF here, which is harmless for safety but hides the crash path.
//! - After a holder crash mid-turn the lock stays until the engine exits (nothing here releases on close), so the
//!   warm engine must exit on stdin EOF / holder death; otherwise that conversation is wedged for ever.
//! - On Windows create the pipe with an owner-only DACL, `PIPE_REJECT_REMOTE_CLIENTS` and
//!   `FILE_FLAG_FIRST_PIPE_INSTANCE`, one instance, and keep a listening instance (the stub does not retry on
//!   `ERROR_PIPE_BUSY`).
//! - Never retry a `take` after sending `release` for the same claim: the stub cannot know a claim is finished (that
//!   is the redeem CAS).
//!
//! In `WriterLock4.tla`, `StubTake` / `StubRelease` are the take and release arms of `handle()` below.

use std::io::{BufRead, Read, Write};
use std::path::Path;

use crate::{Lock, Taken, take};

const MAX_LINE: usize = 1024;

/// The lock this stub holds now, if any: key, claim id.
struct Held {
    key: String,
    claim: String,
    // Dropping it closes the file and so releases the lock.
    _lock: Lock,
}

fn claim_ok(claim: &str) -> bool {
    !claim.is_empty()
        && claim.len() <= 128
        && claim
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Serves the channel until it closes (or a write to it fails). `on_taken` runs after each new lock is taken
/// and before `locked` is answered (the holder record, the run-token redeem); an `Err` frees the lock and answers `error`.
/// It runs on the serving thread with the lock held, so it does in-process I/O only: it never forks, execs or spawns
/// (the stub's `redeem_run_token` is checked for that, and `tests/resident.rs` runs a hook under the lock). When the channel ends the held lock is kept for the life of the process
/// (the caller exits only after the engine does), never dropped here.
pub fn serve(
    folder: &Path,
    mut reader: impl BufRead,
    mut writer: impl Write,
    on_taken: impl Fn(&Lock) -> Result<(), String>,
) {
    let mut held: Option<Held> = None;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let n = match (&mut reader)
            .take(MAX_LINE as u64 + 1)
            .read_until(b'\n', &mut buf)
        {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let reply = if buf.last() == Some(&b'\n') {
            handle(folder, &buf[..n - 1], &mut held, &on_taken)
        } else if buf.len() > MAX_LINE {
            // Drop the rest of the line so its tail is not read as a command.
            if !skip_line(&mut reader) {
                break;
            }
            "error - line-too-long".to_string()
        } else {
            // EOF in the middle of a line: the holder died while writing. Do not act on half a command.
            break;
        };
        if writer
            .write_all(reply.as_bytes())
            .and_then(|()| writer.write_all(b"\n"))
            .and_then(|()| writer.flush())
            .is_err()
        {
            break;
        }
    }
    // Channel closed: keep the lock (the engine may still be mid-turn). Forgotten, so that nothing here, even a
    // later refactor, can drop it before the process ends.
    std::mem::forget(held);
}

/// Reads to the next newline; false at EOF or error.
fn skip_line(reader: &mut impl BufRead) -> bool {
    loop {
        let (used, found) = match reader.fill_buf() {
            Ok([]) | Err(_) => return false,
            Ok(b) => match b.iter().position(|&c| c == b'\n') {
                Some(i) => (i + 1, true),
                None => (b.len(), false),
            },
        };
        reader.consume(used);
        if found {
            return true;
        }
    }
}

fn handle(
    folder: &Path,
    line: &[u8],
    held: &mut Option<Held>,
    on_taken: &impl Fn(&Lock) -> Result<(), String>,
) -> String {
    let Ok(line) = std::str::from_utf8(line) else {
        return "error - not-utf8".into();
    };
    let line = line.strip_suffix('\r').unwrap_or(line);
    let parts: Vec<&str> = line.split(' ').collect();
    let [verb, key, claim] = parts[..] else {
        return "error - expected: <take|release> <conv-key> <claim-id>".into();
    };
    if verb != "take" && verb != "release" {
        return "error - unknown-command".into();
    }
    if !claim_ok(claim) {
        return "error - bad-claim-id".into();
    }
    if let Err(e) = crate::file_name(key) {
        return format!("error {claim} {}", one_line(&e.to_string()));
    }
    let same = held
        .as_ref()
        .is_some_and(|h| h.key == key && h.claim == claim);
    if verb == "release" {
        if same {
            *held = None;
            return format!("released {claim}");
        }
        return format!("not-held {claim}");
    }
    if same {
        return format!("locked {claim}");
    }
    if held.is_some() {
        return format!("busy {claim}");
    }
    match take(folder, key) {
        Ok(Taken::Held(lock)) => {
            // A failed hook (the run-token redeem) drops the lock again: never `locked` without it.
            if let Err(e) = on_taken(&lock) {
                return format!("error {claim} {}", one_line(&e));
            }
            *held = Some(Held {
                key: key.into(),
                claim: claim.into(),
                _lock: lock,
            });
            format!("locked {claim}")
        }
        Ok(Taken::Busy) => format!("busy {claim}"),
        Err(e) => format!("error {claim} {}", one_line(&e.to_string())),
    }
}

fn one_line(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}
