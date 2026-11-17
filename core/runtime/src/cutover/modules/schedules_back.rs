//! The schedules rollback's hand-back (M4-5 handover, rule H4; architect MUST 5 and SHOULD 2). Wiring only in
//! `schedules.rs`: it calls these while it holds `host.lock` and `tick.lock` (`schedules_switch::hold_both`, taken by
//! `rollback_lock` before the rolling-back record, so the whole write-back happens under both).
//!
//! The rule is per job, never a global rewind (a rewind replays every job that was not cancelled; TLC fails `AtMostOnce`).
//! 1. [`cancel_waiting`] ends the schedule turns that wait (queued, parked, claimed with no process) and turns their fire
//!    rows into `skipped:rollback`, stamped with the time of the rollback. A turn that runs is left to run. A claimed turn
//!    with no process still counts in `live` and refuses the rollback, so the "claimed with no process" cancel only runs
//!    under `--force`.
//! 2. [`next_runs`]: a routine trigger's `next_run` in serve.db is, in this order: the slot of the turn rollback cancelled;
//!    else its anchor while no core plan reached it (`anchor::unused`, and only while `anchorTrigger` still matches);
//!    else the next slot after what the core decided (a cron: the watermark; an interval: its last timed slot); a `once`
//!    with a decided row gets none, one never decided keeps its `at`. A webhook or event trigger has none to write. A
//!    rollback writes back only the flag and the key, so the host keeps its own trigger: where `jobs.json` edited trigger
//!    i, the host's trigger rules (no anchor) and the edit is said to be dropped; a trigger the host lacks gets nothing.
//! 3. [`write_state_back`]: `state.json`'s `tick` is the core's watermark and `last` carries each job's newest run: the
//!    one kith-sched had, or a run the core started that is still going ([`going_in_core`]: any claim with a pid, so the
//!    external claims package C adds are read the same way), so kith-sched's still-running rule skips it.
//! 4. [`rearm`], on a later step: the anchor read from the written-back `next_run` is the slot rollback cancelled; its
//!    `skipped:rollback` row would count as decided and the core would never run it, so the row goes.
//!
//! A cancelled slot of a kith-sched (cron) job is reported, not replayed: kith-sched has one global tick.

use std::collections::BTreeMap;

use kith_kernel::schedule::Trigger;
use rusqlite::{Connection, OptionalExtension as _, params};
use serde_json::{Map, Value, json};

use crate::cutover::Env;
use crate::db::Core;
use crate::error::{Error, Result};
use crate::schedules::{anchor, local_minute, ms_of_slot, slot_of_ms, watermark_in};

/// A claim that is a run still going: it has a process and has not ended. The one rule for `live` (which refuses on any
/// other running turn) and [`going_in_core`] (which hands the run to kith-sched), so a run `live` lets pass always lands
/// in `state.json`'s `last`. SQL over the alias `c` (claims).
pub(super) const CLAIM_GOING: &str =
    "c.pid IS NOT NULL AND c.state IN ('claimed', 'running', 'ending')";

/// The host's triggers by routine id and trigger index, as the kernel's, read from `routines.v1`.
pub(super) fn host_triggers(state: &Value) -> BTreeMap<Key, Trigger> {
    let mut out = BTreeMap::new();
    let list = state.get("routines").and_then(Value::as_array);
    for r in list.into_iter().flatten() {
        let Some(id) = r.get("id").and_then(Value::as_str) else {
            continue;
        };
        let triggers = r.get("triggers").and_then(Value::as_array);
        for (i, t) in triggers.into_iter().flatten().enumerate() {
            if let Ok(t) = super::schedules::convert(t) {
                out.insert((id.to_owned(), i), t);
            }
        }
    }
    out
}

/// The outcome a cancelled slot's fire row is given.
pub(super) const ROLLBACK_OUTCOME: &str = "skipped:rollback";

/// One routine trigger: the routine's id and the trigger's index.
pub(super) type Key = (String, usize);

/// What to write into `nextRuns`: `Some(ms)` a time, `None` no next run. A trigger not in the map is left as it is.
pub(super) type NextRuns = BTreeMap<Key, NextRun>;

/// One trigger's next run: a time (ms), or none.
pub(super) type NextRun = Option<i64>;

/// A run the core started that still goes: agent, job, slot, pid, the process' start stamp, when it started (ms).
type Going = (String, String, String, i64, Option<String>, i64);

