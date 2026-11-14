//! What a person in a group room sees and does there (rooms with people, design 2026-11-12, rules 1, 2 and 4).
//!
//! One rule decides what they are shown, in every path (history, thread, search, live events, catch-up):
//! **they see an entry only if** their stay is open, they pass the live check ([`room_people::live_seat`] and
//! [`seat_now`] ask the authority now), and the entry's `origin_gen` is at least their stay's `joined_gen`
//! ([`sees`]). An entry never stamped (`NULL`) is hidden from every person: only a writer that names a gen makes an
//! entry visible to one, so a writer that forgot is a missing line, not a leak. The test is made at the moment of
//! the read, never remembered, so a leave, a removal or a revoke cuts the next read at once and a re-add shows only
//! what came after it.
//!
//! A person is shown messages only (the user's and the agents'), with a short list of fields, never a tool call, a
//! thought, a card or a notice. The owner sees everything, and each message a current person can't see carries
//! `hiddenFrom` on the copy that is sent (never stored).
//!
//! Posting is one step with the check (`Hub::commit_room_post`). A post starts no turn: the room-person turns are the
//! next slice, and the setting that lets a person into a room stays off until they exist.

use super::room_people::{self, Seat, live_seat};
use crate::api::authority::Authority;
use crate::api::devices::NotAllowed;
use crate::hub::Hub;
use crate::store::{Entry, Store};
use anyhow::{Result, bail};
use serde_json::{Map, Value, json};
use std::sync::Arc;

/// The kinds of entry a person is ever shown.
pub const SEEN_KINDS: [&str; 2] = ["user", "agent"];

/// The fields of a message's `data` a person is shown. Everything else (attachments, thread summaries, handoff and
/// card state, who the owner marked it hidden from) stays on the computer.
const SEEN_DATA: [&str; 7] = ["text", "author", "final", "reactions", "sender", "clientNonce", "status"];

/// The longest message a person may post, in bytes.
const MAX_POST: usize = 16 * 1024;

fn not_in_room() -> anyhow::Error {
    NotAllowed("You aren't in that room.".to_owned()).into()
}

/// Whether `id` is a live group room.
pub fn is_room(store: &Store, id: &str) -> bool {
    store.bot(id).ok().flatten().is_some_and(|r| !r.deleted && r.config.is_group())
}

/// Whether a person whose stay opened at `seat` is shown an entry stamped `origin`. `NULL` is never shown.
pub fn sees(seat: i64, origin: Option<i64>) -> bool {
    origin.is_some_and(|g| seat <= g)
}

/// [`live_seat`] for the places that can't wait on the authority (a live event): `access` is the authority the stream
/// chose, so it answers the same question as [`crate::api::roles::may_use`]. The open stay's `joined_gen`, or `None`.
pub fn seat_now(access: &dyn Authority, store: &Store, person: &str, room: &str) -> Option<i64> {
    // A revoke leaves the host's people mirror as it stands (its rows wait for the undo), so a person set aside is a no
    // here whatever the mirror says; on the core, `granted` already says no.
    if store.is_suspended(person) {
        return None;
    }
    let seat = store.room_open_seat(room, person)?;
    let agents = room_people::room_agents(store, room).ok()?;
    agents.iter().all(|agent| access.granted(person, agent)).then_some(seat)
}

/// An entry's `data` cut to what a person is shown.
fn shown_data(data: &Value, viewer: &str) -> Value {
    let mut out = Map::new();
    for key in SEEN_DATA {
        // A message id the sender chose is the sender's alone to see.
        if key == "clientNonce" && data["sender"]["person"] != viewer {
            continue;
        }
        if let Some(v) = data.get(key) {
            out.insert(key.to_owned(), v.clone());
        }
    }
    Value::Object(out)
}

/// The entry `e` (as JSON) as a person is shown it, or `None` when they are not shown it at all: not a message, or
/// not from a gen they were in the room for.
pub fn shown_entry(store: &Store, viewer: &str, seat: i64, e: &Value) -> Option<Value> {
    if !e["kind"].as_str().is_some_and(|k| SEEN_KINDS.contains(&k)) || !sees(seat, store.origin_gen(e["id"].as_str()?))
    {
        return None;
    }
    let mut out = e.clone();
    out["data"] = shown_data(&e["data"], viewer);
    Some(out)
}

