//! The writer claim files into core.db (M3-10b3): `$KITH_STATE/live/terminal/<sid>.claim`, the files `kith-sessions`, the
//! host and the core's own `terminal.open` write, into the `claims` table, so a core that starts serving finds the
//! writers that are already running.
//!
//! | file                              | core.db                                                                          |
//! |-----------------------------------|----------------------------------------------------------------------------------|
//! | `<sid>.claim`, claimer alive      | a `running` claim row (`terminal` for `kind: "terminal"`, `external` for the rest)  |
//! |   `pid`, `procStart`              | `pid`, `pid_started` (a stamp or the old `ps` text, kept as written: the old text   |
//! |                                   | reads unknown, so it ends only when the pid is gone, as `claims::end_gone` rules)   |
//! |   `person`                        | `person` (empty when the file has none)                                            |
//! | `<sid>.claim`, claimer gone       | nothing: a claim whose process is gone is no claim                                 |
//! | `<sid>.takeover.claim`            | nothing: a take-over is a queued claim, which comes with the terminal entrance      |
//! | `kind: "starting"`                | nothing: a start in flight names the daemon's own pid                              |
//!
//! What it promises:
//! - Read-only on the files (they are only ever read; the claim files stay the writer authority until they are deleted).
//! - `--dry-run` opens nothing for writing and says what would change.
//! - A real run holds the core's lifetime lock and writes in one transaction: the rows and a row in `import_log`.
//! - Idempotent: a conversation that already has a live claim row on the file's pid is left alone, and nothing is written
//!   (not even a log row) when nothing is to be added. A live row on another pid is never replaced: it is listed.
//! - A session that is no conversation in core.db (the transcripts import has not run) is skipped and listed.

use std::path::Path;

use kith_kernel::threads;
use rusqlite::{Connection, OpenFlags, OptionalExtension as _, params};

use crate::db::{Core, MIGRATIONS};
use crate::error::{Error, Result};
use crate::holder_link::now_ms;
use crate::layout::Layout;

/// What an import did, a line each.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Report {
    pub dry_run: bool,
    /// Whether core.db changed, or in a dry run would.
    pub changed: bool,
    pub lines: Vec<String>,
}

/// A live claim file, as a claim row would hold it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Wanted {
    sid: String,
    external: bool,
    pid: i64,
    started: Option<String>,
    person: String,
}

/// The live claims the files hold, and a line for each file that is not one.
#[allow(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "the writers name the file exactly `<sid>.claim` (and `<sid>.takeover.claim`); another case is not theirs"
)]
fn read_files(state: &Path) -> Result<(Vec<Wanted>, Vec<String>)> {
    let dir = threads::registry_dir(state);
    let rd = match std::fs::read_dir(&dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), Vec::new())),
        Err(e) => {
            return Err(Error::Io(format!("claims import: {}: {e}", dir.display())));
        }
    };
    let mut names: Vec<String> = rd
        .filter_map(std::result::Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".claim") && !n.starts_with('.'))
        .collect();
    names.sort();
    let (mut wanted, mut skipped) = (Vec::new(), Vec::new());
    for name in names {
        if name.ends_with(".takeover.claim") {
            skipped.push(format!("{name}: a take-over claim, not imported"));
            continue;
        }
        let sid = name.trim_end_matches(".claim").to_owned();
        let Some(claim) = threads::read_claim(&dir.join(&name)) else {
            skipped.push(format!("{name}: not a claim, skipped"));
            continue;
        };
        if !claim.is_live() {
            skipped.push(format!("{name}: its claimer is gone, skipped"));
            continue;
        }
        let Some(pid) = claim.pid else { continue };
        if claim.kind == "starting" {
            skipped.push(format!("{name}: a start in flight, skipped"));
            continue;
        }
        wanted.push(Wanted {
            sid,
            external: claim.kind != "terminal",
            pid,
            started: claim.proc_start,
            person: claim.person.unwrap_or_default(),
        });
    }
    Ok((wanted, skipped))
}