/// Ends the schedule turns that wait, so none is stranded in a core that no longer runs them, and says what it did.
/// `now` (ms) stamps the rows: [`next_runs`] finds this rollback's cancelled slots by it, so a re-run after a crash reads
/// the same answer.
pub(super) fn cancel_waiting(core: &mut Core, now: i64) -> Result<Vec<String>> {
    let tx = core.claim_tx()?;
    let waiting: Vec<(String, String, String, i64)> = {
        let mut st = tx.prepare(
            "SELECT f.agent, f.job, f.slot, t.id FROM sched_fires f JOIN turns t ON t.id = CAST(f.turn_id AS INTEGER)
             WHERE f.turn_id IS NOT NULL AND t.state IN ('queued', 'parked', 'claimed') ORDER BY f.agent, f.job, f.slot",
        )?;
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<std::result::Result<_, _>>()?
    };
    let mut lines = Vec::new();
    for (agent, job, slot, turn) in waiting {
        let done = crate::conversations::claims::cancel_waiting_in(&tx, &[turn], "rollback", now)?;
        if done.is_empty() {
            continue;
        }
        tx.execute(
            "UPDATE sched_fires SET outcome = ?4, at = ?5 WHERE agent = ?1 AND job = ?2 AND slot = ?3",
            params![agent, job, slot, ROLLBACK_OUTCOME, now],
        )?;
        let routine: Option<bool> = tx
            .query_row(
                "SELECT json_extract(def, '$.routine') IS NOT NULL FROM sched_jobs WHERE agent = ?1 AND job = ?2",
                params![agent, job],
                |r| r.get(0),
            )
            .optional()?;
        lines.push(match routine {
            Some(true) => {
                format!("{agent}/{job}: slot {slot} was cancelled; the host runs it once it starts")
            }
            Some(false) => format!(
                "{agent}/{job}: slot {slot} was cancelled and is not replayed (kith-sched has one global tick, past that slot)"
            ),
            None => format!("{agent}/{job}: slot {slot} was cancelled (the job is gone)"),
        });
    }
    tx.commit()?;
    Ok(lines)
}

/// The next runs to write for these jobs (this Mac's routine jobs, as `(agent, job definition)`); `since` (ms) is when
/// the step began; `host` the host's own triggers ([`host_triggers`]). Also the lines for edits dropped.
pub(super) fn next_runs(
    conn: &Connection,
    mac: &str,
    since: i64,
    jobs: &[(String, Value)],
    host: &BTreeMap<Key, Trigger>,
) -> Result<(NextRuns, Vec<String>)> {
    let wm = watermark_in(conn, mac)?;
    let mut out = NextRuns::new();
    let mut said = Vec::new();
    for (agent, def) in jobs {
        let Some(routine) = def.get("routine") else {
            continue;
        };
        let (Some(id), Some(n), Some(job)) = (
            routine.get("id").and_then(Value::as_str),
            routine.get("trigger").and_then(Value::as_u64),
            def.get("id").and_then(Value::as_str),
        ) else {
            continue;
        };
        let (Ok(trigger), Some(i)) = (
            crate::schedules::trigger_of(def),
            usize::try_from(n).ok().and_then(|n| n.checked_sub(1)),
        ) else {
            continue;
        };
        // The host keeps the trigger it has: an edit in `jobs.json` is not written back, so its slot would be one the
        // host's trigger never has. No host trigger at that index (fewer than the jobs): nothing to write.
        let key = (id.to_owned(), i);
        let Some(theirs) = host.get(&key) else {
            continue;
        };
        let edited = *theirs != trigger;
        if edited {
            said.push(format!(
                "{agent}/{job}: its trigger was edited in jobs.json; the edit is dropped, the host keeps its own trigger"
            ));
        }
        let anchored = anchor::of(def).filter(|_| !edited);
        if let Some(next) = next_of(conn, agent, job, anchored, theirs, wm, since)? {
            out.insert(key, next);
        }
    }
    Ok((out, said))
}

fn last_timed(conn: &Connection, agent: &str, job: &str) -> Result<Option<i64>> {
    let slot: Option<String> = conn
        .query_row(
            "SELECT slot FROM sched_fires WHERE agent = ?1 AND job = ?2
               AND slot NOT LIKE 'manual:%' AND slot NOT LIKE 'event:%' ORDER BY slot DESC LIMIT 1",
            params![agent, job],
            |r| r.get(0),
        )
        .optional()?;
    Ok(slot.and_then(|s| ms_of_slot(&s)))
}

