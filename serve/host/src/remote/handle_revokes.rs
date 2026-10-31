//! A revoke of a person known only by a chat handle (a Telegram or Discord id, no Kith account) on the owner's other
//! computers. The cloud knows nothing of such a person and is told nothing new; the computers tell each other over Mac
//! link (`peerRevokes`).
//!
//! Each revoke or undo is stamped with the time it was made and kept per handle (`handle_revokes`); the latest stamp
//! wins on every computer, so two computers that disagree end up agreeing. A computer asks each paired computer for its
//! list and sends its own in the same call. The per-peer outbox is the acknowledged stamp (`peer_revokes_ack.<id>`): while
//! this computer holds a change the peer has not acknowledged (the peer was off), every later pull tries again, and the
//! answer brings back what the peer knows that this one doesn't, so a computer that was off catches up when it is on.

use super::account;
use super::members::AuthPerson;
use crate::api::people::local_revoke;
use crate::hub::Hub;
use crate::store::{HandleRevoke, PeerOut, now_ms};
use anyhow::{Result, anyhow};
use serde_json::json;
use std::future::Future;
use std::sync::Arc;

/// kv: the newest stamp a peer is known to hold, per computer id.
const ACK: &str = "peer_revokes_ack.";
/// kv: when a peer was last asked, per computer id.
const LAST: &str = "peer_revokes_last.";
/// Even with nothing to send, a peer is asked this often, so a change made on a computer that cannot call this one arrives.
const POLL_MS: i64 = 10 * 60 * 1000;

/// `platform:id` for each chat handle the person has.
pub fn handles_of(p: &AuthPerson) -> Vec<String> {
    p.handles.iter().map(|(platform, id)| format!("{platform}:{id}")).collect()
}

/// A handle as it travels: `platform:id`, short, nothing a command line or a row could be confused by.
fn valid(handle: &str) -> bool {
    handle.len() <= 80
        && handle.split_once(':').is_some_and(|(platform, id)| {
            !platform.is_empty()
                && !id.is_empty()
                && platform != "kith"
                && platform.chars().all(|c| c.is_ascii_lowercase())
                && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
}

/// The owner revoked or restored a person known only by handle, here: stamped, kept, and sent to the paired computers.
pub fn changed(hub: &Arc<Hub>, who: &AuthPerson, revoked: bool) {
    let at = now_ms();
    for handle in handles_of(who) {
        if let Err(e) = hub.store.put_handle_revoke(&HandleRevoke { handle, revoked, at }) {
            tracing::warn!(
                error = format!("{e:#}"),
                person = who.id,
                "couldn't keep a handle revoke for the other computers"
            );
        }
    }
    tokio::spawn(sync_peers(hub.clone()));
}

/// Takes the other side's list: every entry newer than what this computer knows of that handle is kept, and the person
/// with the handle is revoked or restored here to match. Answers this computer's own list. The state of a person
/// revoked before stamps existed counts as stamped when kith-auth says they were revoked.
pub async fn exchange_in(hub: &Arc<Hub>, sent: Vec<HandleRevoke>) -> Result<Vec<HandleRevoke>> {
    let bin = account::bin().await;
    for entry in sent.into_iter().filter(|e| valid(&e.handle)) {
        let people = match &bin {
            Some(bin) => super::members::auth_people(bin).await?,
            None => Vec::new(),
        };
        let mine: Vec<&AuthPerson> = people
            .iter()
            .filter(|p| !p.owner && p.bot_of.is_empty() && p.kith.is_empty() && handles_of(p).contains(&entry.handle))
            .collect();
        let legacy = mine.iter().filter_map(|p| p.revoked_at).max().map(|s| s * 1000);
        let known = hub.store.handle_revoke_at(&entry.handle).or(legacy).unwrap_or(0);
        if entry.at <= known {
            continue;
        }
        hub.store.put_handle_revoke(&entry)?;
        if let Some(bin) = &bin {
            for p in mine.into_iter().filter(|p| p.revoked_at.is_some() != entry.revoked) {
                tracing::info!(
                    person = p.id,
                    revoked = entry.revoked,
                    "another of the owner's computers (Mac link) changed this person"
                );
                local_revoke(hub, bin, &p.id, entry.revoked).await?;
            }
        }
    }
    hub.store.handle_revokes()
}

fn newest(list: &[HandleRevoke]) -> i64 {
    list.iter().map(|h| h.at).max().unwrap_or(0)
}

fn kv_num(hub: &Hub, prefix: &str, cid: &str) -> i64 {
    hub.store.kv_get(&format!("{prefix}{cid}")).and_then(|v| v.parse().ok()).unwrap_or(0)
}

/// Whether the peer is owed a call: it has not acknowledged our newest change, or it was not asked for a while.
pub fn due(hub: &Hub, cid: &str) -> bool {
    let top = newest(&hub.store.handle_revokes().unwrap_or_default());
    top > kv_num(hub, ACK, cid) || now_ms() - kv_num(hub, LAST, cid) > POLL_MS
}

/// One exchange with one peer through `send` (its list in, the peer's list back). The peer's acknowledgement is only
/// recorded after `send` succeeded, so a peer that is off stays owed. Then the peer's list is taken as it would be
/// had the peer called.
pub async fn exchange_with<F, Fut>(hub: &Arc<Hub>, cid: &str, send: F) -> Result<()>
where
    F: FnOnce(Vec<HandleRevoke>) -> Fut,
    Fut: Future<Output = Result<Vec<HandleRevoke>>>,
{
    let ours = hub.store.handle_revokes()?;
    let top = newest(&ours);
    let theirs = send(ours).await?;
    hub.store.kv_set(&format!("{ACK}{cid}"), &top.to_string())?;
    hub.store.kv_set(&format!("{LAST}{cid}"), &now_ms().to_string())?;
    exchange_in(hub, theirs).await?;
    Ok(())
}

async fn over_mac_link(hub: &Arc<Hub>, p: &PeerOut, list: Vec<HandleRevoke>) -> Result<Vec<HandleRevoke>> {
    let answer = super::peers::call_peer(hub, p, kith_wire::rpc::method::PEER_REVOKES, json!({ "handles": list }))
        .await
        .map_err(|e| anyhow!("{e}"))?;
    Ok(serde_json::from_value::<kith_wire::rpc::HandleRevokes>(answer)?.handles)
}

/// Calls each paired computer that is owed a call. Failures are logged and retried by the next pull; one pass at a time.
pub async fn sync_peers(hub: Arc<Hub>) {
    static ONE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _one = ONE.lock().await;
    for p in hub.store.peers_out().unwrap_or_default().into_iter().filter(|p| p.paired) {
        if !due(&hub, &p.computer_id) {
            continue;
        }
        let peer = p.clone();
        let h = hub.clone();
        if let Err(e) =
            exchange_with(&hub, &p.computer_id, |list| async move { over_mac_link(&h, &peer, list).await }).await
        {
            tracing::debug!(
                error = format!("{e:#}"),
                computer = p.computer_id,
                "a paired computer wasn't reached for the handle revokes"
            );
        }
    }
}
