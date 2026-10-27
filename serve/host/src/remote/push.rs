//! Push via the Kith relay (it holds the APNs key; the host only holds opaque
//! per-device tickets the relay issued to the phone).
//!
//! Alerts cover completion, input requests, and failures;
//! none are sent while the phone app is connected (it's in the foreground).

use crate::LockExt;
use crate::hub::{BotStatus, Hub, Runtime};
use crate::remote::identity::Identity;
use crate::remote::{cloud, crypto};
use crate::store::BotConfig;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};
use x25519_dalek::StaticSecret;

/// Alert kinds (wire values double as the APNs category).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AlertKind {
    Done,
    NeedsInput,
    Failed,
}

/// (bot id, kind) → last alert.
static LAST_SENT: LazyLock<Mutex<HashMap<(String, AlertKind), Instant>>> = LazyLock::new(Mutex::default);
/// Tickets the relay reported as dead (app uninstalled / token rotated); purged on the next push.
static GONE: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Seconds between the Unix epoch and Swift's `Date` reference date (2001-01-01).
const SWIFT_REFERENCE_EPOCH: f64 = 978_307_200.0;

fn relay_url(hub: &Hub) -> Option<String> {
    std::env::var("KITH_SERVE_RELAY_URL").ok().or_else(|| hub.store.kv_get("relay_url")).filter(|u| !u.is_empty())
}

/// True when `key` was sent less than `min_gap` ago; otherwise records now.
fn throttled(key: (String, AlertKind), min_gap: Duration) -> bool {
    let mut last = LAST_SENT.locked();
    if last.get(&key).is_some_and(|t| t.elapsed() < min_gap) {
        return true;
    }
    last.insert(key, Instant::now());
    false
}

/// The body bytes and their `Kith-Sig`: the relay only lets this computer use tickets registered for it.
/// A relay from before signed pushes ignores the header.
fn signed(identity: &Identity, url: &str, body: &Value) -> (Vec<u8>, Option<String>) {
    let bytes = body.to_string().into_bytes();
    let sig = reqwest::Url::parse(url).ok().zip(cloud::authority(url).ok()).map(|(u, authority)| {
        let path = match u.query() {
            Some(q) => format!("{}?{q}", u.path()),
            None => u.path().to_owned(),
        };
        identity.sign_request("POST", &authority, &path, &bytes)
    });
    (bytes, sig)
}

fn post(hub: &Hub, url: String, body: Value) {
    let (bytes, sig) = signed(&hub.identity, &url, &body);
    tokio::spawn(async move {
        let mut req = crate::http().post(&url).header("content-type", "application/json").body(bytes);
        if let Some(sig) = sig {
            req = req.header(crypto::Labels::Kith.sig_header(), sig);
        }
        match req.timeout(Duration::from_secs(10)).send().await {
            Ok(r) if r.status().is_success() => {
                if let Some(notifications) = body["notifications"].as_array() {
                    match r.json::<Value>().await {
                        Ok(reply) => GONE.locked().extend(gone_in_batch(notifications, &reply)),
                        Err(error) => tracing::warn!(%error, "invalid relay batch response"),
                    }
                }
            }
            Ok(r) if r.status() == reqwest::StatusCode::GONE => {
                if let Some(t) = body["ticket"].as_str() {
                    GONE.locked().push(t.to_owned());
                }
            }
            Ok(r) => tracing::warn!(%url, status = %r.status(), "relay rejected push"),
            Err(error) => tracing::warn!(%url, %error, "relay unreachable"),
        }
    });
}

/// Tickets the relay says to forget: expired, a dead device token (410), or superseded by a newer one.
fn gone_in_batch(notifications: &[Value], reply: &Value) -> Vec<String> {
    let mut gone = Vec::new();
    for result in reply["results"].as_array().into_iter().flatten() {
        let Some(index) = result["index"].as_u64().and_then(|index| usize::try_from(index).ok()) else { continue };
        let Some(ticket) = notifications.get(index).and_then(|item| item["ticket"].as_str()) else { continue };
        if result["status"] == 410 || result["superseded"] == true {
            gone.push(ticket.to_owned());
        } else if result["status"] != 200 {
            tracing::warn!(status = ?result["status"], "relay rejected a batched notification");
        }
    }
    gone
}

