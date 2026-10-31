//! Mac link pairing (designs/2026-10-29-mac-link): this computer and another of the owner's agree on keys with the
//! commit-then-reveal SAS account devices use, run through the cloud. The asking computer (A) makes a device key for
//! the other one and pins the sign key it computed the code with; the answering computer (B) lets that device key in
//! as a Peer only after someone at B typed the code A shows. The cloud offers the pair and can end it, never make it.

use super::cloud::{self, Conflict};
use super::crypto::{self, Labels, b64};
use crate::LockExt;
use crate::api::UserError;
use crate::hub::Hub;
use crate::market::vault;
use crate::store::{PeerIn, PeerOut, now_ms};
use anyhow::{Result, anyhow};
use ed25519_dalek::SigningKey;
use kith_wire::cloud::{
    AccessRequestCreated, AccessState, AccessStatus, CreatePeerRequest, Decided, Decision, EmptyAnswer, HostDecision,
    HostNonce, HostPeer, HostPeerRequest, PeerComputer, PeerComputerList, Reveal, path,
};
use reqwest::Method;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// kv: when this host sent its recent nonces to computers asking to pair. A budget of its own, so pairing requests
/// and account devices can't use up each other's guesses at a code.
const PEER_SAS_NONCES: &str = "peer_sas_nonces";
/// How long A waits for B's channel and its first answer.
const FIRST_CALL: Duration = Duration::from_secs(15);

/// Pairings in progress. Their nonces live in memory only: after a restart a pairing starts again.
#[derive(Default)]
pub struct Pairings {
    /// B: computers asking to pair with this one.
    incoming: Mutex<Vec<Incoming>>,
    /// A: this computer's side of each pairing it started, by the other computer's id.
    outgoing: Mutex<HashMap<String, Outgoing>>,
}

#[derive(Clone)]
struct Incoming {
    id: String,
    computer_id: String,
    name: String,
    device_key: [u8; 32],
    commit: [u8; 32],
    host_nonce: [u8; 32],
    /// Set with the code, once A revealed a nonce that matches its commit.
    device_nonce: Option<[u8; 32]>,
    code: Option<String>,
    expires_at: i64,
}

#[derive(Clone)]
struct Outgoing {
    request_id: String,
    device_nonce: [u8; 32],
    /// The first nonce B sent; the code is computed with it and an approval under another one is refused.
    host_nonce: Option<[u8; 32]>,
    code: Option<String>,
}

/// The pairing's epoch: both computers compute it from the code exchange, so every pairing gets a new one.
pub fn epoch(b_sign: &[u8; 32], a_device: &[u8; 32], device_nonce: &[u8; 32], host_nonce: &[u8; 32]) -> String {
    b64(&crypto::sha256(&[b"kith/peer-epoch/v1", b_sign, a_device, device_nonce, host_nonce])[..16])
}

/// The vault slot of A's device key for the computer `cid` (ciphertext in the store, its master key in the Keychain).
fn key_slot(cid: &str) -> String {
    format!("peer.key.{cid}")
}

// MARK: B, answering

