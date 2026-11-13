//! The `hostclaim.*` rows (M3-10f): the host's way onto the writer claims of `conversations::claims`. Host principal only (the
//! table says so; `call::check` refuses every other), unswitched: the host calls them only when it runs with
//! `KITH_HOST_CORE_CLAIMS=1`. Nothing here adds a rule: each row is one call of the claims API (one `claim_tx` each), with
//! the refusals the host must tell apart answered as data instead of an error:
//!
//! | answer | meaning |
//! |---|---|
//! | `{refused: "held", why}` | a writer holds the conversation (claimed, running or ending): the turn stays queued or parks |
//! | `{refused: "stopped", why}` | the conversation's ask was stopped: the core has cancelled the turn already |
//! | `{refused: "revoked", why}` | the person's revoke epoch moved after the caller's may check: the caller cancels the turn |
//! | `{refused: "waiting", why}` | the claims wait for this generation's reconcile |
//! | `{refused: "notWaiting", why}` | the turn is not waiting for that person (cancelled, expired, claimed) |
//! | `{refused: "budget", why}` | the turn's per-message budget is spent (R-f): the caller drops the item with the note's line |
//! | `{refused: "other", why}` | any other refusal (an unknown conversation) |
//!
//! A conversation is the Claude session id the host names its claim files by.

use serde_json::{Value, json};

use crate::conversations::basis::{self, Basis, Chain, Level};
use crate::conversations::claims::{self, Kind, NewClaim, NewTurn, Redeemed, Reported};
use crate::conversations::warm::{self, WarmProc};
use crate::db::Core;
use crate::error::{Error, Result};

/// `hostclaim.*` methods this module answers.
pub(crate) fn serves(method: &str) -> bool {
    method.starts_with("hostclaim.")
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(0))
}

fn bad(what: &str) -> Error {
    Error::BadParams(format!("kith-core: hostclaim: {what}"))
}

fn word<'a>(p: &'a Value, key: &str) -> Option<&'a str> {
    p.get(key).and_then(Value::as_str)
}

fn need<'a>(p: &'a Value, key: &str) -> Result<&'a str> {
    word(p, key).ok_or_else(|| bad(&format!("{key} is required")))
}

/// The principal of a claim: an empty one is the owner's, as the wake queue names it.
fn person(p: &Value) -> Result<&str> {
    need(p, "person").map(|w| if w.is_empty() { "owner" } else { w })
}

fn int(p: &Value, key: &str) -> Result<i64> {
    p.get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| bad(&format!("{key} is required")))
}

/// The room fields of `hostclaim.enqueue` (R-a, R-b, R-f, R-g): `None` for a plain turn, which is an owner-level one with
/// no budget. A turn is a room turn when it names `basis`, `room` or `level`.
fn basis_of(p: &Value) -> Result<Option<Basis>> {
    let level = match word(p, "level") {
        None | Some("owner") => Level::Owner,
        Some("room") => Level::Room,
        Some(_) => return Err(bad("level is owner or room")),
    };
    let listed: Vec<&str> = match p.get("basis") {
        None | Some(Value::Null) => Vec::new(),
        Some(v) => v
            .as_array()
            .ok_or_else(|| bad("basis is a list of people"))?
            .iter()
            .map(|w| w.as_str().ok_or_else(|| bad("basis is a list of people")))
            .collect::<Result<_>>()?,
    };
    let touched = !listed.is_empty()
        || level == Level::Room
        || [
            "room",
            "joinEpoch",
            "gen",
            "author",
            "budget",
            "budgetFrom",
            "key",
        ]
        .iter()
        .any(|k| p.get(*k).is_some_and(|v| !v.is_null()));
    if !touched {
        return Ok(None);
    }
    let epochs = p.get("epochs").and_then(Value::as_object);
    let mut people = Vec::new();
    for who in listed {
        let seen = epochs
            .and_then(|e| e.get(who))
            .and_then(Value::as_i64)
            .ok_or_else(|| bad(&format!("epochs names no revoke epoch for {who}")))?;
        people.push((who.to_owned(), seen));
    }
    let chain = match (
        p.get("budget").and_then(Value::as_i64),
        p.get("budgetFrom").and_then(Value::as_i64),
    ) {
        (None, None) => Chain::None,
        (Some(n), None) => Chain::Root(n),
        (_, Some(from)) => Chain::Follow(from),
    };
    Ok(Some(Basis {
        people,
        room: word(p, "room").map(str::to_owned),
        room_gen: p.get("gen").and_then(Value::as_i64),
        level,
        join_epoch: p.get("joinEpoch").and_then(Value::as_i64),
        author: word(p, "author").map(str::to_owned),
        chain,
        key: word(p, "key").map(str::to_owned),
    }))
}

