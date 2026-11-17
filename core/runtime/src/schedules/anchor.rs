//! The per-job anchor (M4-5 handover, H2): where an imported routine's FIRST core plan counts from.
//!
//! The host keeps, per trigger of a routine, the next time it would fire (`next_runs` in serve.db's `routines.v1`). The
//! cutover's import writes that instant, as ms, into the job's `routine` block in jobs.json (`routine.anchor`). It lives
//! in the file, so it survives `sync_in` re-indexing, and it needs no migration. Jobs without one (every kith-sched cron
//! job, and jobs imported before this rule) count as before: a cron from the per-Mac watermark, an interval from its last
//! slot. One anchor per job is one per trigger: the import makes one job per trigger, and `next_runs` is per trigger.
//!
//! Why not the watermark: it is kith-sched's last tick, which may lag the host's last fire (a slot the host already ran
//! would run again), and a daily routine's next slot is a day past it (the first tick must not use the anchor up).
//!
//! - **Counting from the anchor.** While the anchor is unused the job is counted from it, on either side of the
//!   watermark: a cron/once from just before it (so `next_after` is the anchor itself), an interval from the anchor minus
//!   its period (so `next_after` is the anchor). The normal rule then applies (window, latest slot once).
//! - **Used up** only by a core plan that REACHES it (now >= anchor): that plan leaves a `sched_fires` row at a slot at or
//!   after the anchor (the fire, a skip, or a `missed` span, whose key is its last slot). A tick still before the anchor
//!   leaves nothing, so the anchor stays. [`unused`] is that one reading; a rollback that must give the host its next
//!   run back reads it too (unused: the host's `next_run` is the anchor again).
//! - A `once` the host already fired has no next run: the import writes a decided row at its slot instead, and the core
//!   never refires it.

use kith_kernel::schedule::Trigger;
use rusqlite::{Connection, OptionalExtension as _, params};
use serde_json::Value;

use crate::error::Result;

const TICK_MS: i64 = 60_000;

/// The anchor of a job definition (ms), when its `routine` block carries one.
pub(crate) fn of(def: &Value) -> Option<i64> {
    def.get("routine")?.get("anchor")?.as_i64()
}

/// The slot the anchor stands for: the anchor cut up to a whole minute (the tick's granularity).
pub(crate) fn slot_ms(anchor: i64) -> i64 {
    (anchor + (TICK_MS - 1)).div_euclid(TICK_MS) * TICK_MS
}

/// Where the plan counts from so that its first slot is the anchor.
pub(crate) fn count_from(trigger: &Trigger, anchor: i64) -> i64 {
    match trigger {
        Trigger::Interval { every } => {
            let every = i64::try_from(*every).unwrap_or(0).saturating_mul(1000);
            anchor.saturating_sub(every)
        }
        _ => anchor - 1,
    }
}

/// Whether no core plan has reached the anchor yet: the job has no timed fire row at or after its slot. (`manual:` and
/// `event:` rows are not times; pruning keeps a job's latest timed row, so a used anchor never reads as unused.)
pub(crate) fn unused(conn: &Connection, agent: &str, job: &str, anchor: i64) -> Result<bool> {
    let Some(slot) = super::slot_of_ms(slot_ms(anchor)) else {
        return Ok(false);
    };
    let used: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM sched_fires WHERE agent = ?1 AND job = ?2
               AND slot NOT LIKE 'manual:%' AND slot NOT LIKE 'event:%' AND slot >= ?3 LIMIT 1",
            params![agent, job, slot],
            |r| r.get(0),
        )
        .optional()?;
    Ok(used.is_none())
}