fn shown_entries(viewer: &str, entries: &[Entry]) -> Vec<Value> {
    entries
        .iter()
        .map(|e| {
            let mut v = serde_json::to_value(e).unwrap_or(Value::Null);
            v["data"] = shown_data(&e.data, viewer);
            v
        })
        .collect()
}

/// The people in `room` with an open stay who are not shown the message `id` of `kind` (older than their stay, or never
/// stamped), for the owner's copy of it. Empty for anything a person is never shown anyway (tool lines, cards):
/// marking those would only be noise.
pub fn hidden_from(store: &Store, room: &str, kind: &str, origin: Option<i64>) -> Vec<String> {
    if !SEEN_KINDS.contains(&kind) {
        return vec![];
    }
    store
        .room_open_people(room)
        .into_iter()
        .filter(|p| store.room_open_seat(room, p).is_some_and(|seat| !sees(seat, origin)))
        .collect()
}

/// The owner's copy of a page of a room's entries: each message some current person is not shown says so.
pub fn mark_hidden(store: &Store, room: &str, entries: &mut [Entry]) {
    if store.room_open_people(room).is_empty() {
        return;
    }
    let ids: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();
    let gens = store.origin_gens(&ids);
    for e in entries {
        let hidden = hidden_from(store, room, &e.kind, gens.get(&e.id).copied());
        if !hidden.is_empty() {
            e.data["hiddenFrom"] = json!(hidden);
        }
    }
}

/// The owner's copy of one entry event (an entry as JSON), marked the same way.
pub fn mark_hidden_event(store: &Store, room: &str, e: &mut Value) {
    let (Some(id), Some(kind)) = (e["id"].as_str(), e["kind"].as_str()) else { return };
    let hidden = hidden_from(store, room, kind, store.origin_gen(id));
    if !hidden.is_empty() {
        e["data"]["hiddenFrom"] = json!(hidden);
    }
}

/// The seat for a read or a write of `person` in `room`, asked of the authority now.
async fn seat_of(hub: &Hub, person: &str, room: &str) -> Result<Seat> {
    live_seat(&hub.store, person, room).await.ok_or_else(not_in_room)
}

/// A page of the room's main chat as `person` is shown it, newest page first by `beforeSeq`.
pub async fn history(hub: &Hub, person: &str, b: &Value) -> Result<Value> {
    let room = b["botId"].as_str().unwrap_or_default();
    let seat = seat_of(hub, person, room).await?;
    let before = b["beforeSeq"].as_i64().unwrap_or(i64::MAX);
    let limit = b["limit"].as_i64().unwrap_or(100).clamp(1, 500);
    let entries = hub.store.history_seen(room, before, limit, seat.joined)?;
    Ok(json!({"entries": shown_entries(person, &entries)}))
}

/// A thread of the room as `person` is shown it.
pub async fn thread(hub: &Hub, person: &str, b: &Value) -> Result<Value> {
    let room = b["botId"].as_str().unwrap_or_default();
    let seat = seat_of(hub, person, room).await?;
    let entries = hub.store.thread_seen(room, b["rootId"].as_str().unwrap_or_default(), 500, seat.joined)?;
    Ok(json!({"entries": shown_entries(person, &entries)}))
}

/// `person` says something in the room: checked live, then written in one step with that check ([`Hub::commit_room_post`]).
/// Text only: a person's attachments, threads, tasks, folders and calls are not part of this slice. The message starts no turn.
pub async fn send(hub: &Arc<Hub>, person: &str, b: &Value) -> Result<Value> {
    let room = b["botId"].as_str().unwrap_or_default();
    let text = b["text"].as_str().unwrap_or_default().trim();
    if text.is_empty() {
        bail!("empty message");
    }
    if text.len() > MAX_POST {
        bail!("That message is too long.");
    }
    let nonce = b["clientNonce"].as_str().unwrap_or_default();
    if nonce.len() > 64 {
        bail!("That message id is too long.");
    }
    let present = |k: &str| match &b[k] {
        Value::Null | Value::Bool(false) => false,
        Value::String(s) => !s.trim().is_empty(),
        Value::Array(a) => !a.is_empty(),
        _ => true,
    };
    if ["attachments", "pasted", "threadId", "newTask", "project", "folder", "clear", "via"].iter().any(|k| present(k))
    {
        bail!("In a room you can send text only for now.");
    }
    let seat = seat_of(hub, person, room).await?;
    // The stay this message was written in (what `myRooms` said): a post a device queued in an earlier stay never lands
    // in a later one (R4), however the stays came about.
    if b["stay"].as_i64() != Some(seat.joined) {
        return Err(NotAllowed("You're not in that room as you were: open it again.".to_owned()).into());
    }
    let name = hub.store.person(person).map_or_else(|| person.to_owned(), |p| p.name);
    let data = json!({
        "text": text, "clientNonce": nonce, "status": "sent", "sender": {"person": person, "name": name},
    });
    let entry = hub.commit_room_post(room, person, &seat, nonce, &data)?;
    Ok(json!({"entry": shown_entries(person, &[entry]).remove(0)}))
}

