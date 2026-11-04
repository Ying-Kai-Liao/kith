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
//! - A garbled, unknown or over-long line is answered `error` and serving goes on. Lock files are never deleted.

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

/// Serves the channel until it closes (or a write to it fails). `on_taken` runs after each new lock is taken,
/// e.g. to write the holder record. When the channel ends the held lock is kept for the life of the process
/// (the caller exits only after the engine does), never dropped here.
pub fn serve(
    folder: &Path,
    mut reader: impl BufRead,
    mut writer: impl Write,
    on_taken: impl Fn(&Lock),
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
    on_taken: &impl Fn(&Lock),
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
            on_taken(&lock);
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
