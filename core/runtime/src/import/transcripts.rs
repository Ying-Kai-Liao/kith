//! `kith migrate`'s transcripts step (M3-6): the session registry `$KITH_STATE/live/terminal/<sid>.json` into
//! `conversations` and `engine_sessions`, with the hash of each session's files computed now.
//!
//! - Read-only on the registry and on the transcripts. `<sid>.claim`, `<sid>.takeover.claim` and `proc-*.claim` are the
//!   writer claims (M3-2 / M3-11) and are not imported; only `<sid>.json` files whose name is the record's own
//!   `sessionId` are.
//! - `--dry-run` opens core.db read-only (or not at all when it is missing) and writes nothing.
//! - A real run holds the core's lifetime lock, takes a backup before its first write, and writes in one transaction
//!   with an `import_log` row.
//! - Idempotent: a conversation or engine session that is already there is left as it is (the core may have read the
//!   transcript since), so a second run writes no row and no log line.
//! - A record without `account` is the engine's default login (`claude`); they are counted in the report. The main login's folder
//!   follows `CLAUDE_CONFIG_DIR` (kith-kernel's definition), so run the import with the owner's normal environment. A record whose `transcriptPath` is gone, or
//!   under an account folder that was removed, still gets its row, with no hash.
//! - `conversations.transcript_ptr/hash` (m0080) are left empty: `engine_sessions` is the source.
//! - Each record is also kept whole in `session_records` (migration 111), replacing a row older than it by the one order
//!   key (A6): the registry is the truth until the switch, and the core writes the files from these rows after it.
//! - Not wired into `kith migrate`'s default run yet (M3-10).

use std::path::Path;

use kith_kernel::threads::{Ctx, safe_id};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::db::{Core, MIGRATIONS};
use crate::error::{Error, Result};
use crate::layout::Layout;
use crate::transcripts::hash::CHANGED;
use crate::transcripts::rows::{self, EngineSession};
use crate::transcripts::{DEFAULT_ACCOUNT, SessionPtr, engine_of, hash_session};

/// What was asked of the step.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Options {
    pub dry_run: bool,
    /// Also delete the `session_records` rows whose `<sid>.json` is gone (a session pruned since an earlier step or cancel):
    /// the cutover module's import, so a second step does not write a pruned record back. `kith migrate --transcripts` keeps
    /// the rows, because on a switched Mac the core's rows are the truth. `engine_sessions` and `conversations` stay.
    pub clear_gone: bool,
}

/// What the step did, one line each.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Report {
    pub dry_run: bool,
    pub changed: bool,
    pub imported: usize,
    pub unchanged: usize,
    /// Records that named no account and were put under the default one (`claude`).
    pub defaulted: usize,
    pub backup: Option<std::path::PathBuf>,
    pub lines: Vec<String>,
}

/// A registry record, reduced to what the tables keep.
struct Record {
    sid: String,
    agent: String,
    account: String,
    /// The record named no account.
    defaulted: bool,
    cwd: String,
    transcript_path: String,
    /// The whole record, for `session_records` (migration 111), and its file's modification time in ns.
    raw: serde_json::Map<String, Value>,
    mtime: i64,
}

fn text(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// The records under the registry folder, by file name, and the files that were left out (with why).
fn read_records(dir: &Path) -> (Vec<Record>, Vec<String>, String) {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| {
            Path::new(n)
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("json"))
        })
        .collect();
    names.sort();
    let (mut records, mut skipped, mut sha) = (Vec::new(), Vec::new(), Sha256::new());
    for name in names {
        let Ok(bytes) = std::fs::read(dir.join(&name)) else {
            skipped.push(format!("{name}: unreadable, skipped"));
            continue;
        };
        sha.update(name.as_bytes());
        sha.update([0]);
        sha.update(&bytes);
        let Ok(Value::Object(raw)) = serde_json::from_slice::<Value>(&bytes) else {
            skipped.push(format!("{name}: not a JSON object, skipped"));
            continue;
        };
        let v = Value::Object(raw.clone());
        let sid = text(&v, "sessionId");
        let stem = name.strip_suffix(".json").unwrap_or(&name);
        if !safe_id(&sid) || sid != stem {
            skipped.push(format!(
                "{name}: its sessionId is not its file name, skipped"
            ));
            continue;
        }
        let given = text(&v, "account");
        let defaulted = given.is_empty();
        let account = if defaulted {
            DEFAULT_ACCOUNT.to_owned()
        } else {
            given
        };
        records.push(Record {
            sid,
            agent: text(&v, "agent"),
            account,
            defaulted,
            cwd: text(&v, "cwd"),
            transcript_path: text(&v, "transcriptPath"),
            mtime: crate::transcripts::project::mtime_ns(&dir.join(&name)),
            raw,
        });
    }
    let digest = sha.finalize().iter().fold(String::new(), |mut s, b| {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
        s
    });
    (records, skipped, digest)
}

