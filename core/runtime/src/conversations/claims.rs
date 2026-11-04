//! The writer claim and its start protocol (runtime-design section 4, "Start protocol"; the row mapping is the doc
//! comment of migration 80). One function per step, each one `Core::claim_tx` transaction, so the generation guard
//! covers every change and a core that lost the lock writes nothing.
//!
//! | step | function |
//! |---|---|
//! | 1 claim | [`claim`] |
//! | 3 redeem, busy, revoke or stop | [`redeem`], `busy` through [`report`], [`stop`] |
//! | 4 exit report and its ack | [`report`] |
//! | 5 reconcile after a generation bump | [`reconcile`] |
//! | 6 revoke a person | [`revoke_person`] |
//! | 7 a different holder instance | [`different_holder`] |
//! | `ending` -> `ended` once the pid is gone | [`end_gone`] |
//!
//! The rules the writer-lock model (`verification/writer-lock/Holder.tla`, switch `RedeemWrite`) checks, and the
//! function that carries each:
//! - **Redeem** is a core.db CAS `claimed -> running`, only while the claim is current (the writer slot) and not
//!   released, recording the redeemer's pid, its start time and the holder instance ([`redeem`]). A redeem of a claim
//!   that is released, ended, ending or unknown changes nothing.
//! - **Release on a new holder instance** is a CAS too: an unlisted `claimed` claim becomes `ended`, so a later redeem
//!   fails (`release_unlisted`).
//! - A `running` claim the holder does not list is never `ended` straight away: it goes to `ending` and stays the writer
//!   (the partial unique index `claims_one_writer` blocks a second claim) until its recorded pid is confirmed gone
//!   ([`end_gone`]) or the holder reports the exit of that same pid ([`report`]).
//! - A `done` or `exit` report ends a `running` or `ending` claim only when it names the recorded pid. One that names
//!   another pid (a respawn that lost the lock) changes nothing. One that names no pid on a claim with a recorded pid
//!   moves it to `ending` (still the writer): [`end_gone`] then ends it once that pid is gone. `busy` follows the same
//!   pid rule. Reports are keyed by `(claim_id, kind, pid)` (migration 81), so the real writer's own report is
//!   never shadowed by another process's.
//! - A `--direct` (`external`) claim has no stub and no holder entry: reconcile and a holder change leave it alone, and
//!   only its recorded pid being gone ends it ([`end_gone`]).
//! - The may check is the caller's, but the revoke it saw is carried in: [`revoke_epoch`] before the check, passed in
//!   [`NewClaim::revoke_seen`], refused in the transaction if [`revoke_person`] bumped it since. [`redeem`] needs no
//!   epoch: [`revoke_person`] moves live claims off `claimed`, so a later redeem fails its CAS.
//! - **Ordering rule:** the authority revoke must be visible to the may check before [`revoke_person`] commits (or in
//!   the same transaction). If the sweep ran first, a caller could read the new epoch, still pass the may check, commit
//!   its claim after the sweep, and the revoked person would keep the run.
//!
//! Process probes (`ps`, `kill -0`) happen outside the transaction ([`end_gone`] reads, probes, then CASes): the CAS
//! decides. Times are milliseconds, passed in so tests set the clock. Not here: promoting a queued take-over claim
//! (`queued -> claimed`) when the writer is gone; it comes with the terminal entrance (M3-11).

use std::collections::HashSet;

use rusqlite::{OptionalExtension as _, Transaction, params};

use crate::db::{Core, RecordedProc};
use crate::error::{Error, Result};
use crate::gate::{Liveness, holder_alive};

/// `meta` key: the core generation whose reconcile has committed. Claims are refused until it equals the core's.
pub(super) const RECONCILED: &str = "claims_reconciled_generation";

/// `meta` key prefix: how many times a person was revoked. `revoke_person` bumps it in its transaction.
const REVOKED: &str = "revoked:";

/// What kind of writer a claim is (the SQL CHECK of `claims.kind`; it cannot grow without a table rebuild).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Terminal,
    Turn,
    External,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Terminal => "terminal",
            Self::Turn => "turn",
            Self::External => "external",
        }
    }
}

/// Step 1's input.
pub(crate) struct NewClaim<'a> {
    pub conversation: &'a str,
    pub kind: Kind,
    /// The principal [`revoke_person`] sweeps by. For a bot guest's run that is the bot's own principal, not its
    /// owner: revoking a person must also call [`revoke_person`] for each of their bots.
    pub person: &'a str,
    /// The queued or parked turn this claim runs, when it runs one.
    pub turn: Option<i64>,
    /// The holder instance the spawn goes to.
    pub holder_instance: Option<&'a str>,
    pub account: Option<&'a str>,
    /// An `external` run's own process (pid and `lock::process_start` text); required for that kind.
    pub process: Option<(u32, &'a str)>,
    /// [`revoke_epoch`] of `person`, read before the caller's may check. The claim is refused if a revoke committed since.
    pub revoke_seen: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Claimed {
    pub claim_id: i64,
    pub generation: i64,
}

/// A kill the caller must send (to the holder by claim id, or by pid and start time) and re-send until it is gone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Kill {
    pub claim_id: Option<i64>,
    pub pid: Option<u32>,
    pub started: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Redeemed {
    /// The claim is `running` on this process (`again`: it already was, a retried redeem).
    Running { again: bool },
    /// Nothing changed; the state the claim is in, or `unknown`.
    Refused(String),
}

/// What [`report`] did with a report.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Reported {
    /// No claim has this id: nothing was stored.
    Unknown,
    /// Stored (or already stored). `applied` is the kind of the report that changed the claim, if one did.
    Stored { applied: Option<&'static str> },
}

/// What [`reconcile`] and [`different_holder`] decided.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Released {
    /// Claims moved to `ended` (never redeemed, or `ending` with no process to wait for).
    pub ended: Vec<i64>,
    /// Claims now `ending` (or still): kill these until gone.
    pub kill: Vec<Kill>,
    /// Holder entries the holder lists whose claim is ended or unknown (step 5).
    pub kill_listed: Vec<i64>,
    /// Recorded holder processes no claim row knows (step 7).
    pub kill_procs: Vec<Kill>,
}

