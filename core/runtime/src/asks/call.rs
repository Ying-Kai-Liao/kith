//! The `ask.*` rows the core answers (M3-8a). The daemon routes them here (a change is a claim transaction, so they need
//! `&mut Core`). The params were checked against the row already. Nothing calls them yet: `kith-ask` keeps writing
//! `live/ask/*.reg` until the switch, and nothing here runs on its own.

use serde_json::{Value, json};

use super::ledger::{self, Callee, Finish};
use super::peer::{self, Apply, InRow};
use super::rows::Kind;
use super::{
    EndReq, LedgerOpen, Open, Origin, cap_told, check_to, delegations, end_tree, limits, open,
    record, settle,
};
use crate::db::Core;
use crate::error::{Error, Result};

/// `ask.*` methods this module answers.
pub(crate) fn serves(method: &str) -> bool {
    matches!(
        method,
        "ask.open"
            | "ask.settle"
            | "ask.finish"
            | "ask.post"
            | "ask.read"
            | "ask.park"
            | "ask.unpark"
            | "ask.capTold"
            | "ask.end"
            | "ask.to"
            | "ask.revokeEverywhere"
            | "ask.delegations"
            | "ask.record"
            | "ask.limits"
            | "ask.peerLink"
            | "ask.endPeer"
            | "ask.peerApply"
            | "ask.peerOwed"
    )
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(0))
}

fn bad(what: &str) -> Error {
    Error::BadParams(format!("kith-core: asks: {what}"))
}

fn word<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params.get(key).and_then(Value::as_str)
}

fn need<'a>(params: &'a Value, key: &str) -> Result<&'a str> {
    word(params, key)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| bad(&format!("{key} is required")))
}

/// The direction of a pairing: `in` (asks that computer sends here) or `out` (asks sent there).
fn need_dir(params: &Value) -> Result<&str> {
    need(params, "dir").and_then(|d| {
        if matches!(d, "in" | "out") {
            Ok(d)
        } else {
            Err(bad("dir is in or out"))
        }
    })
}

fn kills(kill: &[crate::conversations::claims::Kill]) -> Vec<Value> {
    kill.iter()
        .map(|k| json!({"claimId": k.claim_id, "pid": k.pid, "started": k.started}))
        .collect()
}

fn flag(params: &Value, key: &str) -> bool {
    params.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn strings(params: &Value, key: &str) -> Vec<String> {
    params
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|s| s.as_str().map(str::to_owned))
        .collect()
}

pub(crate) fn call(core: &mut Core, method: &str, params: &Value) -> Result<Value> {
    call_at(core, method, params, now_ms())
}

