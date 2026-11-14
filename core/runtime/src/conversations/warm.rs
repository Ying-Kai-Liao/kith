//! The host's warm engine processes in the core (B4) and the host's own instance (B5), designs/2026-11-14-m3-cutover
//! sections 1.3 and 1.5.
//!
//! - **`warm_procs`** (migrations 82, 83): one row per engine process the host keeps and conversation it has loaded (a
//!   process with none has one row with `''`), idle or not. An idle guest's process has no claim, so without a row a revoke could not find it. [`register`] upserts a
//!   row, [`gone`] deletes it, [`revoke_in`] (called by `claims::revoke_person`) marks the rows a revoke reaches `kill_at`
//!   and answers their kills, [`settle`] deletes a marked row once its process is confirmed gone. The daemon's kill
//!   re-send signals the marked rows until then.
//! - **One instance per host start** (`kith-serve-host/<pid>-<start stamp>`): [`adopt`] is the first thing a host does
//!   at its start. It ends the claims and marks the processes of every other host instance, so a claim of a host that is
//!   gone is not read as "ours" and a process that outlived it is killed rather than forgotten.

use rusqlite::{OptionalExtension as _, Transaction, params};

use super::claims::{self, Kill, is_host_instance};
use crate::db::Core;
use crate::error::{Error, Result};
use crate::gate::Liveness;

/// A warm process as the host reports it.
#[derive(Clone, Debug)]
pub(crate) struct WarmProc<'a> {
    pub pid: u32,
    pub started: &'a str,
    pub instance: &'a str,
    /// Whose process it is (a guest's principal); empty for the owner's.
    pub person: &'a str,
    pub agent: &'a str,
    /// The session it has loaded now.
    pub conversation: Option<&'a str>,
    /// [`claims::revoke_epoch`] of `person` read before the process was started: refused if a revoke committed since, so
    /// a process is never registered past a revoke that already swept.
    pub revoke_seen: Option<i64>,
}

fn kill(pid: i64, started: String) -> Kill {
    Kill {
        claim_id: None,
        pid: u32::try_from(pid).ok(),
        started: Some(started),
    }
}

/// Registers a process, or adds a session it has loaded (one row per conversation; [`gone`] with a conversation takes one
/// away). A process already marked for a kill marks its new row too.
pub(crate) fn register(core: &mut Core, w: &WarmProc<'_>) -> Result<()> {
    if !is_host_instance(w.instance) {
        return Err(Error::BadParams(format!(
            "kith-core: {:?} is not a host instance",
            w.instance
        )));
    }
    // A guest's process is registered with the epoch read before the access check, or a revoke that committed between
    // the two would have swept already and never marked the new row.
    if !w.person.is_empty() && w.person != super::basis::OWNER && w.revoke_seen.is_none() {
        return Err(Error::BadParams(format!(
            "kith-core: a process of {} is registered with revokeSeen",
            w.person
        )));
    }
    let tx = core.claim_tx()?;
    let moved = match w.revoke_seen {
        Some(seen) => claims::revoke_epoch_in(&tx, w.person)? != seen,
        None => false,
    };
    if moved {
        return Err(Error::Refused(format!(
            "kith-core: {} was revoked after the may check",
            w.person
        )));
    }
    let conversation = w.conversation.unwrap_or("");
    if conversation.is_empty() {
        // No session to name: the process keeps the rows it has, and gets one placeholder row only when it has none.
        let n = tx.execute(
            "UPDATE warm_procs SET instance = ?3, person = ?4, agent = ?5 WHERE pid = ?1 AND started = ?2",
            params![w.pid, w.started, w.instance, w.person, w.agent],
        )?;
        if n == 0 {
            insert_row(&tx, w, "")?;
        }
    } else {
        insert_row(&tx, w, conversation)?;
        tx.execute(
            "DELETE FROM warm_procs WHERE pid = ?1 AND started = ?2 AND conversation = ''",
            params![w.pid, w.started],
        )?;
    }
    tx.commit()?;
    Ok(())
}

fn insert_row(tx: &Transaction<'_>, w: &WarmProc<'_>, conversation: &str) -> Result<()> {
    // The placeholder row may carry the kill mark; so may the siblings. The new row takes it.
    tx.execute(
        "INSERT INTO warm_procs (pid, started, instance, person, agent, conversation, kill_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, (SELECT MAX(kill_at) FROM warm_procs WHERE pid = ?1 AND started = ?2))
         ON CONFLICT (pid, started, conversation) DO UPDATE SET
            instance = excluded.instance, person = excluded.person, agent = excluded.agent",
        params![w.pid, w.started, w.instance, w.person, w.agent, conversation],
    )?;
    Ok(())
}