/// What [`revoke_person`] did.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Revoked {
    pub cancelled_turns: usize,
    pub kill: Vec<Kill>,
}

struct Row {
    conversation: String,
    state: String,
    pid: Option<i64>,
    pid_started: Option<String>,
}

fn row(tx: &Transaction<'_>, claim_id: i64) -> Result<Option<Row>> {
    Ok(tx
        .query_row(
            "SELECT conversation, state, pid, pid_started FROM claims WHERE claim_id = ?1",
            [claim_id],
            |r| {
                Ok(Row {
                    conversation: r.get(0)?,
                    state: r.get(1)?,
                    pid: r.get(2)?,
                    pid_started: r.get(3)?,
                })
            },
        )
        .optional()?)
}

/// The conversation's current claim is its newest one that is not a queued take-over (that one waits behind it).
fn is_newest(tx: &Transaction<'_>, claim_id: i64, conversation: &str) -> Result<bool> {
    let newest: Option<i64> = tx.query_row(
        "SELECT max(claim_id) FROM claims WHERE conversation = ?1 AND state <> 'queued'",
        [conversation],
        |r| r.get(0),
    )?;
    Ok(newest == Some(claim_id))
}

/// How many times `person` has been revoked. Read it before the may check and pass it to [`claim`]: a revoke that
/// commits between the check and the claim moves it, and the claim is refused. Only sound when [`revoke_person`] runs
/// after the authority revoke is visible to the may check (see the module doc).
pub(crate) fn revoke_epoch(core: &Core, person: &str) -> Result<i64> {
    epoch(core.conn(), person)
}

fn epoch(conn: &rusqlite::Connection, person: &str) -> Result<i64> {
    let v: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            [format!("{REVOKED}{person}")],
            |r| r.get(0),
        )
        .optional()?;
    Ok(v.and_then(|t| t.parse().ok()).unwrap_or(0))
}

/// The newest claim id handed out. Read it before asking the holder what it runs and pass it to [`reconcile`] and
/// [`different_holder`], which then touch only claims at or below it.
pub(crate) fn claim_high_water(core: &Core) -> Result<i64> {
    Ok(core.conn().query_row(
        "SELECT value FROM counters WHERE name = 'claim_id'",
        [],
        |r| r.get(0),
    )?)
}

fn reconciled(tx: &Transaction<'_>) -> Result<Option<i64>> {
    let text: Option<String> = tx
        .query_row("SELECT value FROM meta WHERE key = ?1", [RECONCILED], |r| {
            r.get(0)
        })
        .optional()?;
    Ok(text.and_then(|t| t.parse().ok()))
}

fn kill_of(claim_id: i64, pid: Option<i64>, started: Option<String>) -> Kill {
    Kill {
        claim_id: Some(claim_id),
        pid: pid.and_then(|p| u32::try_from(p).ok()),
        started,
    }
}

/// Puts a turn back in the queue when its claim ends before the turn finished.
fn requeue_turn(tx: &Transaction<'_>, claim_id: i64) -> Result<()> {
    tx.execute(
        "UPDATE turns SET state = 'queued', claim_id = NULL, requeues = requeues + 1, started_at = NULL
         WHERE claim_id = ?1 AND state IN ('claimed', 'running')",
        [claim_id],
    )?;
    Ok(())
}

fn cancel_turn(tx: &Transaction<'_>, claim_id: i64, now: i64) -> Result<()> {
    tx.execute(
        "UPDATE turns SET state = 'cancelled', finished_at = ?2
         WHERE claim_id = ?1 AND state IN ('claimed', 'running')",
        params![claim_id, now],
    )?;
    Ok(())
}

