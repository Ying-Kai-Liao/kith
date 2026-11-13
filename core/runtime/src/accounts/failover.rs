//! Account failover in the core (M3-9b). q216: an account is never changed under a running claim. A turn whose exit
//! report says the account ran out of usage ends that claim; the report's ack transaction ([`on_exit`], called from
//! `conversations::claims::report`) records why and, once the core owns account state, marks the account out, and the
//! turn goes back in the queue with its deadline. The NEXT claim for the turn ([`claim_next`]) picks from the top of the
//! chain with 9a's `pick` over core.db, leaving out every account this turn already ran out on (at most once per entry),
//! sets `claims.account` and starts a new engine session row on that account, never a copy of the old one.
//!
//! The report: an `exit` whose body carries `"out_of_usage": true` (and, when the provider said when the limit lifts,
//! `"resets_at"`, epoch seconds). The stub or the holder says it; nothing here reads words in a message.
//!
//! One writer per conversation, never per (conversation, account): the main account coming back while the backup's claim
//! is live is no reason to start another, and `claims_one_writer` refuses it.
//!
//! UNSWITCHED. The account row is written only while `meta.accounts_core_writes` is `1` ([`super::core_writes`]); nothing
//! sets it, so live Python stays the only writer of account state and this changes no account row in production. The
//! requeue, the recorded reason and the "tried" memory (the claims of the turn that ended `out_of_usage`) are core.db's
//! own and work either way. Nothing calls [`claim_next`] until the start protocol switches (M3-10).
//!
//! "Tried" lives in claim rows (the turn's claims that ended `out_of_usage`). Cleaning up old claims, or a wake that
//! makes a new turn for the same message, starts the list over; harmless once switched, because the account marks carry
//! across turns.
//!
//! No backup left: the turn ends `failed` (not requeued again) and [`Started::Exhausted`] carries the notice the host gives
//! today (`all_out_text` of `agent/backups.rs`). Python's chain run has no such notice (it prints the last attempt's own
//! output), and no golden holds a failover, so there is nothing to replay; the text is the host's.

use kith_kernel::accounts::{self, Ctx};
#[cfg(unix)]
use kith_kernel::threads;
use rusqlite::{Connection, OptionalExtension as _, Transaction, params};

use super::CoreState;
#[cfg(unix)]
use crate::conversations::claims::{
    Claimed, NewClaim, cancel_fenced, claim_checked_in, stopped_fence,
};
use crate::db::Core;
#[cfg(unix)]
use crate::error::Error;
use crate::error::Result;
#[cfg(unix)]
use crate::transcripts::{self, SessionPtr, rows::EngineSession};

/// `claims.end_reason` of a claim that ended on a usage limit.
pub(crate) const OUT_OF_USAGE: &str = "out_of_usage";

/// The furthest a reported `resets_at` is believed: a milliseconds-for-seconds slip or a bad value from a guest's run
/// would otherwise take the owner's account out for years. Beyond it the usual `OUT_FOR` applies.
const MAX_RESET_SECS: i64 = 8 * 24 * 3600;

/// Whether an exit report says the account ran out of usage.
pub(crate) fn says_out_of_usage(body: &serde_json::Value) -> bool {
    body.get("out_of_usage")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
}

/// `resets_at` of the report, epoch seconds: a float is truncated like Python's `int(until)`; none, a past value or one
/// further than [`MAX_RESET_SECS`] away is `None` (the default hour; `mark_out` also reads 0 that way).
#[allow(
    clippy::cast_possible_truncation,
    reason = "a float that fits the cap is far inside an i64; one that does not is rejected before the cast"
)]
fn resets_at(body: &serde_json::Value, now: i64) -> Option<i64> {
    let at = body.get("resets_at")?.as_f64()?;
    if !at.is_finite() || at.abs() > 1e15 {
        return None;
    }
    let secs = at.trunc() as i64;
    (secs <= now + MAX_RESET_SECS).then_some(secs)
}

/// What a usage-limit exit that ended the claim changes in the ack transaction. The caller has just moved the claim to
/// `ending` or `ended` and requeued (or left alone, if a stop already cancelled it) the turn. Records the reason on the
/// claim (it overrides a stop's or a holder change's, which already did their work on the turn, but never a revoke's). The
/// account's own mark is [`mark_out_of_usage`].
pub(crate) fn on_exit(tx: &Transaction<'_>, claim_id: i64) -> Result<()> {
    tx.execute(
        "UPDATE claims SET end_reason = ?2 WHERE claim_id = ?1 AND COALESCE(end_reason, '') <> 'revoked'",
        params![claim_id, OUT_OF_USAGE],
    )?;
    Ok(())
}

