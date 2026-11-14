//! People in a room (designs/2026-11-12-rooms-with-people, Build 1), against a real kith-auth in a temp folder: a join or
//! leave is one transaction that moves the gen, a join needs every agent in the room, an agent joins a room with people
//! only when each of them has it, and a revoke and its undo never touch the stays.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::devices::Caller;
use super::dispatch;
use super::rig::Rig;
use crate::remote::members::{self, Listed};
use crate::store::RoomStay;
use serde_json::{Value, json};

async fn call(r: &Rig, method: &str, b: Value) -> anyhow::Result<Value> {
    dispatch(&r.hub, &Caller::Local, method, b).await
}

/// `sam`, invited to `agents` and bound to `user_sam`.
async fn joined(r: &Rig, agents: Value) {
    let made = call(r, "createInvite", json!({"name": "Sam", "agents": agents})).await.unwrap();
    let listed = [Listed {
        membership_id: "mem_sam".into(),
        user_id: "user_sam".into(),
        invite_id: made["inviteId"].as_str().unwrap().to_owned(),
        ..Default::default()
    }];
    members::apply(&r.hub, Some(&r.bin), &listed, Some("user_owner")).await.unwrap();
}

/// A rig with the `rooms.people` setting on, as it will be once the turns for a room's people have landed.
async fn rig() -> Rig {
    let r = Rig::new().await;
    r.hub.store.kv_set(crate::hub::rooms::PEOPLE_KEY, "on").unwrap();
    r
}

fn room(r: &Rig, members: &[&str]) -> String {
    let cfg = serde_json::from_value(json!({"id": "", "kind": "group", "name": "Crew", "members": members})).unwrap();
    r.hub.create_bot(cfg).unwrap()["id"].as_str().unwrap().to_owned()
}

async fn may(r: &Rig, room: &str) -> bool {
    crate::hub::rooms::room_member_may(&r.hub.store, "sam", room).await
}

fn stay(joined_gen: i64, left_gen: Option<i64>) -> RoomStay {
    RoomStay { person: "sam".into(), joined_gen, left_gen }
}

#[tokio::test]
async fn a_join_and_a_leave_each_move_the_gen_and_the_stay() {
    let r = rig().await;
    joined(&r, json!({"house": "member", "money": "member"})).await;
    let g = room(&r, &[&r.owner_bot("house"), &r.owner_bot("money")]);
    assert!(!may(&r, &g).await, "not in the room yet");

    let out = call(&r, "addRoomPerson", json!({"roomId": g, "person": "sam"})).await.unwrap();
    assert_eq!(out, json!({"gen": 1, "people": [{"person": "sam", "joinedGen": 1, "leftGen": null}]}));
    assert!(may(&r, &g).await);
    assert!(call(&r, "addRoomPerson", json!({"roomId": g, "person": "sam"})).await.is_err(), "already in");
    assert_eq!(r.hub.store.room_gen(&g), 1, "a refused join moves nothing");

    let out = call(&r, "removeRoomPerson", json!({"roomId": g, "person": "sam"})).await.unwrap();
    assert_eq!(out["gen"], 2);
    assert_eq!(r.hub.store.room_stays(&g).unwrap(), [stay(1, Some(2))]);
    assert!(!may(&r, &g).await, "left");
    assert_eq!(call(&r, "roomPeople", json!({"roomId": g})).await.unwrap(), out);
}

#[tokio::test]
async fn a_join_is_refused_while_the_setting_is_off_but_a_leave_and_the_listing_work() {
    let r = Rig::new().await;
    joined(&r, json!({"house": "member"})).await;
    let g = room(&r, &[&r.owner_bot("house")]);
    let e = call(&r, "addRoomPerson", json!({"roomId": g, "person": "sam"})).await.unwrap_err();
    assert!(format!("{e:#}").contains("rooms.people"), "{e:#}");
    assert_eq!(r.hub.store.room_gen(&g), 0);
    assert_eq!(r.hub.store.room_stays(&g).unwrap(), []);
    assert_eq!(call(&r, "roomPeople", json!({"roomId": g})).await.unwrap(), json!({"gen": 0, "people": []}));

    // Once it is on the same call works; turned off again, a stay can still be closed.
    r.hub.store.kv_set(crate::hub::rooms::PEOPLE_KEY, "on").unwrap();
    call(&r, "addRoomPerson", json!({"roomId": g, "person": "sam"})).await.unwrap();
    r.hub.store.kv_set(crate::hub::rooms::PEOPLE_KEY, "off").unwrap();
    let out = call(&r, "removeRoomPerson", json!({"roomId": g, "person": "sam"})).await.unwrap();
    assert_eq!(out["gen"], 2);
}

