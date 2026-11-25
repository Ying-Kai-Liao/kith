//! The wake calls when the `wake` module is switched: every one is a `wake.*` row of the core (`core/api/src/table/wake.rs`),
//! asked over the owner link (`recover`, the clock, is the host's own row). Nothing here starts a process or reads the
//! queue folder: with the switch on the files are not the store (design 2026-11-21-wake-s4 section 4).
//!
//! A call the core does not answer is an error to its caller, never a fall back to the files: the module's tombstone
//! stays where the Python helpers read it.

use super::{Queued, Request, Turn};
use crate::core_front::{Answer, call_host, call_owner};
use kith_kernel::paths::Paths;
use serde_json::{Value, json};
use std::collections::HashMap;

/// One call, off the async worker's way when the caller is on one (the link blocks on a socket).
fn ask(f: &Paths, method: &str, params: &Value, repeatable: bool, host: bool) -> anyhow::Result<Value> {
    let go = || if host { call_host(f, method, params) } else { call_owner(f, method, params, repeatable) };
    let said = match tokio::runtime::Handle::try_current() {
        Ok(h) if h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => tokio::task::block_in_place(go),
        _ => go(),
    };
    match said {
        Answer::Done(v) => Ok(v),
        Answer::Refused(why) => anyhow::bail!("{method}: the core refused: {why}"),
        Answer::Down { why, .. } => anyhow::bail!("{method}: the core cannot be asked: {why}"),
    }
}

fn read(f: &Paths, method: &str, params: &Value) -> anyhow::Result<Value> {
    ask(f, method, params, true, false)
}

/// A write: never sent twice.
fn write(f: &Paths, method: &str, params: &Value) -> anyhow::Result<Value> {
    ask(f, method, params, false, false)
}

pub fn enqueue(f: &Paths, r: &Request<'_>) -> anyhow::Result<Queued> {
    let mut params = json!({
        "session": r.session, "id": r.id, "agent": r.agent,
        "door": if r.host_door { "host" } else { "run" },
        "text": [r.text],
    });
    for (key, v) in [("person", r.person), ("room", r.room), ("why", Some(r.why).filter(|w| !w.is_empty()))] {
        if let Some(v) = v {
            params[key] = json!(v);
        }
    }
    let said = write(f, "wake.enqueue", &params)?;
    Ok(if said["result"] == "duplicate" { Queued::Duplicate } else { Queued::New })
}

pub fn status(f: &Paths, id: &str) -> Option<String> {
    asked_status(f, id).inspect_err(|e| tracing::warn!("wake: {e:#}")).ok()
}

fn asked_status(f: &Paths, id: &str) -> anyhow::Result<String> {
    let said = read(f, "wake.queueStatus", &json!({"id": id}))?;
    said["state"].as_str().map(str::to_owned).ok_or_else(|| anyhow::anyhow!("wake.queueStatus: no state in the answer"))
}

/// Queued and running requests from `wake.queueList`; each of `ids` it does not hold is asked as `wake.queueStatus`
/// (the list carries no answered ids), and an `unknown` one is left out, as the files' queue forgets them. None: a read
/// failed, which says nothing about any request (a caller must not take a missing id for a forgotten one).
pub fn statuses(f: &Paths, ids: &[String]) -> Option<HashMap<String, String>> {
    let mut out = HashMap::new();
    for r in ls(f)?.iter().flat_map(|it| it["requests"].as_array().into_iter().flatten()) {
        if let Some(id) = r["id"].as_str() {
            out.insert(id.to_owned(), if r["run"].is_null() { "queued" } else { "running" }.to_owned());
        }
    }
    let missing: Vec<&String> = ids.iter().filter(|id| !out.contains_key(*id)).collect();
    for id in missing {
        match asked_status(f, id) {
            Ok(state) if state != "unknown" => {
                out.insert(id.clone(), state);
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!("wake: {e:#}");
                return None;
            }
        }
    }
    Some(out)
}

pub fn has_items(f: &Paths) -> bool {
    ls(f).is_some_and(|items| !items.is_empty())
}

pub fn ls(f: &Paths) -> Option<Vec<Value>> {
    let listed = read(f, "wake.queueList", &json!({})).inspect_err(|e| tracing::warn!("wake: {e:#}")).ok()?;
    Some(listed["items"].as_array().cloned().unwrap_or_default())
}

pub fn take(f: &Paths, session: Option<&str>) -> anyhow::Result<Option<Turn>> {
    let mut params = json!({"holder": std::process::id()});
    if let Some(s) = session {
        params["session"] = json!(s);
    }
    let said = write(f, "wake.take", &params)?;
    match &said["turn"] {
        Value::Null => Ok(None),
        turn => Ok(Some(serde_json::from_value(turn.clone())?)),
    }
}

pub fn done(f: &Paths, run: &str, how: &str) {
    log("done", write(f, "wake.done", &json!({"run": run, "how": how})));
}

pub fn back(f: &Paths, run: &str, held: bool) {
    log("back", write(f, "wake.back", &json!({"run": run, "held": held})));
}

pub fn withdraw(f: &Paths, session: &str, id: Option<&str>) {
    let mut params = json!({"session": session});
    if let Some(id) = id {
        params["id"] = json!(id);
    }
    log("withdraw", write(f, "wake.withdraw", &params));
}

pub fn withdraw_person(f: &Paths, persons: &[String]) {
    log("withdraw", write(f, "wake.withdrawPerson", &json!({"persons": persons})));
}

/// The core's clock does what `kith-wake drain` did: recovers the turns whose holder is gone.
#[cfg(test)]
pub fn recover(f: &Paths) {
    log("recover", ask(f, "wake.tick", &json!({}), false, true));
}

fn log(what: &str, said: anyhow::Result<Value>) {
    if let Err(error) = said {
        tracing::warn!(what, error = format!("{error:#}"), "wake call failed");
    }
}
