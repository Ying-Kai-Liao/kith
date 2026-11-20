//! The transcripts module's same answer and its re-read (the model's `StaleRow`, `Reread`).
//!
//! [`differences`] is the import invariant: every session the registry names is in the core with the same engine, session
//! id, pointer and working folder (the hash and the size are ignored: the core reads the transcript itself), and every
//! valid file has a `session_records` row holding the same record. A row the core holds whose file is gone, or that is newer
//! than its file, is a difference too: before the switch the files are the truth.
//!
//! [`reread`] runs with the tombstone already in place, under the step's lifetime lock: it drains the spool, takes the files
//! in again (newer-wins, A7), drops rows whose file is gone, and refuses only on a [`stale`] row. A file newer than its row is
//! no refusal: a hook that wrote it after the tombstone is in Writer mode on its next look, and the daemon's reconcile takes
//! the file in.

use std::path::Path;

use rusqlite::Connection;
use serde_json::{Map, Value};

use super::transcripts::{ctx, marker_of, state};
use crate::cutover::Env;
use crate::db::Core;
use crate::error::{Error, Result};
use crate::import::transcripts::{expected, rows_without_file};
use crate::transcripts::reconcile::{drain_once, files, import_files};
use crate::transcripts::record::{self, Key};
use crate::transcripts::rows;

fn has_records(db: &Connection) -> bool {
    db.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'session_records'",
        [],
        |r| r.get::<_, i64>(0),
    )
    .is_ok_and(|n| n > 0)
}

/// A record as a file and its row compare: the file stamped with its key's `recordedAt` as the row stores it (A6), and the
/// `startedAt` of a compact left out (the row keeps the first one; the compare is about the rest).
fn normal(mut rec: Map<String, Value>, key: &Key) -> Map<String, Value> {
    record::stamp(&mut rec, key);
    if rec.get("source").and_then(Value::as_str) == Some("compact") {
        rec.remove("startedAt");
    }
    rec
}

/// Rows against files. `strict`: also a file with no row, a file newer than its row, or a record that differs (the import
/// invariant). Not strict: only the rows [`reread`] refuses on, a row newer than its file or a row with no file.
fn record_diffs(dir: &Path, db: &Connection, strict: bool) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let table = has_records(db);
    for (sid, rec, mtime) in files(dir, &mut Vec::new()) {
        let key = Key::of(&rec, mtime);
        let got = if table { record::row(db, &sid)? } else { None };
        let Some((row, rkey)) = got else {
            if strict {
                out.push(format!(
                    "{sid}: its record is in the registry, not in the core"
                ));
            }
            continue;
        };
        if rkey > key {
            out.push(format!(
                "{sid}: the core's record is newer than the registry file"
            ));
        } else if strict && rkey < key {
            out.push(format!(
                "{sid}: the registry file is newer than the core's record"
            ));
        } else if strict && normal(row, &rkey) != normal(rec, &key) {
            out.push(format!(
                "{sid}: the record differs between the registry and the core"
            ));
        }
    }
    if table {
        for sid in rows_without_file(db, dir)? {
            out.push(format!(
                "{sid}: the core has a record but the registry has no file"
            ));
        }
    }
    Ok(out)
}

pub(super) fn differences(env: &Env, db: &Connection) -> Result<Vec<String>> {
    let c = ctx(env);
    let mut diffs = Vec::new();
    for (_, want) in expected(&c) {
        let id = format!("{} ({})", want.conversation, want.account);
        let conversations: i64 = db.query_row(
            "SELECT COUNT(*) FROM conversations WHERE id = ?1",
            [&want.conversation],
            |r| r.get(0),
        )?;
        let Some(got) = rows::get(db, &want.conversation, &want.account)? else {
            diffs.push(format!("{id}: in the registry, not in the core"));
            continue;
        };
        if conversations == 0 {
            diffs.push(format!(
                "{id}: the core has the session but no conversation"
            ));
        }
        for (what, a, b) in [
            ("engine", &want.engine, &got.engine),
            ("session id", &want.session_id, &got.session_id),
            ("pointer", &want.pointer, &got.pointer),
            ("working folder", &want.cwd, &got.cwd),
        ] {
            if a != b {
                diffs.push(format!(
                    "{id}: {what} is {a:?} in the registry, {b:?} in the core"
                ));
            }
        }
    }
    if let Some(dir) = c.reg_dir().filter(|d| d.is_dir()) {
        diffs.extend(record_diffs(&dir, db, true)?);
    }
    Ok(diffs)
}

