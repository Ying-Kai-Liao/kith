//! A run `kith-sched` left going at the switch (M4-5 handover, H3): the import carries each job's last run with its pid
//! and, when `kith-sched` wrote one, the process' start stamp (`procStart`, the format of `procstart::start_stamp`).
//! The fire's skip rule counts such a run as an unfinished turn, so the core does not start a second run of a job while
//! the first still works.
//!
//! A run is going when its row is this Mac's, was started less than [`WINDOW_MS`] ago (kith-sched's own "still running"
//! window), and its process is alive and is the one recorded: a pid that now belongs to another process (a different start
//! stamp) is not the run. A row with only a pid falls back to kith-sched's rule: alive and under six hours. A probe that
//! cannot tell (a start that cannot be read, stamps of different forms) counts as going: skipping a tick is cheap, a
//! second run is not.

use kith_kernel::procstart::{Probe, probe, same_process};
use rusqlite::{Transaction, params};

use super::ms_of_slot;
use crate::error::Result;

/// kith-sched's "still running" window (`kith-sched`: a run younger than this with a live pid is not started again).
pub(crate) const WINDOW_MS: i64 = 6 * 3_600_000;

/// Whether one recorded process is the run it was recorded as and still going.
pub(crate) fn alive(pid: i64, recorded: Option<&str>) -> bool {
    let Ok(pid) = u32::try_from(pid) else {
        return false;
    };
    match probe(pid) {
        Probe::Gone => false,
        Probe::Unknown => true,
        Probe::Alive(current) => match (recorded.filter(|r| !r.is_empty()), current) {
            (Some(rec), Some(cur)) => same_process(rec, &cur),
            // No stamp to compare: kith-sched's rule (alive, under the window) is all there is.
            _ => true,
        },
    }
}

/// Whether the job has an imported run of this Mac that is still going.
pub(super) fn imported_going(
    tx: &Transaction<'_>,
    agent: &str,
    job: &str,
    mac: &str,
    now: i64,
) -> Result<bool> {
    let mut st = tx.prepare(
        "SELECT slot, pid, proc_start FROM sched_runs
         WHERE agent = ?1 AND job = ?2 AND mac = ?3 AND turn_id IS NULL AND state = 'started' AND pid IS NOT NULL
         ORDER BY id DESC LIMIT 5",
    )?;
    let rows = st
        .query_map(params![agent, job, mac], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows.iter().any(|(slot, pid, start)| {
        ms_of_slot(slot).is_some_and(|at| now - at < WINDOW_MS) && alive(*pid, start.as_deref())
    }))
}