/// `person` reacts to a message they are shown. The check and the reaction are one step under the membership lock,
/// so a removal can't fall between them.
pub async fn react(hub: &Hub, person: &str, b: &Value) -> Result<Value> {
    let id = b["entryId"].as_str().unwrap_or_default();
    let entry = hub.store.entry(id).ok_or_else(not_in_room)?;
    let seat = seat_of(hub, person, &entry.bot_id).await?;
    let _members = super::group::membership_lock();
    let shown = hub.store.room_open_seat(&entry.bot_id, person) == Some(seat.joined)
        && !hub.store.is_suspended(person)
        && SEEN_KINDS.contains(&entry.kind.as_str())
        && sees(seat.joined, hub.store.origin_gen(id));
    if !shown {
        return Err(not_in_room());
    }
    let e = hub.react(id, b["emoji"].as_str().unwrap_or_default())?;
    Ok(json!({"entry": shown_entries(person, &[e]).remove(0)}))
}

/// The rooms `person` is in right now (an open stay and every agent still granted), for `myRooms`. Agents come as names
/// and as `agentList` `{id, name}`; `ownerName` is the owner's display name, left out when kith-auth has none.
pub async fn my_rooms(hub: &Hub, person: &str) -> Vec<Value> {
    let mut out = vec![];
    let mut owner: Option<Option<String>> = None;
    for row in hub.store.bots().unwrap_or_default() {
        if row.deleted || !row.config.is_group() || live_seat(&hub.store, person, &row.config.id).await.is_none() {
            continue;
        }
        let bots: Vec<_> = row.config.members.iter().filter_map(|m| hub.store.bot(m).ok().flatten().filter(|b| !b.deleted)).collect();
        let agents: Vec<&str> = bots.iter().map(|b| b.config.name.as_str()).collect();
        // A reply's author is the bot id: only id and name of each agent leave here, nothing else of the bot.
        let agent_list: Vec<Value> = bots.iter().map(|b| json!({"id": b.config.id, "name": b.config.name})).collect();
        // The owner is one person for every room: asked once, through the first bot that has a Kith agent behind it.
        if owner.is_none() {
            let mut name = None;
            for agent in bots.iter().filter_map(|b| b.config.hq_agent.as_deref()) {
                name = crate::api::roles::owner_name(agent).await;
                if name.is_some() {
                    break;
                }
            }
            owner = Some(name);
        }
        let stay = hub.store.room_open_seat(&row.config.id, person);
        let mut room = json!({"id": row.config.id, "name": row.config.name, "agents": agents, "agentList": agent_list, "stay": stay});
        if let Some(Some(name)) = &owner {
            room["ownerName"] = json!(name);
        }
        out.push(room);
    }
    out
}

/// `person` leaves `room` (only an open stay is needed: leaving never widens anything, and someone whose agent was taken
/// away can still get out). Whether it changed anything.
pub fn leave(hub: &Arc<Hub>, person: &str, room: &str) -> Result<bool> {
    if hub.store.room_open_seat(room, person).is_none() {
        return Err(not_in_room());
    }
    Ok(room_people::remove_person(hub, room, person)?.changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_is_shown_from_the_stay_on_and_never_when_unstamped() {
        assert!(sees(5, Some(5)), "a post at the join gen itself");
        assert!(sees(5, Some(9)));
        assert!(!sees(5, Some(4)), "from before the join");
        assert!(!sees(5, None), "never stamped");
    }

    #[test]
    fn only_the_listed_fields_of_a_message_are_shown() {
        let data = json!({"text": "hi", "attachments": [{"path": "/x"}], "thread": {"summary": "s"}, "hiddenFrom": ["a"],
            "author": "bot1", "final": true, "status": "sent"});
        assert_eq!(shown_data(&data, "sam"), json!({"text": "hi", "author": "bot1", "final": true, "status": "sent"}));
    }
}
