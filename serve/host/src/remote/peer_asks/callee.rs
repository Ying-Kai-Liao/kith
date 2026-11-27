//! B's end (design 2026-11-24-ask-ledger §9, D2, D3): the four peer methods a paired computer calls here, answered from the core's
//! rows. B runs the callee as this computer's owner through the run door (`routines/core_ask.rs`, A2); nothing here starts a process.
//!
//! - `peerAsk` opens an ordinary `bg` ask carrying where it came from (`ask.open` with the origin), answers from the row, and
//!   answers the same row to a send that is made again (the origin key is unique). Every refusal is a row.
//! - `peerThread` reads the rows after the caller's last, its state and cost, and writes nothing.
//! - `peerStop` ends the ask in one transaction; a stop that comes first leaves a `stopped` row for the ask to find.
//! - `peerTo` queues the follow-up as a turn of the ask's session (the same words are one message).
//!
//! The caller's pairing is checked twice: the host's own row (`peers_in`) and the core's fence (`ask_peers`). A caller that is no
//! longer paired at the epoch it names gets `paired: false` and nothing else.

use kith_kernel::paths::Paths;
use serde_json::{Value, json};

use super::{Said, core, short_hash, unpaired};
use crate::core_front::{Answer, call_owner};

/// Rows after this number are none: a read of a thread that wants only its session and state.
const NO_ROWS: i64 = 1_000_000_000;

/// The caller: the computer, as this host knows it, and the epoch it named.
pub(crate) struct Caller<'a> {
    pub computer: &'a str,
    /// The name the owner gave the computer when it paired (what its asks say they came from).
    pub name: &'a str,
    pub epoch: &'a str,
}

/// A peer method of the callee's end. `Ok(Err(..))` never: every refusal of the core's that is not "not paired" is an error the
/// caller retries; "not paired" is an answer.
pub(crate) async fn handle(
    folders: &Paths,
    caller: &Caller<'_>,
    method: &str,
    params: &Value,
    has_agent: &(dyn Fn(&str) -> bool + Sync),
    now_ms: i64,
) -> Result<Value, String> {
    let text = |k: &str| params.get(k).and_then(Value::as_str).filter(|s| !s.is_empty());
    let Some(key) = text("key") else {
        return Err("`key` is the caller's id for the ask".to_owned());
    };
    let said = match method {
        "peerAsk" => ask(folders, caller, key, params, has_agent, now_ms).await,
        "peerThread" => {
            let after = params.get("after").and_then(Value::as_i64).unwrap_or(0);
            thread(folders, caller, key, after).await
        }
        "peerStop" => stop(folders, caller, key).await,
        "peerTo" => to(folders, caller, key, text("text").unwrap_or_default()).await,
        other => return Err(format!("{other} is not an ask method")),
    };
    match said {
        Ok(v) => Ok(v),
        Err(Said::Refused(why)) if not_paired(&why) => Ok(unpaired(method)),
        Err(e) => Err(e.to_string()),
    }
}

/// Whether the core refused for a pairing that is not live (`asks::peer_b::not_paired`'s words, or the open's).
fn not_paired(why: &str) -> bool {
    why.contains("is not paired with this computer")
}

/// The id this host gives the row of a key another computer chose: one per (computer, key), the same on every send.
pub(crate) fn row_id(caller: &Caller<'_>, key: &str) -> String {
    format!("px-{}", short_hash(&[caller.computer, key]))
}

/// `ask.peerThread` in the contract's words (`PeerThread`).
fn as_thread(got: &Value) -> Value {
    if got["found"].as_bool() != Some(true) {
        return json!({"paired": true, "found": false, "rows": []});
    }
    json!({"paired": true, "found": true, "state": got["state"], "rows": got["rows"],
           "cost": got["cost"], "waiting": got["waiting"], "takesTo": got["takesTo"]})
}

async fn read(folders: &Paths, caller: &Caller<'_>, key: &str, after: i64) -> Result<Value, Said> {
    core(
        folders,
        "ask.peerThread",
        json!({"computer": caller.computer, "key": key, "epoch": caller.epoch, "after": after}),
    )
    .await
}

async fn thread(folders: &Paths, caller: &Caller<'_>, key: &str, after: i64) -> Result<Value, Said> {
    Ok(as_thread(&read(folders, caller, key, after).await?))
}

async fn stop(folders: &Paths, caller: &Caller<'_>, key: &str) -> Result<Value, Said> {
    let got = core(
        folders,
        "ask.peerStop",
        json!({"id": row_id(caller, key), "computer": caller.computer, "key": key, "epoch": caller.epoch}),
    )
    .await?;
    Ok(json!({"paired": true, "state": got["state"]}))
}