/// Step 1: re-check may (the caller's, other lanes' tables), take the claim and mark the turn claimed, in one
/// transaction. The partial index `claims_one_writer` is the last word on a second writer. Refused until this
/// generation's reconcile has committed (step 5).
pub(crate) fn claim(core: &mut Core, req: &NewClaim<'_>, now: i64) -> Result<Claimed> {
    if req.kind == Kind::External && req.process.is_none() {
        return Err(Error::BadParams(
            "kith-core: an external claim needs its pid and start time".into(),
        ));
    }
    let generation = core.generation();
    let tx = core.claim_tx()?;
    if reconciled(&tx)? != Some(generation) {
        return Err(Error::Refused(format!(
            "kith-core: claims wait for the reconcile of generation {generation}"
        )));
    }
    if epoch(&tx, req.person)? != req.revoke_seen {
        return Err(Error::Refused(format!(
            "kith-core: {} was revoked after the may check",
            req.person
        )));
    }
    let known: Option<i64> = tx
        .query_row(
            "SELECT 1 FROM conversations WHERE id = ?1",
            [req.conversation],
            |r| r.get(0),
        )
        .optional()?;
    if known.is_none() {
        return Err(Error::Refused(format!(
            "kith-core: no conversation {}",
            req.conversation
        )));
    }
    // A stopped ask's conversation takes no new automated turn (the durable half of the stop: a wake given back or left
    // by a crash, a card resume). A person's turn to that thread stays allowed. Mirrors kith-ask:472, :834 and :430.
    if let Some(turn) = req.turn {
        let fenced: Option<String> = tx
            .query_row(
                "SELECT a.id FROM turns t JOIN asks a ON a.conversation = t.conversation
                 WHERE t.id = ?1 AND t.source IN ('ask', 'wake') AND (a.state = 'stopped' OR a.ended_ms IS NOT NULL)",
                [turn],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(ask) = fenced {
            return Err(Error::Refused(format!(
                "kith-core: ask {ask} was stopped; conversation {} takes no ask or wake turn",
                req.conversation
            )));
        }
    }
    let writer: Option<(i64, String)> = tx
        .query_row(
            "SELECT claim_id, state FROM claims
             WHERE conversation = ?1 AND state IN ('claimed', 'running', 'ending')",
            [req.conversation],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((id, state)) = writer {
        return Err(Error::Refused(format!(
            "kith-core: conversation {} has a writer: claim {id} is {state}",
            req.conversation
        )));
    }
    let claim_id = Core::next_claim_id(&tx)?;
    let (pid, started) = req
        .process
        .map_or((None, None), |(p, s)| (Some(p), Some(s)));
    tx.execute(
        "INSERT INTO claims (claim_id, conversation, generation, kind, state, holder_instance, person, turn, pid,
                             pid_started, account, created_at, claimed_at)
         VALUES (?1, ?2, ?3, ?4, 'claimed', ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)",
        params![
            claim_id,
            req.conversation,
            generation,
            req.kind.as_str(),
            req.holder_instance,
            req.person,
            req.turn,
            pid,
            started,
            req.account,
            now
        ],
    )?;
    if let Some(turn) = req.turn {
        let n = tx.execute(
            "UPDATE turns SET claim_id = ?1, state = 'claimed', parked_reason = NULL, parked_at = NULL
             WHERE id = ?2 AND conversation = ?3 AND state IN ('queued', 'parked') AND person = ?4
               AND (deadline IS NULL OR deadline > ?5)",
            params![claim_id, turn, req.conversation, req.person, now],
        )?;
        if n != 1 {
            // Not waiting, past its deadline (expire_parked has not swept it yet), or another person's turn: a claim
            // for one person must not run another's, or the second person's revoke would miss the run.
            return Err(Error::Refused(format!(
                "kith-core: turn {turn} is not waiting for {} in conversation {}",
                req.person, req.conversation
            )));
        }
    }
    tx.commit()?;
    Ok(Claimed {
        claim_id,
        generation,
    })
}

/// Step 3, the redeem: CAS `claimed -> running`, only while the claim is still the writer slot (`claims_one_writer`
/// lets no other live claim exist, so a `claimed` row is the current one), recording the redeemer's pid, start time and
/// holder instance, and the turn `running`. Anything else changes nothing; a retried redeem by the same process is
/// answered `again`. No epoch check here: `revoke_person` already moves the person's live claims off `claimed`, so a
/// redeem after a revoke fails this CAS (and a refused epoch would only leave the claim stuck `claimed`).
pub(crate) fn redeem(
    core: &mut Core,
    claim_id: i64,
    pid: u32,
    started: &str,
    holder_instance: &str,
    now: i64,
) -> Result<Redeemed> {
    let tx = core.claim_tx()?;
    // An `external` claim has its own process and no stub: a stray redeem must not overwrite the run's pid.
    let n = tx.execute(
        "UPDATE claims SET state = 'running', running_at = ?2, pid = ?3, pid_started = ?4, holder_instance = ?5
         WHERE claim_id = ?1 AND state = 'claimed' AND kind <> 'external'",
        params![claim_id, now, pid, started, holder_instance],
    )?;
    if n == 1 {
        tx.execute(
            "UPDATE turns SET state = 'running', started_at = ?2 WHERE claim_id = ?1 AND state = 'claimed'",
            params![claim_id, now],
        )?;
        tx.commit()?;
        return Ok(Redeemed::Running { again: false });
    }
    Ok(match row(&tx, claim_id)? {
        None => Redeemed::Refused("unknown".into()),
        Some(r)
            if r.state == "running"
                && r.pid == Some(i64::from(pid))
                && r.pid_started.as_deref() == Some(started) =>
        {
            Redeemed::Running { again: true }
        }
        Some(r) => Redeemed::Refused(r.state),
    })
}

/// The report kinds in precedence order (step 4): the first that applies wins, the rest are acked with it.
const PRECEDENCE: [&str; 4] = ["done", "exit", "busy", "spawned"];

/// Steps 3 (busy) and 4: keeps what the holder or stub said about a claim (`INSERT OR IGNORE`, so a retry and a late
/// `busy` change nothing already kept), then acts on the unacked reports of the claim if it is the conversation's newest.
/// `body` is JSON; `pid` (and for `spawned` `started`) say which process it is about.
pub(crate) fn report(
    core: &mut Core,
    claim_id: i64,
    kind: &str,
    body: &str,
    now: i64,
) -> Result<Reported> {
    if !PRECEDENCE.contains(&kind) {
        return Err(Error::BadParams(format!(
            "kith-core: no such report kind {kind}"
        )));
    }
    let tx = core.claim_tx()?;
    let Some(claim) = row(&tx, claim_id)? else {
        return Ok(Reported::Unknown);
    };
    // Keyed by the process it names, so another process's report of the same kind never shadows the writer's own.
    let said = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|b| b.get("pid").and_then(serde_json::Value::as_i64))
        .unwrap_or(0);
    tx.execute(
        "INSERT OR IGNORE INTO reports (claim_id, kind, pid, body, received_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![claim_id, kind, said, body, now],
    )?;
    let mut applied = None;
    if is_newest(&tx, claim_id, &claim.conversation)? {
        let mut unacked: Vec<(String, String)> = Vec::new();
        {
            let mut st = tx.prepare(
                "SELECT kind, body FROM reports WHERE claim_id = ?1 AND acked_at IS NULL ORDER BY received_at, pid",
            )?;
            for r in st.query_map([claim_id], |r| Ok((r.get(0)?, r.get(1)?)))? {
                unacked.push(r?);
            }
        }
        'kinds: for k in PRECEDENCE {
            for (_, b) in unacked.iter().filter(|(uk, _)| uk == k) {
                if apply(&tx, claim_id, k, b, now)? {
                    applied = Some(k);
                    break 'kinds;
                }
            }
        }
    }
    tx.execute(
        "UPDATE reports SET acked_at = ?2 WHERE claim_id = ?1 AND acked_at IS NULL",
        params![claim_id, now],
    )?;
    tx.commit()?;
    Ok(Reported::Stored { applied })
}

