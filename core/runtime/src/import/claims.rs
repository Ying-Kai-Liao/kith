//! The writer claim files into core.db (M3-10b3): `$KITH_STATE/live/terminal/<sid>.claim`, the files `kith-sessions`, the
//! host and the core's own `terminal.open` write, into the `claims` table, so a core that starts serving finds the
//! writers that are already running.
//!
//! | file                              | core.db                                                                          |
//! |-----------------------------------|----------------------------------------------------------------------------------|
//! | `<sid>.claim`, claimer alive      | a `running` `external` claim row, whatever the file's kind (see below)             |
//! |   `pid`, `procStart`              | `pid`, `pid_started` (a stamp or the old `ps` text, kept as written: the old text   |
//! |                                   | reads unknown, so it ends only when the pid is gone, as `claims::end_gone` rules)   |
//! |   `person`                        | `person` (`owner` when the file has none, as `claim.external` writes it)           |
//! | `<sid>.claim`, claimer gone       | nothing: a claim whose process is gone is no claim                                 |
//! | `<sid>.takeover.claim`            | nothing: a take-over is a queued claim, which comes with the terminal entrance      |
//! | `kind: "starting"`                | nothing: a start in flight names the daemon's own pid                              |
//! | `proc-<pid>.claim`, claimer alive | a `running` `external` claim on the conversation `proc-<pid>-<start>`, engine `proc` |
//! |                                   | (the key `kith-sessions proc` makes in Writer mode), so a revoke finds the person's run |
//! | a session the core has no row of  | a conversation row first (agent from the file, else the registry record; none: refused) |
//!
//! What it promises:
//! - Read-only on the files (they are only ever read; the claim files stay the writer authority until they are deleted).
//! - `--dry-run` opens nothing for writing and says what would change.
//! - A real run holds the core's lifetime lock and writes in one transaction: the rows and a row in `import_log`.
//! - Idempotent: a conversation that already has a live claim row on the file's pid is left alone, and nothing is written
//!   (not even a log row) when nothing is to be added. A live row on another pid is never replaced: it is listed.
//! - A live claim the core cannot hold is never skipped quietly: it is a difference and a [`blockers`] line, because once
//!   the claims are the core's a writer with no row is a writer nobody sees.
//! - Every row is `external`, a terminal's too (H1): no holder started these processes, so a holder's hello never ends
//!   them (`release_unlisted` skips external claims) and only a revoke or a stop sends them a kill. A `terminal` row with
//!   no holder instance would move to `ending` on the next holder change and the kill re-send would signal the owner's
//!   own claude.

use std::path::Path;

use kith_kernel::threads;
use rusqlite::{Connection, OpenFlags, OptionalExtension as _, params};

use crate::conversations::basis::OWNER;
use crate::conversations::external::lockable;
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
    /// The file's name, for the lines.
    file: String,
    /// The conversation: the session id, or `proc-<pid>-<start>` for a `proc-<pid>.claim`.
    sid: String,
    pid: i64,
    started: Option<String>,
    person: String,
    /// The agent the file names (only a person's run has one).
    agent: String,
    proc: bool,
}

/// The conversation of a person's session-less run, exactly as `kith-sessions proc` keys it in Writer mode
/// (`f'proc-{pid}-{started}'.lower().replace(':', '.')`): a reused pid is another key.
pub(crate) fn proc_key(pid: i64, started: &str) -> String {
    format!("proc-{pid}-{started}")
        .to_lowercase()
        .replace(':', ".")
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
        let proc = name.starts_with("proc-");
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
        let sid = if proc {
            proc_key(pid, claim.proc_start.as_deref().unwrap_or_default())
        } else {
            name.trim_end_matches(".claim").to_owned()
        };
        wanted.push(Wanted {
            file: name,
            sid,
            pid,
            started: claim.proc_start,
            // The owner's claim is `owner` in the table, as `claim.external` writes it: `claim.release` frees only that.
            person: claim
                .person
                .filter(|p| !p.is_empty())
                .unwrap_or_else(|| OWNER.to_owned()),
            agent: claim.agent.unwrap_or_default(),
            proc,
        });
    }
    Ok((wanted, skipped))
}

