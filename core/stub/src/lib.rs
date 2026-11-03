//! The conversation writer lock (docs/plans/runtime-design.md section 4).
//!
//! One lock file per key in a folder on the internal disk. `take` opens (creating if needed) the key's file and
//! takes an exclusive, non-blocking OS lock on it: `flock` on Unix, `LockFileEx` on Windows (both through
//! `std::fs::File::try_lock`, so the two platforms share one code path). The OS releases it when the holding
//! process dies, however it dies.
//!
//! Rules this module keeps:
//! - The lock file is never unlinked, renamed or truncated by any code path. A lock on an unlinked inode is a
//!   second writer: the next taker would create a fresh file and lock that. There is deliberately no `remove`.
//! - The handle is close-on-exec (Unix, std opens with `O_CLOEXEC`) and non-inheritable (Windows, std opens with
//!   `bInheritHandle = FALSE`), so nothing the holder spawns keeps the lock after the holder is gone.
//! - A second `take` of the same key in the same process is `Busy` too: each take opens its own file description
//!   and locks belong to descriptions, not processes.
//! - The folder is created owner-only (0700 on Unix; an access list naming this account and SYSTEM on Windows)
//!   by whoever comes first. Nothing here needs `core.db`.
//!
//! Keys: a key is a session id or similar. It is rejected, not rewritten, when it is empty, longer than 200 bytes,
//! is `.` or contains `..`, or contains `/`, `\`, NUL, any control character or any of `<>:"|?*` (a name that is
//! unsafe on any platform is unsafe everywhere, so a key behaves the same on every machine). The file is named
//! `w-<key>.lock`: the prefix keeps a key like `CON` from being a Windows device name.
//!
//! An existing folder on Unix that belongs to this user but has a wider mode is tightened to 0700; one that
//! belongs to someone else is refused (`Error::Folder`). On Windows an existing folder is used as found.

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as os;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as os;

#[derive(Debug)]
pub enum Error {
    /// The key cannot be a file name here; the text says why.
    Key(String),
    /// The folder is not ours or cannot be made private; the text says why.
    Folder(String),
    Io(io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Key(why) => write!(f, "bad lock key: {why}"),
            Error::Folder(why) => write!(f, "lock folder: {why}"),
            Error::Io(e) => write!(f, "lock: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

/// A held lock. Dropping it releases the lock (the file stays).
#[derive(Debug)]
pub struct Lock {
    // Closing the file releases the lock; the explicit unlock in Drop only makes the order plain.
    file: File,
    path: PathBuf,
}

impl Lock {
    /// The lock file's path (it exists for as long as the folder does).
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[derive(Debug)]
pub enum Taken {
    Held(Lock),
    /// Someone else (another process, or an earlier `take` in this one) holds it.
    Busy,
}

/// Checks a key against the rules in the module docs and returns the lock file's name.
pub fn file_name(key: &str) -> Result<String, Error> {
    let bad = |why: &str| Err(Error::Key(format!("{why}: {key:?}")));
    if key.is_empty() {
        return bad("empty");
    }
    if key.len() > 200 {
        return bad("longer than 200 bytes");
    }
    if key == "." || key.contains("..") {
        return bad("contains ..");
    }
    if let Some(c) = key
        .chars()
        .find(|c| c.is_control() || "/\\<>:\"|?*".contains(*c))
    {
        return bad(&format!("contains {c:?}"));
    }
    Ok(format!("w-{key}.lock"))
}

/// Takes the writer lock for `key` in `folder`, creating the folder (owner-only) and the lock file if needed.
/// Never blocks: `Busy` means someone else holds it now.
pub fn take(folder: &Path, key: &str) -> Result<Taken, Error> {
    let name = file_name(key)?;
    os::private_folder(folder)?;
    let path = folder.join(name);
    let mut open = OpenOptions::new();
    // No truncate: the file is only ever a lock target, and it is never emptied or replaced.
    open.read(true).write(true).create(true).truncate(false);
    os::file_private(&mut open);
    let file = open.open(&path)?;
    match file.try_lock() {
        Ok(()) => Ok(Taken::Held(Lock { file, path })),
        Err(TryLockError::WouldBlock) => Ok(Taken::Busy),
        Err(TryLockError::Error(e)) => Err(Error::Io(e)),
    }
}
