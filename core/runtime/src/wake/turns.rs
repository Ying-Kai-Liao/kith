//! A wake's turn in the conversations lane (M3-10c, runtime-design section 4).
//!
//! `take` marks what a session is owed as carried by a run (queue.rs); here the same step writes the run's turn through the
//! claims API (`source = 'wake'`, the prompt is the `[kith wake RUN]` note). The turn is found again through the
//! `wake_turns` link (run to turn), so a take that is replayed makes no second turn for the run. A session a terminal (or another person's writer) holds gets the
//! turn parked as `terminal_live`: it waits for the session to end and is never typed into the terminal (q217), and
//! `claims::unpark_ready` queues it once the terminal's claim has ended.
//!
//! The wake record stays the record of what is owed: when a run leaves it (done, given back, recovered, withdrawn) the
//! turn that was waiting for it is closed in the same transaction, so no waiting turn outlives its run. A turn that already
//! has a claim is the claim's to finish. Nothing runs the turns until the switch (I14): the Python host still delivers.
//!
//! A Stop reaches a run that has not started (R5, `stop_unstarted_in`): its turn is cancelled and the run given back.

use rusqlite::{OptionalExtension as _, Transaction};
use serde_json::Value;

use super::model::State;
use super::outside::Outside;
use crate::conversations::claims::{self, NewTurn};
use crate::error::Result;

/// `turns.source` of a turn the wake queue made.
const SOURCE: &str = "wake";
/// The engine of a wake turn for a conversation core.db has not seen yet (a host-door session is a Claude session).
const DEFAULT_ENGINE: &str = "claude";

#[allow(
    clippy::cast_possible_truncation,
    reason = "epoch seconds times a thousand fit an i64"
)]
fn ms(now: f64) -> i64 {
    (now * 1000.0) as i64
}

/// The agent core.db names for a conversation, when it has one.
pub(crate) fn agent_of_in(tx: &Transaction<'_>, session: &str) -> Result<Option<String>> {
    Ok(tx
        .query_row(
            "SELECT agent FROM conversations WHERE id = ?1",
            [session],
            |r| r.get(0),
        )
        .optional()?)
}

/// A conversation has one agent: an owed host-door item for a session core.db gives to another agent gets no turn. Its
/// requests that no run carries end as `failed` (the answer `kith-wake status` gives), the same way a waiter whose session
/// belongs to someone else is dropped, so the take goes on to the next item instead of stopping every unnamed take behind it.
pub(crate) fn fail_foreign_in(tx: &Transaction<'_>, st: &mut State, now: f64) -> Result<()> {
    for s in st.order() {
        let Some(it) = st.items.get(&s) else { continue };
        if it.door != "host" || it.turn.is_some() {
            continue;
        }
        match agent_of_in(tx, &s)? {
            Some(agent) if agent != it.agent => {}
            _ => continue,
        }
        let Some(it) = st.items.get_mut(&s) else {
            continue;
        };
        let ids: Vec<String> = it
            .requests
            .iter()
            .filter(|r| r.run.is_none())
            .map(|r| r.id.clone())
            .collect();
        it.requests.retain(|r| r.run.is_some());
        st.mark_answered(&ids, "failed", None, now);
        st.settle(&s);
    }
    Ok(())
}

/// Creates the turn for the run `turn` (the JSON `queue::take` answered) and parks it when a writer holds the session.
/// `sid` is the session the caller resumes, when it said (the conversation otherwise). The claim files count as writers
/// too (`wake.take`, until the switch).
pub(crate) fn start_in(
    tx: &Transaction<'_>,
    out: &dyn Outside,
    turn: &Value,
    sid: Option<&str>,
    now: f64,
) -> Result<i64> {
    start_with_in(tx, Some(out), turn, sid, now)
}

/// [`start_in`] for the tick: the claim rows are the only writers it asks (the `claims` module is switched), so it reads no
/// file.
pub(crate) fn start_claims_in(
    tx: &Transaction<'_>,
    turn: &Value,
    sid: Option<&str>,
    now: f64,
) -> Result<i64> {
    start_with_in(tx, None, turn, sid, now)
}