/// Answers the pairing requests the cloud lists: a host nonce within the hourly budget, then the code once the asking
/// computer revealed. Nothing here approves one.
pub(super) async fn sync(hub: &Hub, base: &str, list: Option<Vec<HostPeerRequest>>) {
    // A cloud that predates pairing lists none: nothing to answer.
    let Some(list) = list else { return };
    let me = hub.identity.computer_id();
    let mut offers: Vec<Incoming> = vec![];
    let mut denies = vec![];
    let mut throttled = false;
    {
        let mut mine = hub.cloud.pairings.incoming.locked();
        mine.retain(|r| list.iter().any(|s| s.request_id == r.id));
        let mut budget = cloud::nonce_budget(&hub.store, PEER_SAS_NONCES);
        for s in &list {
            let (Ok(dk), Ok(commit)) = (crypto::unb64_n::<32>(&s.device_key), crypto::unb64_n::<32>(&s.commit)) else {
                denies.push(s.request_id.clone());
                continue;
            };
            let Some(i) = mine.iter().position(|r| r.id == s.request_id) else {
                if s.host_nonce.is_some() || s.computer_id == me {
                    // Our nonce was in memory only, or the cloud asks this computer to pair with itself.
                    denies.push(s.request_id.clone());
                } else if mine.iter().any(|r| r.computer_id == s.computer_id && r.code.is_none())
                    || offers.iter().any(|r| r.computer_id == s.computer_id)
                {
                    // One unrevealed request per computer at a time.
                } else if budget.len() >= cloud::SAS_NONCES_PER_HOUR {
                    throttled = true;
                } else {
                    budget.push(now_ms());
                    offers.push(Incoming {
                        id: s.request_id.clone(),
                        computer_id: s.computer_id.clone(),
                        name: s.name.chars().take(100).collect(),
                        device_key: dk,
                        commit,
                        host_nonce: crypto::random(),
                        device_nonce: None,
                        code: None,
                        expires_at: s.expires_at,
                    });
                }
                continue;
            };
            let r = &mut mine[i];
            let ours = b64(&r.host_nonce);
            if r.device_key != dk
                || r.commit != commit
                || r.computer_id != s.computer_id
                || s.host_nonce.as_deref() != Some(ours.as_str())
            {
                tracing::warn!(request_id = r.id, "pairing request changed in the cloud; denying it");
                denies.push(r.id.clone());
                mine.remove(i);
                continue;
            }
            if r.code.is_some() {
                continue;
            }
            let Some(nd) = s.device_nonce.as_deref().and_then(|n| crypto::unb64_n::<32>(n).ok()) else { continue };
            if crypto::sas_commit(Labels::Kith, &dk, &nd) == r.commit {
                r.code = Some(crypto::sas_code(Labels::Kith, &hub.identity.sign_pub(), &dk, &nd, &r.host_nonce));
                r.device_nonce = Some(nd);
            } else {
                tracing::warn!(request_id = r.id, "pairing nonce doesn't match its commit; denying the request");
                denies.push(r.id.clone());
                mine.remove(i);
            }
        }
        // Saved before any nonce leaves, so a restart can't reset the budget.
        cloud::set_nonce_budget(&hub.store, PEER_SAS_NONCES, &budget);
    }
    if throttled {
        tracing::warn!("too many computers asked to pair this hour; new requests wait up to an hour");
    }
    for r in offers {
        let nonce = HostNonce { nonce: b64(&r.host_nonce) };
        match cloud::send::<_, EmptyAnswer>(hub, base, &path::host_peer_nonce(&r.id), &nonce).await {
            Ok(_) => hub.cloud.pairings.incoming.locked().push(r),
            Err(e) if cloud::cloud_status_of(&e) == Some(409) => denies.push(r.id),
            Err(e) => tracing::warn!(error = format!("{e:#}"), request_id = r.id, "couldn't answer a pairing request"),
        }
    }
    for id in denies {
        if let Err(e) = decision(hub, base, &id, false).await {
            tracing::warn!(error = format!("{e:#}"), request_id = id, "couldn't deny a pairing request");
        }
    }
}

async fn decision(hub: &Hub, base: &str, id: &str, approve: bool) -> Result<Decided> {
    let body = HostDecision { decision: if approve { Decision::Approve } else { Decision::Deny } };
    cloud::send(hub, base, &path::host_peer_decision(id), &body).await
}

/// `peerRequests`: the computers asking to pair with this one, each with its code once it has one.
pub fn requests_json(hub: &Hub) -> Vec<Value> {
    hub.cloud
        .pairings
        .incoming
        .locked()
        .iter()
        .map(|r| {
            json!({"requestId": r.id, "computerId": r.computer_id, "name": r.name, "code": r.code, "expiresAt": r.expires_at})
        })
        .collect()
}

/// `decidePeerRequest` (on this computer only): a yes needs the code the asking computer shows, typed here.
pub async fn decide(hub: &Hub, id: &str, approve: bool, code: Option<&str>) -> Result<()> {
    let base = cloud::base(hub)?;
    let r = hub.cloud.pairings.incoming.locked().iter().find(|r| r.id == id).cloned();
    let r = r.ok_or_else(|| UserError("No computer is asking to pair with that request.".into()))?;
    let nd = if approve {
        if hub.cloud.status().owner_change.is_some() {
            return Err(Conflict("Confirm this computer's owner first.").into());
        }
        let (Some(ours), Some(nd)) = (&r.code, r.device_nonce) else {
            return Err(Conflict("The other computer hasn't shown its code yet.").into());
        };
        if !code.is_some_and(|c| crypto::ct_eq(c.trim().as_bytes(), ours.as_bytes())) {
            return Err(UserError("That isn't the code this computer shows for the request.".into()).into());
        }
        Some(nd)
    } else {
        None
    };
    let answer = decision(hub, &base, id, approve).await;
    hub.cloud.pairings.incoming.locked().retain(|x| x.id != id);
    let answer = answer?;
    let Some(nd) = nd else { return Ok(()) };
    let grant = answer.grant_id.filter(|g| g.starts_with("pgr_")).ok_or_else(|| anyhow!("the cloud gave no grant"))?;
    hub.store.put_peer_in(&PeerIn {
        computer_id: r.computer_id,
        name: r.name,
        device_key: b64(&r.device_key),
        epoch: epoch(&hub.identity.sign_pub(), &r.device_key, &nd, &r.host_nonce),
        grant_id: grant.clone(),
        paired_at: now_ms(),
    })?;
    tracing::info!(grant_id = grant, "paired with another computer");
    hub.auth_changed();
    Ok(())
}

