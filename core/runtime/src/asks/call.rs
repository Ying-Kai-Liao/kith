//! The `ask.*` rows the core answers (M3-8a). The daemon routes them here (a change is a claim transaction, so they need
//! `&mut Core`). The params were checked against the row already. Nothing calls them yet: `kith-ask` keeps writing
//! `live/ask/*.reg` until the switch, and nothing here runs on its own.

use serde_json::{Value, json};

use super::{
    EndReq, Open, cap_told, check_to, delegations, end_tree, limits, open, record, settle,
};
use crate::db::Core;
use crate::error::{Error, Result};

/// `ask.*` methods this module answers.
pub(crate) fn serves(method: &str) -> bool {
    matches!(
        method,
        "ask.open"
            | "ask.settle"
            | "ask.capTold"
            | "ask.end"
            | "ask.to"
            | "ask.revokeEverywhere"
            | "ask.delegations"
            | "ask.record"
            | "ask.limits"
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
            let o = Open {
                id: need(params, "id")?,
                asked: word(params, "asked").unwrap_or_default(),
                crossing: flag(params, "crossing"),
                brief: word(params, "brief"),
                pid: params.get("pid").and_then(Value::as_i64),
                conversation: word(params, "conversation"),
            };
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
        "ask.to" => {
            let id = need(params, "id")?;
            check_to(core.conn(), id)?;
            Ok(json!({ "id": id, "ok": true }))
        }
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
