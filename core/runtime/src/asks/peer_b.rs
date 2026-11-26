//! What the link carries beyond A5a's rows (design 2026-11-24-ask-ledger §9, A5b): the callee's end answers its caller's follow
//! and stop without a write the caller did not ask for, keeps its books (at most two runs and ten waiting asks per computer, a
//! queued ask expires after a day), and the caller's end keeps the `--to` messages it owes.
//!
//! - **`ask.peerThread`** (callee): the rows of an origin ask after the caller's last, its state and cost. Read only: the rows are
//!   never marked read here (nobody here reads them), so a send that is answered twice gives the same rows twice.
//! - **`ask.peerStop`** (callee): stops the origin ask of (computer, key); a stop that comes before the ask leaves a `stopped`
//!   row, so the ask of that key, when it arrives, finds it and never runs (`MacLink2` C1).
//! - **Limits** (mac-link, "Limits"): a computer has at most [`MAX_QUEUED`] asks waiting here (a refused row says so) and at most
//!   [`MAX_RUNNING`] running at once (the claim of one more is refused as `held`: the turn stays queued and starts when a run ends).
//! - **Expiry**: an origin ask whose turn nobody claimed in [`EXPIRE_MS`] expires `lost`, by this computer's clock.
//! - **`--to`** (caller): `ask.to` with `text` keeps the message on the peer ask; `ask.peerOwed` lists it until `ask.peerApply`
//!   says it was sent (`toSent`).

use std::collections::BTreeSet;

use rusqlite::{Connection, OptionalExtension as _, params};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::peer::{by_origin_in, closed_origin_in, is_live, live_in};
use super::rows::{self, Kind};
use super::{Ask, EndReq, Origin, check_to, end_tree_in, get, load_all, settle_in, waiting};
use crate::db::Core;
use crate::error::{Error, Result};
use crate::wake;

/// Asks one computer may have waiting here (turn queued, not yet started).
pub(crate) const MAX_QUEUED: i64 = 10;
/// Runs of one computer's asks at once; the next waits for a free place.
pub(crate) const MAX_RUNNING: i64 = 2;
/// How long an ask may wait unstarted (mac-link: B's own clock).
pub(crate) const EXPIRE_MS: i64 = 24 * 3600 * 1000;

#[allow(clippy::cast_precision_loss)]
fn secs(now: i64) -> f64 {
    now as f64 / 1000.0
}

/// The words of the refusal a call about an epoch that is not the live one gets. The host tells a caller it is no longer paired
/// by them.
pub(crate) fn not_paired(computer: &str, epoch: &str) -> String {
    format!(
        "kith-core: asks: {computer} is not paired with this computer at epoch {epoch}; the call is refused"
    )
}

/// Whether a refusal says the pairing is gone ([`not_paired`]).
pub(crate) fn is_not_paired(why: &str) -> bool {
    why.contains("is not paired with this computer")
}

/// The first twelve hex digits of the digest of `text`: what names a `--to` message (the same words again are one message).
fn sha12(text: &str) -> String {
    use std::fmt::Write as _;
    let digest = Sha256::digest(text.as_bytes());
    let mut hex = String::new();
    for b in digest.iter().take(6) {
        let _ = write!(hex, "{b:02x}");
    }
    hex
}

/// An open of an ask from `computer` finds its places: too many waiting asks are refused (as a row, by the open).
pub(super) fn room_in(tx: &Connection, computer: &str) -> Result<()> {
    let waiting: i64 = tx.query_row(
        "SELECT COUNT(*) FROM asks a JOIN turns t ON t.id = a.turn
         WHERE a.origin_computer = ?1 AND a.state = 'open' AND a.ended_ms IS NULL AND t.state = 'queued'",
        [computer],
        |r| r.get(0),
    )?;
    if waiting >= MAX_QUEUED {
        return Err(Error::Refused(format!(
            "kith-core: asks: {computer} already has {MAX_QUEUED} asks waiting here; wait for some to start"
        )));
    }
    Ok(())
}