// MARK: both, from the state pull

/// The cloud can only end a pairing: a row whose grant it no longer lists (with the same keys and computers) is
/// removed, and nothing it lists adds one. A pending pairing on A has no grant yet and waits for its answer.
pub(super) fn apply(hub: &Hub, peers: Option<&[HostPeer]>) -> Result<()> {
    let Some(peers) = peers else { return Ok(()) };
    let me = hub.identity.computer_id();
    let mut changed = false;
    for p in hub.store.peers_in()? {
        let listed = peers.iter().any(|g| {
            g.grant_id == p.grant_id
                && g.device_key == p.device_key
                && g.from_computer_id == p.computer_id
                && g.to_computer_id == me
        });
        if !listed {
            tracing::info!(computer_id = p.computer_id, "the cloud no longer lists a pairing; unpairing");
            changed |= hub.store.remove_peer_in(&p.computer_id)?;
        }
    }
    for p in hub.store.peers_out()? {
        let Some(grant) = &p.grant_id else { continue };
        let listed = peers.iter().any(|g| {
            &g.grant_id == grant
                && g.device_key == p.device_key
                && g.from_computer_id == me
                && g.to_computer_id == p.computer_id
        });
        if !listed {
            tracing::info!(computer_id = p.computer_id, "the cloud no longer lists a pairing; unpairing");
            hub.store.remove_peer_out(&p.computer_id)?;
        }
    }
    if changed {
        hub.auth_changed();
    }
    Ok(())
}

/// Every pairing ends, and none in progress finishes: the owner they were made for is not the cloud's any more.
pub(super) fn drop_all(hub: &Hub) -> Result<()> {
    hub.cloud.pairings.incoming.locked().clear();
    hub.cloud.pairings.outgoing.locked().clear();
    let mut changed = false;
    for p in hub.store.peers_in()? {
        changed |= hub.store.remove_peer_in(&p.computer_id)?;
    }
    for p in hub.store.peers_out()? {
        hub.store.remove_peer_out(&p.computer_id)?;
    }
    if changed {
        tracing::warn!("the cloud names another owner: every paired computer is unpaired");
        hub.auth_changed();
    }
    Ok(())
}

// MARK: A, asking

async fn get<T: serde::de::DeserializeOwned>(hub: &Hub, base: &str, path: &str) -> Result<T> {
    let v = cloud::call(hub, base, Method::GET, path, None).await?;
    Ok(serde_json::from_value(v)?)
}

/// `peerComputers`: the account's other computers, each with this computer's pairing with it.
pub async fn computers(hub: &Hub) -> Result<Value> {
    let list: PeerComputerList = get(hub, &cloud::base(hub)?, path::HOST_PEER_COMPUTERS).await?;
    let computers: Vec<Value> = list
        .computers
        .iter()
        .map(|c| {
            let out = hub.store.peer_out(&c.computer_id);
            let link = match &out {
                Some(p) if p.paired => "paired",
                Some(_) => "pending",
                None => "none",
            };
            json!({
                "computerId": c.computer_id,
                "name": c.name,
                "link": link,
                "letsIn": hub.store.peer_in(&c.computer_id).is_some(),
            })
        })
        .collect();
    Ok(json!({ "computers": computers }))
}

/// `linkComputer`: starts pairing with one of the account's other computers, named by its id or its name. A name
/// two of them share is refused with both listed.
pub async fn link(hub: &Arc<Hub>, which: &str) -> Result<Value> {
    let base = cloud::base(hub)?;
    let list: PeerComputerList = get(hub, &base, path::HOST_PEER_COMPUTERS).await?;
    let found: Vec<&PeerComputer> =
        list.computers.iter().filter(|c| c.computer_id == which || c.name.eq_ignore_ascii_case(which)).collect();
    let c = match found.as_slice() {
        [c] => *c,
        [] => return Err(UserError(format!("No other computer of this account is called {which}.")).into()),
        many => {
            let ids: Vec<String> = many.iter().map(|c| format!("{} ({})", c.name, c.computer_id)).collect();
            return Err(UserError(format!("More than one computer is called {which}: {}.", ids.join(", "))).into());
        }
    };
    let sign_key = crypto::unb64_n::<32>(&c.sign_key)?;
    vault::unlock(hub.clone()).await?;
    let seed: [u8; 32] = crypto::random();
    let dk = SigningKey::from_bytes(&seed).verifying_key().to_bytes();
    vault::write(&hub.store, &key_slot(&c.computer_id), &b64(&seed))?;
    // A pairing already there ends here: its grant is no longer this computer's to use.
    hub.store.start_peer_out(&c.computer_id, &c.name, &b64(&sign_key), &b64(&dk))?;
    hub.cloud.pairings.outgoing.locked().remove(&c.computer_id);
    let device_nonce: [u8; 32] = crypto::random();
    let body = CreatePeerRequest {
        computer_id: c.computer_id.clone(),
        device_key: b64(&dk),
        commit: b64(&crypto::sas_commit(Labels::Kith, &dk, &device_nonce)),
    };
    let made: AccessRequestCreated = cloud::send(hub, &base, path::HOST_CREATE_PEER_REQUEST, &body).await?;
    hub.store.set_peer_out_request(&c.computer_id, &made.request_id)?;
    hub.cloud.pairings.outgoing.locked().insert(
        c.computer_id.clone(),
        Outgoing { request_id: made.request_id, device_nonce, host_nonce: None, code: None },
    );
    status(hub, &c.computer_id).await
}