/// Acts on one kept report. True when it changed the claim.
fn apply(tx: &Transaction<'_>, claim_id: i64, kind: &str, body: &str, now: i64) -> Result<bool> {
    let b: serde_json::Value = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
    let said_pid = b.get("pid").and_then(serde_json::Value::as_i64);
    match kind {
        "done" | "exit" => {
            // A report that names a process other than the one the redeem recorded (or none) is about another process of
            // the same claim (a respawn that lost the lock). It says nothing about the writer, so it ends nothing.
            let Some(r) = row(tx, claim_id)? else {
                return Ok(false);
            };
            if r.pid.is_some() && said_pid.is_some() && said_pid != r.pid {
                return Ok(false);
            }
            if r.pid.is_none() && r.state != "claimed" {
                return Ok(false);
            }
            // A report that names no process cannot be matched to the writer's: the claim stays the writer (`ending`)
            // and `end_gone` ends it once the recorded process is gone.
            let nameless = r.pid.is_some() && said_pid.is_none();
            let n = if nameless {
                tx.execute(
                    "UPDATE claims SET state = 'ending', end_reason = COALESCE(end_reason, ?2)
                     WHERE claim_id = ?1 AND state IN ('claimed', 'running')",
                    params![claim_id, kind],
                )?
            } else {
                tx.execute(
                    "UPDATE claims SET state = 'ended', ended_at = ?2, end_reason = COALESCE(end_reason, ?3)
                     WHERE claim_id = ?1 AND state IN ('claimed', 'running', 'ending')",
                    params![claim_id, now, kind],
                )?
            };
            if n == 1 {
                if kind == "done" {
                    tx.execute(
                        "UPDATE turns SET state = 'done', finished_at = ?2
                         WHERE claim_id = ?1 AND state IN ('claimed', 'running')",
                        params![claim_id, now],
                    )?;
                } else {
                    requeue_turn(tx, claim_id)?;
                }
            }
            Ok(n == 1)
        }
        "busy" => {
            // A busy names the stub that found the lock taken; with a recorded stub, only that one's counts (same rule as
            // done and exit: a nameless busy is ignored).
            if let Some(r) = row(tx, claim_id)?
                && r.pid.is_some()
                && said_pid != r.pid
            {
                return Ok(false);
            }
            let n = tx.execute(
                "UPDATE claims SET state = 'ended', ended_at = ?2, end_reason = 'busy'
                 WHERE claim_id = ?1 AND state = 'claimed'",
                params![claim_id, now],
            )?;
            if n == 1 {
                tx.execute(
                    "UPDATE turns SET state = 'parked', parked_reason = 'lock_busy', parked_at = ?2, claim_id = NULL
                     WHERE claim_id = ?1 AND state IN ('claimed', 'running')",
                    params![claim_id, now],
                )?;
            }
            Ok(n == 1)
        }
        _ => {
            // spawned: the holder started the stub; record its process so a lost redeem still leaves a pid to kill.
            let (Some(pid), Some(started)) = (
                said_pid,
                b.get("started").and_then(serde_json::Value::as_str),
            ) else {
                return Ok(false);
            };
            let n = tx.execute(
                "UPDATE claims SET pid = ?2, pid_started = ?3
                 WHERE claim_id = ?1 AND state = 'claimed' AND pid IS NULL",
                params![claim_id, pid, started],
            )?;
            Ok(n == 1)
        }
    }
}

