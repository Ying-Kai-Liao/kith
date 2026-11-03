//! The resource gate in the core (M2-9, lane D): at most `max` turns hold a slot at once, the rest wait in order.
//!
//! Same rules as the host's gate (`serve/host/src/agent/gate/mod.rs`, which still decides until lane H switches its
//! callers), over core.db instead of memory:
//!
//! - A slot is a turn in flight, not a warm process. A holder (a run or claim id) [`take`]s one before it starts and
//!   [`release`]s it when it ends. A take that finds no room joins the line and answers `waiting`; the caller asks again
//!   (a take of the same holder is idempotent) or watches [`list`] to see it granted.
//! - Waiting turns are served in the order they began waiting. The owner's go ahead of invited people's, and invited
//!   people together hold at most [`INVITEE_SLOTS`] of the active slots.
//! - A turn that waits on another bot's reply [`park`]s: its slot is given up so that bot can take it. [`unpark`] puts
//!   it back in line AHEAD of every turn that has not started (it is part way through). Asks that run side by side
//!   share one parking: the first gives the slot up, the last takes it back.
//! - Releasing a holder that holds nothing does nothing, and a double take or double release is harmless.
//!
//! What differs from the host, on purpose:
//! - The rows survive a daemon restart, because a holder is a process that may outlive the daemon. A holder that gave
//!   its pid is checked on every call (as `core.holder` does): a pid that is gone, or now has another start time, frees
//!   its slot or its place in line.
//! - The memory check stays out: it reads the Mac, not core.db, and moves with the host's reclaim task. The caller
//!   passes `max`; the core never evicts, so a lowered `max` just stops new grants until enough holders release.
//! - A cancelled park (the host's `Parked` dropped before it got its slot back) re-counts the turn at once there. Here
//!   the caller releases or unparks; the core cannot see a cancelled future.
//!
//! Every state change is one `BEGIN IMMEDIATE` transaction through [`Core::claim_tx`], so two threads of this process
//! (the daemon serves calls on several) never see half a change; a second daemon cannot exist (the lifetime lock).

use rusqlite::{OptionalExtension as _, Transaction, params};
use serde_json::{Value, json};

use crate::db::Core;
use crate::error::{Error, Result};

/// How many slots invited people's turns may hold together.
pub(crate) const INVITEE_SLOTS: i64 = 1;

const MAX_KEY: &str = "max_running";

/// The start time of a pid, as `core.holder` records it. A parameter so tests can end a "process" without killing one.
pub(crate) type StartOf<'a> = &'a dyn Fn(u32) -> Option<String>;

/// One request for a slot.
pub(crate) struct Take<'a> {
    pub holder: &'a str,
    /// The actor the turn belongs to (the host's `actor_key`); display only.
    pub rt: &'a str,
    /// An invited person's turn: behind the owner's, and counted against [`INVITEE_SLOTS`].
    pub invitee: bool,
    /// How many slots there are. Remembered for the later calls that make room.
    pub max: i64,
    /// The holder's process, so a crash frees the slot. `None`: nothing to check, only `release` frees it.
    pub pid: Option<u32>,
}

/// Where a holder stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum State {
    /// Holds a counted slot.
    Granted,
    /// Gave its slot up while it waits on another bot; no longer in the line.
    Parked,
    /// In the line; `position` counts from 1 in the order the line is kept.
    Waiting { position: usize },
    /// Neither holds nor waits.
    Gone,
}

