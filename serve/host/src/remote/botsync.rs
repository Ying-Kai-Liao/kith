//! Bot guests, from the cloud's side (docs/kith-bot-guests.md 5.5, 7.1): which bot keys the cloud lists as having
//! accepted an invite. The cloud only records the accept; this host decides. It admits a key only for an invite it made
//! and still has open, binds the key to that invite's bot, and lists it in the signed ACL. A key the cloud made up is
//! never admitted, and one that never shows the invite token (`proveBotInvite`) is dropped again after ten minutes.

use crate::LockExt;
use crate::api::botguests;
use crate::hub::Hub;
use crate::remote::crypto;
use crate::store::{BotState, Device, DeviceSource, Scope, Store, now_ms};
use kith_wire::cloud::path;
use serde_json::json;
use std::sync::Mutex;

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
        // An invite this host made but closed (revoked, used, expired, or its bot ended) can still be accepted in the
        // cloud first: `retry_revokes` drops the invite on a 409, so the device it names is ours to revoke. An invite
        // this host never made may be another host's, and stays alone.
        let closed = hub
            .store
            .bot_invite(&l.invite_id)
            .is_some_and(|i| !i.open(now) || !hub.store.bot_guest(&i.bot_id).is_some_and(|g| g.state.live()));
        if closed {
            // Queued once (it dedupes) and awaited with no lock held; a repeat pull before the cloud confirms is harmless.
            revoke_device_in_cloud(hub, &l.bot_device_id).await;
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
            queue(&hub.store, PENDING_REVOKES, cloud);
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

/// Changes a queue in one step: it is read again here, so an id queued while a retry was awaiting the cloud is kept,
/// and no `.await` sits between the read and the write (same shape as cloud.rs `update_pending_revokes`).
fn update_list(store: &Store, key: &str, change: impl FnOnce(&mut Vec<String>)) {
    static WRITING: Mutex<()> = Mutex::new(());
    let _one_at_a_time = WRITING.locked();
    let mut ids = list(store, key);
    change(&mut ids);
    let saved = serde_json::to_string(&ids).map_err(anyhow::Error::from).and_then(|v| store.kv_set(key, &v));
    if let Err(e) = saved {
        tracing::warn!(error = format!("{e:#}"), "couldn't remember a bot revoke for the cloud");
    }
}

fn queue(store: &Store, key: &str, id: &str) {
    update_list(store, key, |ids| {
        if !ids.iter().any(|i| i == id) {
            ids.push(id.to_owned());
        }
    });
}

/// Tells the cloud a bot device ended here; a failure is retried on later pulls (local access is already gone).
pub async fn revoke_device_in_cloud(hub: &Hub, cloud_device: &str) {
    queue(&hub.store, PENDING_REVOKES, cloud_device);
    retry_revokes(hub).await;
}

/// Cancels a bot invite in the cloud that was never used.
pub async fn revoke_invite_in_cloud(hub: &Hub, invite_id: &str) {
    queue(&hub.store, PENDING_INVITE_REVOKES, invite_id);
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
        let mut confirmed = vec![];
        for id in ids {
            match super::cloud::post(hub, &path(&id), &json!({})).await {
                Ok(_) => confirmed.push(id),
                // Gone there already, or (an invite) accepted meanwhile: the device is what to revoke then.
                Err(e) if super::cloud::refused_with(&e, 404) || super::cloud::refused_with(&e, 409) => {
                    confirmed.push(id);
                }
                Err(e) => {
                    tracing::warn!(error = format!("{e:#}"), id, "couldn't revoke a bot in the cloud");
                }
            }
        }
        // Only what the cloud confirmed leaves the queue: an id queued during the awaits above stays.
        update_list(&hub.store, key, |ids| ids.retain(|i| !confirmed.contains(i)));
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::remote::identity::Identity;
    use axum::response::IntoResponse;
    use std::sync::Arc;
    use tokio::sync::Notify;

    #[derive(Default)]
    struct Gate {
        entered: Notify,
        release: Notify,
        first: std::sync::atomic::AtomicBool,
    }

    /// A cloud that holds its first revoke call (device or invite) until released and refuses every other.
    async fn gated_cloud(gate: Arc<Gate>) -> String {
        use axum::extract::State as Ax;
        use axum::http::StatusCode;
        use std::sync::atomic::Ordering;
        let hold = |Ax(g): Ax<Arc<Gate>>| async move {
            if g.first.swap(true, Ordering::SeqCst) {
                return (StatusCode::INTERNAL_SERVER_ERROR, "no").into_response();
            }
            g.entered.notify_one();
            g.release.notified().await;
            axum::Json(json!({})).into_response()
        };
        let app = axum::Router::new()
            .route("/v1/host/bot-devices/{id}/revoke", axum::routing::post(hold))
            .route("/v1/host/bot-invites/{id}/revoke", axum::routing::post(hold))
            .with_state(gate);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        base
    }

    async fn queued_during_retry(key: &str) {
        let dir = std::env::temp_dir().join(format!("kith-botsync-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir.join("t.db")).unwrap();
        let hub = Hub::new(store, "host".into(), Identity::load_or_create(&dir).unwrap(), "token".into(), 0);
        let gate = Arc::new(Gate::default());
        hub.store.kv_set("test_cloud_url", &gated_cloud(gate.clone()).await).unwrap();
        queue(&hub.store, key, "a");

        let retry = tokio::spawn({
            let hub = hub.clone();
            async move { retry_revokes(&hub).await }
        });
        gate.entered.notified().await;
        queue(&hub.store, key, "b");
        gate.release.notify_one();
        retry.await.unwrap();

        assert_eq!(list(&hub.store, key), ["b"], "the confirmed id is gone, the one queued meanwhile is not");
    }

    #[tokio::test]
    async fn a_bot_device_queued_during_a_retry_is_kept() {
        queued_during_retry(PENDING_REVOKES).await;
    }

    #[tokio::test]
    async fn a_bot_invite_queued_during_a_retry_is_kept() {
        queued_during_retry(PENDING_INVITE_REVOKES).await;
    }

    /// A cloud that records every device revoke it is asked for.
    async fn recording_cloud(calls: Arc<Mutex<Vec<String>>>) -> String {
        use axum::extract::{Path, State as Ax};
        let app = axum::Router::new()
            .route(
                "/v1/host/bot-devices/{id}/revoke",
                axum::routing::post(|Ax(c): Ax<Arc<Mutex<Vec<String>>>>, Path(id): Path<String>| async move {
                    c.lock().unwrap().push(id);
                    axum::Json(json!({}))
                }),
            )
            .with_state(calls);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        base
    }

    fn hub_with_cloud(base: &str) -> Arc<Hub> {
        let dir = std::env::temp_dir().join(format!("kith-botsync-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir.join("t.db")).unwrap();
        let hub = Hub::new(store, "host".into(), Identity::load_or_create(&dir).unwrap(), "token".into(), 0);
        hub.store.kv_set("test_cloud_url", base).unwrap();
        hub
    }

    /// A live bot with one invite; `revoked` closes the invite as the owner would.
    fn bot_with_invite(hub: &Hub, revoked: bool) {
        let g = crate::store::BotGuest {
            bot_id: "b1".into(),
            person_id: "bot-00000001".into(),
            owner_person: None,
            name: "bot".into(),
            provider: "x".into(),
            agents: vec![],
            state: BotState::Invited,
            device_key: None,
            cloud_device: None,
            rate_per_hour: 60,
            concurrent: 1,
            turn_timeout_s: 60,
            wake_profile: None,
            created_at: 0,
            joined_at: None,
            decided_at: None,
            decided_by: None,
            last_seen_at: None,
            revoked_at: None,
            revoked_why: None,
        };
        hub.store.put_bot_guest(&g).unwrap();
        let i = crate::store::BotInvite {
            invite_id: "inv1".into(),
            bot_id: "b1".into(),
            token_hash: "h".into(),
            created_at: 0,
            expires_at: i64::MAX,
            used_at: None,
            revoked_at: None,
        };
        hub.store.put_bot_invite(&i).unwrap();
        if revoked {
            hub.store.revoke_bot_invite("inv1").unwrap();
        }
    }

    fn device(invite: &str) -> Listed {
        Listed {
            bot_device_id: "bd_1".into(),
            invite_id: invite.into(),
            device_key: crypto::b64(&[7u8; 32]),
            name: "bot".into(),
            provider: None,
            created_at: 0,
        }
    }

    #[tokio::test]
    async fn a_device_on_an_invite_closed_here_is_revoked_in_the_cloud() {
        let calls = Arc::default();
        let hub = hub_with_cloud(&recording_cloud(Arc::clone(&calls)).await);
        bot_with_invite(&hub, true);
        apply(&hub, &[device("inv1")]).await.unwrap();
        assert_eq!(*calls.lock().unwrap(), ["bd_1"]);
        assert_eq!(list(&hub.store, PENDING_REVOKES), Vec::<String>::new());
        assert!(hub.store.bot_guest("b1").unwrap().state.live(), "no local bot is ended");
    }

    #[tokio::test]
    async fn a_device_on_an_unknown_invite_is_left_alone() {
        let calls = Arc::default();
        let hub = hub_with_cloud(&recording_cloud(Arc::clone(&calls)).await);
        bot_with_invite(&hub, true);
        apply(&hub, &[device("someone-elses")]).await.unwrap();
        assert!(calls.lock().unwrap().is_empty());
        assert_eq!(list(&hub.store, PENDING_REVOKES), Vec::<String>::new());
    }

    #[tokio::test]
    async fn a_device_on_an_open_invite_is_admitted_not_revoked() {
        let calls = Arc::default();
        let hub = hub_with_cloud(&recording_cloud(Arc::clone(&calls)).await);
        bot_with_invite(&hub, false);
        apply(&hub, &[device("inv1")]).await.unwrap();
        assert!(calls.lock().unwrap().is_empty());
        assert_eq!(hub.store.bot_guest("b1").unwrap().cloud_device.as_deref(), Some("bd_1"));
    }
}