/// [`call`] at a given time (a test fixes the clock).
pub(crate) fn call_at(core: &mut Core, method: &str, params: &Value, now: i64) -> Result<Value> {
    match method {
        "ask.open" => {
            let (folders, _) = core.folders_for(params)?;
            let env = |k: &str| {
                params
                    .get("env")
                    .and_then(|e| e.get(k))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            };
            let origin = match (
                word(params, "originComputer"),
                word(params, "originKey"),
                word(params, "originEpoch"),
            ) {
                (Some(computer), Some(key), Some(epoch))
                    if !computer.is_empty() && !key.is_empty() && !epoch.is_empty() =>
                {
                    Some(Origin {
                        computer,
                        key,
                        epoch,
                    })
                }
                (None, None, None) => None,
                _ => return Err(bad("originComputer, originKey and originEpoch go together")),
            };
            let o = Open {
                id: need(params, "id")?,
                asked: word(params, "asked").unwrap_or_default(),
                crossing: flag(params, "crossing"),
                brief: word(params, "brief"),
                pid: params.get("pid").and_then(Value::as_i64),
                conversation: word(params, "conversation"),
                claim_id: params.get("claimId").and_then(Value::as_i64),
                ledger: word(params, "mode").map(|mode| LedgerOpen {
                    mode,
                    task: word(params, "task"),
                    cli_pid: params.get("cliPid").and_then(Value::as_i64),
                    cli_start: word(params, "cliStart"),
                    engine: word(params, "engine"),
                    peer: word(params, "peer"),
                    cont: word(params, "cont"),
                    origin,
                    opts: {
                        let m: serde_json::Map<String, Value> =
                            ["model", "effort", "folder", "project"]
                                .into_iter()
                                .filter_map(|k| {
                                    word(params, k)
                                        .filter(|v| !v.is_empty())
                                        .map(|v| (k.to_owned(), json!(v)))
                                })
                                .collect();
                        (!m.is_empty()).then_some(m)
                    },
                }),
            };
            if let Some(l) = &o.ledger {
                if !matches!(l.mode, "fg" | "bg" | "peer") {
                    return Err(bad("mode is fg, bg or peer"));
                }
                if (l.mode == "peer") != l.peer.is_some() {
                    return Err(bad("peer goes with mode peer, and only with it"));
                }
                if l.origin.is_some() && l.mode != "bg" {
                    return Err(bad("an ask from another computer is a bg ask"));
                }
            } else if origin.is_some() || word(params, "peer").is_some() {
                return Err(bad("peer and origin need a mode"));
            }
            open(
                core,
                &folders.machine,
                &env,
                &o,
                flag(params, "checkOnly"),
                now,
            )
        }
        "ask.settle" => {
            let (folders, _) = core.folders_for(params)?;
            let state = word(params, "state");
            if state.is_some_and(|s| !matches!(s, "answered" | "stopped")) {
                return Err(bad("state is answered or stopped"));
            }
            let cost = params.get("cost").and_then(Value::as_f64).unwrap_or(0.0);
            let id = need(params, "id")?;
            settle(core, &folders.machine, id, cost, state, now)?
                .ok_or_else(|| Error::Refused(format!("kith-core: asks: no ask {id}")))
        }
        "ask.finish" => {
            let (folders, _) = core.folders_for(params)?;
            let callee = word(params, "result").map(|text| Callee {
                text: text.to_owned(),
                cost: params.get("cost").and_then(Value::as_f64).unwrap_or(0.0),
                is_error: flag(params, "isError"),
                session: word(params, "session").map(str::to_owned),
            });
            ledger::finish(
                core,
                &folders.machine,
                &Finish {
                    id: need(params, "id")?,
                    out: word(params, "out"),
                    callee,
                    session: word(params, "session"),
                    turn: params.get("turn").and_then(Value::as_i64),
                },
                now,
            )
        }
        "ask.post" => {
            let kind = word(params, "kind").map_or(Some(Kind::Answer), Kind::parse);
            ledger::post(
                core,
                need(params, "id")?,
                need(params, "text")?,
                word(params, "as"),
                kind.ok_or_else(|| bad("kind is answer or progress"))?,
                now,
            )
        }
        "ask.read" => ledger::read(core, need(params, "id")?, now),
        "ask.park" => ledger::park(core, need(params, "id")?, Some(need(params, "card")?), now),
        "ask.unpark" => {
            let (folders, _) = core.folders_for(params)?;
            ledger::unpark(
                core,
                &folders.machine,
                need(params, "id")?,
                word(params, "note"),
                word(params, "session"),
                now,
            )
        }
        "ask.capTold" => {
            let n = cap_told(core, need(params, "chain")?, now)?;
            Ok(json!({ "marked": n }))
        }
        "ask.end" => {
            let (roots, sessions, persons) = (
                strings(params, "roots"),
                strings(params, "sessions"),
                strings(params, "persons"),
            );
            let ended = end_tree(
                core,
                &EndReq {
                    roots: &roots,
                    sessions: &sessions,
                    persons: &persons,
                },
                now,
            )?;
            let kill: Vec<Value> = ended
                .kill
                .iter()
                .map(|k| json!({"claimId": k.claim_id, "pid": k.pid, "started": k.started}))
                .collect();
            Ok(json!({ "ended": ended.ids, "sessions": ended.sessions, "kill": kill }))
        }
        "ask.peerLink" => {
            let ended = peer::link(
                core,
                need(params, "computer")?,
                need_dir(params)?,
                need(params, "epoch")?,
                now,
            )?;
            Ok(json!({
                "computer": need(params, "computer")?,
                "dir": need_dir(params)?,
                "epoch": need(params, "epoch")?,
                "linked": true,
                "ended": ended.ids,
                "sessions": ended.sessions,
                "kill": kills(&ended.kill),
            }))
        }
        "ask.endPeer" => {
            let (computer, dir) = (need(params, "computer")?, need_dir(params)?);
            let ended = peer::end(core, computer, dir, now)?;
            Ok(json!({
                "computer": computer,
                "dir": dir,
                "ended": ended.ids,
                "sessions": ended.sessions,
                "kill": kills(&ended.kill),
            }))
        }
        "ask.peerApply" => {
            let (folders, _) = core.folders_for(params)?;
            let mut rows = Vec::new();
            for r in params
                .get("rows")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let kind = word(r, "kind").and_then(Kind::parse);
                let (Some(n), Some(kind), Some(text)) =
                    (r.get("n").and_then(Value::as_i64), kind, word(r, "text"))
                else {
                    return Err(bad(
                        "a row needs n, kind (answer, progress or lost) and text",
                    ));
                };
                rows.push(InRow {
                    n,
                    who: word(r, "as").map(str::to_owned),
                    kind,
                    text: text.to_owned(),
                    at_ms: r.get("at").and_then(Value::as_i64),
                });
            }
            peer::apply(
                core,
                &folders.machine,
                &Apply {
                    id: need(params, "id")?,
                    epoch: need(params, "epoch")?,
                    rows,
                    state: word(params, "state"),
                    cost: params.get("cost").and_then(Value::as_f64),
                },
                now,
            )
        }
        "ask.peerOwed" => peer::owed(core),
        "ask.to" => {
            let id = need(params, "id")?;
            check_to(core.conn(), id)?;
            Ok(json!({ "id": id, "ok": true }))
        }
        // The revoke order is Unix only (the authority tables it reads are).
        #[cfg(unix)]
        "ask.revokeEverywhere" => {
            let out = crate::revoke_order::revoke_everywhere(
                core,
                need(params, "person")?,
                &strings(params, "bots"),
                now,
            )?;
            let kill: Vec<Value> = out
                .kill
                .iter()
                .map(|k| json!({"claimId": k.claim_id, "pid": k.pid, "started": k.started}))
                .collect();
            Ok(json!({
                "principals": out.principals,
                "cancelledTurns": out.cancelled_turns,
                "ended": out.ended,
                "sessions": out.sessions,
                "kill": kill,
            }))
        }
        "ask.delegations" => {
            let (folders, _) = core.folders_for(params)?;
            delegations(core, &folders.machine)
        }
        "ask.record" => {
            let id = need(params, "id")?;
            record(core, id)?.ok_or_else(|| Error::Refused(format!("kith-core: asks: no ask {id}")))
        }
        "ask.limits" => {
            let (folders, _) = core.folders_for(params)?;
            Ok(limits(&folders.machine))
        }
        _ => Err(Error::NotServed(format!(
            "kith-core: {method} is not answered in the core yet"
        ))),
    }
}