fn answer(p: &PeerOut, state: &str) -> Value {
    json!({"computerId": p.computer_id, "name": p.name, "state": state})
}

/// `linkStatus`: moves a pairing this computer started one step on and says where it is: `waiting` for the other
/// computer, `code` to compare on both, `connecting` once approved, then `paired`; or `denied`, `expired`, `none`.
pub async fn status(hub: &Arc<Hub>, cid: &str) -> Result<Value> {
    let Some(p) = hub.store.peer_out(cid) else { return Ok(json!({"computerId": cid, "state": "none"})) };
    if p.paired {
        return Ok(answer(&p, "paired"));
    }
    if p.grant_id.is_none() {
        let base = cloud::base(hub)?;
        let o = hub.cloud.pairings.outgoing.locked().get(cid).cloned();
        let (Some(mut o), Some(request_id)) = (o, p.request_id.clone()) else {
            // Its nonce went with a restart: it can't be finished.
            forget(hub, &p).await;
            return Ok(answer(&p, "expired"));
        };
        let s: AccessStatus = get(hub, &base, &path::host_get_peer_request(&request_id)).await?;
        if o.request_id != request_id || s.computer_id.as_deref().is_some_and(|c| c != cid) {
            forget(hub, &p).await;
            return Ok(answer(&p, "expired"));
        }
        let dk = crypto::unb64_n::<32>(&p.device_key)?;
        let b_sign = crypto::unb64_n::<32>(&p.sign_key)?;
        let nh = s.host_nonce.as_deref().map(crypto::unb64_n::<32>).transpose()?;
        match s.status {
            AccessState::Pending => {
                if let (None, Some(nh)) = (o.host_nonce, nh) {
                    let body = Reveal { nonce: b64(&o.device_nonce) };
                    cloud::send::<_, EmptyAnswer>(hub, &base, &path::host_reveal_peer_request(&request_id), &body)
                        .await?;
                    // Computed with the sign key pinned when the pairing started, not whatever the cloud lists now.
                    o.code = Some(crypto::sas_code(Labels::Kith, &b_sign, &dk, &o.device_nonce, &nh));
                    o.host_nonce = Some(nh);
                    hub.cloud.pairings.outgoing.locked().insert(cid.to_owned(), o.clone());
                }
                let mut v = answer(&p, if o.code.is_some() { "code" } else { "waiting" });
                v["code"] = o.code.into();
                return Ok(v);
            }
            AccessState::Approved => {
                let (Some(ours), Some(grant)) = (o.host_nonce, s.grant_id.as_deref()) else {
                    forget(hub, &p).await;
                    return Ok(answer(&p, "expired"));
                };
                if nh != Some(ours) {
                    tracing::warn!(
                        computer_id = cid,
                        "pairing approved under another nonce than the code's; dropping it"
                    );
                    forget(hub, &p).await;
                    return Ok(answer(&p, "expired"));
                }
                hub.store.set_peer_out_granted(cid, grant, &epoch(&b_sign, &dk, &o.device_nonce, &ours))?;
                hub.cloud.pairings.outgoing.locked().remove(cid);
            }
            AccessState::Denied => {
                forget(hub, &p).await;
                return Ok(answer(&p, "denied"));
            }
            AccessState::Expired | AccessState::Cancelled => {
                forget(hub, &p).await;
                return Ok(answer(&p, "expired"));
            }
            // A state from a newer cloud: nothing moves until it is one this computer knows.
            AccessState::Unknown => return Ok(answer(&p, "waiting")),
        }
    }
    let Some(p) = hub.store.peer_out(cid) else { return Ok(json!({"computerId": cid, "state": "none"})) };
    match first_call(hub, &p).await {
        Ok(agents) => {
            let grant = p.grant_id.as_deref().unwrap_or_default();
            if !hub.store.set_peer_out_paired(cid, grant)? {
                return Ok(json!({"computerId": cid, "state": "none"}));
            }
            tracing::info!(computer_id = cid, "paired: the other computer answered over its pinned channel");
            let mut v = answer(&p, "paired");
            v["agents"] = agents;
            Ok(v)
        }
        Err(why) => {
            // B may not have published its new list of keys yet: the next step tries again.
            let mut v = answer(&p, "connecting");
            v["error"] = why.into();
            Ok(v)
        }
    }
}

