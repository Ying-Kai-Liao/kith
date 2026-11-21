//! The "waiting for the owner" pointer of a room turn's card (rooms with people, design 2026-11-12, rule 7).
//!
//! A room turn's card opens in the owner's own chat with the agent (`cards::approver_of`), so the people in the room
//! would see their question just sit there. The turn writes a notice into the room beside it: `notice:
//! "roomCardWaiting"`, the card's entry id (`card`), the agent's name and a `state`. It carries the turn's start gen
//! like every line of the turn, so a person sees it only if they were seated when the turn began (rule R1), and it is
//! never part of a room turn's input (`room_post::seen_by` knows only messages).
//!
//! A person is shown the pointer cut to `{notice, author, agent, state}`: no card id, no title, no command, no owner
//! name (the apps build the line with the owner's name from `myRooms`). Two rules decide whether it is shown:
//! a read (history, thread) shows it only while the card row is open (`SEEN_ROWS_SQL`, so a pointer that raced its
//! card's close, or outlived a crash, is never shown); a live event shows it in any state, so the app hears `closed`.
//! The stored `state` only feeds that event, never visibility on a read.
//!
//! Every way a card leaves `pending` writes its entry through `Hub::set_entry`, which calls [`after_set`] here.

use crate::hub::Hub;
use crate::store::{CardState, Entry, EntryKind};
use serde_json::{Value, json};

/// `data.notice` of the pointer.
pub const NOTICE: &str = "roomCardWaiting";

/// The rows of a room's `entries` a person may be shown on a read: messages, and the pointer of a card still open.
pub const SEEN_ROWS_SQL: &str = "(kind IN ('user', 'agent')
    OR (kind = 'notice' AND json_extract(entries.data, '$.notice') = 'roomCardWaiting'
        AND EXISTS (SELECT 1 FROM cards WHERE cards.entry_id = json_extract(entries.data, '$.card')
                    AND cards.state IN ('pending', 'live', 'parked'))))";

/// Whether the entry of `kind` with `data` is a pointer.
pub fn is_pointer(kind: &str, data: &Value) -> bool {
    kind == EntryKind::Notice.as_str() && data["notice"] == NOTICE
}

/// The pointer's `data` as a person is shown it.
pub fn person_copy(data: &Value) -> Value {
    let state = if data["state"] == "closed" { "closed" } else { "waiting" };
    json!({"notice": NOTICE, "author": data["author"], "agent": data["agent"], "state": state})
}

/// The pointer's `data` for the card entry `card`, written by the agent named `name`.
pub fn data_for(card: &str, name: &str) -> Value {
    json!({
        "text": format!("{name} is waiting for the owner to answer a card."), "style": "info",
        "notice": NOTICE, "card": card, "agent": name, "state": "waiting",
    })
}

/// The card entry `card_entry` (raised by a room turn) has left `pending` with `status`: its pointers close. The
/// owner's copy keeps how it ended (`ended`); a person is only told `closed`. Idempotent.
pub fn card_closed(hub: &Hub, card_entry: &str, status: &str) {
    for mut p in hub.store.room_pointers_of(card_entry) {
        if p.data["state"] == "closed" {
            continue;
        }
        p.data["state"] = "closed".into();
        p.data["ended"] = status.into();
        hub.set_entry(&p.id, &p.data);
    }
}

/// The hook of `Hub::set_entry`: `e` was just written; a room turn's card that is no longer `pending` closes its pointers.
pub fn after_set(hub: &Hub, e: &Entry) {
    if e.kind == EntryKind::Permission.as_str() && e.data["room"].is_string() && e.data["status"] != "pending" {
        card_closed(hub, &e.id, e.data["status"].as_str().unwrap_or("cancelled"));
    }
}

/// At start: the restart sweep (`Store::expire_pending`) closes the card entries of a previous run without `set_entry`, so
/// the pointers left `waiting` for a card that is no longer open close here, with the status its entry was given.
pub fn sweep(hub: &Hub) {
    for p in hub.store.waiting_room_pointers() {
        let Some(card) = p.data["card"].as_str() else { continue };
        let open = hub
            .store
            .card_by_entry(card)
            .is_some_and(|c| matches!(c.state, CardState::Pending | CardState::Live | CardState::Parked));
        if !open {
            let status = hub.store.entry(card).and_then(|e| e.data["status"].as_str().map(str::to_owned));
            card_closed(hub, card, status.as_deref().filter(|s| *s != "pending").unwrap_or("expired"));
        }
    }
}
