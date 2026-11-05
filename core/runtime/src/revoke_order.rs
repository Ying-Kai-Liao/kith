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
//! The record (M2-18c): every removal also writes a row in `open_removals` (the bot set, possibly empty), in the same
//! transaction: "removed, order not finished". The core runs this order right after the removal commits; the order
//! unions the row in (a retry needs no `--bots`) and deletes it last, after its effect. A row left by a crash or an
//! error is run again at core start and on the supervisor's pass ([`replay_open`]). While a row names an id,
//! `people.addPerson` and `people.addBot` refuse it ([`open_removal_naming`]), and the order refuses (never skips) a
//! recorded name that is in `people` and does not chain to the person. A row that can not be read stops the order.
//! The names passed in `--bots` are checked first: the owner, or a name in `people` whose `bot_of` chain does not lead to
//! the person, is refused before anything is written; a name with no row and no record is ignored. A removal from before
//! the record falls back to the bots still in `people.bot_of`.
//!
//! A suspend (`people.suspend`) writes the same kind of row, with no bots (its rows stay in `people`), and the core runs
//! the order after the commit the same way. `people.undo` before the order has run does not touch the row: [`run_open`]
//! drops the row of someone who is in `people`, no longer suspended and has no recorded bots, without acting. Cores that
//! suspended people before this record existed are not swept retroactively.
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

/// The bots a removal of `person` recorded (see `open_removals`), empty when it recorded none (a removal from before
/// the record, or a suspend, whose rows are still in `people`). A row that is not a JSON array of ids is an error that
/// names the row: reading it as "no bots" would lose the very set the row exists to keep.
fn recorded_bots(conn: &rusqlite::Connection, person: &str) -> Result<Vec<String>> {
    let json: Option<String> = conn
        .query_row(
            "SELECT bots FROM open_removals WHERE person = ?1",
            [person],
            |r| r.get(0),
        )
        .optional()?;
    json.map_or(Ok(Vec::new()), |j| parse_bots(person, &j))
}

fn parse_bots(person: &str, json: &str) -> Result<Vec<String>> {
    serde_json::from_str(json).map_err(|e| {
        Error::Refused(format!(
            "kith-core: revoke: the open_removals row of {person} holds bots that are not a JSON list of ids ({e}); not guessing what it named"
        ))
    })
}

/// Whose open removal names `id`, as the person or as one of their bots. `people.addPerson` and `people.addBot` refuse
/// such an id until the order has run, so a recorded name can not vouch for someone who joined again.
pub(crate) fn open_removal_naming(conn: &rusqlite::Connection, id: &str) -> Result<Option<String>> {
    let mut st = conn.prepare("SELECT person, bots FROM open_removals ORDER BY person")?;
    let rows: Vec<(String, String)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<_, _>>()?;
    for (person, bots) in rows {
        if person == id || parse_bots(&person, &bots)?.iter().any(|b| b == id) {
            return Ok(Some(person));
        }
    }
    Ok(None)
}

/// Called by the authority removal write, in its transaction: keeps the removed person's transitive bot set (possibly
/// empty) until [`revoke_everywhere`] has run, because the write drops the bots' `people` rows. A row means "removed,
/// order not finished"; merges with a row already there (a repeat removal).
pub(crate) fn record_removal(
    conn: &rusqlite::Connection,
    person: &str,
    bots: &[String],
    now_ms: f64,
) -> Result<()> {
    let mut all: BTreeSet<String> = recorded_bots(conn, person)?.into_iter().collect();
    all.extend(bots.iter().cloned());
    let all: Vec<String> = all.into_iter().collect();
    #[allow(clippy::cast_possible_truncation)]
    let at = now_ms as i64;
    let json = serde_json::to_string(&all).map_err(|e| {
        Error::Refused(format!(
            "kith-core: revoke: could not record the bots of {person}: {e}"
        ))
    })?;
    conn.execute(
        "INSERT OR REPLACE INTO open_removals (person, bots, at) VALUES (?1, ?2, ?3)",
        rusqlite::params![person, json, at],
    )?;
    Ok(())
}

