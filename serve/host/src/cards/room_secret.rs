//! Room secrets, the host's side (designs/2026-10-31-room-secrets): the owner hands ONE member of a group room (a Kith
//! agent, or their own bot guest such as Grok) a secret, once. The value goes into kith-auth's Keychain item
//! `kith/once/<id>` and nowhere else: not this database, not an entry, not an event, not a log. The host keeps who, when
//! and the state (`room_secrets`), and the room shows a quiet line ("Kai sent Grok a secret").
//!
//! Order of a send, which the TLA+ model of the design checks (an earlier order loses a send or delivers across a
//! removal): `once put` first; then, in ONE step under the lock that saves room edits, the membership check, the row and
//! the quiet line (`Hub::commit_room_secret`); a recipient who is not a member then costs only a `once drop`.
//!
//! A bot fetches its secret with `botSecretFetch` (api/botguests.rs `secret_fetch`: claim, take, re-check, finish). A
//! Kith agent uses it with `kith-auth exec --secret <id>`, told of it in its next room prompt (`prompt_lines`). Every end
//! of either party drops the secret (`dropped`), and the sweep expires what is left.

use super::secret::kith_auth;
use crate::api::UserError;
use crate::hub::Hub;
use crate::store::{BotKind, BotState, RoomSecret, SecretTo, Store, now_ms};
use anyhow::Result;
use serde_json::{Value, json};
use std::sync::Arc;
use zeroize::Zeroizing;

/// The default life of a secret, and what the owner may pick (seconds).
pub const DEFAULT_TTL_S: i64 = 10 * 60;
pub const TTL_RANGE_S: (i64, i64) = (60, 60 * 60);
/// A once item with no row is only an orphan after this long: a send puts the item first and writes the row a moment
/// later (the architect's fix 2).
const ORPHAN_GRACE_MS: i64 = 2 * 60 * 1000;
/// What kith-auth takes (`MAX_VALUE`).
const MAX_VALUE: usize = 1536;
const MAX_LABEL: usize = 40;
/// Everything that goes wrong in a send says only this: the cause may name the value's neighbours (a member, a path).
const NOT_SENT: &str = "Couldn't send the secret";

/// A member a secret can be handed to, as the room stands right now.
pub struct Recipient {
    pub kind: SecretTo,
    /// The hq agent behind a Kith agent member.
    pub agent: Option<String>,
    pub name: String,
}

/// Whether `to` is a member of `room` that may be handed a secret: a Kith agent of the room, or the owner's own bot
/// guest that is let in and has its join point. `owner_person.is_none()` is named here, not left to `is_room_member`, so
/// that letting other people's bots into rooms one day does not widen who can be handed a secret (the architect's
/// fix 5).
pub fn recipient(store: &Store, room: &str, to: &str) -> Option<Recipient> {
    let row = store.bot(room).ok().flatten().filter(|r| !r.deleted && r.config.is_group())?;
    if !row.config.members.iter().any(|m| m == to) {
        return None;
    }
    if let Some(guest) = store.bot_guest(to) {
        let ok = guest.owner_person.is_none()
            && guest.state == BotState::Active
            && crate::chat::group::is_room_member(store, room, to)
            && store.room_bot_point(room, to).is_some();
        return ok.then(|| Recipient { kind: SecretTo::Bot, agent: None, name: guest.name });
    }
    let cfg = store.bot(to).ok().flatten().filter(|r| !r.deleted && r.config.kind == BotKind::Agent)?.config;
    let agent = cfg.hq_agent.clone()?;
    Some(Recipient { kind: SecretTo::Agent, agent: Some(agent), name: cfg.name })
}

fn clean_label(s: Option<&str>) -> Option<String> {
    let l: String = s?.chars().filter(|c| !c.is_control()).take(MAX_LABEL).collect();
    let l = l.trim().to_owned();
    (!l.is_empty()).then_some(l)
}

