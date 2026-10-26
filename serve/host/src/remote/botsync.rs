//! Bot guests, from the cloud's side (docs/kith-bot-guests.md 5.5, 7.1): which bot keys the cloud lists as having
//! accepted an invite. The cloud only records the accept; this host decides. It admits a key only for an invite it made
//! and still has open, binds the key to that invite's bot, and lists it in the signed ACL. A key the cloud made up is
//! never admitted, and one that never shows the invite token (`proveBotInvite`) is dropped again after ten minutes.

use crate::api::botguests;
use crate::hub::Hub;
use crate::remote::crypto;
use crate::store::{BotState, Device, DeviceSource, Scope, Store, now_ms};
use kith_wire::cloud::path;
use serde_json::json;

/// kv: cloud bot devices (`bd_...`) this host ended and the cloud has not yet confirmed as revoked (JSON array).
const PENDING_REVOKES: &str = "bot_device_revokes";
/// kv: bot invites to cancel in the cloud (JSON array).
const PENDING_INVITE_REVOKES: &str = "bot_invite_revokes";

/// A key that joined and does not show its token in this long is removed again.
pub const JOIN_MS: i64 = 10 * 60 * 1000;
/// A request nobody answers in this long is a no (Kai's standing answer to open question 6).
pub const PENDING_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// One active bot device of `GET /v1/host/state`: the cloud contract's `BotDevice`.
pub use kith_wire::cloud::BotDevice as Listed;

/// Applies the cloud's bot list. `None` (an older cloud that doesn't list bots) changes nothing. Never fails the pull:
/// a problem is logged and the next pull tries again.
pub async fn sync(hub: &Hub, listed: Option<&[Listed]>) {
    if let Some(listed) = listed
        && let Err(e) = apply(hub, listed).await
    {
        tracing::warn!(error = format!("{e:#}"), "couldn't apply the cloud's bot devices");
    }
    sweep(hub).await;
}

pub async fn apply(hub: &Hub, listed: &[Listed]) -> anyhow::Result<()> {
    let now = now_ms();
    for l in listed {
        let known = hub.store.bot_guests()?.into_iter().any(|g| g.cloud_device.as_deref() == Some(&l.bot_device_id));
        if known {
            continue;
        }
        admit(hub, l, now);
    }
    // The cloud stopped listing a bot it had: it removed access (a block, or a revoke this host missed).
    for g in hub.store.bot_guests()?.into_iter().filter(|g| g.state.live() && g.cloud_device.is_some()) {
        let cloud = g.cloud_device.as_deref().unwrap_or_default();
        if !listed.iter().any(|l| l.bot_device_id == cloud) {
            tracing::info!(bot = g.bot_id, "a bot's device is gone from the cloud; ending it");
            botguests::end_bot(hub, &g.bot_id, BotState::Revoked, "cloud", "cloud").await;
        }
    }
    Ok(())
}

/// A raw Ed25519 public key as the relay's `dk` carries it.
fn valid_key(k: &str) -> bool {
    crypto::unb64_n::<32>(k).is_ok()
}

fn admit(hub: &Hub, l: &Listed, now: i64) {
    let Some(invite) = hub.store.bot_invite(&l.invite_id).filter(|i| i.open(now)) else {
        // Not ours (or used up, revoked, expired): nothing here grants it, so it stays a name in the cloud.
        tracing::debug!(device = l.bot_device_id, "ignoring a bot device this computer didn't invite");
        return;
    };
    let Some(bot) = hub.store.bot_guest(&invite.bot_id).filter(|g| g.state.live()) else { return };
    if !valid_key(&l.device_key) {
        tracing::warn!(device = l.bot_device_id, "ignoring a bot device with an invalid key");
        return;
    }
    // A key is one bot's. (The same key on a second bot would be a copied VM or a forged list.)
    if hub.store.device(&l.device_key).is_some() {
        tracing::warn!(device = l.bot_device_id, "ignoring a bot device whose key this computer already knows");
        return;
    }
    let device = Device {
        key: l.device_key.clone(),
        name: bot.name.clone(),
        platform: "bot".into(),
        source: DeviceSource::Bot,
        grant_id: Some(format!("bot:{}", l.bot_device_id)),
        scopes: vec![Scope::Control],
        lease_until: None,
        created_at: now,
        last_seen_at: None,
        person_id: Some(bot.person_id.clone()),
    };
    // A re-keyed bot keeps its state; a new one is `joining` until it shows the invite token.
    let state = if bot.state == BotState::Invited { BotState::Joining } else { bot.state };
    let bound = hub
        .store
        .put_device(&device)
        .and_then(|()| hub.store.bind_bot_device(&bot.bot_id, &l.device_key, &l.bot_device_id, state));
    match bound {
        Ok(true) => {}
        Ok(false) => {
            hub.store.remove_device(&l.device_key).ok();
            return;
        }
        Err(e) => {
            tracing::warn!(error = format!("{e:#}"), bot = bot.bot_id, "couldn't admit a bot device");
            hub.store.remove_device(&l.device_key).ok();
            return;
        }
    }
    // Rotation: the old key stops working in the same step.
    if let Some(old) = bot.device_key.filter(|k| *k != l.device_key) {
        if let Err(e) = hub.revoke_device(&old) {
            tracing::warn!(error = format!("{e:#}"), bot = bot.bot_id, "couldn't remove a bot's old key");
        }
        if let Some(cloud) = &bot.cloud_device {
            queue_device_revoke(&hub.store, cloud);
        }
    }
    tracing::info!(bot = bot.bot_id, "a bot guest joined at the cloud");
    hub.auth_changed();
    botguests::emit_updated(hub, &bot.bot_id);
}

