//! The kernel takers' door onto the writer claims (M3 P5; designs/2026-11-14-m3-cutover section 1.6): `claim.external`,
//! `claim.release`, `claim.sweepPerson` and `claim.epoch`, for `kith-sessions claim|terminal|resolve|release|proc|sweep` once
//! the claims are the core's, plus the read-only `claim.list` that `kith-wake` asks. The claim rows are for the Owner and Host
//! principals only (the table says so; `call::check` refuses every other); `claim.list` is the owner's read.
//!
//! One switch. The rows serve only while the `claims` cutover module is switched ([`switched`], the same `may_serve` every
//! module answers by); before that they refuse, so a marker written without the switch (a step part-way through, a forged
//! file) never makes core claims that a Files-mode host cannot see. Nothing here adds a rule: each row is one call of
//! `claims` / `warm` in one `claim_tx`, with the refusals a CLI must tell apart answered as data:
//!
//! | answer | meaning |
//! |---|---|
//! | `{refused: "held", holder}` | a writer holds the conversation (claimed, running or ending): `holder` is `{claimId, kind, pid}` |
//! | `{refused: "takeover", why}` | a take-over is queued behind the writer: the app is taking the session |
//! | `{refused: "waiting", why}` | an idle warm process holds the conversation and is being stopped, or the claims wait for this generation's reconcile: retry |
//! | `{refused: "revoked", why}` | the person's revoke epoch moved after `revokeSeen` |
//! | `{refused: "agent", why}` | the conversation belongs to another agent |
//!
//! The warm wait (P7's idle-warm rule, whose core half lives here): a `warm_procs` row that has the conversation loaded
//! is marked `kill_at`; the daemon's kill re-send signals it; each retry of this row first settles the rows whose process
//! group is gone ([`warm::settle`]) and ends the dead externals ([`claims::end_gone`]), so the claim is made only once the
//! group is empty. A writer claim of a running turn is a plain `held`: its process is never killed to make room. The
//! terminal entrance (`terminal.open` once switched, [`claim_terminal`]) asks the same questions through [`blocked_in`].
//!
//! A person's claim carries `revokeSeen`, the revoke epoch the caller read (`claim.epoch`) BEFORE the may check that let the
//! run in: a revoke that commits after it moves the epoch and the claim is refused (`c4-epochleak`). The row refuses a
//! person's claim without it, and the fence runs before the "same process asking again" answer, so a retry after a revoke
//! never gets its own `revoked` row handed back as a claim.
//!
//! A session id is a file name for the stub's lock (`a-z0-9._-`, the stub's own rule): any other id would run without the
//! lock, so these rows refuse it.

use rusqlite::{OptionalExtension as _, Transaction, params};
use serde_json::{Value, json};

use super::basis::OWNER;
use super::claims::{self, Claimed, Kind, NewClaim};
use super::warm;
use crate::db::Core;
use crate::error::{Error, Result};
use crate::gate::Liveness;

/// The `claim.*` methods this module answers.
pub(crate) fn serves(method: &str) -> bool {
    matches!(
        method,
        "claim.external" | "claim.release" | "claim.sweepPerson" | "claim.epoch" | "claim.list"
    )
}

/// Whether the claims are the core's on this Mac: the `claims` module reads `switched` (its switch file and the marker in
/// core.db; the tombstone in `live/terminal/` is not read here), through the same `may_serve` as every module. A core
/// opened without its folders cannot tell, and says no. Off Unix there is no cutover framework, so never switched.
#[cfg(unix)]
pub(crate) fn switched(core: &Core) -> bool {
    let Some(folders) = core.folders() else {
        return false;
    };
    let reg = crate::cutover::registry();
    let Some(m) = reg.iter().find(|m| m.name() == "claims") else {
        return false;
    };
    crate::cutover::same_answer::may_serve(
        m.as_ref(),
        &crate::cutover::Env::with_paths(folders),
        core.conn(),
        false,
    )
    .ok
}