/// `sendSecret {room, to, value, label?, ttlS?}` -> `{id, expiresAt}`. `value` is already in a `Zeroizing`; it is handed
/// to kith-auth on its stdin and the buffer is zeroed when it goes out of scope.
pub async fn send(
    hub: &Arc<Hub>,
    room: &str,
    to: &str,
    mut value: Zeroizing<String>,
    label: Option<&str>,
    ttl_s: Option<i64>,
) -> Result<Value> {
    let refuse = |why: &str| UserError(why.to_owned());
    if value.is_empty() || value.len() > MAX_VALUE || value.contains('\0') {
        return Err(refuse("A secret is text, up to 1,500 characters.").into());
    }
    let ttl = ttl_s.unwrap_or(DEFAULT_TTL_S);
    if !(TTL_RANGE_S.0..=TTL_RANGE_S.1).contains(&ttl) {
        return Err(refuse("A secret can last from 1 to 60 minutes.").into());
    }
    // A quick look first, so a plain mistake costs no Keychain write. The real check is the one under the lock.
    let Some(who) = recipient(&hub.store, room, to) else { return Err(refuse(NOT_SENT).into()) };
    let id = format!("rs_{}", &uuid::Uuid::new_v4().simple().to_string()[..16]);
    let now = now_ms();
    let expires_at = now + ttl * 1000;
    let (flag, holder) = match who.kind {
        SecretTo::Agent => ("--agent", who.agent.clone().unwrap_or_default()),
        SecretTo::Bot => ("--bot", to.to_owned()),
    };
    // The owner's name for the line, asked before the lock: it runs a process.
    let owner = owner_name(hub, room).await;
    // Moved, not copied: the String's buffer becomes kith-auth's input and is zeroed with it.
    let body = Zeroizing::new(std::mem::take(&mut *value).into_bytes());
    let put = kith_auth(&["once", "put", &id, flag, &holder, "--expires", &expires_at.to_string()], Some(body)).await;
    if !put.is_some_and(|r| r.ok) {
        return Err(refuse(NOT_SENT).into());
    }
    let row = RoomSecret {
        id: id.clone(),
        room: room.to_owned(),
        sender_kind: "owner".into(),
        sender: String::new(),
        recipient_kind: who.kind.as_str().into(),
        recipient: to.to_owned(),
        agent: who.agent.clone(),
        label: clean_label(label),
        notice_seq: None,
        created_at: now,
        expires_at,
        state: "pending".into(),
        done_at: None,
    };
    let notice = json!({
        "notice": "secretSent",
        "from": {"kind": "owner", "name": owner},
        "to": {"kind": who.kind.as_str(), "id": to, "name": who.name},
        "secret": id, "style": "info",
        "text": format!("{owner} sent {} a secret", who.name),
    });
    let committed =
        hub.commit_room_secret(&row, |s| recipient(s, room, to).is_some_and(|r| r.kind == who.kind), &notice);
    if let Err(error) = committed {
        // Neither a row nor a line: the item has no purpose.
        tracing::info!(error = format!("{error:#}"), "a room secret was not sent");
        drop_items(&[id]).await;
        return Err(refuse(NOT_SENT).into());
    }
    // Names and times only. Only the owner's devices hear it, and the recipient bot (`roles::event_for_bot`).
    let _ = hub.events.send(json!({
        "type": "roomSecret", "room": room, "id": id, "to": to, "from": owner, "label": row.label, "expiresAt": expires_at,
    }));
    Ok(json!({"id": id, "expiresAt": expires_at}))
}

/// The owner's name for the line: kith-auth's, asked through an agent of the room; "The owner" when none answers.
async fn owner_name(hub: &Hub, room: &str) -> String {
    let members = hub.store.bot(room).ok().flatten().map(|r| r.config.members).unwrap_or_default();
    for id in members {
        if let Some(agent) = hub.store.bot(&id).ok().flatten().and_then(|r| r.config.hq_agent)
            && let Some(name) = crate::api::roles::owner_name(&agent).await
        {
            return name;
        }
    }
    "The owner".into()
}

/// `withdrawSecret {id}`: the owner takes it back. `{withdrawn}` is false when it was already used, over or gone.
pub async fn withdraw(hub: &Hub, id: &str) -> Value {
    let ended = hub.store.drop_room_secret(id);
    if ended {
        drop_items(&[id.to_owned()]).await;
    }
    json!({"withdrawn": ended})
}

/// `roomSecrets {room}`: this room's secrets and where they stand. Never a value.
pub fn list(hub: &Hub, room: &str) -> Value {
    json!({"secrets": hub.store.room_secrets_of(room, 50)})
}

// ---- kith-auth's once items

/// Removes the Keychain items and their entries. Failures are left to the sweep, which drops the item of any row that
/// has ended.
pub async fn drop_items(ids: &[String]) {
    for id in ids {
        let _ = kith_auth(&["once", "drop", id], None).await;
    }
}