/// Where a record's session is, and the pointer text the row stores ([`crate::transcripts::pointer`]).
fn pointer_of(ctx: &Ctx<'_>, r: &Record) -> (Option<SessionPtr>, String) {
    crate::transcripts::pointer(ctx, &r.account, &r.sid, &r.transcript_path)
}

/// The registry's sessions as the import would write them, but without reading any transcript (no hash, no size): what the
/// cutover module's same-answer compares the core against. `agent` first. No registry folder = none.
pub(crate) fn expected(ctx: &Ctx<'_>) -> Vec<(String, EngineSession)> {
    let Some(dir) = ctx.reg_dir().filter(|d| d.is_dir()) else {
        return Vec::new();
    };
    read_records(&dir)
        .0
        .iter()
        .map(|r| {
            let row = EngineSession {
                conversation: r.sid.clone(),
                account: r.account.clone(),
                engine: engine_of(&r.account).to_owned(),
                session_id: r.sid.clone(),
                pointer: pointer_of(ctx, r).1,
                cwd: r.cwd.clone(),
                hash: None,
                size: None,
                last_read_at: None,
            };
            (r.agent.clone(), row)
        })
        .collect()
}

/// The `engine_sessions` row a record stands for, hash computed now.
/// The second answer is true when the transcript kept changing while it was read, so no hash was taken.
fn session_of(ctx: &Ctx<'_>, r: &Record, now: i64) -> (EngineSession, bool) {
    let engine = engine_of(&r.account).to_owned();
    let (ptr, pointer) = pointer_of(ctx, r);
    let read = ptr
        .as_ref()
        .filter(|_| engine == "claude")
        .map(hash_session);
    let churned = matches!(&read, Some(Err(e)) if e.to_string() == CHANGED);
    let digest = read.and_then(|r| r.ok().flatten());
    let row = EngineSession {
        conversation: r.sid.clone(),
        account: r.account.clone(),
        engine,
        session_id: r.sid.clone(),
        pointer,
        cwd: r.cwd.clone(),
        size: digest
            .as_ref()
            .map(|d| i64::try_from(d.size).unwrap_or(i64::MAX)),
        last_read_at: digest.as_ref().map(|_| now),
        hash: digest.map(|d| d.hash),
    };
    (row, churned)
}

fn has_tables(c: &Connection) -> bool {
    ["conversations", "engine_sessions", "import_log"]
        .iter()
        .all(|t| {
            c.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [t],
                |r| r.get::<_, i64>(0),
            )
            .is_ok_and(|n| n > 0)
        })
}

/// The record as its `session_records` row would take it, when the row is missing or older by the one order key (A6, A7;
/// a file with no `recordedAt` by its mtime): the same newer-wins decision as `session.record`'s.
fn newer_row(
    c: &Connection,
    r: &Record,
) -> Result<
    Option<(
        serde_json::Map<String, Value>,
        crate::transcripts::record::Key,
    )>,
> {
    use crate::transcripts::record::{Tie, newer};
    newer(c, &r.sid, r.raw.clone(), r.mtime, Tie::Strict)
}

/// Whether the record's `session_records` row is missing or older than it (migration 111; a core.db from before it
/// has no such table, and a dry run then counts every record).
fn record_stale(c: Option<&Connection>, r: &Record, mirror: bool) -> Result<bool> {
    let Some(c) = c else {
        return Ok(true);
    };
    let has: i64 = c.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'session_records'",
        [],
        |row| row.get(0),
    )?;
    if has == 0 {
        return Ok(true);
    }
    Ok(newer_row(c, r)?.is_some() || (mirror && row_newer_than_file(c, r)?))
}

