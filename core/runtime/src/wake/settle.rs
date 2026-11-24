//! R4 of the S4 design (`designs/2026-11-21-wake-s4`): a run is settled by its turn. Once the turn of a run is `done`,
//! `failed` or `cancelled`, the tick answers the run's requests from what the turn and its claim say, level-triggered, each
//! tick, with no process of the tick's own to ask. "Final" means the claim's group is gone and the transcript will not grow
//! (R3a: an exit report only sets the claim `ending`; `end_gone` ends it), so the marker read for it is not stale.
//!
//! The order of the rules matters: a request that was delivered is never recorded `withdrawn` (the marker is looked at before
//! the fence), and a run that never reached its session is given back, whatever fenced it, only when nothing was sent.
//!
//! 1. the turn is `done`: answered.
//! 2. redeemed (its claim ran) and the session has no transcript to read (a channel thread, another engine): answered (the
//!    prompt may have gone; it is never sent twice).
//! 3. redeemed, a Claude session, and the run's `[kith wake RUN]` marker is in the transcript read when the turn was seen
//!    final: answered (`recovered`).
//! 4. the conversation is fenced (a stopped or ended ask) or the claim was revoked: withdrawn.
//! 5. redeemed, no marker: given back, a failed try.
//! 6. never redeemed: given back; not a failed try when a terminal or another writer holds the conversation (the claim was
//!    refused as held), a failed try otherwise. After five failed tries the requests end `failed`.
//!
//! A run with no link to a turn (imported from the files, held by a deliverer's pid) keeps the old rule (`queue::recover`).

use rusqlite::{Connection, OptionalExtension as _};

use super::model::{State, Turn};
use super::{queue, turns};
use crate::conversations::claims;
use crate::error::Result;

/// A run's turn and claim, as the tick reads them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Linked {
    pub run: String,
    pub session: String,
    pub turn: i64,
    pub state: String,
    pub conversation: String,
    /// The turn's claim ran (it was redeemed or attached): the prompt may have reached the engine.
    pub redeemed: bool,
    /// Why the turn's claim ended, when it did.
    pub end_reason: Option<String>,
    /// The Claude session whose transcript holds the marker; none for a conversation with no transcript to read.
    pub sid: Option<String>,
}

impl Linked {
    /// The turn is over and will not change: its claim's group is gone.
    pub(crate) fn is_final(&self) -> bool {
        matches!(
            self.state.as_str(),
            "done" | "failed" | "cancelled" | "expired"
        )
    }

    /// Whether the marker is worth reading: the prompt may have gone, the turn did not end `done`, and there is a transcript.
    pub(crate) fn wants_marker(&self) -> bool {
        self.state != "done" && self.redeemed && self.sid.is_some()
    }
}

/// The turn of `t`'s run through the link, when it has one.
pub(crate) fn linked_in(tx: &Connection, session: &str, t: &Turn) -> Result<Option<Linked>> {
    let Some(turn) = turns::linked_turn_in(tx, &t.run)? else {
        return Ok(None);
    };
    let row = tx
        .query_row(
            "SELECT t.state, t.conversation, COALESCE(c.running_at IS NOT NULL, 0), c.end_reason
             FROM turns t LEFT JOIN claims c ON c.claim_id = t.claim_id WHERE t.id = ?1",
            [turn],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, bool>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .optional()?;
    Ok(row.map(|(state, conversation, redeemed, end_reason)| Linked {
        run: t.run.clone(),
        session: session.to_owned(),
        turn,
        state,
        conversation,
        redeemed,
        end_reason,
        sid: t.sid.clone(),
    }))
}

/// Every run of the state that has a turn, and the runs that have none (the old rule's).
pub(crate) fn runs_in(tx: &Connection, st: &State) -> Result<(Vec<Linked>, Vec<(String, Turn)>)> {
    let (mut linked, mut legacy) = (Vec::new(), Vec::new());
    for (session, it) in &st.items {
        let Some(t) = &it.turn else { continue };
        match linked_in(tx, session, t)? {
            Some(l) => linked.push(l),
            None => legacy.push((session.clone(), t.clone())),
        }
    }
    Ok((linked, legacy))
}

/// What R4 says to do with a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    Answered,
    Recovered,
    Withdrawn,
    /// Owed again; `try_` says whether it counts as a failed try.
    Back { try_: bool },
}

/// The facts R4 decides from.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Facts {
    pub done: bool,
    pub redeemed: bool,
    pub transcript: bool,
    pub marker: bool,
    pub fenced: bool,
    pub held: bool,
}

/// The rules above, in their order.
pub(crate) fn decide(f: &Facts) -> Verdict {
    if f.done {
        Verdict::Answered
    } else if f.redeemed && !f.transcript {
        Verdict::Answered
    } else if f.redeemed && f.marker {
        Verdict::Recovered
    } else if f.fenced {
        Verdict::Withdrawn
    } else if f.redeemed {
        Verdict::Back { try_: true }
    } else {
        Verdict::Back { try_: !f.held }
    }
}

/// Settles one run whose turn was seen final: reads what only the transaction can say (the fence, a writer holding the
/// conversation) and writes the verdict into `st`. `marker` is what the transcript said when the turn was seen final.
pub(crate) fn settle_in(
    tx: &Connection,
    st: &mut State,
    now: f64,
    run: &Linked,
    marker: bool,
) -> Result<Verdict> {
    let facts = Facts {
        done: run.state == "done",
        redeemed: run.redeemed,
        transcript: run.sid.is_some(),
        marker,
        fenced: claims::stopped_in(tx, run.turn)?.is_some()
            || run.end_reason.as_deref() == Some("revoked"),
        held: claims::person_writer_live_in(tx, &run.conversation)?,
    };
    let verdict = decide(&facts);
    match verdict {
        Verdict::Answered => {
            queue::commit(st, now, &run.run, "answered");
        }
        Verdict::Recovered => {
            queue::commit(st, now, &run.run, "recovered");
        }
        Verdict::Withdrawn => {
            queue::commit(st, now, &run.run, "withdrawn");
        }
        Verdict::Back { try_ } => {
            queue::give_back(st, now, &run.run, !try_);
        }
    }
    Ok(verdict)
}
