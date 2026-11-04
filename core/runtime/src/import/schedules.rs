//! `kith migrate`'s schedules step (M4-2): copies `kith-sched`'s machine state (`<machine>/sched/state.json`: the last
//! `tick` and, per job, the `last` run) into core.db's `sched_tick` and `sched_runs`.
//!
//! What it promises (the same shape as the authority step, `import/mod.rs`):
//! - Read-only on the source: state.json is only ever read. jobs.json is NOT imported: definitions stay in the agents'
//!   folders and the core indexes them where they are (architect, 2026-11-04).
//! - `--dry-run` opens nothing for writing (core.db read-only, the lock not taken, no `mac-id` made) and says what would
//!   change.
//! - A real run holds the core's lifetime lock, takes a `VACUUM INTO` backup before its first write, and writes in one
//!   transaction: the tick, the runs, a row in `import_log`.
//! - Idempotent: a run whose `at` is already there for this job on this machine writes nothing, and a tick that
//!   already matches is left alone. A second run changes nothing, makes no backup and no log row.
//! - A job's `last` keeps being overwritten by `kith-sched` while it ticks, so each import adds the runs it has not
//!   seen: that is the history the file itself never kept.
//! - Old files: entries without `at` are skipped and said so; `pid`, `log` and `why` may be missing; keys that are not
//!   `agent/job` and other top-level keys (`pruned`) are ignored.

use std::path::Path;

use rusqlite::{Connection, OpenFlags, params};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::db::MIGRATIONS;
use crate::error::{Error, Result};
use crate::layout::Layout;

/// What the step did, one line each.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub dry_run: bool,
    /// Whether core.db changed, or in a dry run would.
    pub changed: bool,
    /// The `VACUUM INTO` copy taken before the first write.
    pub backup: Option<std::path::PathBuf>,
    pub lines: Vec<String>,
}

struct Run {
    agent: String,
    job: String,
    at: String,
    why: Option<String>,
    pid: Option<i64>,
    log: Option<String>,
}

struct Source {
    tick: Option<String>,
    runs: Vec<Run>,
    skipped: Vec<String>,
}

fn sha_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut s, b| {
            use std::fmt::Write as _;
            let _ = write!(s, "{b:02x}");
            s
        })
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

fn read_source(path: &Path) -> Result<Option<(Source, String)>> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(Error::Refused(format!(
                "sched state: {}: {e}; nothing was imported",
                path.display()
            )));
        }
    };
    let bad = |what: &str| {
        Error::Refused(format!(
            "sched state: {} {what}; nothing was imported",
            path.display()
        ))
    };
    let v: Value =
        serde_json::from_slice(&bytes).map_err(|e| bad(&format!("is not valid JSON ({e})")))?;
    let obj = v.as_object().ok_or_else(|| bad("is not a JSON object"))?;
    let tick = obj.get("tick").and_then(Value::as_str).map(str::to_owned);
    let mut runs = Vec::new();
    let mut skipped = Vec::new();
    if let Some(last) = obj.get("last").and_then(Value::as_object) {
        for (key, entry) in last {
            let Some((agent, job)) = key.split_once('/') else {
                skipped.push(format!("{key}: not agent/job"));
                continue;
            };
            let Some(at) = entry.get("at").and_then(Value::as_str) else {
                skipped.push(format!("{key}: no start time"));
                continue;
            };
            let text = |k: &str| entry.get(k).and_then(Value::as_str).map(str::to_owned);
            runs.push(Run {
                agent: agent.to_owned(),
                job: job.to_owned(),
                at: at.to_owned(),
                why: text("why"),
                pid: entry.get("pid").and_then(Value::as_i64),
                log: text("log"),
            });
        }
    }
    Ok(Some((
        Source {
            tick,
            runs,
            skipped,
        },
        sha_hex(&bytes),
    )))
}

fn has_table(c: &Connection, t: &str) -> bool {
    c.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [t],
        |r| r.get::<_, i64>(0),
    )
    .is_ok_and(|n| n > 0)
}