#[tokio::test]
async fn a_join_is_refused_unless_the_person_has_every_agent_in_the_room() {
    let r = rig().await;
    joined(&r, json!({"house": "member"})).await;
    let g = room(&r, &[&r.owner_bot("house"), &r.owner_bot("money")]);
    let e = call(&r, "addRoomPerson", json!({"roomId": g, "person": "sam"})).await.unwrap_err();
    assert!(format!("{e:#}").contains("money"), "{e:#}");
    assert_eq!(r.hub.store.room_gen(&g), 0);
    assert_eq!(r.hub.store.room_stays(&g).unwrap(), []);

    // A bot that isn't a Kith agent can't be granted, so a room with one takes nobody.
    let cfg = serde_json::from_value(json!({"id": "", "name": "Other", "backend": "claude", "cwd": "/"})).unwrap();
    let other = r.hub.create_bot(cfg).unwrap()["id"].as_str().unwrap().to_owned();
    let g = room(&r, &[&r.owner_bot("house"), &other]);
    assert!(call(&r, "addRoomPerson", json!({"roomId": g, "person": "sam"})).await.is_err());
    assert_eq!(r.hub.store.room_gen(&g), 0);
}

#[tokio::test]
async fn an_agent_joins_a_room_with_people_only_when_each_of_them_has_it() {
    let r = rig().await;
    joined(&r, json!({"house": "member"})).await;
    let house = r.owner_bot("house");
    let money = r.owner_bot("money");
    let g = room(&r, &[&house]);
    call(&r, "addRoomPerson", json!({"roomId": g, "person": "sam"})).await.unwrap();

    let e = call(&r, "updateBot", json!({"id": g, "members": [house, money]})).await.unwrap_err();
    assert!(format!("{e:#}").contains("money"), "{e:#}");
    assert_eq!(r.hub.store.bot(&g).unwrap().unwrap().config.members, std::slice::from_ref(&house));

    call(&r, "setPersonAccess", json!({"personId": "sam", "agents": {"house": "member", "money": "member"}}))
        .await
        .unwrap();
    call(&r, "updateBot", json!({"id": g, "members": [house, money]})).await.unwrap();
    assert_eq!(r.hub.store.room_stays(&g).unwrap(), [stay(1, None)], "an agent edit moves no stay");
    assert_eq!(r.hub.store.room_gen(&g), 1, "an edit keeps the room's gen");
    assert!(may(&r, &g).await);
}

#[tokio::test]
async fn a_revoke_and_its_undo_leave_the_stays_as_they_were() {
    let r = rig().await;
    joined(&r, json!({"house": "member"})).await;
    let g = room(&r, &[&r.owner_bot("house")]);
    call(&r, "addRoomPerson", json!({"roomId": g, "person": "sam"})).await.unwrap();
    let before = (r.hub.store.room_gen(&g), r.hub.store.room_stays(&g).unwrap());

    call(&r, "revokePerson", json!({"personId": "sam"})).await.unwrap();
    assert_eq!((r.hub.store.room_gen(&g), r.hub.store.room_stays(&g).unwrap()), before);
    assert!(!may(&r, &g).await, "a revoked person reaches nothing, stay or not");

    call(&r, "unrevokePerson", json!({"personId": "sam"})).await.unwrap();
    assert_eq!((r.hub.store.room_gen(&g), r.hub.store.room_stays(&g).unwrap()), before);
    assert!(may(&r, &g).await, "the undo gives the room back");
}
