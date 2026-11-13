//! The room rules of the claims and turns tables (designs/2026-11-14-m3-cutover section 1.5; the rooms-with-people
//! build plan). Schema: migration 82. `claims.rs` calls in here; the rules live here so that file stays the start
//! protocol.
//!
//! A turn that speaks for people in a room carries its **basis**: the real people it speaks for (`turn_people`, with each
//! person's revoke epoch as the caller read it before its may check), the room, the room's generation at the start and
//! its level (`owner` or `room`). Nothing in core.db changes them after the insert.
//!
//! | rule | where |
//! |---|---|
//! | R-a basis fixed once; a `room` turn with no basis is refused | [`enqueue_in`] writes it, [`claim_gate`] refuses a claim without it, a trigger refuses a change |
//! | R-b one epoch per basis person | [`enqueue_in`] (the caller's epochs), [`epoch_moved`] (the claim's fence) |
//! | R-c revoke by basis | `claims::revoke_person`'s turn and claim queries take the basis, `warm::revoke_in` the processes |
//! | R-f the per-message turn budget | [`chain_of`] |
//! | R-g the room conversation key | [`check_target`] |
//!
//! R-d (the D3 start gate on room turns) stays in the host's `start_gate` until wake moves into the core; R-e (a room
//! process is never warm) is a host rule, `warm_procs` is its backstop.

use rusqlite::{OptionalExtension as _, Transaction, params};

use super::claims::{self, NewTurn, REVOKED};
use crate::db::Core;
use crate::error::{Error, Result};

/// The principal of the owner's own turns (`hostclaim` names an empty person this).
pub(crate) const OWNER: &str = "owner";

/// Who a turn speaks for: its own person (`owner`), or the people of a room (`room`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Level {
    Owner,
    Room,
}

impl Level {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Room => "room",
        }
    }
}

/// How a turn takes part in the per-message budget (R-f).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Chain {
    /// No budget: an owner-level 1:1 turn.
    None,
    /// A person's post opens a chain with this many follow-up rounds.
    Root(i64),
    /// A bot's or agent's turn, caused by this turn: it spends one of that turn's rounds.
    Follow(i64),
}

/// What a room turn adds to a [`NewTurn`].
#[derive(Clone, Debug)]
pub(crate) struct Basis {
    /// The real people the turn speaks for, each with `revoke_epoch` as the caller read it before its may check.
    pub people: Vec<(String, i64)>,
    pub room: Option<String>,
    /// The room's membership generation at the start.
    pub room_gen: Option<i64>,
    pub level: Level,
    /// The room's join epoch the turn belongs to: the conversation key of R-g.
    pub join_epoch: Option<i64>,
    /// The principal whose post made this turn: a person, `bot:<id>`, or an agent name.
    pub author: Option<String>,
    pub chain: Chain,
    /// Makes a retried enqueue land on the turn it made, so its budget is spent once.
    pub key: Option<String>,
}

/// The columns [`claims::enqueue_turn_in`] inserts beside the plain ones. The default is an owner-level turn without a
/// budget, which is every turn that existed before migration 82.
#[derive(Debug, Default)]
pub(super) struct Fixed {
    pub level: Option<&'static str>,
    pub room: Option<String>,
    pub start_gen: Option<i64>,
    pub budget_root: Option<i64>,
    pub budget_left: Option<i64>,
    pub author: Option<String>,
    pub key: Option<String>,
}

fn refused(what: &str) -> Error {
    Error::Refused(format!("kith-core: {what}"))
}

/// Queues a room turn: [`claims::enqueue_turn_in`] with the basis, the budget and the conversation key checked in the
/// same transaction. Every refusal rolls the transaction back, so nothing is half written.
pub(crate) fn enqueue(core: &mut Core, t: &NewTurn<'_>, b: &Basis, now: i64) -> Result<i64> {
    let tx = core.claim_tx()?;
    let id = enqueue_in(&tx, t, b, now)?;
    tx.commit()?;
    Ok(id)
}