/// The H4 rule for one job; `anchored` is the host's own next run, none when the trigger was edited. `None`: nothing to write. `Some(None)`: no next run.
fn next_of(
    conn: &Connection,
    agent: &str,
    job: &str,
    anchored: Option<i64>,
    trigger: &Trigger,
    wm: Option<i64>,
    since: i64,
) -> Result<Option<NextRun>> {
    if matches!(trigger, Trigger::Webhook | Trigger::Event { .. }) {
        return Ok(None);
    }
    // 1. The slot of the turn this rollback cancelled (the earliest: the host fires it, then moves on).
    let cancelled: Option<String> = conn
        .query_row(
            "SELECT slot FROM sched_fires WHERE agent = ?1 AND job = ?2 AND outcome = ?3 AND at >= ?4
               AND slot NOT LIKE 'manual:%' AND slot NOT LIKE 'event:%' ORDER BY slot LIMIT 1",
            params![agent, job, ROLLBACK_OUTCOME, since],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(ms) = cancelled.as_deref().and_then(ms_of_slot) {
        return Ok(Some(Some(ms)));
    }
    let last = last_timed(conn, agent, job)?;
    // 4. A `once` the core decided is done; one it never reached keeps its time.
    if let Trigger::Once { at } = trigger {
        if last.is_some() {
            return Ok(Some(None));
        }
        return Ok(Some(Some(anchored.unwrap_or(*at))));
    }
    // 2. The host's own next run, while no core plan has reached it.
    if let Some(a) = anchored
        && anchor::unused(conn, agent, job, a)?
    {
        return Ok(Some(Some(a)));
    }
    // 3. After what the core decided.
    let base = match trigger {
        Trigger::Interval { .. } => last
            .or(conn
                .query_row(
                    "SELECT first_seen FROM sched_jobs WHERE agent = ?1 AND job = ?2",
                    params![agent, job],
                    |r| r.get::<_, Option<i64>>(0),
                )
                .optional()?
                .flatten()
                .filter(|t| *t != 0))
            .or(wm),
        _ => wm.map(|w| w.max(last.unwrap_or(w))),
    };
    Ok(base.map(|b| trigger.next_after(b)))
}

/// Puts the next runs into the host's routines (`routines.v1`'s JSON); how many routines changed that are not in `counted`
/// (the ones whose flags the caller already counted).
pub(super) fn apply_next(
    state: &mut Value,
    next: &NextRuns,
    counted: &std::collections::BTreeSet<String>,
) -> usize {
    let mut changed = 0;
    let Some(list) = state.get_mut("routines").and_then(Value::as_array_mut) else {
        return 0;
    };
    for r in list {
        let Some(id) = r.get("id").and_then(Value::as_str).map(str::to_owned) else {
            continue;
        };
        let Some(o) = r.as_object_mut() else { continue };
        let mut hit = false;
        for ((rid, i), want) in next {
            if *rid != id {
                continue;
            }
            let runs = o.entry("nextRuns").or_insert_with(|| json!([]));
            let Some(a) = runs.as_array_mut() else {
                continue;
            };
            while a.len() <= *i {
                a.push(Value::Null);
            }
            let v = want.map_or(Value::Null, |t| json!(t));
            if a[*i] != v {
                a[*i] = v;
                hit = true;
            }
        }
        changed += usize::from(hit && !counted.contains(&id));
    }
    changed
}

/// A later step: an anchor that is the slot a rollback cancelled makes that slot's row not count as decided.
pub(super) fn rearm(core: &mut Core, jobs: &[(&str, &str, &Value)]) -> Result<()> {
    let tx = core.claim_tx()?;
    for (agent, job, def) in jobs {
        let Some(slot) = anchor::of(def).and_then(|a| slot_of_ms(anchor::slot_ms(a))) else {
            continue;
        };
        tx.execute(
            "DELETE FROM sched_fires WHERE agent = ?1 AND job = ?2 AND slot = ?3 AND outcome = ?4",
            params![agent, job, slot, ROLLBACK_OUTCOME],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Runs the core started that still go ([`CLAIM_GOING`]: a claim with a process, and its start stamp) on a schedule turn
/// that has not finished. Read from the claim whatever its kind, so the external claims package C makes for cron jobs need no
/// change here. Oldest first.
fn going_in_core(db: &Connection) -> Result<Vec<Going>> {
    let mut st = db.prepare(&format!(
        "SELECT f.agent, f.job, f.slot, c.pid, c.pid_started, COALESCE(c.running_at, c.claimed_at, c.created_at)
         FROM sched_fires f JOIN turns t ON t.id = CAST(f.turn_id AS INTEGER) JOIN claims c ON c.claim_id = t.claim_id
         WHERE f.turn_id IS NOT NULL AND t.state IN ('claimed', 'running') AND {CLAIM_GOING} ORDER BY c.claim_id"
    ))?;
    Ok(st
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })?
        .collect::<std::result::Result<_, _>>()?)
}

/// `state.json` again, over the tombstone: the last tick (the core's watermark) and each job's last run on this Mac,
/// and any other top-level key the original had (read from the copy the tombstone kept, `state.json.pre-cutover-<time>`).
pub(super) fn write_state_back(env: &Env, db: &Connection, mac: &str) -> Result<String> {
    let dir = env.machine.join("sched");
    let file = dir.join("state.json");
    let mut out = Map::new();
    // Keys the original had besides the two this file is rebuilt from (`pruned`).
    let kept = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("state.json.pre-cutover-")
        })
        .max_by_key(std::fs::DirEntry::file_name)
        .and_then(|e| std::fs::read(e.path()).ok())
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    if let Some(Value::Object(o)) = kept {
        for (k, v) in o {
            if k != "tick" && k != "last" {
                out.insert(k, v);
            }
        }
    }
    let wm: Option<i64> = db
        .query_row(
            "SELECT at FROM sched_watermark WHERE mac = ?1",
            [mac],
            |r| r.get(0),
        )
        .optional()?;
    let tick = match wm.and_then(local_minute) {
        Some(t) => Some(t),
        None => db
            .query_row("SELECT tick FROM sched_tick WHERE mac = ?1", [mac], |r| {
                r.get::<_, String>(0)
            })
            .optional()?,
    };
    if let Some(t) = tick {
        out.insert("tick".into(), json!(t));
    }
    let mut last = Map::new();
    let mut st = db.prepare(
        "SELECT agent, job, at, why, pid, log, proc_start FROM sched_runs
         WHERE mac = ?1 AND id IN (SELECT max(id) FROM sched_runs WHERE mac = ?1 GROUP BY agent, job)
         ORDER BY agent, job",
    )?;
    let rows = st.query_map([mac], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<i64>>(4)?,
            r.get::<_, Option<String>>(5)?,
            r.get::<_, Option<String>>(6)?,
        ))
    })?;
    for row in rows {
        let (agent, job, at, why, pid, log, start) = row?;
        if at.is_empty() {
            continue;
        }
        let mut e = Map::new();
        e.insert("at".into(), json!(at));
        if let Some(p) = pid {
            e.insert("pid".into(), json!(p));
        }
        if let Some(s) = start.filter(|s| !s.is_empty()) {
            e.insert("procStart".into(), json!(s));
        }
        if let Some(l) = log {
            e.insert("log".into(), json!(l));
        }
        if let Some(w) = why {
            e.insert("why".into(), json!(w));
        }
        last.insert(format!("{agent}/{job}"), Value::Object(e));
    }
    // A run the core started that still goes wins over an older entry (or none): kith-sched's still-running rule then
    // skips the job instead of starting a second run beside it.
    let mut going = 0;
    for (agent, job, slot, pid, start, started) in going_in_core(db)? {
        let Some(at) = local_minute(started) else {
            continue;
        };
        let key = format!("{agent}/{job}");
        if last
            .get(&key)
            .and_then(|e| e.get("at"))
            .and_then(Value::as_str)
            .is_some_and(|own| own > at.as_str())
        {
            continue;
        }
        let mut e = Map::new();
        e.insert("at".into(), json!(at));
        e.insert("pid".into(), json!(pid));
        if let Some(s) = start.filter(|s| !s.is_empty()) {
            e.insert("procStart".into(), json!(s));
        }
        e.insert(
            "why".into(),
            json!(format!("running in the core (slot {slot})")),
        );
        last.insert(key, Value::Object(e));
        going += 1;
    }
    let n = last.len();
    out.insert("last".into(), Value::Object(last));
    let io = |e: std::io::Error| Error::Io(format!("cutover: schedules: {}: {e}", file.display()));
    std::fs::create_dir_all(&dir).map_err(io)?;
    let tmp = dir.join("state.json.tmp");
    std::fs::write(
        &tmp,
        serde_json::to_string_pretty(&Value::Object(out)).unwrap_or_default(),
    )
    .map_err(io)?;
    std::fs::rename(&tmp, &file).map_err(io)?;
    Ok(format!(
        "schedules: wrote sched/state.json again (the last tick and {n} job(s)' last run, {going} still running in the core)"
    ))
}
