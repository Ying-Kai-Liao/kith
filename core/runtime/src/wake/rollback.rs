//! What a rollback of the `wake` module reads and changes in core.db before the files are written (WK6 brief section 2). Only
//! the `cutover/modules/wake_back.rs` export calls it, with the daemon down and the core's lifetime lock held.
//!
//! - [`refusals`]: what is still true after adopt, `end_gone` and the settle pass, and makes the rollback refuse (nothing is
//!   written yet; `--force` does not get past it): a wake turn `claimed` with no process, a host-door run whose turn has not
//!   settled, a session with two runs that are not waiting, and a running or `ending` run-door run whose claim lacks a pid or
//!   its start stamp. A rollback cannot hand any of them to Python.
//! - [`hand_back`]: in one claim transaction, every run whose turn still waits (`queued` or `parked`) has its turn cancelled and
//!   its requests owed again, with no failed try (`tries` stays). The state to write is what is left; a run whose turn is
//!   `running` or `ending` is written as the item's `turn` with its claim's pid and start stamp, so Python's `recover` leaves
//!   it alone while that process lives and while the core claim on its conversation is not ended.

use std::collections::BTreeMap;

use rusqlite::{Connection, params};

use super::model::{State, Turn};
use super::{load, queue, save};
use crate::conversations::claims;
use crate::error::{Error, Result};

/// A run that has a turn, and what the turn and its claim say.
struct Row {
    session: String,
    run: String,
    turn: i64,
    turn_state: String,
    conversation: String,
    door: String,
    claim_state: Option<String>,
    pid: Option<i64>,
    started: Option<String>,
}

fn rows(c: &Connection) -> Result<Vec<Row>> {
    let mut st = c.prepare(
        "SELECT s.session, s.turn_run, w.turn_id, t.state, t.conversation, s.door, k.state, k.pid, k.pid_started
         FROM wake_sessions s
           JOIN wake_turns w ON w.run = s.turn_run
           JOIN turns t ON t.id = w.turn_id
           LEFT JOIN claims k ON k.claim_id = t.claim_id
         ORDER BY s.session",
    )?;
    let rows = st.query_map([], |r| {
        Ok(Row {
            session: r.get(0)?,
            run: r.get(1)?,
            turn: r.get(2)?,
            turn_state: r.get(3)?,
            conversation: r.get(4)?,
            door: r.get(5)?,
            claim_state: r.get(6)?,
            pid: r.get(7)?,
            started: r.get(8)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// The claim still holds the conversation: its process may be running, or its group not gone yet.
fn claim_live(r: &Row) -> bool {
    matches!(
        r.claim_state.as_deref(),
        Some("claimed" | "running" | "ending")
    )
}

fn waiting(r: &Row) -> bool {
    matches!(r.turn_state.as_str(), "queued" | "parked")
}

/// Why a rollback cannot go on, one line each (empty: it can). Read-only.
pub(crate) fn refusals(c: &Connection) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut st = c.prepare(
        "SELECT t.id, t.conversation FROM turns t LEFT JOIN claims k ON k.claim_id = t.claim_id
         WHERE t.source = 'wake' AND t.state = 'claimed' AND k.pid IS NULL ORDER BY t.id",
    )?;
    for row in st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
        let (turn, conversation) = row?;
        out.push(format!(
            "(a) wake turn {turn} on {conversation} is claimed and no process has attached yet"
        ));
    }
    let mut st = c.prepare(
        "SELECT t.conversation, count(*) FROM turns t WHERE t.source = 'wake' AND t.state IN ('claimed', 'running')
         GROUP BY t.conversation HAVING count(*) > 1 ORDER BY t.conversation",
    )?;
    for row in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
        let (conversation, n) = row?;
        out.push(format!(
            "(c) {conversation} has {n} wake runs that are not waiting"
        ));
    }
    for r in rows(c)? {
        let started = matches!(r.turn_state.as_str(), "claimed" | "running") || claim_live(&r);
        if r.door == "host" && started {
            out.push(format!(
                "(b) the host-door wake run {} of {} has not settled (its turn is {})",
                r.run, r.session, r.turn_state
            ));
        }
        let going = matches!(r.claim_state.as_deref(), Some("running" | "ending"));
        if r.door == "run" && going && (r.pid.is_none() || r.started.is_none()) {
            out.push(format!(
                "(d) the run {} of {} is {} with no process recorded on its claim (pid and start stamp)",
                r.run,
                r.session,
                r.claim_state.as_deref().unwrap_or_default()
            ));
        }
    }
    Ok(out)
}

/// What [`hand_back`] leaves to write.
pub(crate) struct Back {
    /// The queue as the files must say it: waiting runs owed again, a running run kept as the item's `turn`.
    pub state: State,
    /// How many runs were handed back as owed, and how many carry a process.
    pub owed: usize,
    pub running: usize,
}

/// See the module's doc. `now` is milliseconds.
pub(crate) fn hand_back(core: &mut crate::db::Core, now: i64) -> Result<Back> {
    let tx = core.claim_tx()?;
    let before = load(&tx)?;
    let mut after = before.clone();
    let runs = rows(&tx)?;
    let waits: Vec<i64> = runs.iter().filter(|r| waiting(r)).map(|r| r.turn).collect();
    let cancelled = claims::cancel_waiting_in(&tx, &waits, "rollback", now)?;
    if cancelled.len() != waits.len() {
        return Err(Error::Refused(format!(
            "cutover: wake: {} waiting wake turn(s) could not be cancelled; nothing was written back (run the rollback again)",
            waits.len() - cancelled.len()
        )));
    }
    let (mut owed, mut running) = (0, 0);
    let mut flying: BTreeMap<String, Turn> = BTreeMap::new();
    for r in &runs {
        if waiting(r) {
            queue::unrun(&mut after, &r.session);
            tx.execute("DELETE FROM wake_turns WHERE run = ?1", params![r.run])?;
            owed += 1;
            continue;
        }
        let at = after
            .items
            .get(&r.session)
            .and_then(|it| it.turn.as_ref())
            .map(|t| t.at.clone())
            .unwrap_or_default();
        let live = claim_live(r) && r.pid.is_some();
        flying.insert(
            r.session.clone(),
            Turn {
                run: r.run.clone(),
                pid: if live { r.pid } else { None },
                proc_start: if live { r.started.clone() } else { None },
                at,
                sid: Some(r.conversation.clone()),
            },
        );
        running += 1;
    }
    for (session, turn) in flying {
        if let Some(it) = after.items.get_mut(&session) {
            it.turn = Some(turn);
        }
    }
    save(&tx, &before, &after)?;
    tx.commit()?;
    Ok(Back {
        state: after,
        owed,
        running,
    })
}