/// Called by the authority suspend write, in its transaction: "suspended, order not finished". The rows stay in `people`,
/// so the record names no bots (the table walk finds them). Merges with a row already there (a repeat suspend, or a
/// suspend that follows an earlier removal's).
pub(crate) fn record_suspend(conn: &rusqlite::Connection, person: &str, now_ms: f64) -> Result<()> {
    record_removal(conn, person, &[], now_ms)
}

/// Runs the order for one open record. A record that names no bots, for someone who is in `people` and no longer
/// suspended (`people.undo` ran before the order did), is dropped without acting: the sweep must not hit someone who has
/// their access back. (A removal's record never meets this: the write deleted the row, and an add of the id is refused
/// while the record is open.) Everything else is [`revoke_everywhere`].
pub(crate) fn run_open(core: &mut Core, person: &str, now: i64) -> Result<Everywhere> {
    let unrevoked = core
        .conn()
        .query_row("SELECT revoked FROM people WHERE id = ?1", [person], |r| {
            r.get::<_, i64>(0)
        })
        .optional()?
        == Some(0);
    // The owner can not be revoked, so a row for them (an old file that marks the owner revoked) would replay forever:
    // drop it, with a line in the log and no error.
    if is_owner(core.conn(), person)? {
        eprintln!(
            "kith-core: revoke: dropping the open record of {person}: the owner can not be revoked"
        );
        core.conn()
            .execute("DELETE FROM open_removals WHERE person = ?1", [person])?;
        return Ok(Everywhere::default());
    }
    if unrevoked && recorded_bots(core.conn(), person)?.is_empty() {
        core.conn()
            .execute("DELETE FROM open_removals WHERE person = ?1", [person])?;
        return Ok(Everywhere::default());
    }
    revoke_everywhere(core, person, &[], now)
}

/// Runs the order again for every open removal, at core start and on the supervisor's pass. One row failing does not
/// stop the others; each failure comes back as a line naming the row. The kills are for the caller to send.
pub(crate) fn replay_open(core: &mut Core, now: i64) -> Result<(Vec<Kill>, Vec<String>)> {
    let persons: Vec<String> = {
        let mut st = core
            .conn()
            .prepare("SELECT person FROM open_removals ORDER BY at, person")?;
        st.query_map([], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?
    };
    let (mut kill, mut failed) = (Vec::new(), Vec::new());
    for p in persons {
        match run_open(core, &p, now) {
            Ok(out) => kill.extend(out.kill),
            Err(e) => failed.push(format!("open removal of {p}: {e}")),
        }
    }
    Ok((kill, failed))
}

fn core_has_row(conn: &rusqlite::Connection, id: &str) -> Result<bool> {
    Ok(conn
        .query_row("SELECT 1 FROM people WHERE id = ?1", [id], |_| Ok(()))
        .optional()?
        .is_some())
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
    // unrelated bot's asks, or the owner's. A name with no row and no record is nobody's: it is ignored, so a retry of
    // a call that finished is not an error.
    let mut bots: BTreeSet<String> = bots_of(core, person)?.into_iter().collect();
    let recorded = recorded_bots(core.conn(), person)?;
    if is_owner(core.conn(), person)? {
        return Err(Error::Refused(format!(
            "kith-core: revoke: {person} is the owner: the owner can not be revoked"
        )));
    }
    // A recorded name that has a current row but does not chain to the person has joined again: the add refuses that
    // while the record is open, so only a broken invariant gets here. Refuse, never skip.
    for b in &recorded {
        if core_has_row(core.conn(), b)? && !bots.contains(b) {
            return Err(Error::Refused(format!(
                "kith-core: revoke: the open_removals row of {person} names {b}, which is in people and not a bot of {person}; not revoking it"
            )));
        }
    }
    bots.extend(recorded);
    for b in removed_bots.iter().filter(|b| *b != person) {
        if is_owner(core.conn(), b)? {
            return Err(Error::Refused(format!(
                "kith-core: revoke: --bots names the owner ({b}): the owner is nobody's bot"
            )));
        }
        if !bots.contains(b) && core_has_row(core.conn(), b)? {
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
    // Last, so a crash before here finds the record again (the replay); nothing is left to lose once the order has run.
    core.conn()
        .execute("DELETE FROM open_removals WHERE person = ?1", [person])?;
    Ok(out)
}

#[cfg(test)]
mod tests;
