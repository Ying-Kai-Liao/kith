//! A room turn's open cards end with the turn (rooms with people, package I): an access change or a revoke of anyone in
//! the turn's basis withdraws them, a restart that lost the registry notwithstanding; other turns' cards, the owner's own
//! and 1:1 cards stay. The model (verification/rooms) has no cards, so these tests hold the rule alone.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::devices::Caller;
use super::dispatch;
use super::rig::Rig;
use crate::chat::room_people::FLAG;
use crate::chat::room_turn::room_person_id;
use crate::remote::members::{self, Listed};
use crate::store::{BotConfig, Card, CardState, EntryKind, Lane, NewCard, Opened, now_ms};
use serde_json::{Value, json};

async fn call(r: &Rig, method: &str, b: Value) -> anyhow::Result<Value> {
    dispatch(&r.hub, &Caller::Local, method, b).await
}

/// Sam is an invited person with `house`; the room is the owner's, with the `house` agent in it.
async fn rig() -> (Rig, String) {
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
    let cfg: BotConfig =
        serde_json::from_value(json!({"id": "", "kind": "group", "name": "Crew", "members": [r.owner_bot("house")]}))
            .unwrap();
    let room = r.hub.create_bot(cfg).unwrap()["id"].as_str().unwrap().to_owned();
    (r, room)
}

fn join(r: &Rig, room: &str, person: &str) -> i64 {
    let line = json!({"notice": "personJoined", "style": "info", "text": "joined"});
    r.hub.store.join_room_person(room, person, &line).unwrap().unwrap().seq
}

/// An open card as a room turn's `add_room_card` leaves it, for `person` (a room person's id, "" the owner, or a person).
fn card_of(r: &Rig, person: &str) -> Card {
    let data = json!({"status": "pending", "options": [], "actor": "x"});
    let entry = r.hub.add_entry(&Lane::main(&r.owner_bot("house")), EntryKind::Permission, 1, &data).unwrap();
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
        deadline: now_ms() + 60_000,
        reply_to: None,
        asked_by: None,
    };
    let Opened::New(card) = r.hub.store.card_open(&new, now_ms()).unwrap() else { panic!() };
    r.hub.store.card_live(&card.id, Some(&entry.id), None).unwrap();
    r.hub.store.card(&card.id).unwrap()
}

fn state(r: &Rig, card: &Card) -> CardState {
    r.hub.store.card(&card.id).unwrap().state
}

fn entry_status(r: &Rig, card: &Card) -> String {
    r.hub.store.entry(card.entry_id.as_ref().unwrap()).unwrap().data["status"].as_str().unwrap().to_owned()
}

/// Two epochs: ann alone (before sam), then ann and sam. The card of each epoch's room person, the owner's own and sam's 1:1.
struct Cards {
    before_sam: Card,
    with_sam: Card,
    owner: Card,
    one_to_one: Card,
}

fn open_cards(r: &Rig, room: &str) -> Cards {
    let ann = join(r, room, "ann");
    let sam = join(r, room, "sam");
    Cards {
        before_sam: card_of(r, &room_person_id(room, ann)),
        with_sam: card_of(r, &room_person_id(room, sam)),
        owner: card_of(r, ""),
        one_to_one: card_of(r, "sam"),
    }
}

fn assert_only_sams_turn_ended(r: &Rig, c: &Cards) {
    assert_eq!(state(r, &c.with_sam), CardState::Withdrawn);
    assert_eq!(entry_status(r, &c.with_sam), "cancelled");
    assert_eq!(state(r, &c.before_sam), CardState::Live, "a turn whose basis lacks sam keeps its card");
    assert_eq!(entry_status(r, &c.before_sam), "pending");
    assert_eq!(state(r, &c.owner), CardState::Live, "the owner's own card");
}

fn running(r: &Rig, room: &str, c: &Cards) -> String {
    let key = crate::agent::bot::actor_key_as(&r.owner_bot("house"), &Lane::main(room), Some(&c.with_sam.person));
    r.hub.room_turn_begins(&key, &["ann".into(), "sam".into()]);
    key
}

