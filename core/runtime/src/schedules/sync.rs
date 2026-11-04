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
use crate::error::{Error, Result};
use kith_kernel::paths::Paths;

/// What a read of one agent's `jobs.json` found: the file's hash and its jobs. `paths` is where to look at the file again
/// under the transaction (see [`sync_in`]); `None` for a read that is the index's own (the file could not be read).
pub(super) struct Read<'a> {
    pub agent: &'a str,
    pub hash: &'a str,
    pub jobs: &'a [Value],
    pub paths: Option<&'a Paths>,
}

impl Read<'_> {
    /// Whether the file still has the hash this was read with, looked at again now under the shared `<agent>.jobs.lock`
    /// (`kith-sched` writes under it). A file that cannot be read now is an error: the caller tries again.
    fn is_current(&self) -> Result<bool> {
        let Some(paths) = self.paths else {
            return Ok(true);
        };
        let file = super::jobs_file(paths, self.agent);
        match super::read_jobs(&paths.machine, self.agent, &file) {
            Ok((hash, _)) => Ok(hash == self.hash),
            Err(e) => Err(Error::Refused(format!(
                "kith-core: schedule: {}: {e}; tried again next tick",
                file.display()
            ))),
        }
    }
}

/// What [`sync_in`] did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Synced {
    /// The index already had this hash.
    Same,
    /// The index was brought up to the read.
    Applied,
    /// The file is no longer what was read: nothing was written. Read again.
    Stale,
}

/// An agent's index as the fire reads it when the file cannot be: its hash and job definitions, in job order. `None` when
/// the agent was never indexed.
pub(super) fn indexed(
    conn: &rusqlite::Connection,
    agent: &str,
) -> Result<Option<(String, Vec<Value>)>> {
    let hash: Option<String> = conn
        .query_row(
            "SELECT hash FROM sched_index WHERE agent = ?1",
            [agent],
            |r| r.get(0),
        )
        .optional()?;
    let Some(hash) = hash else {
        return Ok(None);
    };
    let mut st = conn.prepare("SELECT def FROM sched_jobs WHERE agent = ?1 ORDER BY job")?;
    let defs = st
        .query_map([agent], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(Some((
        hash,
        defs.iter()
            .filter_map(|d| serde_json::from_str(d).ok())
            .collect(),
    )))
}

/// When the job was first indexed (ms), the anchor of an interval that never fired. Set when its row is inserted and
/// never changed by an update, so an edit to another job in the file does not move it.
pub(super) fn first_seen(
    conn: &rusqlite::Connection,
    agent: &str,
    job: &str,
) -> Result<Option<i64>> {
    // NULL and 0 both mean "not known": 0 would count an interval from 1970.
    Ok(conn
        .query_row(
            "SELECT first_seen FROM sched_jobs WHERE agent = ?1 AND job = ?2",
            params![agent, job],
            |r| r.get::<_, Option<i64>>(0),
        )
        .optional()?
        .flatten()
        .filter(|t| *t != 0))
}

/// The latest timed slot of the job with a row, whatever it decided (ms): where an interval counts from.
pub(super) fn last_timed_slot(
    conn: &rusqlite::Connection,
    agent: &str,
    job: &str,
) -> Result<Option<i64>> {
    // Timed slots are UTC text, so the latest sorts last; `manual:` and `event:` keys are not times.
    let slot: Option<String> = conn
        .query_row(
            "SELECT slot FROM sched_fires WHERE agent = ?1 AND job = ?2
               AND slot NOT LIKE 'manual:%' AND slot NOT LIKE 'event:%'
             ORDER BY slot DESC LIMIT 1",
            params![agent, job],
            |r| r.get(0),
        )
        .optional()?;
    Ok(slot.and_then(|s| super::ms_of_slot(&s)))
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

pub(super) use super::trigger_of;

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

/// Brings the index up to the file just read. Nothing changes when the hash is the index's. "Differs" is not "newer": a
/// read made before another one was applied must not put the index back, so the file is looked at again here, inside the
/// transaction (the writes of core.db are one at a time), and a read that is no longer the file's is not applied.
pub(super) fn sync_in(tx: &Transaction<'_>, read: &Read<'_>, now: i64) -> Result<Synced> {
    // The file is looked at on `Same` too: a pause written after this read (after the settle of the same tick, say) must
    // not be decided over. It costs one file read per fire.
    let same = hash_in(tx, read.agent)?.as_deref() == Some(read.hash);
    if !read.is_current()? {
        return Ok(Synced::Stale);
    }
    if same {
        return Ok(Synced::Same);
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
            "INSERT INTO sched_jobs (agent, job, def, paused, person, first_seen) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (agent, job) DO UPDATE SET def = excluded.def, paused = excluded.paused, person = excluded.person",
            params![read.agent, id, def.to_string(), i64::from(paused), person, now],
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
    Ok(Synced::Applied)
}