/// Marks the claim's account out for an exit report that says it ran out, when the report is the writer's own and the core
/// owns account state. Independent of what the report did to the claim, so it holds for a `done` that won the batch, an
/// exit after `end_gone`, and a repeat (extend-only). `writer_pid` is the pid the claim recorded; a report that names a
/// different process is not the writer's word, and `could_be_writer` is false for a claim without a pid that already left
/// `claimed`.
pub(crate) fn mark_out_of_usage(
    tx: &Transaction<'_>,
    claim_id: i64,
    writer_pid: Option<i64>,
    could_be_writer: bool,
    body: &str,
    now_ms: i64,
) -> Result<()> {
    let Ok(b) = serde_json::from_str::<serde_json::Value>(body) else {
        return Ok(());
    };
    if !says_out_of_usage(&b) || !could_be_writer {
        return Ok(());
    }
    let said = b.get("pid").and_then(serde_json::Value::as_i64);
    if writer_pid.is_some() && said.is_some() && said != writer_pid {
        return Ok(());
    }
    let account: Option<String> = tx
        .query_row(
            "SELECT account FROM claims WHERE claim_id = ?1",
            [claim_id],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    if let Some(account) = account
        && super::core_writes(tx)?
    {
        let now = now_ms / 1000;
        super::mark_out(tx, &account, resets_at(&b, now), "rate_limit", now, true)?;
    }
    Ok(())
}

/// The accounts this turn has already run out of usage on.
pub(crate) fn tried(conn: &Connection, turn: i64) -> Result<Vec<String>> {
    let mut q = conn.prepare(
        "SELECT DISTINCT account FROM claims WHERE turn = ?1 AND end_reason = ?2 AND account IS NOT NULL ORDER BY account",
    )?;
    let rows = q
        .query_map(params![turn, OUT_OF_USAGE], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// Where the next claim for a turn runs.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Next {
    Account(String),
    /// Every account is out or already tried by this turn; the notice for the user.
    Exhausted(String),
}

/// 9a's `pick` over core.db, minus what this turn already ran out on. With nothing tried it is the ordinary choice (the
/// first candidate; `pick` runs the first of the static order when everything is out, as the main account always did). Once
/// the turn has failed over, "everything out" or "nothing untried" ends the turn instead.
#[allow(
    clippy::cast_precision_loss,
    reason = "milliseconds since 1970 fit an f64's mantissa for the next 285,000 years"
)]
pub(crate) fn next_account(
    core: &Core,
    ctx: &Ctx<'_>,
    agent: &str,
    turn: Option<i64>,
    now_ms: i64,
) -> Result<Next> {
    let tried = match turn {
        Some(t) => tried(core.conn(), t)?,
        None => Vec::new(),
    };
    let settings = accounts::settings::read(ctx);
    let p = accounts::pick_from(
        ctx,
        &settings,
        agent,
        None,
        false,
        now_ms as f64 / 1000.0,
        &CoreState(core.conn()),
    );
    let first = p.candidates.iter().find(|c| !tried.contains(c));
    match first {
        Some(c) if tried.is_empty() || !p.all_out => Ok(Next::Account(c.clone())),
        _ => Ok(Next::Exhausted(all_out_text(&p, &tried, now_ms / 1000))),
    }
}

/// The notice for a turn with nothing to run on: who is out and when the first is back, and the accounts this turn ran
/// out on that carry no mark (unswitched, a mark already past, one cleared meanwhile).
fn all_out_text(p: &accounts::Pick, tried: &[String], now: i64) -> String {
    let outs: Vec<(&str, i64)> = p
        .skipped
        .iter()
        .filter_map(|s| Some((s.id.as_str(), s.status.out_until?)))
        .collect();
    let unmarked: Vec<&str> = tried
        .iter()
        .map(String::as_str)
        .filter(|t| !outs.iter().any(|(id, _)| id == t))
        .collect();
    if outs.is_empty() && unmarked.is_empty() {
        return "No account is ready to run this agent: none of its backup accounts is signed in and available."
            .into();
    }
    let mut list: Vec<String> = outs
        .iter()
        .map(|(id, until)| format!("{id} (back in {})", humanize(until - now)))
        .collect();
    list.extend(
        unmarked
            .iter()
            .map(|id| format!("{id} (ran out on this turn)")),
    );
    let list = list.join(", ");
    match outs.iter().map(|(_, u)| *u).min() {
        Some(first) => format!(
            "Every account for this agent is out of usage: {list}. The first is back in {}.",
            humanize(first - now)
        ),
        None => format!("Every account for this agent is out of usage: {list}."),
    }
}

