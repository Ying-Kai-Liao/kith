//! People in a group room (rooms with people, slice A), against a real kith-auth in a temp folder: the owner's three
//! methods, the gen each step takes, the live `room_member_may`, and a revoke that writes no room row.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::devices::Caller;
use super::dispatch;
use super::rig::Rig;
use crate::chat::room_people::{self, FLAG};
use crate::remote::members::{self, Listed};
use serde_json::{Value, json};

async fn call(r: &Rig, method: &str, b: Value) -> anyhow::Result<Value> {
    dispatch(&r.hub, &Caller::Local, method, b).await
}

/// Sam, let in with `agents` (agent -> role).
async fn sam_with(r: &Rig, agents: Value) {
    let made = call(r, "createInvite", json!({"name": "Sam", "agents": agents})).await.unwrap();
    let listed = [Listed {
        membership_id: "mem_sam".into(),
        user_id: "user_sam".into(),
        invite_id: made["inviteId"].as_str().unwrap().to_owned(),
        ..Default::default()
    }];
    members::apply(&r.hub, Some(&r.bin), &listed, Some("user_owner")).await.unwrap();
}

/// A group room of the owner's bots of `agents`.
fn room_of(r: &Rig, agents: &[&str]) -> String {
    let members: Vec<String> = agents.iter().map(|a| r.owner_bot(a)).collect();
    let cfg = serde_json::from_value(json!({"id": "", "kind": "group", "name": "Crew", "members": members})).unwrap();
    r.hub.create_bot(cfg).unwrap()["id"].as_str().unwrap().to_owned()
}

fn flag_on(r: &Rig) {
    r.hub.store.kv_set(FLAG, "on").unwrap();
}

async fn add(r: &Rig, room: &str, person: &str) -> anyhow::Result<Value> {
    call(r, "addRoomPerson", json!({"room": room, "person": person})).await
}

async fn remove(r: &Rig, room: &str, person: &str) -> anyhow::Result<Value> {
    call(r, "removeRoomPerson", json!({"room": room, "person": person})).await
}

#[tokio::test]
async fn adding_is_refused_while_the_setting_is_off_and_taking_out_is_not() {
    let r = Rig::new().await;
    sam_with(&r, json!({"house": "member"})).await;
    let room = room_of(&r, &["house"]);
    let e = add(&r, &room, "sam").await.unwrap_err();
    assert!(format!("{e:#}").contains("aren't turned on"), "{e:#}");
    assert!(r.hub.store.room_stays(&room).is_empty());
    assert_eq!(remove(&r, &room, "sam").await.unwrap()["changed"], false, "nobody to take out, still fine");
    let listed = call(&r, "roomPeople", json!({"room": room})).await.unwrap();
    assert_eq!(listed, json!({"enabled": false, "gen": 0, "people": []}));
}

#[tokio::test]
async fn an_add_and_a_remove_each_take_one_gen_and_a_re_add_opens_a_new_stay() {
    let r = Rig::new().await;
    flag_on(&r);
    sam_with(&r, json!({"house": "member"})).await;
    let room = room_of(&r, &["house"]);

    let first = add(&r, &room, "sam").await.unwrap();
    assert_eq!(first["changed"], true);
    let g1 = first["gen"].as_i64().unwrap();
    let line = r.hub.store.entries_since(0, 100).unwrap().into_iter().find(|e| e.seq == g1).unwrap();
    assert_eq!(line.data["notice"], "personJoined", "the gen is the seq of the join line in the room");
    assert_eq!(line.bot_id, room);

    let repeat = add(&r, &room, "sam").await.unwrap();
    assert_eq!(repeat, json!({"gen": g1, "changed": false}), "a repeat takes no second gen");

    let left = remove(&r, &room, "sam").await.unwrap();
    let g2 = left["gen"].as_i64().unwrap();
    assert!(left["changed"] == true && g2 > g1);
    assert_eq!(remove(&r, &room, "sam").await.unwrap(), json!({"gen": g2, "changed": false}), "no second leave");

    let again = add(&r, &room, "sam").await.unwrap();
    let g3 = again["gen"].as_i64().unwrap();
    assert!(g3 > g2);
    let listed = call(&r, "roomPeople", json!({"room": room})).await.unwrap();
    assert_eq!(listed["gen"], g3);
    assert_eq!(
        listed["people"],
        json!([
            {"person": "sam", "joinedGen": g1, "leftGen": g2},
            {"person": "sam", "joinedGen": g3, "leftGen": null},
        ])
    );
}

