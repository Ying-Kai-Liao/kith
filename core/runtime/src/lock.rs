//! The lifetime lock: an exclusive lock on `core.lock` (flock on Unix, `LockFileEx` on Windows) held for the whole life of
//! a core, daemon or embedded. It is a file of its own, never core.db: SQLite takes its own POSIX locks on the database,
//! and on macOS closing any other handle to that file drops them. Nothing deletes core.lock; a crash releases the lock
//! and leaves the file, which is the "free" state.
//!
//! The holder writes its pid and process start time into a second file, `core.holder`, so `kith doctor` can end a holder
//! that has the lock but does not answer the socket, after re-checking both. It is not in core.lock because on Windows
//! `LockFileEx` locks the whole of that file, so nobody else could read the line there.

use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};

use fs2::FileExt as _;

use crate::error::{Error, Result};
use crate::layout::Layout;
use crate::perms;

pub(crate) struct Lifetime {
    file: File,
}

pub(crate) enum Taken {
    Held(Lifetime),
    /// Another core has it; its pid and start time when the file could be read.
    Busy(Option<Holder>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Holder {
    pub pid: u32,
    pub started: String,
}

/// Tries the lock without waiting. Two embedded commands at once: the second does not wait, it gets `Busy`.
pub(crate) fn take(layout: &Layout) -> Result<Taken> {
    let file = perms::open_private(&layout.lock)?;
    match file.try_lock_exclusive() {
        Ok(()) => {
            write_holder(layout)?;
            Ok(Taken::Held(Lifetime { file }))
        }
        Err(e)
            if e.kind() == fs2::lock_contended_error().kind()
                || e.kind() == std::io::ErrorKind::WouldBlock =>
        {
            Ok(Taken::Busy(read_holder(layout)))
        }
        Err(e) => Err(Error::Io(format!("core.lock: {e}"))),
    }
}

/// Written after the lock is ours, so a line that names a process is never older than the lock it describes.
fn write_holder(layout: &Layout) -> Result<()> {
    let pid = std::process::id();
    let started = process_start(pid).unwrap_or_default();
    let mut file = perms::open_private(&layout.holder)?;
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(format!("pid={pid}\nstarted={started}\n").as_bytes())?;
    file.flush()?;
    Ok(())
}

/// The last holder's line. A free lock leaves it behind, so a reader checks the lock too.
pub(crate) fn read_holder(layout: &Layout) -> Option<Holder> {
    let mut text = String::new();
    File::open(&layout.holder)
        .ok()?
        .read_to_string(&mut text)
        .ok()?;
    parse_holder(&text)
}

pub(crate) fn parse_holder(text: &str) -> Option<Holder> {
    let get = |k: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(k)?.strip_prefix('='))
    };
    Some(Holder {
        pid: get("pid")?.trim().parse().ok()?,
        started: get("started").unwrap_or("").trim().to_owned(),
    })
}

#[cfg(test)]
pub(crate) fn recorded_holder(layout: &Layout) -> Option<Holder> {
    read_holder(layout)
}

/// The start time of a process as `ps` prints it in the C locale (Linux and macOS); `None` where there is no `ps`.
pub(crate) fn process_start(pid: u32) -> Option<String> {
    let out = std::process::Command::new("ps")
        .env("LC_ALL", "C")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let squeezed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!squeezed.is_empty()).then_some(squeezed)
}

impl Drop for Lifetime {
    fn drop(&mut self) {
        // Released by closing the file as well; unlocking first makes the intent plain. The file stays.
        let _ = fs2::FileExt::unlock(&self.file);
    }
}
