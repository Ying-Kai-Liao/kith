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
pub(super) async fn peer_call(
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
        Ok(()) => peer_method(hub, computer_id, method, b).await,
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

async fn peer_method(hub: &Arc<Hub>, computer_id: &str, method: &str, b: &Value) -> Result<Value> {
    match method {
        "peerAgents" => Ok(json!({ "agents": owner_agents(hub) })),
        "peerAsk" => ask_method(hub, computer_id, "peerAsk", b).await,
        "peerThread" => ask_method(hub, computer_id, "peerThread", b).await,
        "peerStop" => ask_method(hub, computer_id, "peerStop", b).await,
        "peerTo" => ask_method(hub, computer_id, "peerTo", b).await,
        "peerRevokes" => {
            let sent: kith_wire::rpc::HandleRevokes = serde_json::from_value(b.clone())
                .map_err(|_| UserError("`handles` is a list of {handle, revoked, at}".into()))?;
            let mine = crate::remote::handle_revokes::exchange_in(hub, sent.handles).await?;
            Ok(json!({ "handles": mine }))
        }
        _ => Err(NotAllowed("This isn't available to a paired computer.".into()).into()),
    }
}

/// An ask method of the Mac link (design 2026-11-24-ask-ledger, A5b): answered from this computer's core, for the epoch the
/// caller names. A caller that is no longer paired at that epoch gets `paired: false` and nothing else.
async fn ask_method(hub: &Arc<Hub>, computer_id: &str, method: &'static str, b: &Value) -> Result<Value> {
    let epoch = str_arg(b, "epoch")?;
    let Some(pairing) = hub.store.peer_in(computer_id).filter(|p| p.epoch == epoch) else {
        return Ok(crate::remote::peer_asks::unpaired(method));
    };
    let folders = crate::remote::peer_asks::folders_of(hub)
        .ok_or_else(|| UserError("This computer can't hold asks for another computer yet.".into()))?;
    // Nothing is answered for a pairing until the reconcile has agreed it with the core (an unlink while the core was away).
    if !crate::remote::peer_asks::fence::settled(hub).await {
        return Err(UserError("This computer is still settling its pairings; try again shortly.".into()).into());
    }
    let agents: std::collections::BTreeSet<String> =
        owner_agents(hub).iter().filter_map(|a| a["agent"].as_str().map(str::to_owned)).collect();
    let caller = crate::remote::peer_asks::callee::Caller { computer: computer_id, name: &pairing.name, epoch };
    crate::remote::peer_asks::callee::handle(
        &folders,
        &caller,
        method,
        b,
        &|a| agents.contains(a),
        crate::store::now_ms(),
    )
    .await
    .map_err(|why| UserError(why).into())
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
