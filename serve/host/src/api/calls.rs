//! Calls between an agent and its person: the agent's `callPerson` (loopback only, from `kith-call`), and the
//! person's answers from their apps.

use super::devices::{Caller, Principal, principal};
use super::str_arg;
use crate::hub::Hub;
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::ops::ControlFlow;
use std::sync::Arc;

pub(super) async fn call(hub: &Arc<Hub>, caller: &Caller, method: &str, b: Value) -> Result<ControlFlow<Value, Value>> {
    Ok(ControlFlow::Break(match method {
        // `kith-call`: the run names its chat with the key the host put in its environment, so it can ring only the
        // person that chat belongs to (an invitee's run never reaches the owner, whatever it unsets).
        "callPerson" => {
            let (chat, reason) = (str_arg(&b, "chat")?, str_arg(&b, "reason")?);
            if !crate::calls::key_ok(hub, chat, b["key"].as_str()) {
                return Ok(ControlFlow::Break(json!({"status": "not-allowed"})));
            }
            crate::calls::ring_and_wait(hub, chat, reason, b["from"].as_str(), b["wait"] == true).await?
        }
        // `kith-sched` and `kith` (a terminal run): the origin of a run they start, for the agent and the person it is
        // for. Only the owner's own processes may ask: the key it hands out rings that person.
        "runEnv" => {
            if !matches!(principal(&hub.store, caller), Principal::Owner) {
                bail!("this device isn't allowed on this computer");
            }
            let person = b["person"].as_str().filter(|p| !p.is_empty());
            crate::calls::run_origin(hub, str_arg(&b, "agent")?, person, b["outbox"] == true)
        }
        "callAccept" | "callDecline" | "callGreet" => {
            let who = match principal(&hub.store, caller) {
                Principal::Owner => None,
                Principal::Member(person) => Some(person),
                Principal::Nobody
                | Principal::Unproven(_)
                | Principal::Bot { .. }
                | Principal::BotPending(_)
                | Principal::Peer(_) => {
                    bail!("this device isn't allowed on this computer")
                }
            };
            let id = str_arg(&b, "id")?;
            match method {
                "callAccept" => crate::calls::accept(hub, who.as_deref(), id)?,
                "callDecline" => crate::calls::decline(hub, who.as_deref(), id)?,
                _ => crate::calls::greet(hub, who.as_deref(), id)?,
            }
        }
        _ => return Ok(ControlFlow::Continue(b)),
    }))
}