/// The first call over the channel to B, pinned to the sign key the code was computed with. A stays pending until
/// it succeeds.
async fn first_call(hub: &Arc<Hub>, p: &PeerOut) -> Result<Value, String> {
    Ok(call_peer(hub, p, kith_wire::rpc::method::PEER_AGENTS, json!({})).await?["agents"].take())
}

/// One call to a paired computer over the channel pinned at pairing: the one way this computer sends B something (Mac
/// link). An error is a message to show; B being off is one.
pub async fn call_peer(hub: &Arc<Hub>, p: &PeerOut, method: &str, params: Value) -> Result<Value, String> {
    let base = cloud::base(hub).map_err(|e| e.to_string())?;
    vault::unlock(hub.clone()).await.map_err(|e| e.to_string())?;
    let seed = vault::read(&hub.store, &key_slot(&p.computer_id)).map_err(|e| e.to_string())?;
    let seed = seed.as_deref().map(crypto::unb64_n::<32>).transpose().map_err(|e| e.to_string())?;
    let seed = seed.ok_or("this computer's key for the pairing is missing")?;
    let host_sign_key = crypto::unb64_n::<32>(&p.sign_key).map_err(|e| e.to_string())?;
    let target = kith_guest::relay::Target { cloud: base, computer_id: p.computer_id.clone(), host_sign_key };
    let fail = |f: kith_guest::relay::Fail| f.into_cli().message;
    let mut channel =
        kith_guest::relay::connect(&target, &SigningKey::from_bytes(&seed), FIRST_CALL).await.map_err(fail)?;
    let answer = channel.call(method, params, FIRST_CALL).await;
    channel.close().await;
    answer.map_err(fail)
}

/// Ends a pairing this computer started that can't finish: the request goes, and the row with it.
async fn forget(hub: &Hub, p: &PeerOut) {
    hub.cloud.pairings.outgoing.locked().remove(&p.computer_id);
    if let (Some(id), Ok(base)) = (&p.request_id, cloud::base(hub)) {
        // Already ended in the cloud is fine.
        drop(cloud::call(hub, &base, Method::DELETE, &path::host_cancel_peer_request(id), None).await);
    }
    if let Err(e) = hub.store.remove_peer_out(&p.computer_id) {
        tracing::warn!(error = format!("{e:#}"), "couldn't remove a pairing");
    }
}

