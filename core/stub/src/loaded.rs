//! Which warm engines have a conversation loaded (design 2026-11-16-offturn-lock-alone, rule L-clear).
//!
//! A resident stub records each conversation key its engine has loaded as an empty file next to the locks:
//! `<folder>/<key>.loaded/<stubpid>-<start>`, `<start>` being the stub's own `kith-procstart` stamp. The holder (the host)
//! sends `loaded <key>` before the engine loads the session and `forgot <key>` when it lets the session go; the stub's exit
//! removes the rest. A stub killed hard leaves its files behind, so a reader believes a marker only while that pid runs with
//! that start. The folders are never removed (like the lock files): a stub marking while another removed the folder would
//! fail its load.
//!
//! The reader is a one-shot stub on the lock alone (`--lock-alone`, see `clear`): with no core answering it cannot ask
//! whether a warm engine has the conversation, and an engine that woke itself writes with no claim and no lock.

use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};

use crate::Error;

/// The folder of `key`'s markers (the key is checked against the lock key rules).
pub fn folder_of(folder: &Path, key: &str) -> Result<PathBuf, Error> {
    crate::file_name(key)?;
    Ok(folder.join(format!("{key}.loaded")))
}

/// The marker name of a process: `<pid>-<start>`.
pub fn name_of(pid: u32, start: &str) -> String {
    format!("{pid}-{start}")
}

/// `<pid>-<start>` back to its parts; `None` for anything else (never a pid of 0 or 1).
pub fn parse_name(name: &str) -> Option<(u32, &str)> {
    let (pid, start) = name.split_once('-')?;
    let pid: u32 = pid.parse().ok()?;
    (pid > 1 && !start.is_empty()).then_some((pid, start))
}

/// This process's marker name; `None` when its own start cannot be read (then it marks nothing: a marker nobody can check is
/// one a reader must not act on).
pub fn own_name() -> Option<String> {
    let pid = std::process::id();
    kith_procstart::start_stamp(pid).map(|s| name_of(pid, &s))
}

/// Records that the process named `me` has `key` loaded. Idempotent.
pub fn mark(folder: &Path, key: &str, me: &str) -> Result<(), Error> {
    let dir = folder_of(folder, key)?;
    crate::os::private_folder(folder)?;
    crate::os::private_folder(&dir)?;
    let mut open = OpenOptions::new();
    open.write(true).create(true).truncate(false);
    crate::os::file_private(&mut open);
    open.open(dir.join(me))?;
    Ok(())
}

/// Removes the marker of `me` for `key`. One that is not there is fine.
pub fn unmark(folder: &Path, key: &str, me: &str) -> Result<(), Error> {
    let dir = folder_of(folder, key)?;
    match std::fs::remove_file(dir.join(me)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::Io(e)),
    }
}

/// The markers of `key`: (file name, pid, start). Names that are not markers are left out.
pub fn markers(folder: &Path, key: &str) -> Result<Vec<(String, u32, String)>, Error> {
    let dir = folder_of(folder, key)?;
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(Error::Io(e)),
    };
    let mut out = Vec::new();
    for entry in entries {
        let name = entry?.file_name();
        let Some(name) = name.to_str() else { continue };
        if let Some((pid, start)) = parse_name(name) {
            out.push((name.to_owned(), pid, start.to_owned()));
        }
    }
    Ok(out)
}

/// Every key the process named `me` has a marker for (the folders `<key>.loaded` holding that name).
pub fn keys_of(folder: &Path, me: &str) -> Result<Vec<String>, Error> {
    let entries = match std::fs::read_dir(folder) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(Error::Io(e)),
    };
    let mut keys = Vec::new();
    for entry in entries {
        let name = entry?.file_name();
        let Some(key) = name.to_str().and_then(|n| n.strip_suffix(".loaded")) else {
            continue;
        };
        if crate::file_name(key).is_ok() && folder.join(name.as_os_str()).join(me).exists() {
            keys.push(key.to_owned());
        }
    }
    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip_and_garbage_is_not_a_marker() {
        assert_eq!(
            parse_name(&name_of(4242, "e1731000000000")),
            Some((4242, "e1731000000000"))
        );
        assert_eq!(parse_name("4242-b0f3a:123"), Some((4242, "b0f3a:123")));
        for bad in ["", "4242", "4242-", "x-e1", "1-e1", "0-e1", "-e1"] {
            assert_eq!(parse_name(bad), None, "{bad:?}");
        }
    }
}