/// "2h 15m", "40m", "less than a minute".
fn humanize(secs: i64) -> String {
    let minutes = (secs.max(0) + 59) / 60;
    match (minutes / 60, minutes % 60) {
        (0, 0) => "less than a minute".into(),
        (0, m) => format!("{m}m"),
        (h, _) if h >= 24 => format!("{}d {}h", h / 24, h % 24),
        (h, 0) => format!("{h}h"),
        (h, m) => format!("{h}h {m}m"),
    }
}

#[cfg(unix)]
/// How the new engine session is made on the account a failover picked.
pub(crate) struct NewSession<'a> {
    pub ctx: &'a threads::Ctx<'a>,
    pub cwd: &'a str,
    /// The id for a NEW session row. An account that already has one keeps its own: use `Started::Claimed.session`.
    pub session_id: &'a str,
}

#[cfg(unix)]
/// What [`claim_next`] did.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Started {
    Claimed {
        claimed: Claimed,
        account: String,
        /// True when the session row is new (the engine starts `session.session_id` empty and is told the latest
        /// messages); false when the account already had one, which the engine resumes whatever id was offered.
        fresh: bool,
        session: Box<EngineSession>,
    },
    /// The turn is `failed`: no account is left for it.
    Exhausted(String),
}

#[cfg(unix)]
/// The claim after a usage-limit exit (or any claim that should choose its account): picks, claims with `account` set,
/// starts the engine session on it. `req.account` is ignored. The claim is the usual step 1, so a second writer, a
/// revoked person or an unreconciled generation refuse it exactly as before. When nothing is left the turn is failed,
/// once (a turn that is no longer queued or parked is left as it is).
pub(crate) fn claim_next(
    core: &mut Core,
    ctx: &Ctx<'_>,
    req: &NewClaim<'_>,
    new: &NewSession<'_>,
    now_ms: i64,
) -> Result<Started> {
    let agent: String = core
        .conn()
        .query_row(
            "SELECT agent FROM conversations WHERE id = ?1",
            [req.conversation],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| {
            Error::Refused(format!("kith-core: no conversation {}", req.conversation))
        })?;
    let account = match next_account(core, ctx, &agent, req.turn, now_ms)? {
        Next::Account(a) => a,
        Next::Exhausted(text) => {
            if let Some(t) = req.turn {
                // A stopped ask's wake is cancelled, not failed: the fence is checked before the turn is marked.
                if let Some(fence) = stopped_fence(core, t)? {
                    cancel_fenced(core, req, &fence, now_ms);
                    return Err(fence.refusal(req));
                }
                let tx = core.claim_tx()?;
                tx.execute(
                    "UPDATE turns SET state = 'failed', finished_at = ?2
                     WHERE id = ?1 AND conversation = ?3 AND person = ?4 AND state IN ('queued', 'parked')",
                    params![t, now_ms, req.conversation, req.person],
                )?;
                tx.commit()?;
            }
            return Ok(Started::Exhausted(text));
        }
    };
    let pointer = transcripts::config_dir(new.ctx, &account)
        .map(|dir| {
            SessionPtr {
                config_dir: dir,
                project: transcripts::slug(new.cwd),
                sid: new.session_id.to_owned(),
            }
            .text()
        })
        .unwrap_or_default();
    // The claim and the session row are one transaction: a failing session write rolls the claim back (no `claimed` claim
    // left without a session), and the generation check of `claim_tx` covers both.
    let started = (|| {
        let generation = core.generation();
        let tx = core.claim_tx()?;
        let claimed = match claim_checked_in(
            &tx,
            generation,
            &NewClaim {
                account: Some(&account),
                conversation: req.conversation,
                kind: req.kind,
                person: req.person,
                turn: req.turn,
                holder_instance: req.holder_instance,
                process: req.process,
                revoke_seen: req.revoke_seen,
            },
            now_ms,
        )? {
            Ok(claimed) => claimed,
            Err(fence) => return Ok(Err(fence)),
        };
        let fresh = transcripts::rows::get(&tx, req.conversation, &account)?.is_none();
        let session = transcripts::rows::start(
            &tx,
            req.conversation,
            &account,
            new.cwd,
            new.session_id,
            &pointer,
            now_ms / 1000,
        )?;
        tx.commit()?;
        Ok::<_, Error>(Ok(Started::Claimed {
            claimed,
            account: account.clone(),
            fresh,
            session: Box::new(session),
        }))
    })()?;
    match started {
        Ok(started) => Ok(started),
        Err(fence) => {
            // Rolled back: a stopped ask's turn is cancelled in its own write. A revoked refusal is the claimer's stale
            // epoch and leaves the turn queued.
            cancel_fenced(core, req, &fence, now_ms);
            Err(fence.refusal(req))
        }
    }
}