/// A refusal this host makes before the core is asked (an agent that is not here): the answer the core's own refused row would
/// give, and no row, so the same send gives the same answer again.
fn refused(why: &str, now_ms: i64) -> Value {
    json!({"paired": true, "found": true, "state": "refused", "cost": 0.0,
           "rows": [{"n": 1, "kind": "lost", "text": format!("refused: {why}"), "at": now_ms}]})
}

/// What the run is told besides the task: where it came from, and that its final answer goes back (kith-remote's words). A
/// continued ask carries only the task: its session knows the rest.
fn told(task: &str, name: &str, continued: bool) -> String {
    if continued {
        return task.to_owned();
    }
    format!("{task}\n\n(Asked from {name} through kith ask. Your final answer is returned to the caller.)")
}

async fn ask(
    folders: &Paths,
    caller: &Caller<'_>,
    key: &str,
    params: &Value,
    has_agent: &(dyn Fn(&str) -> bool + Sync),
    now_ms: i64,
) -> Result<Value, Said> {
    let text = |k: &str| params.get(k).and_then(Value::as_str).filter(|s| !s.is_empty());
    let (Some(agent), Some(task)) = (text("agent"), text("task")) else {
        return Err(Said::Refused("`agent` and `task` are what is asked".to_owned()));
    };
    // A send that is made again finds the row it made; only a first one is judged here.
    let seen = read(folders, caller, key, NO_ROWS).await?;
    if seen["found"].as_bool() == Some(true) {
        return thread(folders, caller, key, 0).await;
    }
    if !has_agent(agent) {
        return Ok(refused(&format!("no agent called {agent} here"), now_ms));
    }
    // `-c`: the earlier ask's session, when it kept one.
    let cont = text("cont");
    let session = match cont {
        Some(prev) => {
            read(folders, caller, prev, NO_ROWS).await?["session"].as_str().filter(|s| !s.is_empty()).map(str::to_owned)
        }
        None => None,
    };
    let mut open = json!({
        "id": row_id(caller, key), "asked": agent, "mode": "bg", "task": told(task, caller.name, cont.is_some()),
        "originComputer": caller.computer, "originKey": key, "originEpoch": caller.epoch,
    });
    if let Some(l) = params.get("lineage").filter(|l| l.is_object()) {
        open["lineage"] = l.clone();
    }
    if let Some(s) = session {
        open["conversation"] = json!(s);
    }
    // Only what the run can use on this computer: a folder there is the caller's own.
    for k in ["model", "effort", "project"] {
        if let Some(v) = params["opts"].get(k).and_then(Value::as_str).filter(|v| !v.is_empty()) {
            open[k] = json!(v);
        }
    }
    core(folders, "ask.open", open).await?;
    thread(folders, caller, key, 0).await
}

/// `peerTo`: a turn of the ask's session with the message. `taken: false` only while there is no session to send to yet.
async fn to(folders: &Paths, caller: &Caller<'_>, key: &str, text: &str) -> Result<Value, Said> {
    if text.is_empty() {
        return Err(Said::Refused("`text` is the message".to_owned()));
    }
    let got = read(folders, caller, key, NO_ROWS).await?;
    let told = |taken: bool, note: Option<&str>| {
        let mut v = json!({"paired": true, "taken": taken});
        if let Some(n) = note {
            v["note"] = json!(n);
        }
        v
    };
    if got["found"].as_bool() != Some(true) {
        // The ask has not reached this computer (yet): the caller sends the message after it.
        return Ok(told(false, None));
    }
    if got["takesTo"].as_bool() != Some(true) {
        return Ok(told(true, Some("the ask is over here, so it takes no follow-up")));
    }
    let (Some(session), Some(agent), Some(id)) =
        (got["session"].as_str().filter(|s| !s.is_empty()), got["agent"].as_str(), got["id"].as_str())
    else {
        // Open with no session yet: the run has not begun one.
        return Ok(told(false, None));
    };
    let params = json!({
        "session": session, "id": format!("ask-to:{id}/{}", short_hash(&[text])), "agent": agent, "text": [text],
        "door": "run", "why": format!("{} sent a follow-up to kith ask {key}", caller.name),
    });
    let folders = folders.clone();
    let answer = tokio::task::spawn_blocking(move || call_owner(&folders, "wake.enqueue", &params, false))
        .await
        .map_err(|e| Said::Down(e.to_string()))?;
    match answer {
        Answer::Done(_) => Ok(told(true, None)),
        Answer::Refused(why) => Ok(told(true, Some(&format!("not delivered: {why}")))),
        Answer::Down { why, .. } => Err(Said::Down(why)),
    }
}
