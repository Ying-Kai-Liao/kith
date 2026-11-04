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
//! One claim transaction per principal (`revoke_person` each), then one for the asks (`end_tree`); a rewrite to share one
//! is the claims lane's. A crash after the person's step leaves the bots' in-flight claims unfenced until the retry, and
//! the asks not yet ended; nothing new of the person's can start. Calling this again (with the same bots) finishes it,
//! because every step is idempotent: a second
//! `revoke_person` bumps the epoch again and finds nothing new to cancel, and `end_tree` settles only asks not yet
//! ended. The kills come back from both calls and are re-sent until gone (`claims::end_gone`).

use std::collections::BTreeSet;

use crate::asks::{self, EndReq};
use crate::conversations::claims::{self, Kill};
use crate::db::Core;
use crate::error::Result;

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

/// Revokes `person` in the conversations: their claims and their bots', then their ask tree. Call it after the authority
/// revoke is visible to the may check (see [`claims::revoke_person`]). `removed_bots` are the bots (the whole transitive
/// set) read before an authority removal deleted their rows; pass `&[]` when the rows are still there. Safe to repeat.
pub(crate) fn revoke_everywhere(
    core: &mut Core,
    person: &str,
    removed_bots: &[String],
    now: i64,
) -> Result<Everywhere> {
    let mut out = Everywhere::default();
    out.principals.push(person.to_owned());
    let mut bots: BTreeSet<String> = bots_of(core, person)?.into_iter().collect();
    bots.extend(removed_bots.iter().filter(|b| *b != person).cloned());
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
    Ok(out)
}

#[cfg(test)]
mod tests;