/// A refusal of the claims API as data (see the table above); any other error stays an error.
fn refusal(e: Error) -> Result<Value> {
    let (kind, why) = match &e {
        Error::Stopped(w) => ("stopped", w.clone()),
        Error::Refused(w) if w.contains("has a writer") => ("held", w.clone()),
        Error::Refused(w) if w.contains("turn budget of the chain") => ("budget", w.clone()),
        Error::Refused(w) if w.contains("was revoked") => ("revoked", w.clone()),
        Error::Refused(w) if w.contains("wait for the reconcile") => ("waiting", w.clone()),
        Error::Refused(w) if w.contains("is not waiting for") => ("notWaiting", w.clone()),
        Error::Refused(w) => ("other", w.clone()),
        _ => return Err(e),
    };
    Ok(json!({ "refused": kind, "why": why }))
}

fn claim_json(c: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "claimId": c.get::<_, i64>(0)?,
        "conversation": c.get::<_, String>(1)?,
        "state": c.get::<_, String>(2)?,
        "kind": c.get::<_, String>(3)?,
        "person": c.get::<_, Option<String>>(4)?.unwrap_or_default(),
        "pid": c.get::<_, Option<i64>>(5)?,
        "started": c.get::<_, Option<String>>(6)?,
        // Which holder the claim went to: the host tells its own claims from another instance's by it, not by memory.
        "holderInstance": c.get::<_, Option<String>>(7)?,
    }))
}

const SHOW: &str = "SELECT claim_id, conversation, state, kind, person, pid, pid_started, holder_instance FROM claims";

pub(crate) fn call(core: &mut Core, method: &str, params: &Value) -> Result<Value> {
    call_at(core, method, params, now_ms())
}