/// The registry record of a session (`<sid>.json`): its agent and engine, for a conversation the core does not know yet.
fn record_of(state: &Path, sid: &str) -> (String, String) {
    let text = |v: &serde_json::Value, k: &str| {
        v.get(k)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    std::fs::read(threads::registry_dir(state).join(format!("{sid}.json")))
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .map_or_else(Default::default, |v| {
            (text(&v, "agent"), text(&v, "engine"))
        })
}

/// A claim row to add, and the conversation row it needs first (agent, engine) when the core has none.
struct Add {
    w: Wanted,
    conversation: Option<(String, String)>,
}

/// What the rows would need: the claims to add, and the lines for the live claims that cannot become one (a conflict with a
/// row on another pid, a session id the stub cannot lock, a run with no start stamp, a conversation no agent can be found for).
struct Plan {
    add: Vec<Add>,
    left: Vec<String>,
}

fn plan(conn: &Connection, state: &Path, wanted: &[Wanted]) -> Result<Plan> {
    let mut plan = Plan {
        add: Vec::new(),
        left: Vec::new(),
    };
    for w in wanted {
        if w.proc
            && !w
                .started
                .as_deref()
                .is_some_and(kith_kernel::procstart::is_stamp)
        {
            plan.left.push(format!(
                "{}: the run (pid {}) recorded no start stamp, so a revoke could not tell it from a reused pid and would not signal it",
                w.file, w.pid
            ));
            continue;
        }
        if !lockable(&w.sid) {
            plan.left.push(format!(
                "{}: the session id cannot be locked by the stub (a-z 0-9 . _ - only), so its live claim (pid {}) cannot be made a core claim",
                w.file, w.pid
            ));
            continue;
        }
        let known: Option<i64> = conn
            .query_row("SELECT 1 FROM conversations WHERE id = ?1", [&w.sid], |r| {
                r.get(0)
            })
            .optional()?;
        let conversation = if known.is_some() {
            None
        } else if w.proc {
            if w.agent.is_empty() {
                plan.left.push(format!(
                    "{}: no agent named, so no conversation can be made for the run (pid {})",
                    w.file, w.pid
                ));
                continue;
            }
            Some((w.agent.clone(), "proc".to_owned()))
        } else {
            let (agent, engine) = record_of(state, &w.sid);
            let agent = if w.agent.is_empty() {
                agent
            } else {
                w.agent.clone()
            };
            if agent.is_empty() {
                plan.left.push(format!(
                    "{}: core.db has no conversation {} and neither the claim file nor the session registry names its agent (pid {})",
                    w.file, w.sid, w.pid
                ));
                continue;
            }
            Some((
                agent,
                if engine.is_empty() {
                    "claude".to_owned()
                } else {
                    engine
                },
            ))
        };
        let live: Option<(i64, Option<i64>)> = conn
            .query_row(
                "SELECT claim_id, pid FROM claims WHERE conversation = ?1 AND state IN ('claimed', 'running', 'ending')",
                [&w.sid],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match live {
            Some((_, pid)) if pid == Some(w.pid) => {}
            Some((id, pid)) => plan.left.push(format!(
                "{}: core.db already has live claim {id} (pid {pid:?}); the file's pid {} is not imported",
                w.sid, w.pid
            )),
            None => plan.add.push(Add {
                w: w.clone(),
                conversation,
            }),
        }
    }
    Ok(plan)
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
        summarize(&mut rep, &conn, state, &wanted)?;
        rep.lines.push("dry run: nothing was written".into());
        return Ok(rep);
    }
    let mut core = crate::start_with(layout, MIGRATIONS)?;
    into(&mut core, state, &wanted, &mut rep)?;
    Ok(rep)
}

/// [`run`] over an open core (the cutover module holds one).
pub(crate) fn run_in(core: &mut Core, state: &Path, dry_run: bool) -> Result<Report> {
    let (wanted, skipped) = read_files(state)?;
    let mut rep = Report {
        dry_run,
        changed: false,
        lines: skipped,
    };
    if dry_run {
        summarize(&mut rep, core.conn(), state, &wanted)?;
        return Ok(rep);
    }
    into(core, state, &wanted, &mut rep)?;
    Ok(rep)
}

/// The live claims the core does not hold yet or cannot hold, a line each (empty = the same answer). Read-only.
pub(crate) fn differences(state: &Path, conn: &Connection) -> Result<Vec<String>> {
    let (wanted, _) = read_files(state)?;
    let p = plan(conn, state, &wanted)?;
    let mut out: Vec<String> = p
        .add
        .iter()
        .map(|a| {
            format!(
                "{}: a live claim file (pid {}), no claim row in the core",
                a.w.sid, a.w.pid
            )
        })
        .collect();
    out.extend(p.left);
    Ok(out)
}

/// Why the files cannot all be imported: a plain line each (empty = every live claim file becomes a row, so a Writer-mode core
/// would know every writer that runs now). It is what [`differences`] says about claims that cannot be held, plus a take-over
/// in flight, which is not imported. Read-only; `conn` is `None` when there is no core.db to compare with, and then every
/// live file is one the core does not know.
pub(crate) fn blockers(state: &Path, conn: Option<&Connection>) -> Result<Vec<String>> {
    let dir = threads::registry_dir(state);
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        let mut names: Vec<String> = rd
            .filter_map(std::result::Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".takeover.claim") && !n.starts_with('.'))
            .collect();
        names.sort();
        for name in names {
            if threads::read_claim(&dir.join(&name)).is_some_and(|c| c.is_live()) {
                out.push(format!(
                    "{name}: a take-over is in flight (its claimer is alive); it is not imported"
                ));
            }
        }
    }
    let (wanted, _) = read_files(state)?;
    match conn {
        Some(conn) => out.extend(plan(conn, state, &wanted)?.left),
        None => out.extend(wanted.iter().map(|w| {
            format!(
                "{}: a live claim file (pid {}), and core.db does not exist yet",
                w.file, w.pid
            )
        })),
    }
    Ok(out)
}