/// The fence of a claim that is about running: the computer whose ask `turn` is, when it already has [`MAX_RUNNING`] other runs
/// going. The claim is refused for now (the turn stays queued), never cancelled.
pub(crate) fn busy_origin_in(tx: &Connection, turn: i64) -> Result<Option<String>> {
    let found: Option<(String, i64)> = tx.query_row(
        "SELECT a.origin_computer,
                (SELECT COUNT(*) FROM asks b JOIN turns u ON u.id = b.turn
                 WHERE b.origin_computer = a.origin_computer AND u.id <> t.id AND u.state IN ('claimed', 'running'))
         FROM turns t JOIN asks a ON a.turn = t.id
         WHERE t.id = ?1 AND t.source = 'ask' AND a.origin_computer IS NOT NULL",
        [turn],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()?;
    if let Some((c, n)) = found
        && n >= MAX_RUNNING
    {
        return Ok(Some(c));
    }
    // A pairing the host has not confirmed since this core started may already be ended on the host (an unlink while the core
    // was away): nothing of it starts until the host's reconcile has told the core (`ask.peerLink` again), and the turn waits.
    Ok(tx
        .query_row(
            "SELECT a.origin_computer FROM turns t JOIN asks a ON a.turn = t.id
             WHERE t.id = ?1 AND t.source = 'ask' AND a.origin_computer IS NOT NULL
               AND NOT EXISTS (SELECT 1 FROM ask_peers p
                               WHERE p.computer = a.origin_computer AND p.dir = 'in'
                                 AND p.epoch = a.origin_epoch AND p.live = 1 AND p.boot = ?2)",
            params![turn, super::peer::boot()],
            |r| r.get(0),
        )
        .optional()?)
}

/// The tick's expiry: an ask that came from another computer and whose turn nobody claimed in [`EXPIRE_MS`] ends `lost`, with
/// its turn expired. Answers the ids. (A caller that was cut off meanwhile, past its own wait, finds the row final.)
pub(crate) fn expire_unstarted_in(
    tx: &Connection,
    machine: &std::path::Path,
    now: i64,
) -> Result<Vec<String>> {
    let old: Vec<(String, i64)> = {
        let mut stmt = tx.prepare(
            "SELECT a.id, a.turn FROM asks a JOIN turns t ON t.id = a.turn
             WHERE a.origin_computer IS NOT NULL AND a.state = 'open' AND a.ended_ms IS NULL
               AND t.state = 'queued' AND COALESCE(a.at_ms, 0) < ?1 ORDER BY a.id",
        )?;
        stmt.query_map([now - EXPIRE_MS], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<_, _>>()?
    };
    let mut done = Vec::new();
    for (id, turn) in old {
        let n = tx.execute(
            "UPDATE turns SET state = 'expired', finished_at = ?2 WHERE id = ?1 AND state = 'queued'",
            params![turn, now],
        )?;
        if n == 0 {
            continue;
        }
        settle_in(tx, machine, &id, 0.0, Some("lost"), now)?;
        let Some(mut a) = get(tx, &id)? else { continue };
        a.done = true;
        let text = format!(
            "{} was not started within 24 hours; the ask expired",
            a.asked.as_deref().unwrap_or("the agent")
        );
        rows::add_in(tx, &id, a.asked.as_deref(), Kind::Lost, &text, now)?;
        a.put(tx)?;
        done.push(id);
    }
    Ok(done)
}

/// What an origin ask and everything it asked have cost here, in US$ (the run and its descendants, not the caller's base).
fn cost_of(a: &Ask, all: &[Ask]) -> f64 {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    seen.insert(a.id.as_str());
    let mut sum = a.cost.unwrap_or(0.0);
    loop {
        let more: Vec<&Ask> = all
            .iter()
            .filter(|c| {
                !seen.contains(c.id.as_str())
                    && c.parent.as_deref().is_some_and(|p| seen.contains(p))
            })
            .collect();
        if more.is_empty() {
            break;
        }
        for c in more {
            seen.insert(c.id.as_str());
            sum += c.cost.unwrap_or(0.0);
        }
    }
    sum
}

/// `ask.peerThread` (callee): the origin ask of (`computer`, `key`) as its caller follows it. Answers `{found: false}` for a key
/// this computer never saw, else `{found, id, agent, state, rows (after `after`), cost, waiting, session, conversation, takesTo}`.
/// Refused when `epoch` is not the live pairing with `computer` (`not_paired`). Writes nothing.
pub(crate) fn thread(
    core: &Core,
    computer: &str,
    key: &str,
    epoch: &str,
    after: i64,
) -> Result<Value> {
    let conn = core.conn();
    if !live_in(conn, computer, "in", epoch)? {
        return Err(Error::Refused(not_paired(computer, epoch)));
    }
    let Some(a) = by_origin_in(conn, computer, key)? else {
        return Ok(json!({"found": false}));
    };
    let all = load_all(conn)?;
    let mut stmt = conn.prepare(
        "SELECT n, \"as\", kind, text, at_ms FROM ask_rows WHERE ask = ?1 AND n > ?2 ORDER BY n",
    )?;
    let listed = stmt
        .query_map(params![a.id, after], |r| {
            Ok(json!({"n": r.get::<_, i64>(0)?, "as": r.get::<_, Option<String>>(1)?,
                      "kind": r.get::<_, String>(2)?, "text": r.get::<_, String>(3)?, "at": r.get::<_, i64>(4)?}))
        })?
        .collect::<std::result::Result<Vec<Value>, _>>()?;
    Ok(json!({
        "found": true, "id": a.id, "agent": a.asked, "state": a.state, "rows": listed,
        "cost": cost_of(&a, &all), "waiting": waiting::of(&a, &all),
        "session": a.session, "conversation": a.conversation,
        "takesTo": check_to(conn, &a.id).is_ok(),
    }))
}

/// `ask.peerStop` (callee): ends the origin ask of (`computer`, `key`) in one transaction (its run's claim goes `ending`, which
/// the waiter reads as a stop; a queued or claimed turn is ended). An ask that is over already answers its state. A key this
/// computer never saw gets a `stopped` row under `id`, so the ask, if it ever arrives, finds it stopped. Answers `{state}`.
pub(crate) fn stop(
    core: &mut Core,
    id: &str,
    computer: &str,
    key: &str,
    epoch: &str,
    now: i64,
) -> Result<Value> {
    wake::write_tx(core, secs(now), |tx, st| {
        if !live_in(tx, computer, "in", epoch)? {
            return Err(Error::Refused(not_paired(computer, epoch)));
        }
        let Some(a) = by_origin_in(tx, computer, key)? else {
            if get(tx, id)?.is_some() {
                return Err(Error::Refused(format!(
                    "kith-core: asks: {id} is not this key's"
                )));
            }
            let og = Origin {
                computer,
                key,
                epoch,
            };
            closed_origin_in(
                tx,
                id,
                None,
                None,
                og,
                "stopped",
                "stopped before it started",
                now,
            )?;
            return Ok(json!({"state": "stopped"}));
        };
        if !is_live(&a) {
            return Ok(json!({"state": a.state}));
        }
        let ended = end_tree_in(
            tx,
            &EndReq {
                roots: std::slice::from_ref(&a.id),
                sessions: &[],
                persons: &[],
            },
            now,
        )?;
        for s in &ended.sessions {
            wake::withdraw_in(tx, st, secs(now), s, None)?;
        }
        Ok(json!({"state": "stopped"}))
    })
}

/// `ask.to` with a message (caller): a peer ask keeps the message to send (`ask.peerOwed`'s `to`); the same words again are one
/// message. Any other ask answers as `ask.to` always did and keeps nothing. An ended or stopped ask refuses (`check_to`).
pub(crate) fn to_owed(core: &mut Core, id: &str, text: &str) -> Result<Value> {
    let tx = core.claim_tx()?;
    check_to(&tx, id)?;
    let Some(mut a) = get(&tx, id)? else {
        return Err(Error::Refused(format!("kith-core: asks: no ask {id}")));
    };
    if a.mode.as_deref() != Some("peer") {
        return Ok(json!({"id": id, "ok": true}));
    }
    let k = sha12(text);
    let mut extra = a.extra.take().unwrap_or_default();
    let mut list = extra
        .get("to")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !list.iter().any(|t| t["k"] == json!(k)) {
        list.push(json!({"k": k, "text": text}));
    }
    extra.insert("to".to_owned(), Value::Array(list));
    a.extra = Some(extra);
    a.put(&tx)?;
    let peer = a.peer.clone();
    tx.commit()?;
    Ok(json!({"id": id, "ok": true, "peer": peer, "k": k}))
}

/// The `--to` messages a peer ask still owes its computer: `[{k, text}]`.
pub(super) fn owed_to(a: &Ask) -> Vec<(String, String)> {
    a.extra
        .as_ref()
        .and_then(|e| e.get("to"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|t| Some((t["k"].as_str()?.to_owned(), t["text"].as_str()?.to_owned())))
        .collect()
}

/// Forgets the messages named `sent` (the computer took them).
pub(super) fn clear_to(a: &mut Ask, sent: &[String]) {
    if sent.is_empty() {
        return;
    }
    let Some(extra) = a.extra.as_mut() else {
        return;
    };
    if let Some(list) = extra.get_mut("to").and_then(Value::as_array_mut) {
        list.retain(|t| !t["k"].as_str().is_some_and(|k| sent.iter().any(|s| s == k)));
    }
    if extra
        .get("to")
        .and_then(Value::as_array)
        .is_some_and(Vec::is_empty)
    {
        extra.remove("to");
    }
}