#[allow(clippy::too_many_lines, reason = "one arm per row")]
pub(crate) fn call_at(core: &mut Core, method: &str, p: &Value, now: i64) -> Result<Value> {
    match method {
        "hostclaim.ensure" => {
            let (id, agent, engine) = (
                need(p, "conversation")?,
                need(p, "agent")?,
                need(p, "engine")?,
            );
            if let Some(room) = word(p, "room") {
                // R-g: the conversation of one (agent, room, join epoch); a second one is refused.
                return match basis::ensure_room(
                    core,
                    id,
                    agent,
                    engine,
                    room,
                    int(p, "joinEpoch")?,
                    now,
                ) {
                    Ok(()) => Ok(json!({ "conversation": id })),
                    Err(e) => refusal(e),
                };
            }
            let tx = core.claim_tx()?;
            // The same insert `enqueue_turn_in` makes; a row that exists keeps its agent and engine.
            tx.execute(
                "INSERT OR IGNORE INTO conversations (id, agent, engine, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
                rusqlite::params![id, agent, engine, now],
            )?;
            tx.commit()?;
            Ok(json!({ "conversation": id }))
        }
        "hostclaim.proc" => {
            let pid = u32::try_from(int(p, "pid")?).map_err(|_| bad("pid"))?;
            let w = WarmProc {
                pid,
                started: need(p, "started")?,
                instance: need(p, "instance")?,
                person: word(p, "person").unwrap_or_default(),
                agent: word(p, "agent").unwrap_or_default(),
                conversation: word(p, "conversation"),
                revoke_seen: p.get("revokeSeen").and_then(Value::as_i64),
            };
            match warm::register(core, &w) {
                Ok(()) => Ok(json!({ "registered": true })),
                Err(e) => refusal(e),
            }
        }
        "hostclaim.procGone" => {
            let pid = u32::try_from(int(p, "pid")?).map_err(|_| bad("pid"))?;
            Ok(json!({ "gone": warm::gone(core, pid, need(p, "started")?)? }))
        }
        "hostclaim.adopt" => {
            let a = warm::adopt(core, need(p, "instance")?, now)?;
            Ok(json!({
                "ended": a.ended,
                "kill": a.kill.iter().map(|k| json!({
                    "claimId": k.claim_id, "pid": k.pid, "started": k.started,
                })).collect::<Vec<_>>(),
            }))
        }
        "hostclaim.epoch" => Ok(json!({ "epoch": claims::revoke_epoch(core, person(p)?)? })),
        "hostclaim.claim" => {
            let kind = match need(p, "kind")? {
                "terminal" => Kind::Terminal,
                "external" => Kind::External,
                _ => {
                    return Err(bad(
                        "kind is terminal or external (a turn claims through hostclaim.turn)",
                    ));
                }
            };
            let pid = p
                .get("pid")
                .and_then(Value::as_u64)
                .and_then(|v| u32::try_from(v).ok());
            let started = word(p, "started");
            let req = NewClaim {
                conversation: need(p, "conversation")?,
                kind,
                person: person(p)?,
                turn: None,
                holder_instance: word(p, "holderInstance"),
                account: None,
                process: pid.zip(started),
                revoke_seen: int(p, "revokeSeen")?,
            };
            match claims::claim(core, &req, now) {
                Ok(c) => Ok(json!({ "claimId": c.claim_id, "generation": c.generation })),
                Err(e) => refusal(e),
            }
        }
        "hostclaim.enqueue" => {
            let t = NewTurn {
                conversation: need(p, "conversation")?,
                agent: need(p, "agent")?,
                engine: need(p, "engine")?,
                source: need(p, "source")?,
                person: person(p)?,
                prompt: need(p, "prompt")?,
                deadline: p.get("deadline").and_then(Value::as_i64),
                revoke_seen: int(p, "revokeSeen")?,
            };
            let queued = match basis_of(p)? {
                Some(b) => basis::enqueue(core, &t, &b, now),
                None => claims::enqueue_turn(core, &t, now),
            };
            match queued {
                Ok(id) => Ok(json!({ "turn": id })),
                Err(e) => refusal(e),
            }
        }
        "hostclaim.turn" => turn(core, p, now),
        "hostclaim.redeem" => {
            let pid = u32::try_from(int(p, "pid")?).map_err(|_| bad("pid"))?;
            Ok(
                match claims::redeem(
                    core,
                    int(p, "claimId")?,
                    pid,
                    need(p, "started")?,
                    need(p, "holderInstance")?,
                    now,
                )? {
                    Redeemed::Running { again } => json!({ "running": true, "again": again }),
                    Redeemed::Refused(state) => json!({ "refused": state }),
                },
            )
        }
        "hostclaim.report" => Ok(
            match claims::report(
                core,
                int(p, "claimId")?,
                need(p, "kind")?,
                need(p, "body")?,
                now,
            )? {
                Reported::Unknown => json!({ "unknown": true }),
                Reported::Stored { applied } => json!({ "stored": true, "applied": applied }),
            },
        ),
        "hostclaim.stop" => {
            let requeue = p.get("requeue").and_then(Value::as_bool).unwrap_or(false);
            let kill = claims::stop(core, int(p, "claimId")?, need(p, "reason")?, requeue, now)?;
            Ok(json!({ "kill": kill.map(|k| json!({
                "claimId": k.claim_id, "pid": k.pid, "started": k.started,
            })) }))
        }
        "hostclaim.settle" => {
            let ended = claims::end_gone(core, &crate::gate::System, now)?;
            warm::settle(core, &crate::gate::System)?;
            Ok(json!({ "ended": ended }))
        }
        "hostclaim.show" => {
            let one = |sql: String, arg: rusqlite::types::Value| -> Result<Value> {
                Ok(core
                    .conn()
                    .query_row(&sql, [arg], claim_json)
                    .map(Some)
                    .or_else(|e| match e {
                        rusqlite::Error::QueryReturnedNoRows => Ok(None),
                        e => Err(e),
                    })?
                    .unwrap_or(Value::Null))
            };
            let claim = if let Some(id) = p.get("claimId").and_then(Value::as_i64) {
                one(format!("{SHOW} WHERE claim_id = ?1"), id.into())?
            } else {
                one(
                    format!(
                        "{SHOW} WHERE conversation = ?1 AND state IN ('claimed', 'running', 'ending')"
                    ),
                    need(p, "conversation")?.to_owned().into(),
                )?
            };
            Ok(json!({ "claim": claim }))
        }
        "hostclaim.cancel" => {
            let turn = int(p, "turn")?;
            let reason = need(p, "reason")?;
            let tx = core.claim_tx()?;
            let done = claims::cancel_waiting_in(&tx, &[turn], reason, now)?;
            tx.commit()?;
            Ok(json!({ "cancelled": !done.is_empty() }))
        }
        "hostclaim.park" => match claims::park(core, int(p, "turn")?, need(p, "reason")?, now) {
            Ok(parked) => Ok(json!({ "parked": parked })),
            Err(e) => refusal(e),
        },
        "hostclaim.unpark" => {
            let flag = |k: &str| p.get(k).and_then(Value::as_bool).unwrap_or(false);
            let turns = claims::unpark_ready(
                core,
                need(p, "conversation")?,
                flag("lockFree"),
                flag("terminalLive"),
                now,
            )?;
            Ok(json!({ "turns": turns }))
        }
        _ => Err(Error::NoSuchMethod(format!(
            "kith-core: no method {method}"
        ))),
    }
}

