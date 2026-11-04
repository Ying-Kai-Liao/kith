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
//! No backup left: the turn ends `failed` (not requeued again) and [`Started::Exhausted`] carries the notice the host gives
//! today (`all_out_text` of `agent/backups.rs`). Python's chain run has no such notice (it prints the last attempt's own
//! output), and no golden holds a failover, so there is nothing to replay; the text is the host's.

use kith_kernel::accounts::{self, Ctx};
use kith_kernel::threads;
use rusqlite::{Connection, OptionalExtension as _, Transaction, params};

use super::CoreState;
use crate::conversations::claims::{Claimed, NewClaim, claim};
use crate::db::Core;
use crate::error::{Error, Result};
use crate::transcripts::{self, SessionPtr, rows::EngineSession};

/// `claims.end_reason` of a claim that ended on a usage limit.
pub(crate) const OUT_OF_USAGE: &str = "out_of_usage";

/// What a usage-limit exit changes in the ack transaction. The caller has just moved the claim to `ending` or `ended`
/// and requeued (or left alone, if a stop already cancelled it) the turn. Records the reason on the claim (it overrides
/// a stop's, which already did its work on the turn) and, only when the core owns account state, marks the account out.
pub(crate) fn on_exit(
    tx: &Transaction<'_>,
    claim_id: i64,
    resets_at: Option<i64>,
    now_ms: i64,
) -> Result<()> {
    tx.execute(
        "UPDATE claims SET end_reason = ?2 WHERE claim_id = ?1",
        params![claim_id, OUT_OF_USAGE],
    )?;
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
        super::mark_out(tx, &account, resets_at, "rate_limit", now_ms / 1000, true)?;
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
        _ => Ok(Next::Exhausted(all_out_text(&p, now_ms / 1000))),
    }
}

/// The notice for a turn with nothing to run on: who is out and when the first is back.
fn all_out_text(p: &accounts::Pick, now: i64) -> String {
    let outs: Vec<(&str, i64)> = p
        .skipped
        .iter()
        .filter_map(|s| Some((s.id.as_str(), s.status.out_until?)))
        .collect();
    if outs.is_empty() {
        return "No account is ready to run this agent: none of its backup accounts is signed in and available."
            .into();
    }
    let list = outs
        .iter()
        .map(|(id, until)| format!("{id} (back in {})", humanize(until - now)))
        .collect::<Vec<_>>()
        .join(", ");
    let first = outs.iter().map(|(_, u)| *u).min().unwrap_or(now);
    format!(
        "Every account for this agent is out of usage: {list}. The first is back in {}.",
        humanize(first - now)
    )
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

/// How the new engine session is made on the account a failover picked.
pub(crate) struct NewSession<'a> {
    pub ctx: &'a threads::Ctx<'a>,
    pub cwd: &'a str,
    /// The id the engine is told to use.
    pub session_id: &'a str,
}

/// What [`claim_next`] did.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Started {
    Claimed {
        claimed: Claimed,
        account: String,
        session: Box<EngineSession>,
    },
    /// The turn is `failed`: no account is left for it.
    Exhausted(String),
}

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
                let tx = core.claim_tx()?;
                tx.execute(
                    "UPDATE turns SET state = 'failed', finished_at = ?2
                     WHERE id = ?1 AND state IN ('queued', 'parked')",
                    params![t, now_ms],
                )?;
                tx.commit()?;
            }
            return Ok(Started::Exhausted(text));
        }
    };
    let claimed = claim(
        core,
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
    )?;
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
    let session = transcripts::rows::start(
        core.conn(),
        req.conversation,
        &account,
        new.cwd,
        new.session_id,
        &pointer,
        now_ms / 1000,
    )?;
    Ok(Started::Claimed {
        claimed,
        account,
        session: Box::new(session),
    })
}