#[cfg(not(unix))]
pub(crate) fn switched(_core: &Core) -> bool {
    false
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(0))
}

fn bad(what: &str) -> Error {
    Error::BadParams(format!("kith-core: claim: {what}"))
}

fn word<'a>(p: &'a Value, key: &str) -> Option<&'a str> {
    p.get(key).and_then(Value::as_str)
}

fn need<'a>(p: &'a Value, key: &str) -> Result<&'a str> {
    word(p, key).ok_or_else(|| bad(&format!("{key} is required")))
}

/// The principal of a claim: an empty one is the owner's.
fn principal(p: &Value) -> &str {
    match word(p, "person") {
        None | Some("") => OWNER,
        Some(w) => w,
    }
}

/// The stub's key rule (`kith-stub --key`): lowercase letters, digits, `.`, `_`, `-`; a name the lock folder can hold.
pub(crate) fn lockable(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id != "."
        && id != ".."
        && id.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

fn pid_of(p: &Value) -> Result<u32> {
    p.get("pid")
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .filter(|v| *v > 1)
        .ok_or_else(|| bad("pid is required"))
}

fn waiting(why: &str) -> Value {
    json!({ "refused": "waiting", "why": why })
}

/// `claim.*` through the real clock and the system's process table. Refused until the claims are switched.
pub(crate) fn call(core: &mut Core, method: &str, params: &Value) -> Result<Value> {
    if !switched(core) {
        return Err(Error::Refused(format!(
            "kith-core: {method}: the claims are not switched to the core on this Mac (kith migrate step claims)"
        )));
    }
    call_at(core, method, params, now_ms(), &crate::gate::System)
}

/// The rows without the switch: what the daemon's [`call`] runs once it has checked it, and what the tests drive.
pub(crate) fn call_at(
    core: &mut Core,
    method: &str,
    p: &Value,
    now: i64,
    live: &dyn Liveness,
) -> Result<Value> {
    match method {
        "claim.external" => external(core, p, now, live),
        "claim.release" => release(core, p, now),
        "claim.sweepPerson" => sweep_person(core, p, now),
        "claim.epoch" => Ok(json!({ "epoch": claims::revoke_epoch(core, principal(p))? })),
        "claim.list" => list(core),
        _ => Err(Error::NoSuchMethod(format!(
            "kith-core: no method {method}"
        ))),
    }
}

/// The conversation row for `conversation`, made when the core has not imported the session yet (the engine is `engine`,
/// default claude). A known row keeps its agent; a different agent named for it is refused, and so is an unknown
/// conversation with no agent (`claim_next` picks the account by agent and the wake compares agents, so an empty one is
/// never made).
fn know_in(
    tx: &Transaction<'_>,
    conversation: &str,
    agent: &str,
    engine: &str,
    now: i64,
) -> Result<Option<Value>> {
    let known: Option<String> = tx
        .query_row(
            "SELECT agent FROM conversations WHERE id = ?1",
            [conversation],
            |r| r.get(0),
        )
        .optional()?;
    match known {
        Some(a) if !a.is_empty() && !agent.is_empty() && a != agent => Ok(Some(json!({
            "refused": "agent",
            "why": format!("kith-core: conversation {conversation} belongs to {a}, not {agent}"),
        }))),
        Some(_) => Ok(None),
        None if agent.is_empty() => Err(bad(&format!(
            "agent is required for {conversation}, which the core does not know yet"
        ))),
        None => {
            tx.execute(
                "INSERT INTO conversations (id, agent, engine, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
                params![conversation, agent, engine, now],
            )?;
            Ok(None)
        }
    }
}

/// What stands in the way of a new external or terminal writer on `conversation` (the answer as data), or `None` when it
/// is free: a writer (`held`), a queued take-over (`takeover`), an idle warm process holding the conversation, which is
/// marked for a kill here and answers `waiting` (`r4-killidle`). The mark is a write: the caller commits on `waiting`.
fn blocked_in(tx: &Transaction<'_>, conversation: &str, now: i64) -> Result<Option<Value>> {
    let holder: Option<(i64, String, Option<i64>, String)> = tx
        .query_row(
            "SELECT claim_id, kind, pid, state FROM claims
             WHERE conversation = ?1 AND state IN ('claimed', 'running', 'ending')",
            [conversation],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    if let Some((id, kind, pid, state)) = holder {
        return Ok(Some(json!({
            "refused": "held",
            "why": format!("kith-core: conversation {conversation} has a writer: claim {id} is {state}"),
            "holder": { "claimId": id, "kind": kind, "pid": pid },
        })));
    }
    let queued = tx
        .query_row(
            "SELECT 1 FROM claims WHERE conversation = ?1 AND state = 'queued'",
            [conversation],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .is_some();
    if queued {
        return Ok(Some(json!({
            "refused": "takeover",
            "why": format!("kith-core: conversation {conversation} is being taken over in the app"),
        })));
    }
    // A process holds several sessions, and killing it ends them all: one that is running a turn of another of its sessions
    // is left alone (the terminal waits, as for a running claim) and is marked once it is idle. Only an idle process dies.
    let has = tx.execute(
        "UPDATE warm_procs SET kill_at = COALESCE(kill_at, ?2)
         WHERE (pid, started) IN (SELECT pid, started FROM warm_procs WHERE conversation = ?1)
           AND NOT EXISTS (
                SELECT 1 FROM claims c
                WHERE c.state IN ('claimed', 'running', 'ending') AND c.conversation <> ?1
                  AND (c.pid = warm_procs.pid
                       OR c.conversation IN (SELECT w.conversation FROM warm_procs w
                                             WHERE w.pid = warm_procs.pid AND w.started = warm_procs.started)))",
        params![conversation, now],
    )?;
    let loaded: i64 = tx.query_row(
        "SELECT count(*) FROM warm_procs WHERE conversation = ?1",
        [conversation],
        |r| r.get(0),
    )?;
    if has > 0 || loaded > 0 {
        return Ok(Some(waiting(&format!(
            "kith-core: waiting for the app's idle agent on {conversation} to stop"
        ))));
    }
    Ok(None)
}

/// Dead externals and stopped idle processes leave first, so a retry sees the world as it is now.
fn settle_all(core: &mut Core, live: &dyn Liveness, now: i64) -> Result<()> {
    claims::end_gone(core, live, now)?;
    warm::settle(core, live)?;
    Ok(())
}

/// Finishes a claim attempt whose refusal may be data: `waiting` committed its kill mark, every other refusal rolled back.
fn refusal(tx: Transaction<'_>, why: Value) -> Result<Value> {
    if why["refused"] == "waiting" && why.get("holder").is_none() {
        tx.commit()?;
    }
    Ok(why)
}

fn external(core: &mut Core, p: &Value, now: i64, live: &dyn Liveness) -> Result<Value> {
    let conversation = need(p, "conversation")?;
    if !lockable(conversation) {
        return Err(bad(&format!(
            "session id {conversation:?} cannot be locked (only a-z 0-9 . _ - are allowed)"
        )));
    }
    let pid = pid_of(p)?;
    let started = need(p, "started")?;
    if started.is_empty() {
        return Err(bad("started is required"));
    }
    let person = principal(p);
    let seen = match p.get("revokeSeen").and_then(Value::as_i64) {
        Some(n) => n,
        // Only the owner's claim may read the epoch here: nobody revokes the owner, so there is nothing to fence.
        None if person == OWNER => claims::revoke_epoch(core, person)?,
        None => {
            return Err(bad(&format!(
                "revokeSeen is required for {person}'s claim: read claim.epoch before the may check"
            )));
        }
    };
    settle_all(core, live, now)?;
    let generation = core.generation();
    let tx = core.claim_tx()?;
    // The fence first: a retry after a revoke must not be handed its own row back as a claim.
    if claims::revoke_epoch_in(&tx, person)? != seen {
        return Ok(json!({
            "refused": "revoked",
            "why": format!("kith-core: {person} was revoked after the may check"),
        }));
    }
    // The same process asking again (a retry after a lost answer, two calls at once) holds it already.
    let mine: Option<i64> = tx
        .query_row(
            "SELECT claim_id FROM claims
             WHERE conversation = ?1 AND kind = 'external' AND pid = ?2 AND pid_started = ?3
               AND state IN ('claimed', 'running')",
            params![conversation, pid, started],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = mine {
        return Ok(json!({ "claimId": id, "generation": generation, "again": true }));
    }
    let engine = word(p, "engine").unwrap_or("claude");
    if let Some(why) = know_in(
        &tx,
        conversation,
        word(p, "agent").unwrap_or_default(),
        engine,
        now,
    )? {
        return Ok(why);
    }
    if let Some(why) = blocked_in(&tx, conversation, now)? {
        return refusal(tx, why);
    }
    let req = NewClaim {
        conversation,
        kind: Kind::External,
        person,
        turn: None,
        holder_instance: None,
        account: None,
        process: Some((pid, started)),
        revoke_seen: seen,
    };
    match claims::claim_checked_in(&tx, generation, &req, now) {
        Ok(Ok(c)) => {
            tx.commit()?;
            Ok(json!({ "claimId": c.claim_id, "generation": c.generation }))
        }
        Ok(Err(fence)) => {
            Ok(json!({ "refused": fence.reason(), "why": fence.refusal(&req).to_string() }))
        }
        Err(Error::Refused(w)) if w.contains("wait for the reconcile") => Ok(waiting(&w)),
        Err(e) => Err(e),
    }
}

/// The terminal entrance (`terminal.open` once the claims are the core's): the owner's `terminal` claim on a session, after
/// the same take-over and idle-warm questions `claim.external` asks. `Ok(Err(data))` is a refusal as data (`held`,
/// `takeover`, `waiting`, `agent`); the claim has no process yet: the caller redeems it to the stub's pid.
pub(crate) fn claim_terminal(
    core: &mut Core,
    conversation: &str,
    agent: &str,
    now: i64,
) -> Result<std::result::Result<Claimed, Value>> {
    if !lockable(conversation) {
        return Err(bad(&format!(
            "session id {conversation:?} cannot be locked (only a-z 0-9 . _ - are allowed)"
        )));
    }
    settle_all(core, &crate::gate::System, now)?;
    let seen = claims::revoke_epoch(core, OWNER)?;
    let generation = core.generation();
    let tx = core.claim_tx()?;
    if let Some(why) = know_in(&tx, conversation, agent, "claude", now)? {
        return Ok(Err(why));
    }
    if let Some(why) = blocked_in(&tx, conversation, now)? {
        return refusal(tx, why).map(Err);
    }
    let req = NewClaim {
        conversation,
        kind: Kind::Terminal,
        person: OWNER,
        turn: None,
        holder_instance: None,
        account: None,
        process: None,
        revoke_seen: seen,
    };
    match claims::claim_checked_in(&tx, generation, &req, now) {
        Ok(Ok(c)) => {
            tx.commit()?;
            Ok(Ok(c))
        }
        Ok(Err(fence)) => Ok(Err(json!({
            "refused": fence.reason(), "why": fence.refusal(&req).to_string(),
        }))),
        Err(Error::Refused(w)) if w.contains("wait for the reconcile") => Ok(Err(waiting(&w))),
        Err(e) => Err(e),
    }
}

/// Gives back the owner's external claims of one process. A CAS per claim, only while it is `claimed` or `running` (an
/// `ending` one is being killed and `end_gone` ends it). A person's claim is not given back this way: it ends by `end_gone`
/// when its process is gone or by a revoke, so a run cannot unregister itself ahead of one.
fn release(core: &mut Core, p: &Value, now: i64) -> Result<Value> {
    let pid = pid_of(p)?;
    let started = need(p, "started")?;
    let only = word(p, "conversation");
    let tx = core.claim_tx()?;
    let ids: Vec<i64> = {
        let mut st = tx.prepare(
            "SELECT claim_id FROM claims
             WHERE kind = 'external' AND person = ?4 AND pid = ?1 AND pid_started = ?2
               AND state IN ('claimed', 'running') AND (?3 IS NULL OR conversation = ?3)",
        )?;
        st.query_map(params![pid, started, only, OWNER], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?
    };
    let mut ended = Vec::new();
    for id in ids {
        let n = tx.execute(
            "UPDATE claims SET state = 'ended', ended_at = ?2, end_reason = COALESCE(end_reason, 'released')
             WHERE claim_id = ?1 AND state IN ('claimed', 'running')",
            params![id, now],
        )?;
        if n == 1 {
            ended.push(id);
        }
    }
    tx.commit()?;
    Ok(json!({ "ended": ended }))
}

fn sweep_person(core: &mut Core, p: &Value, now: i64) -> Result<Value> {
    let persons: Vec<&str> = p
        .get("persons")
        .and_then(Value::as_array)
        .ok_or_else(|| bad("persons is required"))?
        .iter()
        .map(|w| w.as_str().ok_or_else(|| bad("persons are names")))
        .collect::<Result<_>>()?;
    if persons.iter().any(|w| w.is_empty() || *w == OWNER) {
        return Err(Error::Refused(
            "kith-core: claim.sweepPerson: the owner's claims are never swept".into(),
        ));
    }
    let mut kill: Vec<claims::Kill> = Vec::new();
    for who in persons {
        kill.extend(claims::revoke_person(core, who, now)?.kill);
    }
    let mut seen = std::collections::HashSet::new();
    kill.retain(|k| seen.insert((k.claim_id, k.pid)));
    // The conversation of each kill, so the caller can write its "access revoked" intent before it signals.
    let conversation_of = |id: Option<i64>| -> Option<String> {
        core.conn()
            .query_row(
                "SELECT conversation FROM claims WHERE claim_id = ?1",
                [id?],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten()
    };
    Ok(json!({ "kill": kill.iter().map(|k| json!({
        "claimId": k.claim_id, "pid": k.pid, "started": k.started, "conversation": conversation_of(k.claim_id),
    })).collect::<Vec<_>>() }))
}

/// `{live, recent}`: the live claims (queued, claimed, running, ending) and the last twenty ended ones.
fn list(core: &Core) -> Result<Value> {
    let rows = |sql: &str| -> Result<Vec<Value>> {
        let mut st = core.conn().prepare(sql)?;
        let out = st
            .query_map([], |r| {
                Ok(json!({
                    "claimId": r.get::<_, i64>(0)?,
                    "conversation": r.get::<_, String>(1)?,
                    "state": r.get::<_, String>(2)?,
                    "kind": r.get::<_, String>(3)?,
                    "person": r.get::<_, String>(4)?,
                    "pid": r.get::<_, Option<i64>>(5)?,
                    "started": r.get::<_, Option<String>>(6)?,
                    // The conversation's agent (fixed for its life) and the holder instance the claim went to: the host judges a
                    // narrowed sweep by the agent, and tells its own turn from an earlier start's by the instance.
                    "agent": r.get::<_, Option<String>>(7)?,
                    "holderInstance": r.get::<_, Option<String>>(8)?,
                }))
            })?
            .collect::<std::result::Result<_, _>>()?;
        Ok(out)
    };
    let cols = "SELECT c.claim_id, c.conversation, c.state, c.kind, c.person, c.pid, c.pid_started, v.agent, c.holder_instance
                FROM claims c LEFT JOIN conversations v ON v.id = c.conversation";
    Ok(json!({
        "live": rows(&format!(
            "{cols} WHERE c.state IN ('queued', 'claimed', 'running', 'ending') ORDER BY c.claim_id"
        ))?,
        "recent": rows(&format!(
            "{cols} WHERE c.state = 'ended' ORDER BY c.claim_id DESC LIMIT 20"
        ))?,
    }))
}

#[cfg(test)]
#[path = "external_tests.rs"]
mod tests;
