//! The hash of a session's files. A Claude session is `<sid>.jsonl` plus the folder `<sid>/` beside it.
//!
//! The order is fixed: the main file first, then every regular file under the sidecar folder, sorted by its path relative
//! to the folder (components joined with `/`, compared as bytes), whatever the OS lists first. Symlinks and anything
//! that is not a regular file or folder are skipped, never followed. The hash is the SHA-256 of, for each file in that
//! order, `name 0x00 length(u64, little endian) 0x00 SHA-256(content)`, where `name` is `<sid>.jsonl` or
//! `<sid>/<relative path>`. Hashing a file's content first keeps a big transcript out of memory; the name and length
//! keep a moved or split file from hashing the same. A missing sidecar folder is an empty one. A missing main file means
//! the engine's copy is missing: [`hash_session`] answers `None`.
//!
//! Two reads of a tree nobody is writing give the same hash; a transcript that is still being written gives a new one
//! whenever a byte was added, which is how the place-or-adopt rule knows to adopt.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};

use super::SessionPtr;

/// A session's hash, its total size in bytes and how many files it covers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Digest {
    pub hash: String,
    pub size: u64,
    pub files: usize,
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// SHA-256 of a file's content and its length, read to the end (the length is what was read, not what `stat` said, so
/// a file growing under us is still consistent with itself).
fn file_digest(path: &Path) -> std::io::Result<([u8; 32], u64)> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0_u8; 64 * 1024];
    let mut len = 0_u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        len += n as u64;
    }
    Ok((h.finalize().into(), len))
}

/// Regular files under `dir`, as `(relative path with '/', absolute path)`, unsorted.
fn files_below(dir: &Path, rel: &str, out: &mut Vec<(String, PathBuf)>) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let rel = if rel.is_empty() {
            name
        } else {
            format!("{rel}/{name}")
        };
        let kind = entry.file_type()?;
        if kind.is_dir() {
            files_below(&entry.path(), &rel, out)?;
        } else if kind.is_file() {
            out.push((rel, entry.path()));
        }
    }
    Ok(())
}

/// Every file of the session in hashing order, as `(name, path)`; the main file is first. `None` when it is missing.
pub(crate) fn session_files(ptr: &SessionPtr) -> std::io::Result<Option<Vec<(String, PathBuf)>>> {
    let main = ptr.jsonl();
    if !main.is_file() {
        return Ok(None);
    }
    let mut side = Vec::new();
    files_below(&ptr.sidecar(), "", &mut side)?;
    side.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut all = vec![(format!("{}.jsonl", ptr.sid), main)];
    all.extend(
        side.into_iter()
            .map(|(rel, p)| (format!("{}/{rel}", ptr.sid), p)),
    );
    Ok(Some(all))
}

/// The hash of the session at `ptr`; `None` when its main file is missing. An unreadable file is an error: the caller
/// must not take a half-read tree for the engine's copy.
pub(crate) fn hash_session(ptr: &SessionPtr) -> std::io::Result<Option<Digest>> {
    let Some(files) = session_files(ptr)? else {
        return Ok(None);
    };
    let (mut outer, mut size) = (Sha256::new(), 0_u64);
    for (name, path) in &files {
        let (inner, len) = file_digest(path)?;
        outer.update(name.as_bytes());
        outer.update([0]);
        outer.update(len.to_le_bytes());
        outer.update([0]);
        outer.update(inner);
        size += len;
    }
    Ok(Some(Digest {
        hash: hex(&outer.finalize()),
        size,
        files: files.len(),
    }))
}
