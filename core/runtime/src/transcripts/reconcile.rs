//! Keeping the registry files and the `session_records` rows the same (architect MUST 1): a session started before the
//! switch keeps the hook its settings named, and that older `kith-sessions record` writes `<sid>.json` itself. So the daemon,
//! while the `transcripts` module serves, runs [`drain_once`] and [`reconcile_once`] at its start and every minute:
//!
//! - [`drain_once`]: each spool entry `inbox/<sid>.<recordedAt>.<pid>.json` goes through the same path as `session.record`
//!   (newer wins, the file from the row) and is deleted once it is in; an invalid one is deleted with a note.
//! - [`reconcile_once`]: the per-session rule of [`project::settle`] for every session with a file or a row: a file newer
//!   than its row (or with no row) is taken into the row, else a row whose file differs or is missing has its file written
//!   again. A file with no `recordedAt` is keyed by its `st_mtime_ns` (A1): an older hook's write after the core's is newer
//!   than the row it replaced.
//!
//! The cutover module (P3) also calls [`import_files`] (files into rows where they are newer, before the switch) and
//! [`export_rows`] (rows into files at a rollback, a row only where it is newer than its file).

use std::path::Path;

use kith_kernel::threads::Ctx;
use serde_json::{Map, Value};

use super::project::{self, file_of, read_file};
use super::record::{self, INBOX, Tie, Took};
use crate::db::Core;
use crate::error::Result;

/// The names in `dir` that end `.json` and do not start with `.` (a temp file), sorted.
fn json_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.strip_suffix(".json").is_some() && !n.starts_with('.'))
        .collect();
    names.sort();
    names
}

/// A spool entry is the record itself; one wrapped as the call's params (`{record}`) is read too.
fn unwrap_entry(v: Value) -> Value {
    match v {
        Value::Object(mut m)
            if !m.contains_key("sessionId") && m.get("record").is_some_and(Value::is_object) =>
        {
            m.remove("record").unwrap_or_default()
        }
        other => other,
    }
}

/// What a pass did.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Pass {
    /// Records the rows took (from the spool, or from a file newer than its row).
    pub taken: usize,
    /// Files written from their rows.
    pub written: usize,
    /// Spool entries deleted (in, older, or invalid).
    pub drained: usize,
    pub notes: Vec<String>,
}

/// Every spool entry through `session.record`'s path, then deleted. An entry that cannot be read stays for the next pass
/// (a hook may be writing it: its temp name starts with `.` and is skipped anyway).
pub(crate) fn drain_once(core: &mut Core, ctx: &Ctx<'_>, dir: &Path) -> Result<Pass> {
    let inbox = dir.join(INBOX);
    let mut pass = Pass::default();
    for name in json_names(&inbox) {
        let path = inbox.join(&name);
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let parsed = serde_json::from_slice::<Value>(&bytes).map(unwrap_entry);
        let fallback = project::mtime_ns(&path);
        match parsed {
            Ok(v) => match record::record_in(core, ctx, dir, Some(&v), fallback)? {
                Ok(took) => {
                    if took == Took::Recorded {
                        pass.taken += 1;
                    }
                }
                Err(why) => pass.notes.push(format!("inbox/{name}: {why}; dropped")),
            },
            Err(_) => pass.notes.push(format!("inbox/{name}: not JSON; dropped")),
        }
        if std::fs::remove_file(&path).is_ok() {
            pass.drained += 1;
        }
    }
    Ok(pass)
}

/// A registry file's session id, record and mtime, or why it is not one.
type FileRecord = std::result::Result<(String, Map<String, Value>, i64), String>;

/// The record of a top-level registry file whose name is its own session id, or why it is not one.
fn file_record(dir: &Path, name: &str) -> Option<FileRecord> {
    let sid = name.strip_suffix(".json")?;
    let (obj, mtime) = read_file(&dir.join(name))?;
    Some(match record::validate(Some(&Value::Object(obj))) {
        Ok((got, rec)) if got == sid => Ok((got, rec, mtime)),
        Ok(_) => Err(format!(
            "{name}: its sessionId is not its file name; left alone"
        )),
        Err(why) => Err(format!("{name}: {why}; left alone")),
    })
}

/// One pass of rows against files: [`project::settle`] for every session that has a file or a row (a file newer than its
/// row is taken into it, else the row is written to its file). Files that are no record are notes.
pub(crate) fn reconcile_once(core: &mut Core, ctx: &Ctx<'_>, dir: &Path) -> Result<Pass> {
    let mut pass = Pass::default();
    let mut sids = std::collections::BTreeSet::new();
    for name in json_names(dir) {
        match file_record(dir, &name) {
            None => {}
            Some(Err(note)) => pass.notes.push(note),
            Some(Ok((sid, _, _))) => {
                sids.insert(sid);
            }
        }
    }
    sids.extend(record::sids(core.conn())?);
    for sid in sids {
        let done = project::settle(core, ctx, dir, &sid)?;
        pass.taken += usize::from(done.taken);
        pass.written += usize::from(done.written);
    }
    Ok(pass)
}

/// Files into rows (the import before a switch, while Python is still the writer): every valid `<sid>.json` whose row is
/// missing or older by the order key (A6; a file with no `recordedAt` by its mtime) replaces the row. Answers how many rows changed. Inside the
/// caller's transaction when it is given one through `conn`.
pub(crate) fn import_files(
    conn: &rusqlite::Connection,
    ctx: &Ctx<'_>,
    dir: &Path,
) -> Result<(usize, Vec<String>)> {
    let (mut changed, mut notes) = (0, Vec::new());
    for (sid, rec, mtime) in files(dir, &mut notes) {
        // The same newer-wins decision as the record (A6, A7): a file replaces its row only when it is strictly newer.
        if let Some((rec, key)) = record::newer(conn, &sid, rec, mtime, Tie::Strict)? {
            record::put(conn, ctx, &sid, &rec, &key)?;
            changed += 1;
        }
    }
    Ok((changed, notes))
}

/// The valid registry files in `dir`: `(sid, record, mtime ns)`; the others are notes.
pub(crate) fn files(dir: &Path, notes: &mut Vec<String>) -> Vec<(String, Map<String, Value>, i64)> {
    let mut out = Vec::new();
    for name in json_names(dir) {
        match file_record(dir, &name) {
            None => {}
            Some(Err(n)) => notes.push(n),
            Some(Ok(r)) => out.push(r),
        }
    }
    out
}

/// Rows into files (a rollback, contract A3): a row's file is written only where it is missing or the row is newer than it.
/// Files with no row (pruned) stay as they are. Answers how many files were written.
pub(crate) fn export_rows(core: &Core, dir: &Path) -> Result<usize> {
    let mut n = 0;
    for sid in record::sids(core.conn())? {
        if project::export_one(core.conn(), dir, &sid)? {
            n += 1;
        }
    }
    Ok(n)
}

/// Whether `<dir>/<sid>.json` holds the same object as the row (P3's same answer can use it).
pub(crate) fn same(core: &Core, dir: &Path, sid: &str) -> Result<bool> {
    let row = record::row(core.conn(), sid)?.map(|(m, _)| m);
    let file = read_file(&file_of(dir, sid)).map(|(m, _)| m);
    Ok(row == file)
}
