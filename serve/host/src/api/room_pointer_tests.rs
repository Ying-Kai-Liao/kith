//! The "waiting for the owner" pointer of a room turn's card (rooms with people, package G): a person in the room is
//! shown that an agent waits on the owner, and nothing else of the card; it goes when the card closes by any road, and
//! it reaches nobody outside the room or from before their stay.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::devices::Caller;
use super::dispatch;
use super::rig::Rig;
use super::roles::event_for;
use crate::chat::room_people::FLAG;
use crate::chat::room_pointer::data_for;
use crate::chat::room_turn::room_person_id;
use crate::remote::members::{self, Listed};
use crate::store::{BotConfig, Card, Entry, EntryKind, Lane, NewCard, Opened, now_ms};
use serde_json::{Value, json};

async fn call(r: &Rig, method: &str, b: Value) -> anyhow::Result<Value> {
    dispatch(&r.hub, &Caller::Local, method, b).await
}

fn room_of(r: &Rig) -> String {
    let cfg: BotConfig =
        serde_json::from_value(json!({"id": "", "kind": "group", "name": "Crew", "members": [r.owner_bot("house")]}))
            .unwrap();
    r.hub.create_bot(cfg).unwrap()["id"].as_str().unwrap().to_owned()
}

/// Sam is an invited person with `house`; the room is the owner's, Sam has joined it (the returned gen is his seat).
async fn rig() -> (Rig, String, i64) {
    let r = Rig::new().await;
    r.hub.store.kv_set(FLAG, "on").unwrap();
    let made = call(&r, "createInvite", json!({"name": "Sam", "agents": {"house": "member"}})).await.unwrap();
    let listed = [Listed {
        membership_id: "mem_sam".into(),
        user_id: "user_sam".into(),
        invite_id: made["inviteId"].as_str().unwrap().to_owned(),
        ..Default::default()
    }];
    members::apply(&r.hub, Some(&r.bin), &listed, Some("user_owner")).await.unwrap();
    let room = room_of(&r);
    let added = call(&r, "addRoomPerson", json!({"room": room, "person": "sam"})).await.unwrap();
    let seat = added["gen"].as_i64().unwrap();
    (r, room, seat)
}

/// A room turn's open card as `add_room_card` and `add_room_pointer` leave them: the card in the owner's chat with the
/// agent (data.room set), its row linked, and the pointer in the room stamped `origin`. `deadline` is the card's.
pub(super) fn open(r: &Rig, room: &str, person: &str, origin: i64, deadline: i64) -> (Card, Entry) {
    let data = json!({
        "status": "pending", "actor": "x", "room": room,
        "options": [{"optionId": "allow", "kind": "allow_once", "name": "Allow"}, {"optionId": "deny", "kind": "reject_once", "name": "Deny"}],
    });
    let card_entry = r.hub.add_entry(&Lane::main(&r.owner_bot("house")), EntryKind::Permission, 1, &data).unwrap();
    let new = NewCard {
        session: format!("acp:{person}:"),
        source: "acp",
        bot_id: Some(r.owner_bot("house")),
        run_id: None,
        person: person.into(),
        agent: "house".into(),
        answerable_by: None,
        in_grants: false,
        tool: "Bash".into(),
        input: "{}".into(),
        resource: format!("net:{person}.example"),
        shown: "x".into(),
        deadline,
        reply_to: None,
        asked_by: None,
    };
    let Opened::New(card) = r.hub.store.card_open(&new, now_ms()).unwrap() else { panic!() };
    r.hub.store.card_live(&card.id, Some(&card_entry.id), None).unwrap();
    let mut pointer = data_for(&card_entry.id, "House");
    pointer["author"] = "agentbot".into();
    let pointer = r.hub.add_entry_from(&Lane::main(room), EntryKind::Notice, 1, &pointer, Some(origin)).unwrap();
    (r.hub.store.card(&card.id).unwrap(), pointer)
}

/// What Sam's history shows of the room.
async fn shown(r: &Rig, room: &str) -> Vec<Value> {
    let v = dispatch(&r.hub, &r.device("sam-phone", Some("sam")), "history", json!({"botId": room})).await.unwrap();
    v["entries"].as_array().unwrap().iter().filter(|e| e["kind"] == "notice").cloned().collect()
}

fn stored(r: &Rig, pointer: &Entry) -> Value {
    r.hub.store.entry(&pointer.id).unwrap().data
}

/// Exactly what a person may be told: no card, no title, no command, no owner's name.
fn assert_bare(entry: &Value, state: &str) {
    // The history call adds `thread` (null here) to every entry it shapes.
    let keys: std::collections::BTreeSet<_> =
        entry["data"].as_object().unwrap().keys().filter(|k| *k != "thread").cloned().collect();
    let want: std::collections::BTreeSet<_> =
        ["notice", "author", "agent", "state"].iter().map(|k| (*k).to_owned()).collect();
    assert_eq!(keys, want, "{entry}");
    assert_eq!(entry["data"]["state"], state);
    assert_eq!(entry["data"]["agent"], "House");
}