/// What time takes away: keys that never proved, requests nobody answered, invites that ran out unused.
pub async fn sweep(hub: &Hub) {
    sweep_at(hub, now_ms()).await;
}

pub async fn sweep_at(hub: &Hub, now: i64) {
    let Ok(bots) = hub.store.bot_guests() else { return };
    for g in bots.into_iter().filter(|g| g.state.live()) {
        let proven = g.device_key.as_deref().is_some_and(|k| hub.store.device_proven(k));
        let joined_at = g.device_key.as_deref().and_then(|k| hub.store.device(k)).map(|d| d.created_at);
        match g.state {
            BotState::Invited => {
                let open = hub.store.bot_invites_of(&g.bot_id).unwrap_or_default().iter().any(|i| i.open(now));
                if !open {
                    botguests::end_bot(hub, &g.bot_id, BotState::Revoked, "expired", "host").await;
                }
            }
            BotState::Pending if g.joined_at.is_some_and(|t| now - t > PENDING_MS) => {
                tracing::info!(bot = g.bot_id, "nobody answered a bot request in seven days; denying it");
                botguests::end_bot(hub, &g.bot_id, BotState::Denied, "noAnswer", "host").await;
            }
            _ if !proven && joined_at.is_some_and(|t| now - t > JOIN_MS) => {
                tracing::info!(bot = g.bot_id, "a bot never showed its invite token; removing it");
                botguests::end_bot(hub, &g.bot_id, BotState::Revoked, "expired", "host").await;
            }
            _ => {}
        }
    }
}

fn list(store: &Store, key: &str) -> Vec<String> {
    store.kv_get(key).and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default()
}

fn set_list(store: &Store, key: &str, ids: &[String]) {
    let saved = serde_json::to_string(ids).map_err(anyhow::Error::from).and_then(|v| store.kv_set(key, &v));
    if let Err(e) = saved {
        tracing::warn!(error = format!("{e:#}"), "couldn't remember a bot revoke for the cloud");
    }
}

fn queue_device_revoke(store: &Store, id: &str) {
    let mut ids = list(store, PENDING_REVOKES);
    if !ids.iter().any(|i| i == id) {
        ids.push(id.to_owned());
        set_list(store, PENDING_REVOKES, &ids);
    }
}

/// Tells the cloud a bot device ended here; a failure is retried on later pulls (local access is already gone).
pub async fn revoke_device_in_cloud(hub: &Hub, cloud_device: &str) {
    queue_device_revoke(&hub.store, cloud_device);
    retry_revokes(hub).await;
}

/// Cancels a bot invite in the cloud that was never used.
pub async fn revoke_invite_in_cloud(hub: &Hub, invite_id: &str) {
    let mut ids = list(&hub.store, PENDING_INVITE_REVOKES);
    if !ids.iter().any(|i| i == invite_id) {
        ids.push(invite_id.to_owned());
        set_list(&hub.store, PENDING_INVITE_REVOKES, &ids);
    }
    retry_revokes(hub).await;
}

/// A contract path made from one id.
type IdPath = fn(&str) -> String;

pub async fn retry_revokes(hub: &Hub) {
    let paths: [(&str, IdPath); 2] =
        [(PENDING_REVOKES, path::host_revoke_bot_device), (PENDING_INVITE_REVOKES, path::host_revoke_bot_invite)];
    for (key, path) in paths {
        let ids = list(&hub.store, key);
        if ids.is_empty() {
            continue;
        }
        let mut left = vec![];
        for id in ids {
            match super::cloud::post(hub, &path(&id), &json!({})).await {
                Ok(_) => {}
                // Gone there already, or (an invite) accepted meanwhile: the device is what to revoke then.
                Err(e) if super::cloud::refused_with(&e, 404) || super::cloud::refused_with(&e, 409) => {}
                Err(e) => {
                    tracing::warn!(error = format!("{e:#}"), id, "couldn't revoke a bot in the cloud");
                    left.push(id);
                }
            }
        }
        set_list(&hub.store, key, &left);
    }
}
