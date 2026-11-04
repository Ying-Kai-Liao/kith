//! `live/ask/*.reg` -> the `asks` table of core.db (M3-8a), one row per record (the column mapping is in
//! `migrations/children/m0100_asks.rs`). Records are read the way the code reads them (`kith_kernel::delegation::records`:
//! `upgrade` fills what an old registration lacks, a damaged file is left out).
//!
//! What it promises:
//! - Read-only on the sources: the `.reg` files are only ever read.
//! - `--dry-run` opens nothing for writing and prints what would change.
//! - A real run takes the core's lifetime lock, a `VACUUM INTO` backup before its first write, and writes in one
//!   transaction with a row in `import_log`.
//! - Idempotent: a record whose row already says the same is skipped, so a second run writes nothing (no backup, no log).
//!   A record whose file changed since (`kith-ask` is still the writer) replaces its row; a row with no file stays (the
//!   core made it, or the file was forgotten after 31 days).
//! - A record whose state the table cannot hold (not open, answered, capped or stopped) is left out and listed.
//!
//! Not here (later): the mailbox (`ID.jsonl`, `.read`, `.parked`) and the `--bg` job files (`ask-bg/`): those are the wake
//! lane's and the conversations' (`conversation` stays empty on an import).

use std::path::Path;

use kith_kernel::delegation as d;
use rusqlite::{Connection, OpenFlags};

use crate::asks::{self, Ask};
use crate::db::MIGRATIONS;
use crate::error::{Error, Result};
use crate::layout::Layout;

/// What an import did (or, with `dry_run`, would do), one line each.
#[derive(Debug, Default)]
pub(crate) struct AsksReport {
    pub dry_run: bool,
    pub lines: Vec<String>,
    pub changed: bool,
    pub backup: Option<std::path::PathBuf>,
    /// Rows written (new or replaced).
    pub written: usize,
}

/// The rows the files say, and the ids left out (with the reason).
fn desired(machine: &Path) -> (Vec<Ask>, Vec<String>) {
    let mut rows = Vec::new();
    let mut left = Vec::new();
    for (id, reg) in d::records(&d::box_dir(machine)) {
        match Ask::from_reg(&id, &reg) {
            Some(a) => rows.push(a),
            None => left.push(format!(
                "ask {id}: state {:?} is not one the table holds; left out",
                reg.get("state")
            )),
        }
    }
    (rows, left)
}

/// The rows as the files can say them: `conversation` is the core's own column, never in a `.reg`.
fn without_conversation(mut rows: Vec<Ask>) -> Vec<Ask> {
    for r in &mut rows {
        r.conversation = None;
    }
    rows
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// The step: `machine` is the Mac's folder (where `live/ask` is), `layout` the core's own.
pub(crate) fn run(layout: &Layout, machine: &Path, dry_run: bool) -> Result<AsksReport> {
    let mut rep = AsksReport {
        dry_run,
        ..AsksReport::default()
    };
    let (rows, left) = desired(machine);
    rep.lines.extend(left);
    if dry_run {
        let existing = std::fs::metadata(&layout.db)
            .ok()
            .and_then(|_| {
                Connection::open_with_flags(&layout.db, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()
            })
            .and_then(|c| asks::load_all(&c).ok())
            .map_or_else(Vec::new, without_conversation);
        let todo = rows.iter().filter(|r| !existing.contains(r)).count();
        rep.changed = todo > 0;
        rep.lines.push(format!(
            "asks: would write {todo} of {} record(s) ({} already the same)",
            rows.len(),
            rows.len() - todo
        ));
        rep.lines.push("dry run: nothing was written".into());
        return Ok(rep);
    }
    // The lifetime lock for the whole step; this also brings the schema up to date, as `kith migrate` does.
    let mut core = crate::start_with(layout, MIGRATIONS)?;
    let have = without_conversation(asks::load_all(core.conn())?);
    let todo: Vec<&Ask> = rows.iter().filter(|r| !have.contains(r)).collect();
    if todo.is_empty() {
        rep.lines.push(format!(
            "nothing to change: core.db already holds the {} record(s) of live/ask",
            rows.len()
        ));
        return Ok(rep);
    }
    let backup = core.backup(layout, "pre-asks-import")?;
    rep.lines
        .push(format!("backup of core.db: {}", backup.display()));
    rep.backup = Some(backup.clone());
    let tx = core.claim_tx()?;
    for a in &todo {
        // A row the core wrote itself (a conversation it already knows) keeps that column.
        let keep = asks::get(&tx, &a.id)?.and_then(|old| old.conversation);
        let mut row = (*a).clone();
        row.conversation = keep;
        row.put(&tx)?;
    }
    tx.execute(
        "INSERT INTO import_log (step, at, source, source_sha256, backup, summary) VALUES ('asks', ?1, ?2, '', ?3, ?4)",
        rusqlite::params![
            now_secs(),
            d::box_dir(machine).display().to_string(),
            backup.display().to_string(),
            format!("asks: {} of {} record(s) written", todo.len(), rows.len()),
        ],
    )?;
    tx.commit().map_err(Error::from)?;
    rep.written = todo.len();
    rep.changed = true;
    rep.lines.push(format!(
        "asks: wrote {} of {} record(s) from live/ask. core.db is a copy: kith-ask still writes the files (the switch is a later step)",
        todo.len(),
        rows.len()
    ));
    Ok(rep)
}