impl State {
    fn json(&self) -> Value {
        match self {
            Self::Granted => json!({"state": "granted"}),
            Self::Parked => json!({"state": "parked"}),
            Self::Waiting { position } => json!({"state": "waiting", "position": position}),
            Self::Gone => json!({"state": "gone"}),
        }
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

fn pid_sql(pid: Option<u32>) -> Option<i64> {
    pid.map(i64::from)
}

fn set_max(tx: &Transaction<'_>, max: i64) -> Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO gate_config (key, value) VALUES (?1, ?2)",
        params![MAX_KEY, max],
    )?;
    Ok(())
}

fn stored_max(tx: &Transaction<'_>) -> Result<Option<i64>> {
    Ok(tx
        .query_row(
            "SELECT value FROM gate_config WHERE key = ?1",
            [MAX_KEY],
            |r| r.get(0),
        )
        .optional()?)
}

/// Frees the slots and places in line whose process is gone or is another process now. A holder with no recorded start
/// time is never judged: with nothing to compare, a live-looking pid proves nothing (the same stance as `core.holder`).
fn prune(tx: &Transaction<'_>, start_of: StartOf<'_>) -> Result<()> {
    let mut dead: Vec<String> = Vec::new();
    for table in ["gate_slots", "gate_waiters"] {
        let mut st = tx.prepare(&format!(
            "SELECT holder, pid, started FROM {table} WHERE pid IS NOT NULL AND started <> ''"
        ))?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (holder, pid, started) = row?;
            let alive = u32::try_from(pid)
                .ok()
                .and_then(start_of)
                .is_some_and(|current| current == started);
            if !alive {
                dead.push(holder);
            }
        }
    }
    for holder in dead {
        tx.execute("DELETE FROM gate_slots WHERE holder = ?1", [&holder])?;
        tx.execute("DELETE FROM gate_waiters WHERE holder = ?1", [&holder])?;
    }
    // A resume in line whose slot is gone has nothing to come back to.
    tx.execute(
        "DELETE FROM gate_waiters WHERE kind = 'resume' AND holder NOT IN (SELECT holder FROM gate_slots)",
        [],
    )?;
    Ok(())
}

struct Waiter {
    ord: i64,
    holder: String,
    rt: String,
    kind: String,
    invitee: bool,
    pid: Option<i64>,
    started: String,
}