fn purge_dead_tickets(hub: &Hub) {
    for t in std::mem::take(&mut *GONE.locked()) {
        if let Err(error) = hub.store.remove_push_ticket(&t) {
            tracing::warn!(%error, "couldn't forget a dead push ticket");
        }
    }
}

/// The chat, its first two members (a group's avatar) and the speaker, just enough for the phone
/// to draw their faces even when its roster copy is older than the bot.
fn faces(hub: &Hub, bot: &BotConfig, from: Option<&str>) -> Vec<Value> {
    let face = |c: &BotConfig| {
        json!({"id": c.id, "kind": c.kind, "members": c.members, "name": c.name,
            "avatarColor": c.avatar_color, "avatarShape": c.avatar_shape,
            "avatarSource": c.avatar_source})
    };
    let mut ids: Vec<&str> = bot.members.iter().take(2).map(String::as_str).collect();
    if let Some(from) = from.filter(|f| *f != bot.id && !ids.contains(f)) {
        ids.push(from);
    }
    std::iter::once(face(bot))
        .chain(ids.into_iter().filter_map(|id| hub.store.bot(id).ok().flatten()).map(|r| face(&r.config)))
        .collect()
}

/// The title and body of every alert asked for, for tests that check what a push would say.
#[cfg(test)]
pub static SENT: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// `bot` is the chat the alert opens; `from` is the member bot speaking when that chat is a group.
pub fn notify(hub: &Hub, bot: &BotConfig, from: Option<&str>, title: &str, body: &str, kind: AlertKind) {
    #[cfg(test)]
    SENT.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(format!("{title}\n{body}"));
    purge_dead_tickets(hub);
    if bot.notify == Some(false) || bot.hidden || hub.phone_connected() {
        return;
    }
    if throttled((bot.id.clone(), kind), Duration::from_secs(5)) {
        return;
    }
    let Some(relay) = relay_url(hub) else { return };
    // The relay and APNs only see a generic line; the real one is sealed to each device (§6.7).
    let generic = match kind {
        AlertKind::NeedsInput => "A bot needs your response. Open Kith to review.",
        AlertKind::Done => "Your task is complete. Open Kith to read the result.",
        AlertKind::Failed => "A task could not finish. Open Kith to review the issue.",
    };
    let subtitle = match kind {
        AlertKind::Done => "Task complete",
        AlertKind::NeedsInput => "Response needed",
        AlertKind::Failed => "Task failed",
    };
    let mut secret = json!({"title": crate::agent::acp::truncate(title, 80), "subtitle": subtitle,
        "body": crate::agent::acp::truncate(body, 400)});
    if let Some(from) = from.filter(|f| *f != bot.id) {
        secret["from"] = from.into();
    }
    secret["faces"] = faces(hub, bot, from).into();
    let secret = secret.to_string();
    let computer_id = hub.identity.computer_id();
    let mut notifications = Vec::new();
    for t in hub.store.push_tickets() {
        // A chat's alerts go only to the devices of the chat's own person; an invitee's never reach the owner's.
        let device_person = hub.store.device(&t.device_key).and_then(|d| d.person_id);
        if device_person != bot.person {
            continue;
        }
        let mut data = json!({"botId": bot.id, "computerId": computer_id, "ctx": t.ctx});
        if let Some(key) = t.push_key.as_deref().and_then(|k| crypto::unb64_n::<32>(k).ok()) {
            let eph = StaticSecret::from(crypto::random::<32>());
            let labels =
                crypto::Labels::from_name(hub.store.kv_get(&format!("push_labels.{}", t.device_key)).as_deref());
            match crypto::seal_push(labels, &hub.identity.cid_raw(), &key, secret.as_bytes(), &eph) {
                Ok(sealed) => data["sealed"] = sealed.into(),
                Err(error) => tracing::warn!(error = format!("{error:#}"), "couldn't seal a notification"),
            }
        }
        notifications.push(json!({
            "ticket": t.ticket,
            "alert": {"title": "Kith", "body": generic},
            "mutableContent": true,
            "threadId": format!("{computer_id}:{}", bot.id),
            "category": kind,
            "data": data,
        }));
    }
    if !notifications.is_empty() {
        post(hub, format!("{relay}/push-batch"), json!({"notifications": notifications}));
    }
}