/// Step 3, revoke or stop: CAS `claimed | running -> ending` (still the writer; the kill is re-sent until the process
/// is gone) and the turn cancelled, or requeued when `requeue`. `None` when the claim is not live. An `ending` claim
/// answers its kill again.
pub(crate) fn stop(
    core: &mut Core,
    claim_id: i64,
    reason: &str,
    requeue: bool,
    now: i64,
) -> Result<Option<Kill>> {
    let tx = core.claim_tx()?;
    let kill = stop_in(&tx, claim_id, reason, requeue, now)?;
    tx.commit()?;
    Ok(kill)
}

/// [`stop`] inside a transaction the caller began (`Core::claim_tx`) and commits, for a caller that stops several claims
/// and settles its own rows in the same step (the ask tree's end).
pub(crate) fn stop_in(
    tx: &Transaction<'_>,
    claim_id: i64,
    reason: &str,
    requeue: bool,
    now: i64,
) -> Result<Option<Kill>> {
    // Never redeemed and no process recorded: nothing can be writing, so it ends by CAS (a late redeem then fails)
    // instead of sticking at `ending`, which has no pid for `end_gone` to wait on.
    let unspawned = tx.execute(
        "UPDATE claims SET state = 'ended', ended_at = ?2, end_reason = ?3
         WHERE claim_id = ?1 AND state = 'claimed' AND pid IS NULL",
        params![claim_id, now, reason],
    )?;
    if unspawned == 1 {
        if requeue {
            requeue_turn(tx, claim_id)?;
        } else {
            cancel_turn(tx, claim_id, now)?;
        }
        return Ok(None);
    }
    let n = tx.execute(
        "UPDATE claims SET state = 'ending', end_reason = ?2
         WHERE claim_id = ?1 AND state IN ('claimed', 'running')",
        params![claim_id, reason],
    )?;
    if n == 1 {
        if requeue {
            requeue_turn(tx, claim_id)?;
        } else {
            cancel_turn(tx, claim_id, now)?;
        }
    }
    let kill = match row(tx, claim_id)? {
        Some(r) if r.state == "ending" => Some(kill_of(claim_id, r.pid, r.pid_started)),
        _ => None,
    };
    Ok(kill)
}

/// What a conversation's automated work stops (`kith ask --end` of the ask that runs it): its queued and parked turns are
/// cancelled, and each live claim of kind `turn` goes through [`stop_in`] (the turn cancelled, the kill returned to be
/// re-sent until the process is gone; an unspawned claim ends at once), and so does an `external` claim whose pid is the
/// one the ask recorded (`ask_pid`: the ask's own `--direct` run). A person's writers are left alone: a queued take-over
/// and a terminal or other external claim are what someone resumed to steer the thread, and the stop is what frees the
/// writer for them. Safe to repeat: a second call finds nothing queued and answers an `ending` claim's kill again.
pub(crate) fn stop_conversation_in(
    tx: &Transaction<'_>,
    conversation: &str,
    ask_pid: Option<i64>,
    reason: &str,
    now: i64,
) -> Result<Vec<Kill>> {
    tx.execute(
        "UPDATE turns SET state = 'cancelled', parked_reason = NULL, parked_at = NULL, finished_at = ?2
         WHERE conversation = ?1 AND state IN ('queued', 'parked')",
        params![conversation, now],
    )?;
    let live: Vec<i64> = {
        let mut st = tx.prepare(
            "SELECT claim_id FROM claims
             WHERE conversation = ?1 AND state IN ('claimed', 'running', 'ending')
               AND (kind = 'turn' OR (kind = 'external' AND pid = ?2))",
        )?;
        st.query_map(params![conversation, ask_pid], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?
    };
    let mut kills = Vec::new();
    for id in live {
        kills.extend(stop_in(tx, id, reason, false, now)?);
    }
    Ok(kills)
}

/// Step 6: the person's queued and parked turns are cancelled, their queued take-overs ended, and their live claims go
/// to `ending` (kills returned, to be re-sent until gone).
///
/// Call it only after the authority revoke is visible to the may check, or in the same transaction: run first, a claim
/// could read the new epoch, pass the stale may check and commit after this sweep. `person` is the principal
/// `claims.person` holds, so a person's bots (whose runs hold the bot principal) each need their own call.
pub(crate) fn revoke_person(core: &mut Core, person: &str, now: i64) -> Result<Revoked> {
    let tx = core.claim_tx()?;
    // A claim that checked may before this commit sees the bump and is refused.
    tx.execute(
        "INSERT INTO meta (key, value) VALUES (?1, '1')
         ON CONFLICT (key) DO UPDATE SET value = CAST(value AS INTEGER) + 1",
        [format!("{REVOKED}{person}")],
    )?;
    let cancelled_turns = tx.execute(
        "UPDATE turns SET state = 'cancelled', parked_reason = NULL, parked_at = NULL, finished_at = ?2
         WHERE person = ?1 AND state IN ('queued', 'parked')",
        params![person, now],
    )?;
    tx.execute(
        "UPDATE claims SET state = 'ended', ended_at = ?2, end_reason = 'revoked'
         WHERE person = ?1 AND state = 'queued'",
        params![person, now],
    )?;
    let mut live: Vec<(i64, Option<i64>, Option<String>)> = Vec::new();
    {
        let mut st = tx.prepare(
            "SELECT claim_id, pid, pid_started FROM claims
             WHERE person = ?1 AND state IN ('claimed', 'running', 'ending')",
        )?;
        for r in st.query_map([person], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))? {
            live.push(r?);
        }
    }
    let mut kill = Vec::new();
    for (id, pid, started) in live {
        let unspawned = tx.execute(
            "UPDATE claims SET state = 'ended', ended_at = ?2, end_reason = 'revoked'
             WHERE claim_id = ?1 AND state = 'claimed' AND pid IS NULL",
            params![id, now],
        )?;
        if unspawned == 1 {
            cancel_turn(&tx, id, now)?;
            continue;
        }
        let n = tx.execute(
            "UPDATE claims SET state = 'ending', end_reason = 'revoked'
             WHERE claim_id = ?1 AND state IN ('claimed', 'running')",
            [id],
        )?;
        if n == 1 {
            cancel_turn(&tx, id, now)?;
        }
        kill.push(kill_of(id, pid, started));
    }
    tx.commit()?;
    Ok(Revoked {
        cancelled_turns,
        kill,
    })
}