/// What a write would change for `mac`: whether the tick differs, and which runs core.db lacks (indexes into
/// `src.runs`).
fn plan(c: &Connection, src: &Source, mac: &str) -> (bool, Vec<usize>) {
    let tick = match &src.tick {
        Some(t) => {
            let have: Option<String> = c
                .query_row("SELECT tick FROM sched_tick WHERE mac = ?1", [mac], |r| {
                    r.get(0)
                })
                .ok();
            have.as_deref() != Some(t.as_str())
        }
        None => false,
    };
    let mut new = Vec::new();
    for (i, r) in src.runs.iter().enumerate() {
        let seen = c
            .query_row(
                "SELECT 1 FROM sched_runs WHERE agent = ?1 AND job = ?2 AND mac = ?3 AND at = ?4",
                params![r.agent, r.job, mac, r.at],
                |_| Ok(()),
            )
            .is_ok();
        if !seen {
            new.push(i);
        }
    }
    (tick, new)
}

fn describe(tick: bool, runs: usize) -> String {
    format!(
        "{} and {runs} run(s)",
        if tick { "the tick" } else { "no tick change" }
    )
}

/// The step. `machine` is the machine folder (state.json is in its `sched/`).
pub(crate) fn run(layout: &Layout, machine: &Path, dry_run: bool) -> Result<Report> {
    let mut rep = Report {
        dry_run,
        ..Report::default()
    };
    let file = machine.join("sched").join("state.json");
    let Some((src, sha)) = read_source(&file)? else {
        rep.lines.push(format!(
            "no sched state at {}: nothing to import",
            file.display()
        ));
        return Ok(rep);
    };
    for s in &src.skipped {
        rep.lines.push(format!("skipped {s}"));
    }
    if dry_run {
        let mac = std::fs::read_to_string(machine.join(crate::schedules::MAC_ID_FILE))
            .map(|s| s.trim().to_owned())
            .unwrap_or_default();
        let db = Connection::open_with_flags(&layout.db, OpenFlags::SQLITE_OPEN_READ_ONLY).ok();
        let (tick, runs) = if let Some(c) = db.as_ref().filter(|c| has_table(c, "sched_runs")) {
            let (t, n) = plan(c, &src, &mac);
            (t, n.len())
        } else {
            rep.lines.push(
                "core.db is missing or not migrated yet: a real run would migrate it first".into(),
            );
            (src.tick.is_some(), src.runs.len())
        };
        if tick || runs > 0 {
            rep.changed = true;
            rep.lines
                .push(format!("schedules: would import {}", describe(tick, runs)));
        } else {
            rep.lines
                .push("schedules: core.db already holds the sched state; nothing to change".into());
        }
        rep.lines.push("dry run: nothing was written".into());
        return Ok(rep);
    }

    let mac = crate::schedules::mac_id(machine)?;
    // The lifetime lock for the whole step; this also brings the schema up to date, as `kith migrate` does.
    let mut core = crate::start_with(layout, MIGRATIONS)?;
    let (tick, new) = {
        let tx = core.claim_tx()?;
        plan(&tx, &src, &mac)
    };
    if !tick && new.is_empty() {
        rep.lines
            .push("nothing to change: core.db already holds the sched state".into());
        return Ok(rep);
    }
    // Before the first write.
    let backup = core.backup(layout, "pre-import")?;
    rep.lines
        .push(format!("backup of core.db: {}", backup.display()));
    rep.backup = Some(backup.clone());
    let tx = core.claim_tx()?;
    let now = now_secs();
    if tick && let Some(t) = &src.tick {
        tx.execute(
            "INSERT INTO sched_tick (mac, tick, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (mac) DO UPDATE SET tick = excluded.tick, updated_at = excluded.updated_at",
            params![mac, t, now * 1000],
        )?;
    }
    for &i in &new {
        let r = &src.runs[i];
        tx.execute(
            "INSERT OR IGNORE INTO sched_runs (agent, job, mac, at, state, why, pid, log)
             VALUES (?1, ?2, ?3, ?4, 'started', ?5, ?6, ?7)",
            params![r.agent, r.job, mac, r.at, r.why, r.pid, r.log],
        )?;
    }
    let summary = format!("schedules: {}", describe(tick, new.len()));
    tx.execute(
        "INSERT INTO import_log (step, at, source, source_sha256, backup, summary) VALUES ('schedules', ?1, ?2, ?3, ?4, ?5)",
        params![
            now,
            file.display().to_string(),
            sha,
            backup.display().to_string(),
            summary
        ],
    )?;
    tx.commit()?;
    rep.changed = true;
    rep.lines
        .push(format!("imported {}", describe(tick, new.len())));
    rep.lines.push(
        "done. core.db is a copy: kith-sched still decides and writes its state files (the cutover is M4-5)".into(),
    );
    Ok(rep)
}