fn summarize(rep: &mut Report, conn: &Connection, state: &Path, wanted: &[Wanted]) -> Result<()> {
    let p = plan(conn, state, wanted)?;
    rep.lines.extend(p.left);
    if p.add.is_empty() {
        rep.lines
            .push("claims: nothing to add; core.db already holds the live claims".into());
    } else {
        rep.lines
            .push(format!("claims: would add {} claim row(s)", p.add.len()));
        rep.changed = true;
    }
    Ok(())
}

fn into(core: &mut Core, state: &Path, wanted: &[Wanted], rep: &mut Report) -> Result<()> {
    let generation = core.generation();
    let tx = core.claim_tx()?;
    let p = plan(&tx, state, wanted)?;
    rep.lines.extend(p.left);
    if p.add.is_empty() {
        rep.lines
            .push("claims: nothing to add; core.db already holds the live claims".into());
        return Ok(());
    }
    let now = now_ms();
    for a in &p.add {
        if let Some((agent, engine)) = &a.conversation {
            tx.execute(
                "INSERT INTO conversations (id, agent, engine, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
                params![a.w.sid, agent, engine, now],
            )?;
        }
        let id = Core::next_claim_id(&tx)?;
        // An `external` row needs a start text; a file without one reads as an unknown start (empty), never as a match.
        let started = a.w.started.clone().unwrap_or_default();
        tx.execute(
            "INSERT INTO claims (claim_id, conversation, generation, kind, state, person, pid, pid_started,
                                 created_at, claimed_at, running_at)
             VALUES (?1, ?2, ?3, 'external', 'running', ?4, ?5, ?6, ?7, ?7, ?7)",
            params![id, a.w.sid, generation, a.w.person, a.w.pid, started, now],
        )?;
    }
    tx.execute(
        "INSERT INTO import_log (step, at, source, source_sha256, backup, summary) VALUES ('claims', ?1, ?2, '', NULL, ?3)",
        params![
            super::now_secs(),
            threads::registry_dir(state).display().to_string(),
            format!("claims: {} live claim file(s) added", p.add.len()),
        ],
    )?;
    tx.commit()?;
    rep.changed = true;
    rep.lines.push(format!(
        "claims: added {} claim row(s). core.db is a copy: the claim files still decide who writes (the switch is a later step)",
        p.add.len()
    ));
    Ok(())
}

#[cfg(test)]
#[path = "claims_tests.rs"]
mod tests;
