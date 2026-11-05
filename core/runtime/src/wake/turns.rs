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

/// Creates the turn for the run `turn` (the JSON `queue::take` answered) and parks it when a writer holds the session.
/// `sid` is the session the caller resumes, when it said (the conversation otherwise).
pub(crate) fn start_in(
    tx: &Transaction<'_>,
    out: &dyn Outside,
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
        "INSERT OR IGNORE INTO wake_turns (run, session, turn_id) VALUES (?1, ?2, ?3)",
        rusqlite::params![run, session, id],
    )?;
    // The claim files are the writers until the switch (the Python's `writer_live`); the claim rows are what the core knows.
    let writer = sid.filter(|s| !s.is_empty()).unwrap_or(session);
    if out.writer_live(writer) || claims::person_writer_live_in(tx, session)? {
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