/// True when the system says this recorded process is gone: no such pid, or the pid now has another start time (reused).
/// A probe that cannot tell is not "gone", and neither is a claim whose start time is the old `lstart` text (the stub
/// still records it): that only ends when the pid is gone.
fn recorded_gone(live: &dyn Liveness, pid: u32, started: &str) -> bool {
    !holder_alive(live, pid, started)
}

/// `ending -> ended` (and a `--direct` run's `claimed -> ended`), only once the recorded process is confirmed gone. Reads, probes outside any transaction, then one
/// CAS per claim that still names the same process. Returns the claims it ended.
pub(crate) fn end_gone(core: &mut Core, live: &dyn Liveness, now: i64) -> Result<Vec<i64>> {
    let mut candidates: Vec<(i64, i64, String)> = Vec::new();
    {
        let mut st = core.conn().prepare(
            "SELECT claim_id, pid, pid_started FROM claims
             WHERE (state = 'ending' OR (kind = 'external' AND state IN ('claimed', 'running')))
               AND pid IS NOT NULL AND pid_started IS NOT NULL",
        )?;
        for r in st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))? {
            candidates.push(r?);
        }
    }
    let gone: Vec<_> = candidates
        .into_iter()
        .filter(|(_, pid, started)| {
            u32::try_from(*pid).is_ok_and(|p| recorded_gone(live, p, started))
        })
        .collect();
    if gone.is_empty() {
        return Ok(Vec::new());
    }
    let tx = core.claim_tx()?;
    let mut ended = Vec::new();
    for (id, pid, started) in gone {
        let n = tx.execute(
            "UPDATE claims SET state = 'ended', ended_at = ?2, end_reason = COALESCE(end_reason, 'gone')
             WHERE claim_id = ?1 AND pid = ?3 AND pid_started = ?4
               AND (state = 'ending' OR (kind = 'external' AND state IN ('claimed', 'running')))",
            params![id, now, pid, started],
        )?;
        if n == 1 {
            ended.push(id);
        }
    }
    tx.commit()?;
    Ok(ended)
}