/// [`enqueue`] inside a transaction the caller holds and commits.
pub(crate) fn enqueue_in(
    tx: &Transaction<'_>,
    t: &NewTurn<'_>,
    b: &Basis,
    now: i64,
) -> Result<i64> {
    if let Some(key) = &b.key {
        // A retry after a lost reply: the turn is already there and its budget already spent.
        let seen: Option<i64> = tx
            .query_row(
                "SELECT id FROM turns WHERE conversation = ?1 AND enq_key = ?2",
                params![t.conversation, key],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = seen {
            return Ok(id);
        }
    }
    if b.level == Level::Room && b.room.is_none() {
        return Err(Error::BadParams(
            "kith-core: a room turn names its room".into(),
        ));
    }
    if b.room.is_some() && b.join_epoch.is_none() {
        return Err(Error::BadParams(
            "kith-core: a turn in a room names its join epoch".into(),
        ));
    }
    // R-a: a room turn speaks for people, so it has some.
    if b.level == Level::Room && b.people.is_empty() {
        return Err(refused("a room turn has no basis: it speaks for nobody"));
    }
    for (p, _) in &b.people {
        if p.is_empty() || p.starts_with("bot:") || p.starts_with("room-") {
            return Err(refused(&format!(
                "{p:?} is not a person: a basis holds people, not bots or rooms"
            )));
        }
    }
    // R-b: each basis person's epoch as the caller read it before its may check.
    for (p, seen) in &b.people {
        if epoch(tx, p)? != *seen {
            return Err(refused(&format!("{p} was revoked after the may check")));
        }
    }
    let (budget_root, budget_left) = chain_of(tx, b)?;
    check_target(tx, t, b, now)?;
    let fixed = Fixed {
        level: Some(b.level.as_str()),
        room: b.room.clone(),
        start_gen: b.room_gen,
        budget_root,
        budget_left,
        author: b.author.clone(),
        key: b.key.clone(),
    };
    let id = claims::enqueue_turn_with_in(tx, t, &fixed, now)?;
    let mut st = tx.prepare(
        "INSERT OR IGNORE INTO turn_people (turn, person, revoke_seen) VALUES (?1, ?2, ?3)",
    )?;
    for (p, seen) in &b.people {
        st.execute(params![id, p, seen])?;
    }
    Ok(id)
}

fn epoch(tx: &Transaction<'_>, person: &str) -> Result<i64> {
    claims::revoke_epoch_in(tx, person)
}

/// R-f: the turn's `(budget_root, budget_left)`. Only a person of the basis opens a chain, with a fresh budget; a bot's
/// or agent's turn names the turn that caused it and spends one of that turn's rounds, refused at none left. A requeue
/// moves the same row, so it is never counted here, and a cancel gives nothing back.
fn chain_of(tx: &Transaction<'_>, b: &Basis) -> Result<(Option<i64>, Option<i64>)> {
    match b.chain {
        Chain::None => {
            if b.level == Level::Room {
                return Err(refused(
                    "a room turn has no turn budget: it opens a chain (a person's post) or follows a turn",
                ));
            }
            Ok((None, None))
        }
        Chain::Root(budget) => {
            let author = b.author.as_deref().unwrap_or_default();
            if !b.people.iter().any(|(p, _)| p == author) {
                return Err(refused(&format!(
                    "{author:?} is not a person of the basis: only a person's post opens a turn budget"
                )));
            }
            if budget < 0 {
                return Err(Error::BadParams(
                    "kith-core: a turn budget is not negative".into(),
                ));
            }
            Ok((None, Some(budget)))
        }
        Chain::Follow(from) => {
            if b.author.as_deref().unwrap_or_default().is_empty() {
                return Err(Error::BadParams(
                    "kith-core: a follow-up turn names its author".into(),
                ));
            }
            let parent: Option<(Option<i64>, Option<i64>)> = tx
                .query_row(
                    "SELECT budget_root, budget_left FROM turns WHERE id = ?1",
                    [from],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let Some((root, left)) = parent else {
                return Err(refused(&format!(
                    "turn {from} does not exist: nothing to follow"
                )));
            };
            let Some(left) = left else {
                return Err(refused(&format!(
                    "turn {from} has no turn budget to follow"
                )));
            };
            if left <= 0 {
                return Err(refused(&format!(
                    "the turn budget of the chain of turn {from} is spent"
                )));
            }
            Ok((Some(root.unwrap_or(from)), Some(left - 1)))
        }
    }
}

/// R-g: the conversation a turn goes to. A room conversation is the session of one (agent, room, join epoch). A room
/// turn goes to the conversation of its own epoch; a conversation of an older epoch takes the owner's turns only.
fn check_target(tx: &Transaction<'_>, t: &NewTurn<'_>, b: &Basis, now: i64) -> Result<()> {
    type Target = (String, String, Option<String>, Option<i64>);
    let existing: Option<Target> = tx
        .query_row(
            "SELECT agent, engine, room, join_epoch FROM conversations WHERE id = ?1",
            [t.conversation],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((_, _, room, epoch)) = existing else {
        let (Some(room), Some(join)) = (&b.room, b.join_epoch) else {
            return Ok(());
        };
        create_room_conversation(tx, t.conversation, t.agent, t.engine, room, join, now)?;
        return Ok(());
    };
    let Some(room) = room else {
        if b.room.is_some() {
            return Err(refused(&format!(
                "conversation {} is not a room conversation",
                t.conversation
            )));
        }
        return Ok(());
    };
    if b.level == Level::Room {
        if b.room.as_deref() != Some(room.as_str()) || b.join_epoch != epoch {
            return Err(refused(&format!(
                "conversation {} is room {room} at join epoch {}: a room turn goes to the conversation of its own epoch",
                t.conversation,
                epoch.map_or_else(|| "none".into(), |e| e.to_string()),
            )));
        }
    } else if t.person != OWNER {
        return Err(refused(&format!(
            "conversation {} is a room conversation: only the owner's turns go to it outside a room turn (older epochs are the owner's alone)",
            t.conversation
        )));
    }
    Ok(())
}

/// R-g: the row of a room conversation, made when it is missing. A second row for the same (agent, room, join epoch) under
/// another id is refused, so there is never a second session for one epoch.
fn create_room_conversation(
    tx: &Transaction<'_>,
    id: &str,
    agent: &str,
    engine: &str,
    room: &str,
    join_epoch: i64,
    now: i64,
) -> Result<()> {
    let holder: Option<String> = tx
        .query_row(
            "SELECT id FROM conversations WHERE agent = ?1 AND room = ?2 AND join_epoch = ?3",
            params![agent, room, join_epoch],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(other) = holder {
        return Err(refused(&format!(
            "room {room} already has conversation {other} for {agent} at join epoch {join_epoch}; a second one is refused"
        )));
    }
    tx.execute(
        "INSERT INTO conversations (id, agent, engine, room, join_epoch, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
        params![id, agent, engine, room, join_epoch, now],
    )?;
    Ok(())
}

/// `hostclaim.ensure` with a room: finds the conversation of (agent, room, join epoch) or makes it. An id that already
/// names a conversation of another room or epoch is refused.
pub(crate) fn ensure_room(
    core: &mut Core,
    id: &str,
    agent: &str,
    engine: &str,
    room: &str,
    join_epoch: i64,
    now: i64,
) -> Result<()> {
    let tx = core.claim_tx()?;
    let existing: Option<(Option<String>, Option<i64>)> = tx
        .query_row(
            "SELECT room, join_epoch FROM conversations WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match existing {
        None => create_room_conversation(&tx, id, agent, engine, room, join_epoch, now)?,
        Some((Some(r), Some(e))) if r == room && e == join_epoch => {}
        Some(_) => {
            return Err(refused(&format!(
                "conversation {id} is not the conversation of room {room} at join epoch {join_epoch}"
            )));
        }
    }
    tx.commit()?;
    Ok(())
}

/// Whether a basis person of this turn was revoked since the turn was queued (R-b at the claim: the fence).
pub(super) fn epoch_moved(tx: &Transaction<'_>, turn: i64) -> Result<bool> {
    let moved: Option<i64> = tx
        .query_row(
            "SELECT 1 FROM turn_people tp
             WHERE tp.turn = ?1
               AND tp.revoke_seen <> COALESCE((SELECT CAST(value AS INTEGER) FROM meta WHERE key = ?2 || tp.person), 0)
             LIMIT 1",
            params![turn, REVOKED],
            |r| r.get(0),
        )
        .optional()?;
    Ok(moved.is_some())
}

/// R-a at the claim: a `room` turn with no basis is never claimed.
pub(super) fn claim_gate(tx: &Transaction<'_>, turn: i64) -> Result<()> {
    let bare: Option<i64> = tx
        .query_row(
            "SELECT 1 FROM turns t
             WHERE t.id = ?1 AND t.level = 'room' AND NOT EXISTS (SELECT 1 FROM turn_people WHERE turn = t.id)",
            [turn],
            |r| r.get(0),
        )
        .optional()?;
    if bare.is_some() {
        return Err(refused(&format!("room turn {turn} has no basis")));
    }
    Ok(())
}