#[tokio::test]
async fn a_person_not_granted_every_agent_of_the_room_is_refused_and_so_is_the_owner() {
    let r = Rig::new().await;
    flag_on(&r);
    sam_with(&r, json!({"house": "member"})).await;
    let both = room_of(&r, &["house", "money"]);
    let e = add(&r, &both, "sam").await.unwrap_err();
    assert!(format!("{e:#}").contains("isn't granted money"), "{e:#}");
    for who in ["kai", "owner", "", "room-abc", "nobody"] {
        assert!(add(&r, &both, who).await.is_err(), "{who:?} must not become a room person");
    }
    assert!(r.hub.store.room_stays(&both).is_empty(), "nothing was written");
}

#[tokio::test]
async fn a_room_takes_people_only_with_kith_agents_in_it() {
    let r = Rig::new().await;
    flag_on(&r);
    sam_with(&r, json!({"house": "member"})).await;
    // Not a group at all.
    let bot = r.owner_bot("house");
    assert!(add(&r, &bot, "sam").await.is_err());
    // A group of plain bots: no Kith agent, so nothing for a revoke to fail in `room_member_may`.
    for id in ["a", "b"] {
        let cfg = serde_json::from_value(json!({"id": id, "name": id, "backend": "claude", "cwd": "/tmp"})).unwrap();
        r.hub.create_bot(cfg).unwrap();
    }
    let plain = r
        .hub
        .create_bot(serde_json::from_value(json!({"id": "", "kind": "group", "name": "P", "members": ["a", "b"]})).unwrap())
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let e = add(&r, &plain, "sam").await.unwrap_err();
    assert!(format!("{e:#}").contains("takes only Kith agents"), "{e:#}");
    // A Kith agent beside a plain bot is no better.
    let mixed = r
        .hub
        .create_bot(
            serde_json::from_value(json!({"id": "", "kind": "group", "name": "M", "members": ["a", bot]})).unwrap(),
        )
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(add(&r, &mixed, "sam").await.is_err());
    assert!(!room_people::room_member_may(&r.hub.store, "sam", &plain).await);
}

#[tokio::test]
async fn an_agent_joins_a_room_with_people_only_when_every_person_has_it() {
    let r = Rig::new().await;
    flag_on(&r);
    sam_with(&r, json!({"house": "member"})).await;
    let room = room_of(&r, &["house"]);
    let money = r.owner_bot("money");
    let house_bot = r.hub.store.bot(&room).unwrap().unwrap().config.members[0].clone();
    add(&r, &room, "sam").await.unwrap();

    let e = call(&r, "updateBot", json!({"id": room, "members": [house_bot, money]})).await.unwrap_err();
    assert!(format!("{e:#}").contains("isn't granted money"), "{e:#}");
    assert_eq!(r.hub.store.bot(&room).unwrap().unwrap().config.members, [house_bot.clone()], "not saved");

    // The same edit through a path that checked nothing is refused too.
    let e = r.hub.update_bot(&json!({"id": room, "members": [house_bot, money]})).unwrap_err();
    assert!(format!("{e:#}").contains("check again"), "{e:#}");

    // Once Sam holds money, the agent may join; and an edit that adds nothing never needed a check.
    call(&r, "setPersonAccess", json!({"personId": "sam", "agents": {"house": "member", "money": "member"}}))
        .await
        .unwrap();
    call(&r, "updateBot", json!({"id": room, "members": [house_bot, money]})).await.unwrap();
    assert_eq!(r.hub.store.bot(&room).unwrap().unwrap().config.members.len(), 2);
    call(&r, "updateBot", json!({"id": room, "name": "Crew 2"})).await.unwrap();
}