/// `unlinkComputer`: ends this computer's pairings with another one (by id, or by a name only one of them has), both
/// ways, here first and then in the cloud. Works without the cloud.
pub async fn unlink(hub: &Hub, which: &str) -> Result<Value> {
    let mut known: Vec<(String, String)> = hub.store.peers_in()?.into_iter().map(|p| (p.computer_id, p.name)).collect();
    known.extend(hub.store.peers_out()?.into_iter().map(|p| (p.computer_id, p.name)));
    let mut ids: Vec<&str> = known
        .iter()
        .filter(|(id, name)| id == which || name.eq_ignore_ascii_case(which))
        .map(|(id, _)| id.as_str())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    let cid = match ids.as_slice() {
        [cid] => cid.to_string(),
        [] => return Err(UserError(format!("This computer isn't paired with {which}.")).into()),
        many => {
            return Err(
                UserError(format!("More than one paired computer is called {which}: {}.", many.join(", "))).into()
            );
        }
    };
    let cid = cid.as_str();
    let incoming = hub.store.peer_in(cid);
    let outgoing = hub.store.peer_out(cid);
    let mut grants = vec![];
    if let Some(p) = incoming {
        hub.store.remove_peer_in(cid)?;
        hub.auth_changed();
        grants.push(p.grant_id);
    }
    if let Some(p) = outgoing {
        forget(hub, &p).await;
        grants.extend(p.grant_id);
    }
    let Ok(base) = cloud::base(hub) else { return Ok(json!({"ok": true})) };
    for g in grants {
        // Removed here already, so a cloud that can't be reached now only keeps a grant nothing here uses.
        if let Err(e) = cloud::call(hub, &base, Method::POST, &path::host_revoke_peer(&g), Some(&json!({}))).await
            && cloud::cloud_status_of(&e) != Some(404)
        {
            tracing::warn!(error = format!("{e:#}"), grant_id = g, "couldn't end a pairing in the cloud");
        }
    }
    Ok(json!({"ok": true}))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::api::devices::{Caller, Principal, principal};
    use crate::api::rig::Rig;
    use crate::store::Scope;

    /// The asking computer's side, as the cloud passes it on.
    struct Asker {
        dk: [u8; 32],
        nd: [u8; 32],
    }

    impl Asker {
        fn new() -> Self {
            Self { dk: crypto::random(), nd: crypto::random() }
        }

        fn request(&self, id: &str, host_nonce: Option<&str>, revealed: bool) -> Value {
            json!({"requestId": id, "computerId": "cmp_a", "name": "Studio", "deviceKey": b64(&self.dk),
                "commit": b64(&crypto::sas_commit(Labels::Kith, &self.dk, &self.nd)), "hostNonce": host_nonce,
                "deviceNonce": revealed.then(|| b64(&self.nd)), "createdAt": 1, "expiresAt": now_ms() + 600_000})
        }

        fn listed(&self, me: &str) -> Value {
            json!({"grantId": "pgr_new", "fromComputerId": "cmp_a", "toComputerId": me, "name": "Studio",
                "deviceKey": b64(&self.dk)})
        }

        fn caller(&self) -> Caller {
            Caller::Device { key: b64(&self.dk), scopes: vec![Scope::Control] }
        }
    }

    fn state(requests: &[Value], peers: &[Value]) -> Value {
        json!({"owner": {"userId": "user_owner"}, "grants": [], "requests": [], "peerRequests": requests, "peers": peers})
    }

    async fn pull(r: &Rig, state: Value) {
        r.set_cloud_state(state);
        cloud::pull(&r.hub, &cloud::url(&r.hub.store).unwrap()).await.unwrap();
    }

    fn nonce_sent(r: &Rig, id: &str) -> Option<String> {
        let to = path::host_peer_nonce(id);
        r.posts().into_iter().rev().find(|(p, _)| *p == to).map(|(_, b)| b["nonce"].as_str().unwrap().to_owned())
    }

    fn decided(r: &Rig, id: &str) -> Vec<String> {
        let to = path::host_peer_decision(id);
        r.posts()
            .into_iter()
            .filter(|(p, _)| *p == to)
            .map(|(_, b)| b["decision"].as_str().unwrap().to_owned())
            .collect()
    }

    /// B's side up to the code: the code A shows, and the host nonce it was computed with.
    async fn offered(r: &Rig, id: &str, a: &Asker) -> (String, [u8; 32]) {
        pull(r, state(&[a.request(id, None, false)], &[])).await;
        let nh = nonce_sent(r, id).expect("answered with a nonce");
        pull(r, state(&[a.request(id, Some(&nh), true)], &[])).await;
        let nh = crypto::unb64_n::<32>(&nh).unwrap();
        (crypto::sas_code(Labels::Kith, &r.hub.identity.sign_pub(), &a.dk, &a.nd, &nh), nh)
    }

    #[tokio::test]
    async fn a_computer_gets_in_only_by_the_code_typed_here() {
        let r = Rig::new().await;
        let a = Asker::new();
        let (code, nh) = offered(&r, "preq_1", &a).await;
        assert_eq!(requests_json(&r.hub)[0]["code"], code.as_str());
        assert!(r.hub.store.peer_in("cmp_a").is_none(), "a matching reveal alone lets nobody in");
        for typed in [None, Some("000000"), Some("")].into_iter().filter(|t| *t != Some(code.as_str())) {
            let e = decide(&r.hub, "preq_1", true, typed).await.unwrap_err();
            assert!(e.is::<UserError>(), "{e:#}");
        }
        assert_eq!(requests_json(&r.hub).len(), 1, "a wrong code keeps the request");
        assert_eq!(decided(&r, "preq_1"), Vec::<String>::new());
        decide(&r.hub, "preq_1", true, Some(&format!(" {code} "))).await.unwrap();
        let row = r.hub.store.peer_in("cmp_a").expect("paired");
        assert_eq!((row.device_key.as_str(), row.grant_id.as_str()), (b64(&a.dk).as_str(), "pgr_new"));
        assert_eq!(row.epoch, epoch(&r.hub.identity.sign_pub(), &a.dk, &a.nd, &nh), "the code exchange's");
        assert_eq!(principal(&r.hub.store, &a.caller()), Principal::Peer("cmp_a".into()));
        assert_eq!(decided(&r, "preq_1"), ["approve"]);
        assert_eq!(requests_json(&r.hub), Vec::<Value>::new());
    }

    #[tokio::test]
    async fn a_request_the_cloud_changed_is_denied() {
        let r = Rig::new().await;
        let a = Asker::new();
        pull(&r, state(&[a.request("preq_1", None, false)], &[])).await;
        let nh = nonce_sent(&r, "preq_1").unwrap();
        let swapped = Asker { dk: crypto::random(), nd: a.nd };
        pull(&r, state(&[swapped.request("preq_1", Some(&nh), true)], &[])).await;
        assert_eq!(decided(&r, "preq_1"), ["deny"], "another device key after the nonce went out");

        let b = Asker::new();
        pull(&r, state(&[b.request("preq_2", None, false)], &[])).await;
        let nh = nonce_sent(&r, "preq_2").unwrap();
        let mut lying = b.request("preq_2", Some(&nh), true);
        let other: [u8; 32] = crypto::random();
        lying["deviceNonce"] = b64(&other).into();
        pull(&r, state(&[lying], &[])).await;
        assert_eq!(decided(&r, "preq_2"), ["deny"], "a reveal that doesn't match its commit");

        let mut own = Asker::new().request("preq_4", None, false);
        own["computerId"] = r.hub.identity.computer_id().into();
        let forgotten = Asker::new().request("preq_3", Some("bm9uY2U"), false);
        pull(&r, state(&[forgotten, own], &[])).await;
        assert_eq!(decided(&r, "preq_3"), ["deny"], "a nonce this computer doesn't hold");
        assert_eq!(decided(&r, "preq_4"), ["deny"], "pairing with itself");
        assert!(nonce_sent(&r, "preq_4").is_none());
        assert_eq!(r.hub.store.peers_in().unwrap(), Vec::<PeerIn>::new());
    }

    #[tokio::test]
    async fn auto_approval_never_pairs_and_pairing_has_its_own_guesses() {
        let r = Rig::new().await;
        cloud::set_approval(&r.hub, cloud::Approval::Auto).unwrap();
        // Account devices used up their nonces this hour; a pairing still gets one.
        cloud::set_nonce_budget(&r.hub.store, "sas_nonces", &[now_ms(); cloud::SAS_NONCES_PER_HOUR]);
        let a = Asker::new();
        offered(&r, "preq_1", &a).await;
        assert!(r.hub.store.peer_in("cmp_a").is_none() && decided(&r, "preq_1").is_empty(), "Auto never approves one");
        // Each nonce is one guess at a code: a few an hour, whatever the account devices did.
        let asked: Vec<Value> = (0..8)
            .map(|i| {
                let mut q = Asker::new().request(&format!("preq_n{i}"), None, false);
                q["computerId"] = format!("cmp_{i}").into();
                q
            })
            .collect();
        pull(&r, state(&asked, &[])).await;
        let sent = (0..8).filter(|i| nonce_sent(&r, &format!("preq_n{i}")).is_some()).count();
        assert_eq!(sent, cloud::SAS_NONCES_PER_HOUR - 1, "preq_1 had one");
        assert_eq!(cloud::nonce_budget(&r.hub.store, "sas_nonces").len(), cloud::SAS_NONCES_PER_HOUR);
    }

    #[tokio::test]
    async fn the_cloud_can_end_a_pairing_but_never_make_one() {
        let r = Rig::new().await;
        let me = r.hub.identity.computer_id();
        let a = Asker::new();
        let (code, _) = offered(&r, "preq_1", &a).await;
        decide(&r.hub, "preq_1", true, Some(&code)).await.unwrap();
        pull(&r, state(&[], &[a.listed(&me)])).await;
        assert!(r.hub.store.peer_in("cmp_a").is_some(), "listed: kept");

        let injected = Asker::new();
        let mut other = injected.listed(&me);
        other["fromComputerId"] = "cmp_x".into();
        other["grantId"] = "pgr_x".into();
        pull(&r, state(&[], &[a.listed(&me), other])).await;
        assert_eq!(r.hub.store.peers_in().unwrap().len(), 1, "a grant nobody typed a code for adds nothing");
        assert_eq!(principal(&r.hub.store, &injected.caller()), Principal::Nobody);

        let mut old = state(&[], &[]);
        old.as_object_mut().unwrap().remove("peers");
        pull(&r, old).await;
        assert!(r.hub.store.peer_in("cmp_a").is_some(), "a cloud that predates pairing ends nothing");

        let mut swapped = a.listed(&me);
        swapped["deviceKey"] = b64(&injected.dk).into();
        pull(&r, state(&[], &[swapped])).await;
        assert!(r.hub.store.peer_in("cmp_a").is_none(), "its grant listed with another key: ended");
        assert_eq!(principal(&r.hub.store, &a.caller()), Principal::Nobody);
    }

    #[tokio::test]
    async fn another_owner_in_the_cloud_ends_every_pairing() {
        let r = Rig::new().await;
        let me = r.hub.identity.computer_id();
        let a = Asker::new();
        let (code, _) = offered(&r, "preq_1", &a).await;
        decide(&r.hub, "preq_1", true, Some(&code)).await.unwrap();
        r.hub.store.start_peer_out("cmp_b", "Laptop", "sign", "dk").unwrap();
        let b = Asker::new();
        pull(&r, state(&[b.request("preq_2", None, false)], &[a.listed(&me)])).await;
        assert_eq!(requests_json(&r.hub).len(), 1);

        let mut held = state(&[b.request("preq_2", None, false)], &[a.listed(&me)]);
        held["owner"] = json!({"userId": "user_mallory"});
        pull(&r, held).await;
        assert!(r.hub.cloud.status().owner_change.is_some());
        assert!(r.hub.store.peers_in().unwrap().is_empty() && r.hub.store.peers_out().unwrap().is_empty());
        assert!(requests_json(&r.hub).is_empty(), "none in progress finishes");
        assert!(decide(&r.hub, "preq_2", true, Some(&code)).await.is_err());
    }

    #[tokio::test]
    async fn pairing_again_is_a_new_epoch() {
        let r = Rig::new().await;
        let a = Asker::new();
        let (code, _) = offered(&r, "preq_1", &a).await;
        decide(&r.hub, "preq_1", true, Some(&code)).await.unwrap();
        let first = r.hub.store.peer_in("cmp_a").unwrap().epoch;
        let again = Asker { dk: a.dk, nd: crypto::random() };
        let (code, _) = offered(&r, "preq_2", &again).await;
        decide(&r.hub, "preq_2", true, Some(&code)).await.unwrap();
        assert_ne!(r.hub.store.peer_in("cmp_a").unwrap().epoch, first, "same keys, new pairing");
    }

    #[tokio::test]
    async fn the_asking_side_pins_the_key_it_computed_the_code_with() {
        let r = Rig::new().await;
        *r.hub.store.secret_key.locked() = Some(zeroize::Zeroizing::new(vec![7; 32]));
        let answer = |path: &str, v: Value| {
            r.fake.answers.lock().unwrap().insert(path.to_owned(), v);
        };
        let computers =
            |key: &[u8; 32]| json!({"computers": [{"computerId": "cmp_b", "name": "Studio", "signKey": b64(key)}]});
        let b_sign: [u8; 32] = crypto::random();
        let at = path::host_get_peer_request("preq_1");
        answer(path::HOST_PEER_COMPUTERS, computers(&b_sign));
        answer(path::HOST_CREATE_PEER_REQUEST, json!({"requestId": "preq_1", "expiresAt": now_ms() + 600_000}));
        answer(&at, json!({"requestId": "preq_1", "computerId": "cmp_b", "status": "pending"}));
        assert_eq!(link(&r.hub, "studio").await.unwrap()["state"], "waiting");
        let made = r.posts().into_iter().find(|(p, _)| p == path::HOST_CREATE_PEER_REQUEST).unwrap().1;
        let dk = crypto::unb64_n::<32>(made["deviceKey"].as_str().unwrap()).unwrap();

        // The cloud lists another key for B once the pairing started.
        let swapped: [u8; 32] = crypto::random();
        answer(path::HOST_PEER_COMPUTERS, computers(&swapped));
        let nh: [u8; 32] = crypto::random();
        answer(&at, json!({"requestId": "preq_1", "computerId": "cmp_b", "status": "pending", "hostNonce": b64(&nh)}));
        let v = status(&r.hub, "cmp_b").await.unwrap();
        let revealed = r.posts().into_iter().find(|(p, _)| *p == path::host_reveal_peer_request("preq_1")).unwrap().1;
        let nd = crypto::unb64_n::<32>(revealed["nonce"].as_str().unwrap()).unwrap();
        assert_eq!(b64(&crypto::sas_commit(Labels::Kith, &dk, &nd)), made["commit"].as_str().unwrap());
        assert_eq!(v["state"], "code");
        assert_eq!(v["code"], crypto::sas_code(Labels::Kith, &b_sign, &dk, &nd, &nh).as_str(), "the pinned key's");

        // Approved: granted with the exchange's epoch, and pending until a call over the pinned channel answers.
        answer(
            &at,
            json!({"requestId": "preq_1", "computerId": "cmp_b", "status": "approved", "hostNonce": b64(&nh),
                "grantId": "pgr_b"}),
        );
        let v = status(&r.hub, "cmp_b").await.unwrap();
        assert_eq!(v["state"], "connecting", "{v}");
        let row = r.hub.store.peer_out("cmp_b").unwrap();
        assert_eq!((row.paired, row.grant_id.as_deref(), row.sign_key), (false, Some("pgr_b"), b64(&b_sign)));
        assert_eq!(row.epoch.unwrap(), epoch(&b_sign, &dk, &nd, &nh));
    }
}
