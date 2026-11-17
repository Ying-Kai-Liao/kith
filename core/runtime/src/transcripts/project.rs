//! The registry files as a projection of the `session_records` rows: `<sid>.json` written from its row (tmp + rename, 0600,
//! in a 0700 folder, as `kith-sessions write_atomic` does), under a lock per session so a read of the row and the rename that
//! follows it are never interleaved with another projection of the same session.
//!
//! The bytes are Python's `json.dump` spacing (`kith_kernel::secrets::pyjson`), so a reader sees the same JSON object the hook
//! wrote; only the object is promised, not the bytes. Lock order: the core first (`&mut Core` or the daemon's core mutex),
//! then the session's lock; nothing takes them the other way round.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use kith_kernel::threads::Ctx;
use rusqlite::Connection;
use serde_json::{Map, Value};

use super::record::{self, Key};
use crate::db::Core;
use crate::error::Result;

type Locks = Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>;

fn locks() -> &'static Locks {
    static LOCKS: OnceLock<Locks> = OnceLock::new();
    LOCKS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Runs `f` holding the lock of one registry file.
pub(crate) fn with_lock<R>(file: &Path, f: impl FnOnce() -> R) -> R {
    let one = locks()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(file.to_path_buf())
        .or_default()
        .clone();
    let out = {
        let _held = one
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f()
    };
    let mut all = locks()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Ours and the map's: nobody else waits on it, so it goes (the map stays as small as the live calls).
    if Arc::strong_count(&one) == 2 {
        all.remove(file);
    }
    out
}

/// `<dir>/<sid>.json`.
pub(crate) fn file_of(dir: &Path, sid: &str) -> PathBuf {
    dir.join(format!("{sid}.json"))
}

/// The registry file's object and its modification time in ns; `None` when it is missing or not a JSON object.
pub(crate) fn read_file(path: &Path) -> Option<(Map<String, Value>, i64)> {
    let bytes = std::fs::read(path).ok()?;
    let mtime = mtime_ns(path);
    match serde_json::from_slice(&bytes).ok()? {
        Value::Object(m) => Some((m, mtime)),
        _ => None,
    }
}

/// A file's modification time in ns since the epoch, 0 when it cannot be read.
pub(crate) fn mtime_ns(path: &Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
}

/// Writes `value` to `path` atomically: `.<name>.<pid>.tmp` beside it (0600), then a rename. The folder is made 0700 and
/// set to 0700, as the hook does.
pub(crate) fn write_atomic(path: &Path, value: &Value) -> Result<()> {
    let parent = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = parent.join(format!(".{name}.{}.tmp", std::process::id()));
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        o.mode(0o600);
    }
    let written = (|| -> std::io::Result<()> {
        use std::io::Write as _;
        let mut f = o.open(&tmp)?;
        f.write_all(kith_kernel::secrets::pyjson::dumps(value).as_bytes())?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    Ok(written?)
}

/// What [`settle`] did for one session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Settled {
    /// The file was newer than the row and the row took it.
    pub taken: bool,
    /// The file was written from the row.
    pub written: bool,
}

/// The one rule between a session's row and its file, under the session's lock (contract A2): a file newer than its row
/// (order key; a file with no `recordedAt` by its `st_mtime_ns`) is taken into the row; otherwise a row whose file is
/// missing or holds another object is written to the file. `session.record` runs it after its commit, and so do the drain
/// and the minute's pass. A file that is no record of this session (bad JSON, another `sessionId`, invalid) is never taken,
/// and is overwritten from the row when there is one. No row and no valid file: nothing.
///
/// The window it leaves (contract A4, documented, not closed): between this look at the file and the rename, an older hook
/// that writes `<sid>.json` itself can land its file, and the rename then replaces it. The next pass sees the row only (that
/// record is lost unless the hook records again); the window is the few microseconds of one write.
pub(crate) fn settle(core: &mut Core, ctx: &Ctx<'_>, dir: &Path, sid: &str) -> Result<Settled> {
    let path = file_of(dir, sid);
    with_lock(&path, || {
        let mut done = Settled::default();
        let file = read_file(&path).and_then(|(obj, mtime)| {
            match record::validate(Some(&Value::Object(obj))) {
                Ok((got, rec)) if got == sid => Some((rec, mtime)),
                _ => None,
            }
        });
        let mut row = record::row(core.conn(), sid)?;
        if let Some((rec, mtime)) = &file {
            let newer = row
                .as_ref()
                .is_none_or(|(have, key)| have != rec && Key::of(rec, *mtime) > *key);
            if newer
                && record::commit(core, ctx, sid, rec.clone(), *mtime)? == record::Took::Recorded
            {
                done.taken = true;
                row = record::row(core.conn(), sid)?;
            }
        }
        let Some((rec, _)) = row else {
            return Ok(done);
        };
        if file.as_ref().is_some_and(|(have, _)| *have == rec) {
            return Ok(done);
        }
        write_atomic(&path, &Value::Object(rec))?;
        done.written = true;
        Ok(done)
    })
}

/// A rollback's rows into files (contract A3): a row is written only when its file is missing or the row is newer than
/// the file (a file with no `recordedAt` by its `st_mtime_ns`), under the session's lock. Answers whether it wrote.
pub(crate) fn export_one(conn: &Connection, dir: &Path, sid: &str) -> Result<bool> {
    let path = file_of(dir, sid);
    with_lock(&path, || {
        let Some((rec, key)) = record::row(conn, sid)? else {
            return Ok(false);
        };
        if let Some((have, mtime)) = read_file(&path)
            && (have == rec || Key::of(&have, mtime) >= key)
        {
            return Ok(false);
        }
        write_atomic(&path, &Value::Object(rec))?;
        Ok(true)
    })
}