/// Live Activities carry only status and dates. Task text stays in the encrypted channel.
pub fn live_activity_update(hub: &Hub, bot_id: &str, rt: &Runtime) {
    purge_dead_tickets(hub);
    // Live Activities are registered by the owner's devices only; an invitee's chat never feeds one.
    if hub.store.bot(bot_id).ok().flatten().is_some_and(|r| r.config.person.is_some()) {
        return;
    }
    let Some(relay) = relay_url(hub) else { return };
    let terminal = matches!(rt.status, BotStatus::Idle | BotStatus::Error);
    let tickets = if terminal { hub.store.take_activity_tickets(bot_id) } else { hub.store.activity_tickets(bot_id) };
    let activity = activity_payload(rt);
    for ticket in tickets {
        post(hub, format!("{relay}/push"), json!({"ticket": ticket, "liveActivity": activity}));
    }
}

fn activity_payload(rt: &Runtime) -> Value {
    #[allow(clippy::cast_precision_loss)]
    let started_at = rt.started_at.map(|ms| ms as f64 / 1000.0 - SWIFT_REFERENCE_EPOCH);
    let terminal = matches!(rt.status, BotStatus::Idle | BotStatus::Error);
    let timestamp = crate::store::now_ms() / 1000;
    json!({
        "event": if terminal { "end" } else { "update" },
        "timestamp": timestamp,
        "staleDate": timestamp + 15 * 60,
        "contentState": {"status": rt.status, "activity": "", "startedAt": started_at},
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pushes_are_signed_by_this_computer_for_the_relay_path() {
        let dir = std::env::temp_dir().join(format!("kith-push-sig-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let id = Identity::load_or_create(&dir).unwrap();
        let body = json!({"notifications": [{"ticket": "t"}]});
        let (bytes, sig) = signed(&id, "https://Relay.Example:8443/push-batch", &body);
        assert_eq!(bytes, body.to_string().into_bytes());
        let sig = sig.unwrap();
        let field = |k: &str| sig.split(',').find_map(|p| p.strip_prefix(&format!("{k}="))).unwrap().to_owned();
        assert_eq!(field("kid"), id.sign_pub_b64());
        let input = crypto::request_sig_input(
            crypto::Labels::Kith,
            "POST",
            "relay.example:8443",
            "/push-batch",
            field("ts").parse().unwrap(),
            &field("nonce"),
            &bytes,
        );
        crypto::verify(&id.sign_pub(), input.as_bytes(), &crypto::unb64_n(&field("sig")).unwrap()).unwrap();
        assert!(signed(&id, "not a url", &body).1.is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn expired_dead_and_superseded_tickets_are_forgotten_refused_ones_kept() {
        let notifications: Vec<Value> =
            ["ok", "expired", "superseded", "other-computer", "limited"].iter().map(|t| json!({"ticket": t})).collect();
        let reply = json!({"results": [
            {"index": 0, "status": 200}, {"index": 1, "status": 410},
            {"index": 2, "status": 200, "superseded": true}, {"index": 3, "status": 403}, {"index": 4, "status": 429},
        ]});
        assert_eq!(gone_in_batch(&notifications, &reply), ["expired", "superseded"]);
    }

    #[test]
    fn activity_failure_ends_with_error_and_never_includes_task_text() {
        let payload = activity_payload(&Runtime {
            status: BotStatus::Error,
            activity: "secret file".into(),
            ..Runtime::default()
        });
        assert_eq!(payload["event"], "end");
        assert_eq!(payload["contentState"]["status"], "error");
        assert_eq!(payload["contentState"]["activity"], "");
    }

    #[test]
    fn waiting_for_input_stays_live_with_a_freshness_deadline() {
        let payload = activity_payload(&Runtime {
            status: BotStatus::NeedsInput,
            started_at: Some(978_307_201_000),
            ..Runtime::default()
        });
        assert_eq!(payload["event"], "update");
        assert_eq!(payload["contentState"]["startedAt"], 1.0);
        assert_eq!(payload["staleDate"].as_i64().unwrap() - payload["timestamp"].as_i64().unwrap(), 900);
    }
}
