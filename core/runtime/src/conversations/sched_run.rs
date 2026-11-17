//! The claim of a scheduled cron run (M4-5 package C; design 2026-11-17-schedules-handover Q1/Q2). A job of `kith-sched`
//! (not a routine's bot) runs as a detached process the host starts, `kith-sched run-turn`, which `exec`s `kith <agent>
//! task`: the pid IS the run. The turn's claim goes through two steps:
//!
//! 1. **[`claim_run`]**: the host claims the pulled turn as kind `turn` with no process and no account (`hostclaim.claim`,
//!    kind `scheduleRun`). Until a run attaches, this is a claim `adopt` gives back (a host change requeues the turn), that a
//!    pause, a revoke and the rollback cancel (`pid IS NULL`), and that no one counts as a run going (`CLAIM_GOING` needs a pid).
//! 2. **[`attach`]**: the run's own process records its pid and start stamp with a check-and-set on the claim
//!    (`claim.attach`: kind `turn`, `claimed`, no pid -> kind `external`, `running`, pid + start) BEFORE it `exec`s the engine,
//!    and goes no further when that fails. From then on the claim is the kind `adopt` leaves alone and `end_gone` ends: a
//!    run whose process is alive keeps going across a host restart; one whose process is gone ends with its turn `failed`
//!    (the outcome is unknown: its prompt may have gone out), never requeued. A revoke sweeps it by person like any external
//!    claim. The check-and-set is what makes `cancel_waiting_in`'s "no pid yet" rule true: a claim a rollback or a pause
//!    ended can never be attached, so no run starts that they cannot see.
//!
//! A schedule turn's claim also reads the job's paused row ([`paused_in`], `Fence::Paused`): the pause is level-triggered, so a
//! turn queued before the pause (or put back by a restart) is cancelled as it is claimed, not run.

use rusqlite::{OptionalExtension as _, Transaction, params};
use serde_json::{Value, json};

use super::claims::{self, Claimed, Kind, NewClaim};
use crate::db::Core;
use crate::error::{Error, Result};

pub(super) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(0))
}

/// Whether `turn` is a schedule's and its job is paused now. Only `source = 'schedule'` is looked at: every other turn is
/// unaffected. The job is found through the fire row that holds the turn's id (`turns` has no job column).
pub(crate) fn paused_in(tx: &rusqlite::Connection, turn: i64) -> Result<bool> {
    let found = tx
        .query_row(
            "SELECT 1 FROM turns t
               JOIN sched_fires f ON f.turn_id = CAST(t.id AS TEXT)
               JOIN sched_jobs j ON j.agent = f.agent AND j.job = f.job
             WHERE t.id = ?1 AND t.source = 'schedule' AND j.paused <> 0",
            [turn],
            |r| r.get::<_, i64>(0),
        )
        .optional();
    match found {
        Ok(row) => Ok(row.is_some()),
        // A core opened with the conversations lane alone (some tests) has no schedule tables, so nothing is paused.
        Err(rusqlite::Error::SqliteFailure(_, Some(m))) if m.contains("no such table") => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Step 1: claims a pulled schedule turn for a run that has no process yet. Refused for any turn of another source (the
/// other kinds of turn go through `claim_next`, which picks their account).
pub(crate) fn claim_run(
    core: &mut Core,
    conversation: &str,
    person: &str,
    turn: i64,
    holder_instance: Option<&str>,
    revoke_seen: i64,
    now: i64,
) -> Result<Claimed> {
    let req = NewClaim {
        conversation,
        kind: Kind::Turn,
        person,
        turn: Some(turn),
        holder_instance,
        account: None,
        process: None,
        revoke_seen,
    };
    let claimed = (|| {
        let generation = core.generation();
        let tx = core.claim_tx()?;
        let source: Option<String> = tx
            .query_row(
                "SELECT source FROM turns WHERE id = ?1 AND conversation = ?2",
                params![turn, conversation],
                |r| r.get(0),
            )
            .optional()?;
        if source.as_deref() != Some("schedule") {
            return Err(Error::Refused(format!(
                "kith-core: turn {turn} is not waiting for {person} in conversation {conversation} as a schedule's"
            )));
        }
        let claimed = claims::claim_checked_in(&tx, generation, &req, now)?;
        tx.commit()?;
        Ok::<_, Error>(claimed)
    })()?;
    match claimed {
        Ok(c) => Ok(c),
        Err(fence) => {
            // A paused job's turn is cancelled in its own write (the refusal rolled the claim back).
            claims::cancel_fenced(core, &req, &fence, now);
            Err(fence.refusal(&req))
        }
    }
}

/// Step 2: the run's own process attaches to its claim. `{attached: true}` (`again` for the same process asking twice), or
/// `{refused: "gone", state}` when the claim is no longer one a run may attach to (ended, cancelled, requeued, already has a
/// process): the caller exits without running.
pub(crate) fn attach(
    core: &mut Core,
    claim_id: i64,
    pid: u32,
    started: &str,
    now: i64,
) -> Result<Value> {
    if started.is_empty() {
        return Err(Error::BadParams(
            "kith-core: claim.attach needs the process's start stamp".into(),
        ));
    }
    let tx = core.claim_tx()?;
    let n = tx.execute(
        "UPDATE claims SET kind = 'external', state = 'running', running_at = ?2, pid = ?3, pid_started = ?4
         WHERE claim_id = ?1 AND kind = 'turn' AND state = 'claimed' AND pid IS NULL AND account IS NULL
           AND EXISTS (SELECT 1 FROM turns t WHERE t.claim_id = ?1 AND t.source = 'schedule' AND t.state = 'claimed')",
        params![claim_id, now, pid, started],
    )?;
    if n == 1 {
        tx.execute(
            "UPDATE turns SET state = 'running', started_at = ?2 WHERE claim_id = ?1 AND state = 'claimed'",
            params![claim_id, now],
        )?;
        tx.commit()?;
        return Ok(json!({ "attached": true }));
    }
    Ok(refusal(&tx, claim_id, pid, started)?)
}

fn refusal(tx: &Transaction<'_>, claim_id: i64, pid: u32, started: &str) -> Result<Value> {
    let row: Option<(String, String, Option<i64>, Option<String>)> = tx
        .query_row(
            "SELECT kind, state, pid, pid_started FROM claims WHERE claim_id = ?1",
            [claim_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    Ok(match row {
        Some((kind, state, Some(p), Some(s)))
            if kind == "external" && state == "running" && p == i64::from(pid) && s == started =>
        {
            json!({ "attached": true, "again": true })
        }
        Some((_, state, _, _)) => json!({ "refused": "gone", "state": state }),
        None => json!({ "refused": "gone", "state": "unknown" }),
    })
}

#[cfg(test)]
#[path = "sched_run_tests.rs"]
mod tests;