/// Mirror mode (the cutover module's import): the files are the truth until the switch, so a row that is newer than its file
/// (the file was pruned and an older spool entry placed, or restored) is replaced by it. Python is the only writer then.
fn row_newer_than_file(c: &Connection, r: &Record) -> Result<bool> {
    use crate::transcripts::record::{Key, row};
    Ok(row(c, &r.sid)?.is_some_and(|(_, k)| k > Key::of(&r.raw, r.mtime)))
}

/// The sids that have a `session_records` row and no `<sid>.json` at all (a file that exists but cannot be read is not gone).
pub(crate) fn rows_without_file(c: &Connection, dir: &Path) -> Result<Vec<String>> {
    let has: i64 = c.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'session_records'",
        [],
        |row| row.get(0),
    )?;
    if has == 0 {
        return Ok(Vec::new());
    }
    Ok(crate::transcripts::record::sids(c)?
        .into_iter()
        .filter(|sid| !crate::transcripts::project::file_of(dir, sid).exists())
        .collect())
}

/// Which records need writing: `(conversation missing, session missing or different from the registry)`.
fn missing(c: Option<&Connection>, s: &EngineSession) -> Result<(bool, bool)> {
    let Some(c) = c else {
        return Ok((true, true));
    };
    let conv: i64 = c.query_row(
        "SELECT COUNT(*) FROM conversations WHERE id = ?1",
        [&s.conversation],
        |r| r.get(0),
    )?;
    // A row that differs is redone as well as a missing one: the registry is the truth until the switch, and a resume that
    // rewrites a record (its working folder, its pointer) would otherwise fail the same-answer check on every try. The
    // hash and the size are not compared: the core reads the transcript itself and moves them on.
    let row_stale = rows::get(c, &s.conversation, &s.account)?.is_none_or(|got| {
        got.engine != s.engine
            || got.session_id != s.session_id
            || got.pointer != s.pointer
            || got.cwd != s.cwd
    });
    Ok((conv == 0, row_stale))
}

fn add_conversation(c: &Connection, s: &EngineSession, agent: &str, now: i64) -> Result<()> {
    c.execute(
        "INSERT OR IGNORE INTO conversations (id, agent, engine, engine_session, account, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
        rusqlite::params![s.conversation, agent, s.engine, s.session_id, s.account, now],
    )?;
    Ok(())
}

/// The step. `ctx.state` names the registry; `ctx.home`, `ctx.machine` and the env say where the transcripts are.
pub(crate) fn run(layout: &Layout, ctx: &Ctx<'_>, opts: Options) -> Result<Report> {
    run_in(layout, ctx, opts, None)
}