fn waiters(tx: &Transaction<'_>) -> Result<Vec<Waiter>> {
    let mut st = tx.prepare(
        "SELECT ord, holder, rt, kind, invitee, pid, started FROM gate_waiters ORDER BY ord",
    )?;
    let rows = st.query_map([], |r| {
        Ok(Waiter {
            ord: r.get(0)?,
            holder: r.get(1)?,
            rt: r.get(2)?,
            kind: r.get(3)?,
            invitee: r.get::<_, i64>(4)? != 0,
            pid: r.get(5)?,
            started: r.get(6)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

fn count(tx: &Transaction<'_>, sql: &str) -> Result<i64> {
    Ok(tx.query_row(sql, [], |r| r.get(0))?)
}

/// Grants waiters from the front for as long as there is room. Who is next: the first waiter that is not an invited
/// person's new turn, else the first of those; anyone who needs an invited person's slot waits while those are taken.
fn pump(tx: &Transaction<'_>) -> Result<()> {
    let Some(max) = stored_max(tx)? else {
        return Ok(());
    };
    loop {
        let line = waiters(tx)?;
        if line.is_empty() {
            return Ok(());
        }
        if count(tx, "SELECT COUNT(*) FROM gate_slots WHERE state = 'active'")? >= max {
            return Ok(());
        }
        let invitees = count(
            tx,
            "SELECT COUNT(*) FROM gate_slots WHERE state = 'active' AND invitee = 1",
        )?;
        let invitee_open = invitees < INVITEE_SLOTS;
        let may = |w: &Waiter| invitee_open || !w.invitee;
        let low = |w: &Waiter| w.invitee && w.kind == "turn";
        let next = line
            .iter()
            .find(|w| !low(w) && may(w))
            .or_else(|| line.iter().find(|w| low(w) && may(w)));
        let Some(w) = next else { return Ok(()) };
        tx.execute("DELETE FROM gate_waiters WHERE ord = ?1", [w.ord])?;
        if w.kind == "resume" {
            tx.execute(
                "UPDATE gate_slots SET state = 'active' WHERE holder = ?1",
                [&w.holder],
            )?;
        } else {
            tx.execute(
                "INSERT INTO gate_slots (holder, rt, invitee, state, asks, pid, started, taken_at)
                 VALUES (?1, ?2, ?3, 'active', 0, ?4, ?5, ?6)",
                params![
                    w.holder,
                    w.rt,
                    i64::from(w.invitee),
                    w.pid,
                    w.started,
                    now()
                ],
            )?;
        }
    }
}

fn state_of(tx: &Transaction<'_>, holder: &str) -> Result<State> {
    let slot: Option<String> = tx
        .query_row(
            "SELECT state FROM gate_slots WHERE holder = ?1",
            [holder],
            |r| r.get(0),
        )
        .optional()?;
    let waiting = waiters(tx)?.iter().position(|w| w.holder == holder);
    Ok(match (slot.as_deref(), waiting) {
        // A parked turn in line to come back is waiting, like any other.
        (_, Some(i)) => State::Waiting { position: i + 1 },
        (Some("active"), None) => State::Granted,
        (Some(_), None) => State::Parked,
        (None, None) => State::Gone,
    })
}

/// Asks for a slot. Idempotent: a holder that already holds one or waits is answered where it stands, and nothing is
/// queued twice.
pub(crate) fn take(core: &mut Core, t: &Take<'_>, start_of: StartOf<'_>) -> Result<State> {
    let tx = core.claim_tx()?;
    prune(&tx, start_of)?;
    set_max(&tx, t.max)?;
    if state_of(&tx, t.holder)? == State::Gone {
        let started = t.pid.and_then(start_of).unwrap_or_default();
        // The back of the line: one past the largest, or the first.
        let ord = count(&tx, "SELECT COALESCE(MAX(ord), 0) + 1 FROM gate_waiters")?;
        tx.execute(
            "INSERT INTO gate_waiters (ord, holder, rt, kind, invitee, pid, started)
             VALUES (?1, ?2, ?3, 'turn', ?4, ?5, ?6)",
            params![
                ord,
                t.holder,
                t.rt,
                i64::from(t.invitee),
                pid_sql(t.pid),
                started
            ],
        )?;
    }
    pump(&tx)?;
    let state = state_of(&tx, t.holder)?;
    tx.commit()?;
    Ok(state)
}

/// Gives a slot (or a place in line) back. False when the holder held nothing.
pub(crate) fn release(core: &mut Core, holder: &str, start_of: StartOf<'_>) -> Result<bool> {
    let tx = core.claim_tx()?;
    prune(&tx, start_of)?;
    let slots = tx.execute("DELETE FROM gate_slots WHERE holder = ?1", [holder])?;
    let line = tx.execute("DELETE FROM gate_waiters WHERE holder = ?1", [holder])?;
    pump(&tx)?;
    tx.commit()?;
    Ok(slots + line > 0)
}

/// The turn waits on another bot's reply: its slot is given up. Nested asks share one parking. False when the holder
/// holds no slot (nothing is parked then).
pub(crate) fn park(core: &mut Core, holder: &str, start_of: StartOf<'_>) -> Result<bool> {
    let tx = core.claim_tx()?;
    prune(&tx, start_of)?;
    let changed = tx.execute(
        "UPDATE gate_slots SET asks = asks + 1, state = 'parked' WHERE holder = ?1",
        [holder],
    )?;
    pump(&tx)?;
    tx.commit()?;
    Ok(changed > 0)
}

/// One ask is back. The last one puts the turn in line again, ahead of turns that have not started. Unparking a holder
/// with no ask out (a double unpark, an unknown holder) changes nothing.
pub(crate) fn unpark(core: &mut Core, holder: &str, start_of: StartOf<'_>) -> Result<State> {
    let tx = core.claim_tx()?;
    prune(&tx, start_of)?;
    let asks: Option<i64> = tx
        .query_row(
            "SELECT asks FROM gate_slots WHERE holder = ?1",
            [holder],
            |r| r.get(0),
        )
        .optional()?;
    if asks.is_some_and(|a| a > 0) {
        tx.execute(
            "UPDATE gate_slots SET asks = asks - 1 WHERE holder = ?1",
            [holder],
        )?;
        // Only a slot that is still given up needs to come back: one that a resume already re-counted holds it.
        let parked: bool = tx.query_row(
            "SELECT state = 'parked' FROM gate_slots WHERE holder = ?1",
            [holder],
            |r| r.get(0),
        )?;
        if asks == Some(1) && parked {
            let (rt, invitee, pid, started): (String, i64, Option<i64>, String) = tx.query_row(
                "SELECT rt, invitee, pid, started FROM gate_slots WHERE holder = ?1",
                [holder],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
            let front = count(&tx, "SELECT COALESCE(MIN(ord), 1) - 1 FROM gate_waiters")?;
            tx.execute(
                "INSERT OR IGNORE INTO gate_waiters (ord, holder, rt, kind, invitee, pid, started)
                 VALUES (?1, ?2, ?3, 'resume', ?4, ?5, ?6)",
                params![front, holder, rt, invitee, pid, started],
            )?;
        }
    }
    pump(&tx)?;
    let state = state_of(&tx, holder)?;
    tx.commit()?;
    Ok(state)
}

/// Everything the gate holds, in the order the line is kept. Frees dead holders first, so it never shows one.
pub(crate) fn list(core: &mut Core, start_of: StartOf<'_>) -> Result<Value> {
    let tx = core.claim_tx()?;
    prune(&tx, start_of)?;
    let max = stored_max(&tx)?;
    let mut slots = Vec::new();
    {
        let mut st = tx.prepare(
            "SELECT holder, rt, invitee, state, asks, pid FROM gate_slots ORDER BY taken_at, holder",
        )?;
        let rows = st.query_map([], |r| {
            Ok(json!({
                "holder": r.get::<_, String>(0)?,
                "rt": r.get::<_, String>(1)?,
                "invitee": r.get::<_, i64>(2)? != 0,
                "state": r.get::<_, String>(3)?,
                "asks": r.get::<_, i64>(4)?,
                "pid": r.get::<_, Option<i64>>(5)?,
            }))
        })?;
        for row in rows {
            slots.push(row?);
        }
    }
    let line: Vec<Value> = waiters(&tx)?
        .iter()
        .enumerate()
        .map(|(i, w)| {
            json!({
                "holder": w.holder, "rt": w.rt, "kind": w.kind, "invitee": w.invitee,
                "position": i + 1,
            })
        })
        .collect();
    let running = count(
        &tx,
        "SELECT COUNT(*) FROM gate_slots WHERE state = 'active'",
    )?;
    tx.commit()?;
    Ok(json!({"max": max, "running": running, "slots": slots, "waiting": line}))
}

fn text<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params.get(key).and_then(Value::as_str)
}

fn holder(params: &Value) -> Result<&str> {
    text(params, "holder")
        .filter(|h| !h.is_empty())
        .ok_or_else(|| Error::BadParams("kith-core: gate needs a holder".into()))
}

/// The `gate.*` rows of the table. The params were checked against the row already.
pub(crate) fn call(
    core: &mut Core,
    method: &str,
    params: &Value,
    start_of: StartOf<'_>,
) -> Result<Value> {
    match method {
        "gate.list" => list(core, start_of),
        "gate.take" => {
            let h = holder(params)?;
            let max = params
                .get("max")
                .and_then(Value::as_i64)
                .filter(|m| *m >= 0)
                .ok_or_else(|| {
                    Error::BadParams("kith-core: gate.take needs max, 0 or more".into())
                })?;
            let pid = match params.get("pid").and_then(Value::as_i64) {
                None => None,
                Some(p) => Some(u32::try_from(p).map_err(|_| {
                    Error::BadParams("kith-core: gate.take: pid is not a process id".into())
                })?),
            };
            let t = Take {
                holder: h,
                rt: text(params, "rt").unwrap_or(h),
                invitee: params
                    .get("invitee")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                max,
                pid,
            };
            Ok(take(core, &t, start_of)?.json())
        }
        "gate.release" => Ok(json!({"released": release(core, holder(params)?, start_of)?})),
        "gate.park" => Ok(json!({"parked": park(core, holder(params)?, start_of)?})),
        "gate.unpark" => Ok(unpark(core, holder(params)?, start_of)?.json()),
        _ => Err(Error::NotServed(format!(
            "kith-core: {method} is not answered in the core yet"
        ))),
    }
}

#[cfg(test)]
mod tests;