/// A revoke while the turn runs: the registry names the turn, its card goes, others stay. Model: jobs-revokekeepsturn.
#[tokio::test]
async fn a_revoke_withdraws_the_card_of_a_running_room_turn_and_only_that_one() {
    let (r, room) = rig().await;
    let c = open_cards(&r, &room);
    running(&r, &room, &c);
    crate::api::people::cut_off(&r.hub, "sam").await;
    assert_only_sams_turn_ended(&r, &c);
    assert_eq!(state(&r, &c.one_to_one), CardState::Withdrawn, "their own 1:1 card goes with a revoke, as before");
    // Twice, and undo then revoke again, close nothing twice and reopen nothing.
    crate::api::people::cut_off(&r.hub, "sam").await;
    r.hub.store.restore_person("sam").unwrap();
    assert_eq!(state(&r, &c.with_sam), CardState::Withdrawn, "undo does not reopen a card");
    crate::api::people::cut_off(&r.hub, "sam").await;
    assert_only_sams_turn_ended(&r, &c);
}

/// A host that restarted lost its registry: the stored stays still name the epochs sam was part of.
#[tokio::test]
async fn a_revoke_after_a_restart_withdraws_the_card_found_by_the_stored_stays() {
    let (r, room) = rig().await;
    let c = open_cards(&r, &room);
    // The restart parked the live card; nothing is registered.
    r.hub.store.card_park(&c.with_sam.id).unwrap();
    crate::api::people::cut_off(&r.hub, "sam").await;
    assert_only_sams_turn_ended(&r, &c);
}

/// A person who left before an epoch began is not in its basis: that epoch's card stays.
#[tokio::test]
async fn an_epoch_after_the_person_left_keeps_its_card() {
    let (r, room) = rig().await;
    join(&r, &room, "sam");
    let line = json!({"notice": "personLeft", "style": "info", "text": "left"});
    r.hub.store.leave_room_person(&room, "sam", &line).unwrap().unwrap();
    let later = join(&r, &room, "ann");
    let card = card_of(&r, &room_person_id(&room, later));
    crate::api::people::cut_off(&r.hub, "sam").await;
    assert_eq!(state(&r, &card), CardState::Live);
}

/// An access change that takes an agent ends the turns whose basis holds the person: their cards go too.
#[tokio::test]
async fn an_access_change_withdraws_the_card_of_the_room_turn_it_ended() {
    let (r, room) = rig().await;
    let c = open_cards(&r, &room);
    running(&r, &room, &c);
    call(&r, "setPersonAccess", json!({"personId": "sam", "agents": {}})).await.unwrap();
    assert_only_sams_turn_ended(&r, &c);
    // Adding an agent (house kept, money given) takes nothing: no turn ends, no card goes.
    let kept = card_of(&r, &room_person_id(&room, r.hub.store.room_gen(&room)));
    let key = crate::agent::bot::actor_key_as(&r.owner_bot("house"), &Lane::main(&room), Some(&kept.person));
    r.hub.room_turn_begins(&key, &["ann".into(), "sam".into()]);
    call(&r, "setPersonAccess", json!({"personId": "sam", "agents": {"house": "member", "money": "member"}}))
        .await
        .unwrap();
    assert_eq!(state(&r, &kept), CardState::Live);
    assert!(r.hub.room_turn_registered(&key), "the turn runs on");
    // A projects-only change names no agents and ends nothing.
    call(&r, "setPersonAccess", json!({"personId": "sam", "projects": []})).await.unwrap();
    assert_eq!(state(&r, &kept), CardState::Live);
}

/// A card under a secret write is left to its own steps.
#[tokio::test]
async fn a_secret_card_being_written_is_left_alone() {
    let (r, room) = rig().await;
    let c = open_cards(&r, &room);
    let _slot = crate::cards::secret::hold_for_test(&c.with_sam.id);
    crate::api::people::cut_off(&r.hub, "sam").await;
    assert_eq!(state(&r, &c.with_sam), CardState::Live);
}

/// A turn parked on its card is still registered (it has not ended), so a revoke stops it and its card goes. A turn that
/// ended (idle) has no registry entry: its card is found by the stored stays (the restart test above).
#[tokio::test]
async fn a_revoke_ends_a_parked_turn_and_withdraws_its_card() {
    let (r, room) = rig().await;
    let c = open_cards(&r, &room);
    r.hub.store.card_park(&c.with_sam.id).unwrap();
    let key = running(&r, &room, &c);
    assert_eq!(r.hub.end_room_turns_of("sam"), std::slice::from_ref(&c.with_sam.person));
    // The actor learns it was ended by access (it withdraws what it opens late, not parks it) from the registry.
    assert!(!r.hub.room_turn_registered(&key));
    crate::api::people::cut_off(&r.hub, "sam").await;
    assert_eq!(state(&r, &c.with_sam), CardState::Withdrawn);
}