/// The claims the holder's listing does not cover, ended or moved to `ending` (steps 5 and 7). `foreign_to` limits it
/// to claims of another holder instance (step 7); `None` takes every live claim (step 5).
fn release_unlisted(
    tx: &Transaction<'_>,
    listed: &HashSet<i64>,
    foreign_to: Option<&str>,
    procs: &[RecordedProc],
    high_water: i64,
    now: i64,
) -> Result<Released> {
    let mut out = Released::default();
    #[allow(
        clippy::type_complexity,
        reason = "one row of the query below, used once"
    )]
    let mut live: Vec<(i64, String, bool, Option<i64>, Option<String>)> = Vec::new();
    {
        // Only claims at or below the high-water mark read before the holder's list: a newer one may simply be missing
        // from a list that was read before it was taken.
        let mut st = tx.prepare(
            "SELECT claim_id, state, kind = 'external', pid, pid_started FROM claims
             WHERE state IN ('claimed', 'running', 'ending') AND claim_id <= ?2
               AND (?1 IS NULL OR holder_instance IS NULL OR holder_instance <> ?1)",
        )?;
        for r in st.query_map(params![foreign_to, high_water], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })? {
            live.push(r?);
        }
    }
    for (id, state, external, pid, started) in live {
        if listed.contains(&id) {
            continue;
        }
        // A `--direct` run has no stub or holder entry: the holder not listing it says nothing. `end_gone` ends it
        // when its recorded pid is gone; a stop's kill is still re-sent.
        if external {
            if state == "ending" {
                out.kill.push(kill_of(id, pid, started));
            }
            continue;
        }
        match state.as_str() {
            // Never redeemed: end it by CAS so a late redeem fails, and give the turn back.
            "claimed" => {
                let n = tx.execute(
                    "UPDATE claims SET state = 'ended', ended_at = ?2, end_reason = 'holder_changed'
                     WHERE claim_id = ?1 AND state = 'claimed'",
                    params![id, now],
                )?;
                if n == 1 {
                    requeue_turn(tx, id)?;
                    out.ended.push(id);
                }
            }
            // Redeemed: the process may still write. It stays the writer, as `ending`, until its pid is gone.
            "running" => {
                let n = tx.execute(
                    "UPDATE claims SET state = 'ending', end_reason = 'holder_changed'
                     WHERE claim_id = ?1 AND state = 'running'",
                    [id],
                )?;
                if n == 1 {
                    requeue_turn(tx, id)?;
                }
                out.kill.push(kill_of(id, pid, started));
            }
            // Already ending: with a process, wait for it; without one it was never redeemed and now cannot be.
            _ => {
                if pid.is_some() {
                    out.kill.push(kill_of(id, pid, started));
                } else {
                    let n = tx.execute(
                        "UPDATE claims SET state = 'ended', ended_at = ?2
                         WHERE claim_id = ?1 AND state = 'ending'",
                        params![id, now],
                    )?;
                    if n == 1 {
                        out.ended.push(id);
                    }
                }
            }
        }
    }
    let mut known: HashSet<(i64, String)> = HashSet::new();
    {
        // Only live claims count: an old-instance process whose claim ended must still be killed (step 7).
        let mut st = tx.prepare(
            "SELECT pid, pid_started FROM claims
             WHERE state IN ('claimed', 'running', 'ending') AND pid IS NOT NULL AND pid_started IS NOT NULL",
        )?;
        for r in st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))? {
            known.insert(r?);
        }
    }
    for p in procs {
        if !known.contains(&(i64::from(p.pid), p.started.clone())) {
            out.kill_procs.push(Kill {
                claim_id: None,
                pid: Some(p.pid),
                started: Some(p.started.clone()),
            });
        }
    }
    Ok(out)
}

/// Step 5: after the generation bump and hello, one transaction releases the claims the holder does not list, lists the
/// holder entries to kill (their claim is ended or unknown), and marks this generation reconciled, so claims open.
/// `listed` is the holder's claim ids; `procs` the old instance's recorded processes with no redeem (step 7), if any;
/// `high_water` is [`claim_high_water`] read before the holder was asked. A second reconcile in one generation is refused.
pub(crate) fn reconcile(
    core: &mut Core,
    listed: &[i64],
    procs: &[RecordedProc],
    high_water: i64,
    now: i64,
) -> Result<Released> {
    let generation = core.generation();
    let tx = core.claim_tx()?;
    if reconciled(&tx)? == Some(generation) {
        return Err(Error::Refused(format!(
            "kith-core: generation {generation} is already reconciled"
        )));
    }
    let set: HashSet<i64> = listed.iter().copied().collect();
    let mut out = release_unlisted(&tx, &set, None, procs, high_water, now)?;
    for id in listed {
        let state = row(&tx, *id)?.map(|r| r.state);
        if !matches!(state.as_deref(), Some("claimed" | "running" | "ending")) {
            out.kill_listed.push(*id);
        }
    }
    tx.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
        params![RECONCILED, generation.to_string()],
    )?;
    tx.commit()?;
    Ok(out)
}

/// Step 7: the core's hello met a holder instance other than the one it recorded. Claims of any other instance that the
/// new one does not list: `claimed` ones are ended by CAS (a late redeem fails), `running` ones go to `ending` and are
/// killed by their recorded pid until confirmed gone ([`end_gone`]), never straight to `ended`. `procs` are the old
/// instance's recorded processes with no redeem. Does not open claims; [`reconcile`] does.
pub(crate) fn different_holder(
    core: &mut Core,
    new_instance: &str,
    listed: &[i64],
    procs: &[RecordedProc],
    high_water: i64,
    now: i64,
) -> Result<Released> {
    let tx = core.claim_tx()?;
    let set: HashSet<i64> = listed.iter().copied().collect();
    let out = release_unlisted(&tx, &set, Some(new_instance), procs, high_water, now)?;
    tx.commit()?;
    Ok(out)
}

// ---- turns: the queue other lanes (wake, asks, accounts, transcripts) call; none of them writes these tables

/// Where a turn came from (`turns.source` is checked here, not in SQL, so a new source needs no table rebuild).
pub(crate) const SOURCES: [&str; 9] = [
    "app", "schedule", "ask", "wake", "terminal", "cli", "channel", "room", "bot",
];
/// Why a turn waits without a claim (`turns.parked_reason`, checked here).
const PARK_REASONS: [&str; 2] = ["lock_busy", "terminal_live"];