/// What the rows would need: the claims to add, and the lines for the ones left alone.
fn plan(conn: &Connection, wanted: &[Wanted]) -> Result<(Vec<Wanted>, Vec<String>)> {
    let (mut add, mut lines) = (Vec::new(), Vec::new());
    for w in wanted {
        let known: Option<i64> = conn
            .query_row("SELECT 1 FROM conversations WHERE id = ?1", [&w.sid], |r| {
                r.get(0)
            })
            .optional()?;
        if known.is_none() {
            lines.push(format!(
                "{}: no such conversation in core.db yet (import the transcripts first), skipped",
                w.sid
            ));
            continue;
        }
        let live: Option<(i64, Option<i64>)> = conn
            .query_row(
                "SELECT claim_id, pid FROM claims WHERE conversation = ?1 AND state IN ('claimed', 'running', 'ending')",
                [&w.sid],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match live {
            Some((_, pid)) if pid == Some(w.pid) => {}
            Some((id, pid)) => lines.push(format!(
                "{}: core.db already has live claim {id} (pid {pid:?}); the file's pid {} is not imported",
                w.sid, w.pid
            )),
            None => add.push(w.clone()),
        }
    }
    Ok((add, lines))
}

/// The step: `state` is `$KITH_STATE`. A real run opens the core (the lifetime lock, migrating an older schema).
pub(crate) fn run(layout: &Layout, state: &Path, dry_run: bool) -> Result<Report> {
    let (wanted, skipped) = read_files(state)?;
    let mut rep = Report {
        dry_run,
        changed: false,
        lines: skipped,
    };
    if dry_run {
        let db = Connection::open_with_flags(&layout.db, OpenFlags::SQLITE_OPEN_READ_ONLY).ok();
        let ready = db.filter(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'claims'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .is_ok_and(|n| n > 0)
        });
        let Some(conn) = ready else {
            rep.lines.push(
                "core.db is missing or not migrated yet: a real run would migrate it first".into(),
            );
            rep.lines.push("dry run: nothing was written".into());
            return Ok(rep);
        };
        summarize(&mut rep, &conn, &wanted)?;
        rep.lines.push("dry run: nothing was written".into());
        return Ok(rep);
    }
    let mut core = crate::start_with(layout, MIGRATIONS)?;
    into(&mut core, state, &wanted, &mut rep)?;
    Ok(rep)
}

/// [`run`] over an open core (a test holds one).
#[cfg(test)]
pub(crate) fn run_in(core: &mut Core, state: &Path, dry_run: bool) -> Result<Report> {
    let (wanted, skipped) = read_files(state)?;
    let mut rep = Report {
        dry_run,
        changed: false,
        lines: skipped,
    };
    if dry_run {
        summarize(&mut rep, core.conn(), &wanted)?;
        return Ok(rep);
    }
    into(core, state, &wanted, &mut rep)?;
    Ok(rep)
}

fn summarize(rep: &mut Report, conn: &Connection, wanted: &[Wanted]) -> Result<()> {
    let (add, lines) = plan(conn, wanted)?;
    rep.lines.extend(lines);
    if add.is_empty() {
        rep.lines
            .push("claims: nothing to add; core.db already holds the live claims".into());
    } else {
        rep.lines
            .push(format!("claims: would add {} claim row(s)", add.len()));
        rep.changed = true;
    }
    Ok(())
}

fn into(core: &mut Core, state: &Path, wanted: &[Wanted], rep: &mut Report) -> Result<()> {
    let generation = core.generation();
    let tx = core.claim_tx()?;
    let (add, lines) = plan(&tx, wanted)?;
    rep.lines.extend(lines);
    if add.is_empty() {
        rep.lines
            .push("claims: nothing to add; core.db already holds the live claims".into());
        return Ok(());
    }
    let now = now_ms();
    for w in &add {
        let id = Core::next_claim_id(&tx)?;
        let kind = if w.external { "external" } else { "terminal" };
        // An `external` row needs a start text; a file without one reads as an unknown start (empty), never as a match.
        let started = if w.external {
            Some(w.started.clone().unwrap_or_default())
        } else {
            w.started.clone()
        };
        tx.execute(
            "INSERT INTO claims (claim_id, conversation, generation, kind, state, person, pid, pid_started,
                                 created_at, claimed_at, running_at)
             VALUES (?1, ?2, ?3, ?4, 'running', ?5, ?6, ?7, ?8, ?8, ?8)",
            params![id, w.sid, generation, kind, w.person, w.pid, started, now],
        )?;
    }
    tx.execute(
        "INSERT INTO import_log (step, at, source, source_sha256, backup, summary) VALUES ('claims', ?1, ?2, '', NULL, ?3)",
        params![
            super::now_secs(),
            threads::registry_dir(state).display().to_string(),
            format!("claims: {} live claim file(s) added", add.len()),
        ],
    )?;
    tx.commit()?;
    rep.changed = true;
    rep.lines.push(format!(
        "claims: added {} claim row(s). core.db is a copy: the claim files still decide who writes (the switch is a later step)",
        add.len()
    ));
    Ok(())
}

#[cfg(test)]
#[path = "claims_tests.rs"]
mod tests;