/// The process is gone (the host saw it exit), or with `conversation` it no longer has that session loaded: forgets the
/// row(s), unless a kill is owed for the process. The engine in the stub's group can outlive the stub the host saw exit,
/// so a marked process leaves only through [`settle`], which looks at the group. Forgetting the last session leaves the
/// process known with none. True when something was forgotten.
pub(crate) fn gone(
    core: &mut Core,
    pid: u32,
    started: &str,
    conversation: Option<&str>,
) -> Result<bool> {
    let tx = core.claim_tx()?;
    let Some(conversation) = conversation else {
        let n = tx.execute(
            "DELETE FROM warm_procs WHERE pid = ?1 AND started = ?2 AND kill_at IS NULL",
            params![pid, started],
        )?;
        tx.commit()?;
        return Ok(n > 0);
    };
    let row: Option<(String, String, String)> = tx
        .query_row(
            "SELECT instance, person, agent FROM warm_procs
             WHERE pid = ?1 AND started = ?2 AND conversation = ?3 AND kill_at IS NULL",
            params![pid, started, conversation],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((instance, person, agent)) = row else {
        return Ok(false);
    };
    tx.execute(
        "DELETE FROM warm_procs WHERE pid = ?1 AND started = ?2 AND conversation = ?3",
        params![pid, started, conversation],
    )?;
    let left: i64 = tx.query_row(
        "SELECT count(*) FROM warm_procs WHERE pid = ?1 AND started = ?2",
        params![pid, started],
        |r| r.get(0),
    )?;
    if left == 0 {
        tx.execute(
            "INSERT INTO warm_procs (pid, started, instance, person, agent, conversation) VALUES (?1, ?2, ?3, ?4, ?5, '')",
            params![pid, started, instance, person, agent],
        )?;
    }
    tx.commit()?;
    Ok(true)
}

/// R-c: the processes a revoke of `person` reaches, marked for a kill and answered. A process is the person's own
/// (`person`), or has loaded a conversation that has had a turn with the person in its basis and is not now running
/// someone else's turn: idle after the turn's claim ended, or still running the person's. Call it before the revoke
/// cancels their turns. Repeating it answers the same kills.
pub(super) fn revoke_in(tx: &Transaction<'_>, person: &str, now: i64) -> Result<Vec<Kill>> {
    let hit: Vec<(i64, String)> = {
        let mut st = tx.prepare(
            "SELECT DISTINCT pid, started FROM warm_procs
             WHERE person = ?1
                OR (conversation IN (
                        SELECT t.conversation FROM turns t
                        WHERE t.id IN (SELECT turn FROM turn_people WHERE person = ?1))
                    AND NOT EXISTS (
                        SELECT 1 FROM claims c
                        WHERE c.conversation = warm_procs.conversation
                          AND c.state IN ('claimed', 'running', 'ending')
                          AND (c.turn IS NULL OR c.turn NOT IN (SELECT turn FROM turn_people WHERE person = ?1))))",
        )?;
        st.query_map([person], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<_, _>>()?
    };
    let mut out = Vec::new();
    for (pid, started) in hit {
        tx.execute(
            "UPDATE warm_procs SET kill_at = COALESCE(kill_at, ?3) WHERE pid = ?1 AND started = ?2",
            params![pid, started, now],
        )?;
        out.push(kill(pid, started));
    }
    Ok(out)
}

/// Adds the process kills to a claim kill list, once per process: a claim's stub is often the very process a warm row
/// names.
pub(super) fn merge_kills(into: &mut Vec<Kill>, more: Vec<Kill>) {
    for k in more {
        if !into.iter().any(|c| c.pid == k.pid) {
            into.push(k);
        }
    }
}

/// Forgets the marked rows whose process the system confirms gone. Probes outside the transaction, as `end_gone` does;
/// the delete names the same process. Returns how many rows went.
pub(crate) fn settle(core: &mut Core, live: &dyn Liveness) -> Result<usize> {
    let marked: Vec<(i64, String)> = {
        let mut st = core
            .conn()
            .prepare("SELECT DISTINCT pid, started FROM warm_procs WHERE kill_at IS NOT NULL")?;
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<_, _>>()?
    };
    let gone: Vec<_> = marked
        .into_iter()
        .filter(|(pid, started)| {
            u32::try_from(*pid).is_ok_and(|p| claims::recorded_gone(live, p, started))
        })
        .collect();
    if gone.is_empty() {
        return Ok(0);
    }
    let tx = core.claim_tx()?;
    let mut n = 0;
    for (pid, started) in gone {
        n += tx.execute(
            "DELETE FROM warm_procs WHERE pid = ?1 AND started = ?2 AND kill_at IS NOT NULL",
            params![pid, started],
        )?;
    }
    tx.commit()?;
    Ok(n)
}

/// What [`adopt`] did.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Adopted {
    /// Claims ended at once (never redeemed: nothing can be writing).
    pub ended: Vec<i64>,
    /// Processes to signal until they are gone (`end_gone` and [`settle`] decide when).
    pub kill: Vec<Kill>,
}

/// B5: the host `instance` has started; every claim and process of another host instance is the old host's, not this
/// one's. A claim never redeemed ends by CAS (a late redeem fails) and its turn is given back; one with a process goes
/// to `ending` and stays the writer until the process is gone. The old host's warm processes are marked for a kill.
/// `external` claims are a `--direct` run's own and are left alone. Repeating it changes nothing more.
type Live = (i64, String, Option<i64>, Option<String>, String);

pub(crate) fn adopt(core: &mut Core, instance: &str, now: i64) -> Result<Adopted> {
    if !is_host_instance(instance) {
        return Err(Error::BadParams(format!(
            "kith-core: {instance:?} is not a host instance"
        )));
    }
    let tx = core.claim_tx()?;
    let live: Vec<Live> = {
        let mut st = tx.prepare(
            "SELECT claim_id, state, pid, pid_started, holder_instance FROM claims
             WHERE state IN ('claimed', 'running', 'ending') AND kind <> 'external' AND holder_instance IS NOT NULL
               AND holder_instance <> ?1",
        )?;
        st.query_map([instance], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<std::result::Result<_, _>>()?
    };
    let mut out = Adopted::default();
    for (id, state, pid, started, holder) in live {
        if !is_host_instance(&holder) {
            continue;
        }
        if state == "claimed" && pid.is_none() {
            let n = tx.execute(
                "UPDATE claims SET state = 'ended', ended_at = ?2, end_reason = 'host_changed'
                 WHERE claim_id = ?1 AND state = 'claimed' AND pid IS NULL",
                params![id, now],
            )?;
            if n == 1 {
                claims::requeue_turn(&tx, id)?;
                out.ended.push(id);
            }
            continue;
        }
        if pid.is_none() {
            // `ending` with no process: it was never redeemed and now cannot be.
            let n = tx.execute(
                "UPDATE claims SET state = 'ended', ended_at = ?2 WHERE claim_id = ?1 AND state = 'ending'",
                params![id, now],
            )?;
            if n == 1 {
                out.ended.push(id);
            }
            continue;
        }
        let n = tx.execute(
            "UPDATE claims SET state = 'ending', end_reason = 'host_changed'
             WHERE claim_id = ?1 AND state IN ('claimed', 'running')",
            [id],
        )?;
        if n == 1 {
            claims::requeue_turn(&tx, id)?;
        }
        // An `ending` row that ended as a `done`, `exit` or out-of-usage report is not a kill to the re-send; the host
        // change makes it one, so a lost signal is sent again.
        tx.execute(
            "UPDATE claims SET end_reason = 'host_changed'
             WHERE claim_id = ?1 AND state = 'ending'
               AND (end_reason IS NULL OR end_reason IN ('done', 'exit', ?2))",
            params![id, crate::accounts::failover::OUT_OF_USAGE],
        )?;
        out.kill.push(claims::kill_of(id, pid, started));
    }
    let old: Vec<(i64, String, String)> = {
        let mut st = tx.prepare(
            "SELECT DISTINCT pid, started, instance FROM warm_procs WHERE instance <> ?1",
        )?;
        st.query_map([instance], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<std::result::Result<_, _>>()?
    };
    for (pid, started, holder) in old {
        if !is_host_instance(&holder) {
            continue;
        }
        tx.execute(
            "UPDATE warm_procs SET kill_at = COALESCE(kill_at, ?3) WHERE pid = ?1 AND started = ?2",
            params![pid, started, now],
        )?;
        out.kill.push(kill(pid, started));
    }
    tx.commit()?;
    Ok(out)
}

/// The conversations a warm process has loaded, if the core knows the process (sorted; none for a process with none).
#[cfg(test)]
pub(crate) fn loaded_all(core: &Core, pid: u32, started: &str) -> Result<Vec<String>> {
    let mut st = core.conn().prepare(
        "SELECT conversation FROM warm_procs WHERE pid = ?1 AND started = ?2 AND conversation <> '' ORDER BY conversation",
    )?;
    Ok(st
        .query_map(params![pid, started], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?)
}

/// The first conversation a warm process has loaded.
#[cfg(test)]
pub(crate) fn loaded(core: &Core, pid: u32, started: &str) -> Result<Option<String>> {
    Ok(loaded_all(core, pid, started)?.into_iter().next())
}