/// A turn to queue. The conversation row is created from `agent` and `engine` when it is missing.
pub(crate) struct NewTurn<'a> {
    pub conversation: &'a str,
    pub agent: &'a str,
    pub engine: &'a str,
    pub source: &'a str,
    pub person: &'a str,
    pub prompt: &'a str,
    /// A scheduled run expires at this time instead of waiting for ever (q218).
    pub deadline: Option<i64>,
}

/// Queues a turn (`queued`), creating the conversation when needed.
pub(crate) fn enqueue_turn(core: &mut Core, t: &NewTurn<'_>, now: i64) -> Result<i64> {
    if !SOURCES.contains(&t.source) {
        return Err(Error::BadParams(format!(
            "kith-core: no such turn source {}",
            t.source
        )));
    }
    if t.person.is_empty() {
        return Err(Error::BadParams("kith-core: a turn needs a person".into()));
    }
    let tx = core.claim_tx()?;
    tx.execute(
        "INSERT OR IGNORE INTO conversations (id, agent, engine, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
        params![t.conversation, t.agent, t.engine, now],
    )?;
    let (agent, engine): (String, String) = tx.query_row(
        "SELECT agent, engine FROM conversations WHERE id = ?1",
        [t.conversation],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if agent != t.agent || engine != t.engine {
        return Err(Error::Refused(format!(
            "kith-core: conversation {} belongs to {agent} on {engine}, not {} on {}",
            t.conversation, t.agent, t.engine
        )));
    }
    tx.execute(
        "INSERT INTO turns (conversation, person, source, prompt, state, deadline, queued_at)
         VALUES (?1, ?2, ?3, ?4, 'queued', ?5, ?6)",
        params![
            t.conversation,
            t.person,
            t.source,
            t.prompt,
            t.deadline,
            now
        ],
    )?;
    let id = tx.last_insert_rowid();
    tx.commit()?;
    Ok(id)
}

/// Parks a queued turn (no claim, no run token) as waiting for `lock_busy` or `terminal_live`. False when the turn is not
/// queued (the busy path of [`report`] parks a claimed one itself).
pub(crate) fn park(core: &mut Core, turn_id: i64, reason: &str, now: i64) -> Result<bool> {
    if !PARK_REASONS.contains(&reason) {
        return Err(Error::BadParams(format!(
            "kith-core: no such park reason {reason}"
        )));
    }
    let tx = core.claim_tx()?;
    let n = tx.execute(
        "UPDATE turns SET state = 'parked', parked_reason = ?2, parked_at = ?3 WHERE id = ?1 AND state = 'queued'",
        params![turn_id, reason, now],
    )?;
    tx.commit()?;
    Ok(n == 1)
}

/// Level-triggered wake: parked turns of the conversation go back to `queued` once nothing holds it. The caller probes
/// outside the transaction (`lock_free`: the writer lock could be taken; `terminal_live`: a terminal session is open);
/// the CAS here also needs the conversation to have no live claim.
pub(crate) fn unpark_ready(
    core: &mut Core,
    conversation: &str,
    lock_free: bool,
    terminal_live: bool,
    _now: i64,
) -> Result<Vec<i64>> {
    let tx = core.claim_tx()?;
    let held: i64 = tx.query_row(
        "SELECT count(*) FROM claims WHERE conversation = ?1 AND state IN ('claimed', 'running', 'ending')",
        [conversation],
        |r| r.get(0),
    )?;
    let mut woken = Vec::new();
    if held == 0 {
        let mut ids: Vec<(i64, String)> = Vec::new();
        {
            let mut st = tx.prepare(
                "SELECT id, parked_reason FROM turns WHERE conversation = ?1 AND state = 'parked' ORDER BY id",
            )?;
            for r in st.query_map([conversation], |r| Ok((r.get(0)?, r.get(1)?)))? {
                ids.push(r?);
            }
        }
        for (id, reason) in ids {
            let ready = match reason.as_str() {
                "lock_busy" => lock_free,
                "terminal_live" => !terminal_live,
                _ => false,
            };
            if ready
                && tx.execute(
                    "UPDATE turns SET state = 'queued', parked_reason = NULL, parked_at = NULL
                     WHERE id = ?1 AND state = 'parked'",
                    [id],
                )? == 1
            {
                woken.push(id);
            }
        }
    }
    tx.commit()?;
    Ok(woken)
}

/// q218: queued and parked turns past their `deadline` become `expired`.
pub(crate) fn expire_parked(core: &mut Core, now: i64) -> Result<Vec<i64>> {
    let tx = core.claim_tx()?;
    let mut ids: Vec<i64> = Vec::new();
    {
        let mut st = tx.prepare(
            "SELECT id FROM turns WHERE state IN ('queued', 'parked') AND deadline IS NOT NULL AND deadline <= ?1 ORDER BY id",
        )?;
        for r in st.query_map([now], |r| r.get(0))? {
            ids.push(r?);
        }
    }
    for id in &ids {
        tx.execute(
            "UPDATE turns SET state = 'expired', parked_reason = NULL, parked_at = NULL, finished_at = ?2
             WHERE id = ?1 AND state IN ('queued', 'parked')",
            params![id, now],
        )?;
    }
    tx.commit()?;
    Ok(ids)
}