fn start_with_in(
    tx: &Transaction<'_>,
    files: Option<&dyn Outside>,
    turn: &Value,
    sid: Option<&str>,
    now: f64,
) -> Result<i64> {
    let text = |k: &str| turn.get(k).and_then(Value::as_str);
    let (Some(run), Some(session), Some(agent)) = (text("run"), text("session"), text("agent"))
    else {
        return Err(crate::error::Error::Refused(
            "wake: a turn without a run, session and agent cannot be queued".into(),
        ));
    };
    let person = text("person").filter(|p| !p.is_empty()).unwrap_or("owner");
    let at = ms(now);
    // A conversation core.db already has keeps its engine.
    let engine: String = tx
        .query_row(
            "SELECT engine FROM conversations WHERE id = ?1",
            [session],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or_else(|| DEFAULT_ENGINE.to_owned());
    // The run's turn is the one `wake_turns` links: a replayed take finds it instead of making a second.
    let linked: Option<i64> = tx
        .query_row(
            "SELECT turn_id FROM wake_turns WHERE run = ?1",
            [run],
            |r| r.get(0),
        )
        .optional()?;
    let id = match linked {
        Some(id) => id,
        None => claims::enqueue_turn_in(
            tx,
            &NewTurn {
                conversation: session,
                agent,
                engine: &engine,
                source: SOURCE,
                person,
                prompt: text("note").unwrap_or_default(),
                deadline: None,
                revoke_seen: claims::revoke_epoch_in(tx, person)?,
            },
            at,
        )?,
    };
    tx.execute(
        "INSERT OR IGNORE INTO wake_turns (run, session, turn_id, revoke_seen) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![run, session, id, claims::revoke_epoch_in(tx, person)?],
    )?;
    // The claim files are the writers until the switch (the Python's `writer_live`); the claim rows are what the core knows.
    let writer = sid.filter(|s| !s.is_empty()).unwrap_or(session);
    if files.is_some_and(|out| out.writer_live(writer))
        || claims::person_writer_live_in(tx, session)?
    {
        claims::park_in(tx, id, "terminal_live", at)?;
    }
    Ok(id)
}

/// Closes the turns of the runs that left the queue between `before` and `after`: `done` when the run's requests were
/// answered, `cancelled` when they were given back, failed or withdrawn (a next take makes a new run and a new turn).
pub(crate) fn close_gone_in(
    tx: &rusqlite::Connection,
    before: &State,
    after: &State,
    now: f64,
) -> Result<()> {
    for (session, it) in &before.items {
        let Some(t) = &it.turn else { continue };
        let carried = after
            .items
            .get(session)
            .and_then(|i| i.turn.as_ref())
            .is_some_and(|n| n.run == t.run);
        if carried {
            continue;
        }
        let delivered = after.answered.values().any(|a| {
            a.run.as_deref() == Some(&t.run) && !matches!(a.how.as_str(), "withdrawn" | "failed")
        });
        // Through the link only; a run with no link (taken before the table) has no turn to close.
        let linked: Option<i64> = tx
            .query_row(
                "SELECT turn_id FROM wake_turns WHERE run = ?1",
                [&t.run],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(turn) = linked {
            claims::end_waiting_turn_in(tx, turn, delivered, ms(now))?;
            tx.execute("DELETE FROM wake_turns WHERE run = ?1", [&t.run])?;
        }
    }
    Ok(())
}

/// The turn that carries `run`, through the link.
pub(crate) fn linked_turn_in(tx: &rusqlite::Connection, run: &str) -> Result<Option<i64>> {
    Ok(tx
        .query_row(
            "SELECT turn_id FROM wake_turns WHERE run = ?1",
            [run],
            |r| r.get(0),
        )
        .optional()?)
}

/// R5: a Stop that reaches a run whose turn has not started (`queued`, `parked`, or `claimed` with no process) cancels the
/// turn and gives the run back, in the Stop's transaction; the Stop then withdraws what it names (`queue::withdraw`) and
/// the rest stays owed, not a failed try. `id` is the one request the Stop names (none: all of them); a Stop that names
/// another request leaves the run alone. A running turn is not touched: its prompt was delivered, so the requests stay
/// with it. A run with no link (imported, with a deliverer's pid) is the old rule's.
pub(crate) fn stop_unstarted_in(
    tx: &rusqlite::Connection,
    st: &mut State,
    now: f64,
    session: &str,
    id: Option<&str>,
) -> Result<()> {
    let Some(it) = st.items.get(session) else {
        return Ok(());
    };
    let Some(run) = it.turn.as_ref().map(|t| t.run.clone()) else {
        return Ok(());
    };
    let named = id.is_none_or(|i| {
        it.requests
            .iter()
            .any(|r| r.run.as_deref() == Some(&run) && r.id == i)
    });
    if !named {
        return Ok(());
    }
    let Some(turn) = linked_turn_in(tx, &run)? else {
        return Ok(());
    };
    // A turn that is already cancelled and never ran (a revoke cancelled it, or the claim was refused) is as unstarted as a
    // waiting one: the run goes back all the same.
    if claims::cancel_waiting_in(tx, &[turn], "stopped", ms(now))?.is_empty()
        && !cancelled_unran_in(tx, turn)?
    {
        return Ok(());
    }
    super::queue::unrun(st, session);
    Ok(())
}

/// The turn is `cancelled` or `expired`, and no claim of it ever ran or still holds the conversation.
fn cancelled_unran_in(tx: &rusqlite::Connection, turn: i64) -> Result<bool> {
    Ok(tx.query_row(
        "SELECT state IN ('cancelled', 'expired') AND NOT EXISTS
                (SELECT 1 FROM claims k WHERE k.turn = turns.id
                   AND (k.running_at IS NOT NULL OR k.state IN ('claimed', 'running', 'ending')))
         FROM turns WHERE id = ?1",
        [turn],
        |r| r.get(0),
    )?)
}

/// The runs that have a turn.
pub(crate) fn linked_runs_in(
    tx: &rusqlite::Connection,
) -> Result<std::collections::BTreeSet<String>> {
    let mut st = tx.prepare("SELECT run FROM wake_turns")?;
    let rows = st.query_map([], |r| r.get(0))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// A Stop of `session` (`wake.withdraw`): [`stop_unstarted_in`], then the withdrawal. Answers how many went.
pub(crate) fn withdraw_in(
    tx: &rusqlite::Connection,
    st: &mut State,
    now: f64,
    session: &str,
    id: Option<&str>,
) -> Result<usize> {
    stop_unstarted_in(tx, st, now, session, id)?;
    Ok(super::queue::withdraw(st, now, session, id))
}

/// A revoke or removal (`wake.withdrawPerson`): a Stop of every session of these people.
pub(crate) fn withdraw_person_in(
    tx: &rusqlite::Connection,
    st: &mut State,
    now: f64,
    persons: &[String],
) -> Result<usize> {
    for s in super::queue::sessions_of(st, persons) {
        stop_unstarted_in(tx, st, now, &s, None)?;
    }
    Ok(super::queue::withdraw_person(st, now, persons))
}