/// `before_tombstone`'s second half (the tombstone is down). On a refusal the tombstone is taken away again, so the module
/// is left `stepping` with hooks writing files as before.
pub(super) fn reread(env: &Env, core: &mut Core) -> Result<()> {
    // Any failure, not only a refusal (a session lock held past its wait, a database error), takes the tombstone away: left
    // in place it would leave hooks in Writer mode with nothing serving `session.record` (architect MUST 1 on #1049). Work
    // already done is safe: each drained entry committed with its file, and the import transaction is all-or-nothing.
    let e = match reread_inner(env, core) {
        Ok(None) => return Ok(()),
        // The refusal has taken the tombstone away itself and said so, also when it could not.
        Ok(Some(refusal)) => return Err(refusal),
        Err(e) => e,
    };
    let Ok(state) = state(env) else { return Err(e) };
    if !marker_of(state).exists() {
        return Err(e);
    }
    Err(with_note(&e, &take_tombstone_away(env)))
}

/// The error with `note` added to its text, in its own kind: `kith restore` still follows a `Corrupt`.
pub(super) fn with_note(e: &Error, note: &str) -> Error {
    let m = format!("{e} ({note})");
    match e {
        Error::Permissions(_) => Error::Permissions(m),
        Error::DatabaseMissing(_) => Error::DatabaseMissing(m),
        Error::DatabaseSwapped(_) => Error::DatabaseSwapped(m),
        Error::Corrupt(_) => Error::Corrupt(m),
        Error::NeedsMigration(_) => Error::NeedsMigration(m),
        Error::BinaryOlder(_) => Error::BinaryOlder(m),
        Error::AlreadyRunning(_) => Error::AlreadyRunning(m),
        Error::StaleGeneration(_) => Error::StaleGeneration(m),
        Error::Io(_) => Error::Io(m),
        Error::Refused(_) => Error::Refused(m),
        Error::Stopped(_) => Error::Stopped(m),
        Error::NoSuchMethod(_) => Error::NoSuchMethod(m),
        Error::NotAllowed(_) => Error::NotAllowed(m),
        Error::BadParams(_) => Error::BadParams(m),
        Error::NotServed(_) => Error::NotServed(m),
    }
}

const TOMBSTONE_GONE: &str = "the tombstone is removed and hooks write their files again";

/// Removes the tombstone and says how it went. One that cannot be removed names its path: hooks stay in Writer mode until
/// someone deletes it.
fn take_tombstone_away(env: &Env) -> String {
    let m = match state(env) {
        Ok(state) => marker_of(state),
        Err(e) => return format!("the tombstone could not be located to remove it: {e}"),
    };
    match std::fs::remove_file(&m) {
        Ok(()) => TOMBSTONE_GONE.into(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => TOMBSTONE_GONE.into(),
        Err(e) => format!(
            "the tombstone {} could NOT be removed ({e}); hooks stay in Writer mode until it is deleted",
            m.display()
        ),
    }
}

fn reread_inner(env: &Env, core: &mut Core) -> Result<Option<Error>> {
    let state = state(env)?;
    let c = ctx(env);
    let dir = kith_kernel::threads::registry_dir(state);
    // The spool first (the hooks that ran since the tombstone), through the record path.
    drain_once(core, &c, &dir)?;
    {
        let tx = core.claim_tx()?;
        import_files(&tx, &c, &dir)?;
        for sid in rows_without_file(&tx, &dir)? {
            tx.execute("DELETE FROM session_records WHERE sid = ?1", [&sid])?;
        }
        tx.commit()?;
    }
    let left = record_diffs(&dir, core.conn(), false)?;
    if left.is_empty() {
        return Ok(None);
    }
    let gone = take_tombstone_away(env);
    Ok(Some(Error::Refused(format!(
        "cutover: transcripts: the registry changed while the step ran ({}); NOT switched, {gone}. Run `kith migrate step transcripts` again, or `kith rollback transcripts`",
        left.join(" | ")
    ))))
}