#[cfg(unix)]
fn turn(core: &mut Core, p: &Value, now: i64) -> Result<Value> {
    use crate::accounts::failover::{NewSession, Started, claim_next};
    use kith_kernel::accounts::{Ctx, System};

    let (folders, kernel) = core.folders_for(p)?;
    let env = p.get("env").and_then(Value::as_object);
    let lookup = |name: &str| {
        env.and_then(|e| e.get(name))
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    let system = System {
        kernel: kernel.clone(),
    };
    let ctx = Ctx {
        machine: folders.machine.clone(),
        agents: folders.agents.clone(),
        kernel,
        home: folders.home.clone(),
        claude_config_dir: lookup("CLAUDE_CONFIG_DIR"),
        out: &system,
    };
    let tctx = kith_kernel::threads::Ctx {
        state: Some(folders.state.clone()),
        machine: Some(folders.machine.clone()),
        home: folders.home.clone(),
        env: &lookup,
    };
    let req = NewClaim {
        conversation: need(p, "conversation")?,
        kind: Kind::Turn,
        person: person(p)?,
        turn: Some(int(p, "turn")?),
        holder_instance: word(p, "holderInstance"),
        account: None,
        process: None,
        revoke_seen: int(p, "revokeSeen")?,
    };
    let started = claim_next(
        core,
        &ctx,
        &req,
        &NewSession {
            ctx: &tctx,
            cwd: need(p, "cwd")?,
            session_id: need(p, "sessionId")?,
        },
        now,
    );
    match started {
        Ok(Started::Claimed {
            claimed,
            account,
            fresh,
            ..
        }) => Ok(json!({
            "status": "claimed", "claimId": claimed.claim_id, "generation": claimed.generation,
            "account": account, "fresh": fresh,
        })),
        Ok(Started::Exhausted(text)) => Ok(json!({ "status": "exhausted", "text": text })),
        Err(e) => refusal(e),
    }
}

#[cfg(not(unix))]
fn turn(_core: &mut Core, _p: &Value, _now: i64) -> Result<Value> {
    Err(Error::NotServed(
        "kith-core: hostclaim.turn is not served on this platform".into(),
    ))
}

#[cfg(test)]
#[path = "hostclaim_tests.rs"]
mod tests;
