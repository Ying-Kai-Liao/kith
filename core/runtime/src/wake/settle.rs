//! R4 of the S4 design (`designs/2026-11-21-wake-s4`): a run is settled by its turn. Once the turn of a run is `done`,
//! `failed` or `cancelled`, the tick answers the run's requests from what the turn and its claim say, level-triggered, each
//! tick, with no process of the tick's own to ask. "Final" means the claim's group is gone and the transcript will not grow
//! (R3a: an exit report only sets the claim `ending`; `end_gone` ends it), so the marker read for it is not stale.
//!
//! The order of the rules matters: a request that was delivered is never recorded `withdrawn` (the marker is looked at before
//! the fence), and a run that never reached its session is given back, whatever fenced it, only when nothing was sent.
//!
//! 1. the turn is `done`: answered.
//! 2. redeemed (any claim of the turn ever ran: the rule of `redeemedBefore`) and the session has no transcript to read (a channel thread, another engine): answered (the
//!    prompt may have gone; it is never sent twice).
//! 3. redeemed, a Claude session, and the run's `[kith wake RUN]` marker is in the transcript read when the turn was seen
//!    final: answered (`recovered`).
//! 4. the conversation is fenced (a stopped or ended ask) or the person was revoked since the turn was queued (their revoke epoch moved): withdrawn.
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
    /// Any claim of the turn ran (it was redeemed or attached), as `redeemedBefore` says; `turns.claim_id` is cleared by a requeue: the prompt may have reached the engine.
    pub redeemed: bool,
    /// The state of the turn's claim, when it still has one: a turn cancelled by a stop or a revoke keeps a claim that is
    /// `ending` while its engine runs on.
    pub claim_state: Option<String>,
    /// The turn's person, and their revoke epoch when the turn was queued (none for a link older than the column).
    pub person: String,
    pub revoke_seen: Option<i64>,
    /// The Claude session whose transcript holds the marker; none for a conversation with no transcript to read.
    pub sid: Option<String>,
}

impl Linked {
    /// The turn is over and will not change: its state is final and its claim, if it has one, is `ended` (its group is
    /// gone, so the transcript will not grow). A cancelled turn whose claim is `ending` is still being delivered.
    pub(crate) fn is_final(&self) -> bool {
        matches!(
            self.state.as_str(),
            "done" | "failed" | "cancelled" | "expired"
        ) && self.claim_state.as_deref().is_none_or(|s| s == "ended")
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
    // A link whose turn row is gone is a broken record, not an unlinked run (which would take the old rule).
    let row = tx
        .query_row(
            "SELECT t.state, t.conversation, t.person, c.state, w.revoke_seen,
                    EXISTS (SELECT 1 FROM claims k WHERE k.turn = t.id AND k.running_at IS NOT NULL)
             FROM wake_turns w JOIN turns t ON t.id = w.turn_id
             LEFT JOIN claims c ON c.claim_id = t.claim_id WHERE w.run = ?1",
            [&t.run],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<i64>>(4)?,
                    r.get::<_, bool>(5)?,
                ))
            },
        )
        .optional()?;
    let Some((state, conversation, person, claim_state, revoke_seen, redeemed)) = row else {
        return Err(crate::error::Error::Refused(format!(
            "wake: run {} is linked to turn {turn}, which is not in the conversations lane",
            t.run
        )));
    };
    Ok(Some(Linked {
        run: t.run.clone(),
        session: session.to_owned(),
        turn,
        state,
        conversation,
        redeemed,
        claim_state,
        person,
        revoke_seen,
        sid: t.sid.clone(),
    }))
}

/// The runs that have a turn, and those that have none (the old rule's).
pub(crate) type Runs = (Vec<Linked>, Vec<(String, Turn)>);

/// Every run of the state, told by whether it has a turn.
pub(crate) fn runs_in(tx: &Connection, st: &State) -> Result<Runs> {
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
    Back {
        try_: bool,
    },
}

/// The facts R4 decides from.
#[allow(
    clippy::struct_excessive_bools,
    reason = "the rules' own facts, each a yes or no"
)]
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
pub(crate) fn decide(f: Facts) -> Verdict {
    if f.done || (f.redeemed && !f.transcript) {
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
        // The person was revoked since the turn was queued: `revoke_person` cancels their waiting turns with no claim, so the
        // claim cannot say it.
        fenced: claims::stopped_in(tx, run.turn)?.is_some()
            || match run.revoke_seen {
                Some(seen) => claims::revoke_epoch_in(tx, &run.person)? != seen,
                None => false,
            },
        held: claims::person_writer_live_in(tx, &run.conversation)?,
    };
    let verdict = decide(facts);
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