/// [`drop_items`] from code that isn't async (a room edit). With no runtime (a test of the store) the sweep does it.
pub fn drop_items_soon(ids: Vec<String>) {
    if ids.is_empty() {
        return;
    }
    if let Ok(rt) = tokio::runtime::Handle::try_current() {
        rt.spawn(async move { drop_items(&ids).await });
    }
}

/// The room edit or end that ends secrets: `member` left `room` (as recipient or sender), a room is gone, a bot ended.
pub fn end(hub: &Hub, room: Option<&str>, member: Option<&str>) {
    drop_items_soon(hub.store.drop_room_secrets(room, member));
}

/// `once take <id> --bot <bot>`: the value, which kith-auth deletes before it prints. None when it isn't there.
pub async fn take(id: &str, bot: &str) -> Option<Zeroizing<String>> {
    let ran = kith_auth(&["once", "take", id, "--bot", bot], None).await?;
    let out = Zeroizing::new(ran.stdout);
    if !ran.ok {
        return None;
    }
    std::str::from_utf8(&out).ok().filter(|v| !v.is_empty()).map(|v| Zeroizing::new(v.to_owned()))
}

/// What kith-auth holds: `once list --json`. None when it can't be asked.
pub async fn held() -> Option<Vec<Value>> {
    let ran = kith_auth(&["once", "list", "--json"], None).await?;
    ran.ok.then_some(())?;
    serde_json::from_slice::<Vec<Value>>(&ran.stdout).ok()
}

/// The minute's work (with `cards::sweep`): what has lapsed ends and its item goes; a Kith agent's secret kith-auth no
/// longer holds was used, so it is `taken`; an item with no live row is dropped, but only past a grace (a send puts the
/// item a moment before it writes the row).
pub async fn sweep(hub: &Hub) {
    let now = now_ms();
    drop_items(&hub.store.expire_room_secrets(now)).await;
    // Rows first, then the list: a row written after the list was read would look used.
    let agent_rows = hub.store.pending_agent_secrets();
    let Some(items) = held().await else { return };
    let ids: Vec<&str> = items.iter().filter_map(|i| i["id"].as_str()).collect();
    for row in agent_rows.iter().filter(|r| !ids.contains(&r.id.as_str())) {
        hub.store.mark_room_secret_taken(&row.id);
    }
    let mut orphans = vec![];
    for item in &items {
        let Some(id) = item["id"].as_str() else { continue };
        let orphan = match hub.store.room_secret(id) {
            None => item["created"].as_i64().unwrap_or(0) < now - ORPHAN_GRACE_MS,
            Some(row) => !matches!(row.state.as_str(), "pending" | "taking"),
        };
        if orphan {
            orphans.push(id.to_owned());
        }
    }
    drop_items(&orphans).await;
}

// ---- telling the recipient

/// The lines a Kith agent's room prompt gets about secrets waiting for it: the id, the label and how to use it, never
/// the value.
pub fn prompt_lines(hub: &Hub, room: &str, me: &str) -> Vec<String> {
    hub.store
        .pending_room_secrets(room, me)
        .into_iter()
        .filter(|s| s.recipient_kind == "agent")
        .map(|s| {
            let what = s.label.as_deref().map_or(String::new(), |l| format!(" \"{l}\""));
            let until = chrono::DateTime::from_timestamp_millis(s.expires_at)
                .map_or_else(String::new, |t| t.with_timezone(&chrono::Local).format("%H:%M").to_string());
            format!(
                "The owner sent you a secret{what} ({id}, expires {until}). Use it once with: kith-auth exec --secret {id} \
                 -- <command>; it is in $KITH_SECRET for that command only. Never print it.",
                id = s.id
            )
        })
        .collect()
}

/// A bot's pending secrets in `room`, as `botRoomRead` lists them (no value).
pub fn pending_for_bot(hub: &Hub, room: &str, bot: &str) -> Vec<RoomSecret> {
    let floor = hub.store.room_bot_floor(room, bot);
    hub.store
        .pending_room_secrets(room, bot)
        .into_iter()
        .filter(|s| s.recipient_kind == "bot" && s.notice_seq.is_some_and(|n| n > floor))
        .collect()
}

/// One of them as the bot is told of it (`roomSecret` event and `botRoomRead.secrets`): no value, no state.
pub fn bot_view(s: &RoomSecret, from: &str) -> Value {
    json!({"id": s.id, "from": from, "label": s.label, "expiresAt": s.expires_at})
}
