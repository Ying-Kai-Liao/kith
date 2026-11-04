//! The revoke order (M3-10d): everything a revoked person's conversations hold, stopped in ONE order, as one function.
//!
//! `kith-auth person --revoke` (the bridge, once M2-18 moves the authority writes) makes the revoke visible to the may
//! check first (`people.suspend`, M2's); this is the second half, the one the conversations need:
//!
//! 1. [`claims::revoke_person`] for the person and for each of their bots. A bot's runs hold the bot principal in
//!    `claims.person`, so the person's own call misses them. Each call bumps that principal's revoke epoch, so a claim
//!    that passed its may check before the revoke is refused by the fence in `claim()`, and cancels the principal's
//!    queued and parked turns and sends their live claims to `ending`.
//! 2. [`asks::end_tree`] for the same principals: their asks, and every ask below those, settled `stopped`, the children's
//!    conversations stopped, the wakes the callers are owed withdrawn.
//!
//! Claims first means a claim that passed its may check just before the revoke is refused by the epoch fence instead of
//! starting on a conversation the stop has just emptied. The verification model (`verification/revoke`) pins the order
//! structurally: `OrderKept` is the same condition as the `AskSweep` guard, so `broken-asks-first` fails because the guard
//! is gone, not because a harm property catches an in-flight claim (a structural pin, not a behaviour proof);
//! `broken-bots-skipped` does fail on a harm property. `ask.end` alone is not a revoke: it never touches a claim of a turn
//! that is not an ask's.
//!
//! The bots: on a removal (`person --rm`) the authority write deletes the person's and their bots' `people` rows, so a
//! walk of `people.bot_of` run afterwards finds nothing. The caller therefore passes the bots it read BEFORE the write
//! (`removed_bots`, the whole transitive set); the table walk still runs and is unioned in, which is what a suspend (rows
//! kept) relies on.
//!
//! The record (M2-18c): the removal write also stores that set in `pending_revokes`, in its own transaction, so a crash
//! between it and this call loses nothing; the order unions it in (a retry needs no `--bots`) and deletes it last.
//! The names passed in `--bots` are checked first: the owner, or a name whose `bot_of` chain (current or recorded)
//! does not lead to the person, is refused before anything is written. A removal from before the record falls back to
//! the bots still in `people.bot_of`, and a name that is in neither is refused.
//!
//! One claim transaction per principal (`revoke_person` each), then one for the asks (`end_tree`); a rewrite to share one
//! is the claims lane's. A crash after the person's step leaves the bots' in-flight claims unfenced until the retry, and
//! the asks not yet ended; nothing new of the person's can start. Calling this again (with the same bots) finishes it,
//! because every step is idempotent: a second
//! `revoke_person` bumps the epoch again and finds nothing new to cancel, and `end_tree` settles only asks not yet
//! ended. The kills come back from both calls and are re-sent until gone (`claims::end_gone`).

use std::collections::BTreeSet;

use rusqlite::OptionalExtension;

use crate::asks::{self, EndReq};
use crate::conversations::claims::{self, Kill};
use crate::db::Core;
use crate::error::{Error, Result};

/// What [`revoke_everywhere`] did.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Everywhere {
    /// The person and their bots, in the order they were revoked.
    pub principals: Vec<String>,
    pub cancelled_turns: usize,
    /// The asks settled `stopped` by this call.
    pub ended: Vec<String>,
    /// The sessions whose wakes and waiters were withdrawn.
    pub sessions: Vec<String>,
    /// Writers to kill, to be re-sent until gone.
    pub kill: Vec<Kill>,
}

/// The person's bots, from the authority tables (`people.bot_of`), and the bots of those, in id order. Reads only.
fn bots_of(core: &Core, person: &str) -> Result<Vec<String>> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut frontier = vec![person.to_owned()];
    let mut st = core
        .conn()
        .prepare("SELECT id FROM people WHERE bot_of = ?1 ORDER BY id")?;
    while let Some(owner) = frontier.pop() {
        let ids: Vec<String> = st
            .query_map([&owner], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        for id in ids {
            if id != person && seen.insert(id.clone()) {
                frontier.push(id);
            }
        }
    }
    Ok(seen.into_iter().collect())
}