#[tokio::test]
async fn a_revoke_writes_no_room_row_and_its_undo_gives_back_exactly_what_was_there() {
    let r = Rig::new().await;
    flag_on(&r);
    sam_with(&r, json!({"house": "member"})).await;
    let room = room_of(&r, &["house"]);
    add(&r, &room, "sam").await.unwrap();
    remove(&r, &room, "sam").await.unwrap();
    add(&r, &room, "sam").await.unwrap();
    let before = (r.hub.store.room_stays(&room), r.hub.store.room_gen(&room));
    assert!(room_people::room_member_may(&r.hub.store, "sam", &room).await);

    call(&r, "revokePerson", json!({"personId": "sam"})).await.unwrap();
    assert_eq!((r.hub.store.room_stays(&room), r.hub.store.room_gen(&room)), before, "a revoke writes no room row");
    assert!(!room_people::room_member_may(&r.hub.store, "sam", &room).await, "revoked: no longer may, live");

    call(&r, "unrevokePerson", json!({"personId": "sam"})).await.unwrap();
    assert_eq!((r.hub.store.room_stays(&room), r.hub.store.room_gen(&room)), before, "undo leaves them as they were");
    assert!(room_people::room_member_may(&r.hub.store, "sam", &room).await, "undone: may again");
}

#[tokio::test]
async fn room_member_may_needs_an_open_stay_and_every_agent_live() {
    let r = Rig::new().await;
    flag_on(&r);
    sam_with(&r, json!({"house": "member", "money": "member"})).await;
    let room = room_of(&r, &["house", "money"]);
    assert!(!room_people::room_member_may(&r.hub.store, "sam", &room).await, "granted, but never let in");
    add(&r, &room, "sam").await.unwrap();
    assert!(room_people::room_member_may(&r.hub.store, "sam", &room).await);
    // A grant taken away later closes it for them (the row stays), and putting it back opens it again.
    call(&r, "setPersonAccess", json!({"personId": "sam", "agents": {"house": "member"}})).await.unwrap();
    assert!(!room_people::room_member_may(&r.hub.store, "sam", &room).await, "lost money");
    assert_eq!(r.hub.store.room_open_people(&room), ["sam"], "the stay itself is untouched");
    call(&r, "setPersonAccess", json!({"personId": "sam", "agents": {"house": "member", "money": "member"}}))
        .await
        .unwrap();
    assert!(room_people::room_member_may(&r.hub.store, "sam", &room).await);
    remove(&r, &room, "sam").await.unwrap();
    assert!(!room_people::room_member_may(&r.hub.store, "sam", &room).await, "taken out");
}

#[tokio::test]
async fn a_deleted_room_takes_its_people_with_it() {
    let r = Rig::new().await;
    flag_on(&r);
    sam_with(&r, json!({"house": "member"})).await;
    let room = room_of(&r, &["house"]);
    add(&r, &room, "sam").await.unwrap();
    call(&r, "deleteBot", json!({"id": room})).await.unwrap();
    assert!(r.hub.store.room_stays(&room).is_empty());
    assert_eq!(r.hub.store.room_gen(&room), 0);
}

#[tokio::test]
async fn an_invited_person_cannot_call_the_methods_not_even_for_themselves() {
    let r = Rig::new().await;
    flag_on(&r);
    sam_with(&r, json!({"house": "member"})).await;
    let room = room_of(&r, &["house"]);
    let phone = r.device("sam-phone", Some("sam"));
    for method in ["addRoomPerson", "removeRoomPerson"] {
        let e = dispatch(&r.hub, &phone, method, json!({"room": room, "person": "sam"})).await.unwrap_err();
        assert!(e.is::<super::devices::NotAllowed>(), "{method}: {e:#}");
    }
    assert!(dispatch(&r.hub, &phone, "roomPeople", json!({"room": room})).await.is_err());
    assert!(r.hub.store.room_stays(&room).is_empty());
}
