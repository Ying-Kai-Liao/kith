//! The index of the agents' `jobs.json` in core.db (M4-3; tables of migration 122). The definitions stay in the files; the
//! fire decides on the index row, read in its own transaction, after comparing the hash of the file it just read with the
//! index's and re-indexing when they differ (git sync, hand edits, and `kith-sched` until M4-5, which keeps writing the
//! files).
//!
//! Every write of a row that is `paused`, and the removal of a job, cancels that job's queued and parked turns, and ends
//! the claim of a `claimed` turn that has no process yet; a running turn is left alone (rules 5 of the fire protocol). It
//! happens on every re-index, not only when the flag flips: a turn the pause missed (it was queued by a fire that read the
//! old row) is caught by the next write.

use rusqlite::{OptionalExtension as _, Transaction, params};
use serde_json::Value;

use crate::conversations::claims;
use crate::error::Result;
use kith_kernel::schedule::{Trigger, Zone};

/// What a read of one agent's `jobs.json` found: the file's hash and its jobs.
pub(super) struct Read<'a> {
    pub agent: &'a str,
    pub hash: &'a str,
    pub jobs: &'a [Value],
}

/// A job as the index holds it.
pub(super) struct Job {
    pub def: Value,
    pub paused: bool,
    pub person: Option<String>,
}

pub(super) fn job_id(def: &Value) -> String {
    match def.get("id") {
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

pub(super) fn is_paused(def: &Value) -> bool {
    def.get("paused")
        .is_some_and(|p| !matches!(p, Value::Null | Value::Bool(false)))
}

/// The job's trigger: its `trigger` object when it has one, else the `cron` of a `kith-sched` job (follows the Mac unless
/// the job names a `tz`).
pub(super) fn trigger_of(def: &Value) -> std::result::Result<Trigger, String> {
    let t = if let Some(t) = def.get("trigger") {
        serde_json::from_value::<Trigger>(t.clone()).map_err(|e| format!("trigger: {e}"))?
    } else {
        let expr = def
            .get("cron")
            .and_then(Value::as_str)
            .ok_or("no cron or trigger")?;
        let tz = match def.get("tz").and_then(Value::as_str) {
            Some(z) => Zone::parse(z).map_err(|e| e.0)?,
            None => Zone::Local,
        };
        Trigger::Cron {
            expr: expr.to_owned(),
            tz,
        }
    };
    t.validate().map_err(|e| e.0)?;
    Ok(t)
}

/// The hash the index holds for the agent's file, if it was ever indexed.
pub(super) fn hash_in(tx: &Transaction<'_>, agent: &str) -> Result<Option<String>> {
    Ok(tx
        .query_row(
            "SELECT hash FROM sched_index WHERE agent = ?1",
            [agent],
            |r| r.get(0),
        )
        .optional()?)
}

/// The job's index row.
pub(super) fn job_in(tx: &Transaction<'_>, agent: &str, job: &str) -> Result<Option<Job>> {
    let row: Option<(String, i64, Option<String>)> = tx
        .query_row(
            "SELECT def, paused, person FROM sched_jobs WHERE agent = ?1 AND job = ?2",
            params![agent, job],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    Ok(row.map(|(def, paused, person)| Job {
        def: serde_json::from_str(&def).unwrap_or(Value::Null),
        paused: paused != 0,
        person,
    }))
}

/// The job's turns that have not finished (`queued`, `parked`, `claimed`, `running`), found through the fire rows of every
/// slot it has, timed, `manual:` and `event:` alike (`turns` has no job column).
pub(super) fn unfinished_in(tx: &Transaction<'_>, agent: &str, job: &str) -> Result<Vec<i64>> {
    let mut st = tx.prepare(
        "SELECT DISTINCT t.id FROM sched_fires f JOIN turns t ON t.id = CAST(f.turn_id AS INTEGER)
         WHERE f.agent = ?1 AND f.job = ?2 AND f.turn_id IS NOT NULL
           AND t.state IN ('queued', 'parked', 'claimed', 'running')
         ORDER BY t.id",
    )?;
    let ids = st
        .query_map(params![agent, job], |r| r.get(0))?
        .collect::<std::result::Result<Vec<i64>, _>>()?;
    Ok(ids)
}

/// Cancels what a paused or removed job has waiting; see the module doc.
pub(super) fn cancel_in(
    tx: &Transaction<'_>,
    agent: &str,
    job: &str,
    why: &str,
    now: i64,
) -> Result<Vec<i64>> {
    let ids = unfinished_in(tx, agent, job)?;
    claims::cancel_waiting_in(tx, &ids, why, now)
}

/// Brings the index up to the file just read. Nothing changes when the hash is the index's. Returns whether it re-indexed.
pub(super) fn sync_in(tx: &Transaction<'_>, read: &Read<'_>, now: i64) -> Result<bool> {
    if hash_in(tx, read.agent)?.as_deref() == Some(read.hash) {
        return Ok(false);
    }
    let mut keep = Vec::new();
    for def in read.jobs {
        let id = job_id(def);
        let paused = is_paused(def);
        let person = def
            .get("person")
            .and_then(Value::as_str)
            .filter(|p| !p.is_empty());
        tx.execute(
            "INSERT INTO sched_jobs (agent, job, def, paused, person) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (agent, job) DO UPDATE SET def = excluded.def, paused = excluded.paused, person = excluded.person",
            params![read.agent, id, def.to_string(), i64::from(paused), person],
        )?;
        if paused {
            cancel_in(tx, read.agent, &id, "paused", now)?;
        }
        keep.push(id);
    }
    let have: Vec<String> = {
        let mut st = tx.prepare("SELECT job FROM sched_jobs WHERE agent = ?1")?;
        st.query_map([read.agent], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?
    };
    for gone in have.iter().filter(|j| !keep.contains(j)) {
        cancel_in(tx, read.agent, gone, "removed", now)?;
        tx.execute(
            "DELETE FROM sched_jobs WHERE agent = ?1 AND job = ?2",
            params![read.agent, gone],
        )?;
    }
    tx.execute(
        "INSERT INTO sched_index (agent, hash, at) VALUES (?1, ?2, ?3)
         ON CONFLICT (agent) DO UPDATE SET hash = excluded.hash, at = excluded.at",
        params![read.agent, read.hash, now],
    )?;
    Ok(true)
}
