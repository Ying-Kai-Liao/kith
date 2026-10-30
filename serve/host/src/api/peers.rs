//! Mac link (designs/2026-10-29-mac-link): what a paired computer may call here (`PEER_METHODS`, nothing else), and the
//! owner's pairing methods on this computer (`remote::peers`).

use super::devices::{Caller, Forbidden, NotAllowed};
use super::{UserError, roles, str_arg};
use crate::hub::Hub;
use crate::remote::peers;
use anyhow::Result;
use kith_wire::rpc::PEER_METHODS;
use serde_json::{Value, json};
use std::ops::ControlFlow;
use std::sync::Arc;

/// A call from a paired computer: only the peer methods, each audited under `peer:<computer id>`.
pub(super) fn peer_call(
    hub: &Arc<Hub>,
    caller: &Caller,
    computer_id: &str,
    method: &str,
    b: &Value,
    permitted: Result<(), Forbidden>,
) -> Result<Value> {
    let verdict = match permitted {
        Err(e) => Err(e.into()),
        Ok(()) if !PEER_METHODS.contains(&method) => {
            Err(NotAllowed("This isn't available to a paired computer.".into()).into())
        }
        Ok(()) => Ok(()),
    };
    let target = roles::target_of(b);
    let outcome = match verdict {
        Ok(()) => peer_method(hub, method),
        Err(e) => Err(e),
    };
    hub.store.audit_add(
        &format!("peer:{computer_id}"),
        caller.device_key(),
        method,
        target.as_deref(),
        outcome.is_ok(),
    );
    let answer = outcome?;
    #[cfg(test)]
    super::answers_as_the_contract_says("peer", method, &answer);
    Ok(answer)
}

fn peer_method(hub: &Hub, method: &str) -> Result<Value> {
    match method {
        "peerAgents" => Ok(json!({ "agents": owner_agents(hub) })),
        _ => Err(NotAllowed("This isn't available to a paired computer.".into()).into()),
    }
}

/// The owner's own agents with a chat here: what a paired computer can ask.
fn owner_agents(hub: &Hub) -> Vec<Value> {
    let mut seen = std::collections::BTreeMap::new();
    for row in hub.store.bots().unwrap_or_default() {
        let c = row.config;
        if let (false, None, Some(agent)) = (row.deleted, &c.person, c.hq_agent) {
            seen.entry(agent).or_insert(c.name);
        }
    }
    seen.into_iter().map(|(agent, name)| json!({"agent": agent, "name": name})).collect()
}

/// The owner's pairing methods. All are this computer's only (`LOCAL_ONLY`): a yes to a pairing is typed here.
pub(super) async fn call(hub: &Arc<Hub>, method: &str, b: Value) -> Result<ControlFlow<Value, Value>> {
    Ok(ControlFlow::Break(match method {
        "peerComputers" => peers::computers(hub).await?,
        "linkComputer" => peers::link(hub, str_arg(&b, "computer")?).await?,
        "linkStatus" => peers::status(hub, str_arg(&b, "computerId")?).await?,
        "peerRequests" => json!({ "requests": peers::requests_json(hub) }),
        "decidePeerRequest" => {
            let approve = match str_arg(&b, "decision")? {
                d @ ("approve" | "deny") => d == "approve",
                other => return Err(UserError(format!("`{other}` isn't approve or deny.")).into()),
            };
            peers::decide(hub, str_arg(&b, "requestId")?, approve, b["code"].as_str()).await?;
            json!({"ok": true})
        }
        "unlinkComputer" => peers::unlink(hub, str_arg(&b, "computerId")?).await?,
        _ => return Ok(ControlFlow::Continue(b)),
    }))
}