#[tokio::test]
async fn a_person_in_the_room_is_shown_that_the_agent_waits_and_nothing_of_the_card() {
    let (r, room, seat) = rig().await;
    let (card, _) = open(&r, &room, &room_person_id(&room, seat), seat, now_ms() + 60_000);
    let list = shown(&r, &room).await;
    assert_eq!(list.len(), 1, "{list:?}");
    assert_bare(&list[0], "waiting");
    assert!(!list[0].to_string().contains(&card.id));
    // A live event is cut the same way.
    let pointer = r.hub.store.room_pointers_of(card.entry_id.as_ref().unwrap()).remove(0);
    let ev = json!({"type": "entry", "rev": pointer.rev, "entry": pointer});
    assert_bare(&event_for(&r.hub.store, Some("sam"), ev).unwrap()["entry"], "waiting");
}

#[tokio::test]
async fn a_parked_card_still_shows_the_pointer() {
    let (r, room, seat) = rig().await;
    let (card, _) = open(&r, &room, &room_person_id(&room, seat), seat, now_ms() + 60_000);
    r.hub.store.card_park(&card.id).unwrap();
    assert_eq!(shown(&r, &room).await.len(), 1);
}

#[tokio::test]
async fn the_pointer_goes_when_the_owner_answers_and_a_person_hears_it_close() {
    let (r, room, seat) = rig().await;
    let (card, pointer) = open(&r, &room, &room_person_id(&room, seat), seat, now_ms() + 60_000);
    let mut events = r.hub.events.subscribe();
    crate::cards::answer(&r.hub, card.entry_id.as_ref().unwrap(), Some("allow"), None).unwrap();
    assert!(shown(&r, &room).await.is_empty(), "closed cards leave history");
    // The owner's copy keeps how it ended; the person's only says closed.
    assert_eq!(stored(&r, &pointer)["state"], "closed");
    assert_eq!(stored(&r, &pointer)["ended"], "answered");
    let mut closed = None;
    while let Ok(ev) = events.try_recv() {
        if ev["entry"]["id"] == pointer.id.as_str()
            && let Some(mine) = event_for(&r.hub.store, Some("sam"), ev.clone())
        {
            closed = Some(mine);
        }
    }
    assert_bare(&closed.expect("the close reached sam")["entry"], "closed");
}

#[tokio::test]
async fn the_pointer_goes_when_the_card_is_withdrawn_or_cut_off_or_expires() {
    let (r, room, seat) = rig().await;
    let person = room_person_id(&room, seat);
    let (_, p1) = open(&r, &room, &person, seat, now_ms() + 60_000);
    crate::cards::withdraw_room_person(&r.hub, &person);
    assert_eq!(shown(&r, &room).await.len(), 0);
    assert_eq!(stored(&r, &p1)["ended"], "cancelled", "the owner's copy says it was withdrawn, not answered");

    let (_, p2) = open(&r, &room, &person, seat, now_ms() + 60_000);
    assert_eq!(shown(&r, &room).await.len(), 1);
    crate::api::people::cut_off(&r.hub, "sam").await;
    assert_eq!(stored(&r, &p2)["state"], "closed", "a revoke ends the turn and its pointer");

    let (_, p3) = open(&r, &room, &person, seat, now_ms() - 1);
    crate::cards::sweep(&r.hub);
    assert_eq!(stored(&r, &p3)["state"], "closed");
    assert_eq!(stored(&r, &p3)["ended"], "expired");
}

/// The card closed between its entry and its pointer: nothing finds a pointer to close, yet no read shows it.
#[tokio::test]
async fn a_pointer_that_landed_after_its_cards_close_is_never_shown() {
    let (r, room, seat) = rig().await;
    let person = room_person_id(&room, seat);
    let (card, pointer) = open(&r, &room, &person, seat, now_ms() + 60_000);
    r.hub.store.card_withdraw(&card.id).unwrap();
    assert_eq!(stored(&r, &pointer)["state"], "waiting", "nothing closed it");
    assert_eq!(shown(&r, &room).await.len(), 0);
}

#[tokio::test]
async fn the_pointer_is_not_shown_from_before_a_stay_nor_to_another_room_nor_after_a_leave() {
    let (r, room, seat) = rig().await;
    let person = room_person_id(&room, seat);
    // A turn that began before sam joined.
    open(&r, &room, &person, seat - 1, now_ms() + 60_000);
    assert!(shown(&r, &room).await.is_empty(), "from before the stay");

    // Another room: the same person sees nothing of it, by read or by event.
    let other = room_of(&r);
    let (card, pointer) = open(&r, &other, "someone-else", 1, now_ms() + 60_000);
    let err = dispatch(&r.hub, &r.device("sam-phone", Some("sam")), "history", json!({"botId": other})).await;
    assert!(err.is_err(), "sam is not in that room");
    let ev = json!({"type": "entry", "rev": pointer.rev, "entry": pointer});
    assert!(event_for(&r.hub.store, Some("sam"), ev).is_none());

    // After a removal the room's own pointer reaches him no more.
    let (_, mine) = open(&r, &room, "another-turn", seat, now_ms() + 60_000);
    call(&r, "removeRoomPerson", json!({"room": room, "person": "sam"})).await.unwrap();
    let ev = json!({"type": "entry", "rev": mine.rev, "entry": mine});
    assert!(event_for(&r.hub.store, Some("sam"), ev).is_none());
    // And the owner sees every pointer, whole.
    let ev = json!({"type": "entry", "rev": pointer.rev, "entry": pointer});
    let seen = event_for(&r.hub.store, None, ev).unwrap();
    assert_eq!(seen["entry"]["data"]["card"], card.entry_id.clone().unwrap());
}