/// The bots a removal of `person` recorded (see `pending_revokes`), empty when it recorded none (a removal from before
/// the record, or a suspend, whose rows are still in `people`).
fn recorded_bots(conn: &rusqlite::Connection, person: &str) -> Result<Vec<String>> {
    let json: Option<String> = conn
        .query_row(
            "SELECT bots FROM pending_revokes WHERE person = ?1",
            [person],
            |r| r.get(0),
        )
        .optional()?;
    Ok(json
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default())
}

/// Called by the authority removal write, in its transaction: keeps the removed person's transitive bot set until
/// [`revoke_everywhere`] has run, because the write drops the bots' `people` rows. Merges with a row already there
/// (a repeat removal), and records nothing for a person with no bots.
pub(crate) fn record_pending(
    conn: &rusqlite::Connection,
    person: &str,
    bots: &[String],
    now_ms: f64,
) -> Result<()> {
    if bots.is_empty() {
        return Ok(());
    }
    let mut all: BTreeSet<String> = recorded_bots(conn, person)?.into_iter().collect();
    all.extend(bots.iter().cloned());
    let all: Vec<String> = all.into_iter().collect();
    #[allow(clippy::cast_possible_truncation)]
    let at = now_ms as i64;
    conn.execute(
        "INSERT OR REPLACE INTO pending_revokes (person, bots, at) VALUES (?1, ?2, ?3)",
        rusqlite::params![person, serde_json::to_string(&all).unwrap_or_default(), at],
    )?;
    Ok(())
}

fn is_owner(conn: &rusqlite::Connection, id: &str) -> Result<bool> {
    Ok(conn
        .query_row("SELECT owner FROM people WHERE id = ?1", [id], |r| {
            r.get::<_, i64>(0)
        })
        .optional()?
        == Some(1))
}

/// Revokes `person` in the conversations: their claims and their bots', then their ask tree. Call it after the authority
/// revoke is visible to the may check (see [`claims::revoke_person`]). `removed_bots` are the bots (the whole transitive
/// set) read before an authority removal deleted their rows; pass `&[]` when the rows are still there. Safe to repeat.
pub(crate) fn revoke_everywhere(
    core: &mut Core,
    person: &str,
    removed_bots: &[String],
    now: i64,
) -> Result<Everywhere> {
    // The names the caller passes are checked before anything is written: only a bot that chains to the person (in
    // `people` now, or recorded by their removal) may be revoked with them. A forged name would otherwise end an
    // unrelated bot's asks, or the owner's.
    let mut bots: BTreeSet<String> = bots_of(core, person)?.into_iter().collect();
    bots.extend(recorded_bots(core.conn(), person)?);
    if is_owner(core.conn(), person)? {
        return Err(Error::Refused(format!(
            "kith-core: revoke: {person} is the owner: the owner can not be revoked"
        )));
    }
    for b in removed_bots.iter().filter(|b| *b != person) {
        if is_owner(core.conn(), b)? {
            return Err(Error::Refused(format!(
                "kith-core: revoke: --bots names the owner ({b}): the owner is nobody's bot"
            )));
        }
        if !bots.contains(b) {
            return Err(Error::Refused(format!(
                "kith-core: revoke: {b} is not a bot of {person} (its bot_of chain does not lead to them)"
            )));
        }
    }
    let mut out = Everywhere::default();
    out.principals.push(person.to_owned());
    out.principals.extend(bots);
    for p in out.principals.clone() {
        let r = claims::revoke_person(core, &p, now)?;
        out.cancelled_turns += r.cancelled_turns;
        out.kill.extend(r.kill);
    }
    let ended = asks::end_tree(
        core,
        &EndReq {
            roots: &[],
            sessions: &[],
            persons: &out.principals,
        },
        now,
    )?;
    out.ended = ended.ids;
    out.sessions = ended.sessions;
    for k in ended.kill {
        if !out.kill.contains(&k) {
            out.kill.push(k);
        }
    }
    // Last, so a crash before here finds the record again; nothing is left to lose once the order has run.
    core.conn()
        .execute("DELETE FROM pending_revokes WHERE person = ?1", [person])?;
    Ok(out)
}

#[cfg(test)]
mod tests;
