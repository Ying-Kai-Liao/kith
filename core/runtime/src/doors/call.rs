//! The `doors.*` rows the core answers (M2-14): the owner's view of the guest doors. The daemon routes them here (a
//! change is a claim transaction, so they need `&mut Core`). The params were checked against the row already.
//! Nothing calls them yet: the host and `kith_door.py` keep their own claim files until a later step switches them.

use serde_json::{Value, json};

use super::{Live, Start, State, end, list, mark_live, start, sweep};
use crate::authority::call::ctx_of;
use crate::db::Core;
use crate::error::{Error, Result};
use crate::gate::Liveness;

/// `doors.*` methods this module answers.
pub(crate) fn serves(method: &str) -> bool {
    matches!(
        method,
        "doors.list" | "doors.start" | "doors.markLive" | "doors.end" | "doors.sweep"
    )
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(0))
}

fn bad(what: &str) -> Error {
    Error::BadParams(format!("kith-core: doors: {what}"))
}

fn word<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params.get(key).and_then(Value::as_str)
}

fn need<'a>(params: &'a Value, key: &str) -> Result<&'a str> {
    word(params, key)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| bad(&format!("{key} is required")))
}

fn int(params: &Value, key: &str) -> Result<i64> {
    params
        .get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| bad(&format!("{key} is required (a whole number)")))
}

fn flag(params: &Value, key: &str) -> bool {
    params.get(key).and_then(Value::as_bool).unwrap_or(false)
}

pub(crate) fn call(
    core: &mut Core,
    method: &str,
    params: &Value,
    live: &dyn Liveness,
) -> Result<Value> {
    call_at(core, method, params, live, now_ms())
}

/// [`call`] at a given time (a test fixes the clock).
pub(crate) fn call_at(
    core: &mut Core,
    method: &str,
    params: &Value,
    live: &dyn Liveness,
    now: i64,
) -> Result<Value> {
    match method {
        "doors.list" => {
            let doors = list(core, flag(params, "all"))?;
            Ok(json!({ "doors": doors.iter().map(super::Door::json).collect::<Vec<_>>() }))
        }
        "doors.start" => {
            let ctx = ctx_of(core, params)?;
            let pid = u32::try_from(int(params, "pid")?).map_err(|_| bad("pid is 2 or more"))?;
            let s = Start {
                id: word(params, "id"),
                agent: need(params, "agent")?,
                door: need(params, "door")?,
                person: need(params, "person")?,
                folder: word(params, "folder").unwrap_or_default(),
                pid,
            };
            let started = start(core, &ctx, live, &s, now)?;
            let mut out = started.door.json();
            out["repeat"] = started.repeat.into();
            Ok(out)
        }
        "doors.markLive" => {
            match mark_live(core, need(params, "id")?, int(params, "version")?, now)? {
                Live::Live(d) => Ok(json!({ "result": "live", "door": d.json() })),
                Live::Lost { state, door } => Ok(json!({
                    "result": "lost",
                    "state": state,
                    "door": door.map(|d| d.json()),
                })),
            }
        }
        "doors.end" => {
            let d = end(core, need(params, "id")?, now)?;
            Ok(json!({
                "result": if d.state == State::Swept { "swept" } else { "ended" },
                "door": d.json(),
            }))
        }
        "doors.sweep" => {
            let ctx = ctx_of(core, params)?;
            let persons: Vec<String> = params
                .get("persons")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|p| p.as_str().map(str::to_owned))
                .collect();
            let recheck = !flag(params, "noRecheck");
            let out = sweep(core, &ctx, live, &persons, recheck, now)?;
            Ok(json!({
                "swept": out.swept.iter().map(super::Door::json).collect::<Vec<_>>(),
                "pending": out.pending.iter().map(super::Door::json).collect::<Vec<_>>(),
            }))
        }
        _ => Err(Error::NotServed(format!(
            "kith-core: {method} is not answered in the core yet"
        ))),
    }
}