/// [`run`] for the cutover module (`cutover/modules/transcripts.rs`), which already holds the core's lifetime lock: a real
/// run writes through that core instead of taking the lock again.
pub(crate) fn run_in(
    layout: &Layout,
    ctx: &Ctx<'_>,
    opts: Options,
    held: Option<&mut Core>,
) -> Result<Report> {
    let mut rep = Report {
        dry_run: opts.dry_run,
        ..Report::default()
    };
    let Some(dir) = ctx.reg_dir() else {
        rep.lines
            .push("KITH_STATE is not set: no session registry to import".into());
        return Ok(rep);
    };
    if !dir.is_dir() {
        rep.lines.push(format!(
            "no session registry at {}: nothing to import",
            dir.display()
        ));
        return Ok(rep);
    }
    let (records, skipped, sha) = read_records(&dir);
    rep.lines.extend(skipped);
    let now = now_secs();
    rep.defaulted = records.iter().filter(|r| r.defaulted).count();
    if rep.defaulted > 0 {
        rep.lines.push(format!(
            "{} record(s) name no account and are put under the default one ({DEFAULT_ACCOUNT})",
            rep.defaulted
        ));
    }
    let mut churned = Vec::new();
    let sessions: Vec<(String, EngineSession)> = records
        .iter()
        .map(|r| {
            let (row, changed) = session_of(ctx, r, now);
            churned.push(changed);
            (r.agent.clone(), row)
        })
        .collect();
    for ((_, s), changed) in sessions.iter().zip(&churned) {
        if *changed {
            rep.lines.push(format!(
                "{}: the transcript changed while it was read; imported without a hash",
                s.session_id
            ));
        } else if s.hash.is_none() && s.engine == "claude" {
            rep.lines.push(format!(
                "{}: no transcript found at {}; imported without a hash",
                s.session_id,
                if s.pointer.is_empty() {
                    "any login"
                } else {
                    &s.pointer
                }
            ));
        }
    }
    if opts.dry_run {
        let db = Connection::open_with_flags(&layout.db, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .ok()
            .filter(has_tables);
        let (mut would, mut same) = (0, 0);
        for ((_, s), r) in sessions.iter().zip(&records) {
            let (c, e) = missing(db.as_ref(), s)?;
            if c || e || record_stale(db.as_ref(), r, opts.clear_gone)? {
                would += 1;
            } else {
                same += 1;
            }
        }
        let gone = match (&db, opts.clear_gone) {
            (Some(c), true) => rows_without_file(c, &dir)?.len(),
            _ => 0,
        };
        rep.imported = would;
        rep.unchanged = same;
        rep.changed = would + gone > 0;
        rep.lines.push(format!(
            "transcripts: would import {would} session(s), {same} already there"
        ));
        if gone > 0 {
            rep.lines.push(format!(
                "transcripts: would drop {gone} record row(s) whose registry file is gone"
            ));
        }
        rep.lines.push("dry run: nothing was written".into());
        return Ok(rep);
    }
    // The lifetime lock for the whole step; this also brings the schema up to date.
    let mut taken;
    let core = if let Some(c) = held {
        c
    } else {
        taken = crate::start_with(layout, MIGRATIONS)?;
        &mut taken
    };
    let mut todo: Vec<&(String, EngineSession)> = Vec::new();
    let mut whole: Vec<&Record> = Vec::new();
    let mut touched = 0;
    for (item, r) in sessions.iter().zip(&records) {
        let (c, e) = missing(Some(core.conn()), &item.1)?;
        let stale = record_stale(Some(core.conn()), r, opts.clear_gone)?;
        if c || e {
            todo.push(item);
        }
        if stale {
            whole.push(r);
        }
        if c || e || stale {
            touched += 1;
        }
    }
    rep.unchanged = sessions.len() - touched;
    let gone = if opts.clear_gone {
        rows_without_file(core.conn(), &dir)?
    } else {
        Vec::new()
    };
    if todo.is_empty() && whole.is_empty() && gone.is_empty() {
        rep.lines.push(format!(
            "nothing to change: core.db already holds the {} session(s) of the registry",
            sessions.len()
        ));
        return Ok(rep);
    }
    let backup = core.backup(layout, "pre-import-transcripts")?;
    rep.lines
        .push(format!("backup of core.db: {}", backup.display()));
    let tx = core.claim_tx()?;
    for (agent, s) in todo.iter().map(|t| (&t.0, &t.1)) {
        add_conversation(&tx, s, agent, now)?;
        rows::put(&tx, s, now)?;
    }
    // The whole record where the file is newer than its row (Python is the writer until the switch). Only the row: the
    // engine session and the conversation were written just above, from the same record, with the hash read now.
    for r in &whole {
        if opts.clear_gone && row_newer_than_file(&tx, r)? {
            tx.execute("DELETE FROM session_records WHERE sid = ?1", [&r.sid])?;
        }
        if let Some((rec, key)) = newer_row(&tx, r)? {
            crate::transcripts::record::put_row(&tx, &r.sid, &rec, &key)?;
        }
    }
    for sid in &gone {
        tx.execute("DELETE FROM session_records WHERE sid = ?1", [sid])?;
    }
    tx.execute(
        "INSERT INTO import_log (step, at, source, source_sha256, backup, summary) VALUES ('transcripts', ?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            now,
            dir.display().to_string(),
            sha,
            backup.display().to_string(),
            format!(
                "transcripts: {} session(s) imported, {} record row(s) dropped",
                touched,
                gone.len()
            ),
        ],
    )
    .map_err(Error::from)?;
    tx.commit()?;
    rep.backup = Some(backup);
    rep.imported = touched;
    rep.changed = true;
    rep.lines.push(format!(
        "transcripts: imported {touched} session(s); the registry and the transcripts were only read",
    ));
    if !gone.is_empty() {
        rep.lines.push(format!(
            "transcripts: dropped {} record row(s) whose registry file is gone (pruned since)",
            gone.len()
        ));
    }
    Ok(rep)
}
