//! An invite that names a room (rooms with people, slice E), against a real kith-auth in a temp folder: the join at the
//! invitee's proof and at host start, the done mark that keeps Kai's removal, and what a cancelled, revoked or
//! room-less invite does. The invitee-access properties of `people/proveInvite`: a removed person's second device never
//! re-joins, a restart between the prove and the join still joins, a cancelled invite never joins.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::devices::Caller;
use super::dispatch;
use super::rig::Rig;
use crate::chat::invite_rooms::reconcile;
use crate::chat::room_people::FLAG;
use crate::remote::members::{self, Listed};
use serde_json::{Value, json};

async fn call(r: &Rig, caller: &Caller, method: &str, b: Value) -> anyhow::Result<Value> {
    dispatch(&r.hub, caller, method, b).await
}

async fn owner(r: &Rig, method: &str, b: Value) -> anyhow::Result<Value> {
    call(r, &Caller::Local, method, b).await
}

fn room_of(r: &Rig, agents: &[&str]) -> String {
    let members: Vec<String> = agents.iter().map(|a| r.owner_bot(a)).collect();
    let cfg = serde_json::from_value(json!({"id": "", "kind": "group", "name": "Crew", "members": members})).unwrap();
    r.hub.create_bot(cfg).unwrap()["id"].as_str().unwrap().to_owned()
}

/// An invite for Sam that names `rooms`, accepted (the membership is recorded) but not proven by any device yet.
/// Answers the invite id and the token of its link.
async fn accepted(r: &Rig, agents: Value, rooms: &[String]) -> (String, String) {
    let made = owner(r, "createInvite", json!({"name": "Sam", "agents": agents, "rooms": rooms})).await.unwrap();
    let invite = made["inviteId"].as_str().unwrap().to_owned();
    let token = made["link"].as_str().unwrap().rsplit("t=").next().unwrap().to_owned();
    let listed = [Listed {
        membership_id: "mem_sam".into(),
        user_id: "user_sam".into(),
        invite_id: invite.clone(),
        ..Default::default()
    }];
    members::apply(&r.hub, Some(&r.bin), &listed, Some("user_owner")).await.unwrap();
    (invite, token)
}

async fn prove(r: &Rig, key: &str, invite: &str, token: &str) {
    let device = r.unproven_device(key, Some("sam"));
    call(r, &device, "proveInvite", json!({"inviteId": invite, "token": token})).await.unwrap();
}

fn flag_on(r: &Rig) {
    r.hub.store.kv_set(FLAG, "on").unwrap();
}

fn people(r: &Rig, room: &str) -> Vec<String> {
    r.hub.store.room_open_people(room)
}

#[tokio::test]
async fn naming_a_room_needs_the_setting_a_room_that_takes_people_and_every_agent_in_it() {
    let r = Rig::new().await;
    let room = room_of(&r, &["house", "money"]);
    let named = json!({"name": "Sam", "agents": {"house": "member", "money": "member"}, "rooms": [room]});
    r.hub.store.kv_set(FLAG, "off").unwrap();
    let e = owner(&r, "createInvite", named.clone()).await.unwrap_err();
    assert!(format!("{e:#}").contains("aren't turned on"), "{e:#}");
    flag_on(&r);
    let e = owner(&r, "createInvite", json!({"name": "Sam", "agents": {"house": "member"}, "rooms": [room]}))
        .await
        .unwrap_err();
    assert!(format!("{e:#}").contains("isn't granted money"), "rule 5: {e:#}");
    assert!(r.hub.store.invites().unwrap().is_empty(), "a refused invite leaves nothing");
    let e = owner(&r, "createInvite", json!({"name": "Sam", "agents": {"house": "member"}, "rooms": ["nope"]}))
        .await
        .unwrap_err();
    assert!(format!("{e:#}").contains("group room"), "{e:#}");
    let made = owner(&r, "createInvite", named).await.unwrap();
    let invite = made["inviteId"].as_str().unwrap();
    assert_eq!(r.hub.store.invite_rooms(invite), vec![room.clone()]);
    let listed = owner(&r, "invites", json!({})).await.unwrap();
    assert_eq!(listed["invites"][0]["rooms"], json!([{"room": room, "done": false}]), "Kai sees the part waiting");
}

#[tokio::test]
async fn a_plain_invite_has_no_room_part_and_the_old_rows_have_none() {
    let r = Rig::new().await;
    flag_on(&r);
    let (invite, token) = accepted(&r, json!({"house": "member"}), &[]).await;
    assert_eq!(r.hub.store.invite_rooms(&invite), Vec::<String>::new());
    prove(&r, "sam-phone", &invite, &token).await;
    let listed = owner(&r, "invites", json!({})).await.unwrap();
    assert!(listed["invites"][0].get("rooms").is_none(), "{listed}");
    assert_eq!(r.hub.store.invite_rooms_todo(), vec![]);
}

#[tokio::test]
async fn an_invite_for_someone_already_here_cannot_name_a_room() {
    let r = Rig::new().await;
    flag_on(&r);
    let room = room_of(&r, &["house"]);
    accepted(&r, json!({"house": "member"}), &[]).await;
    let e = owner(&r, "createInvite", json!({"personId": "sam", "agents": {"house": "member"}, "rooms": [room]}))
        .await
        .unwrap_err();
    assert!(format!("{e:#}").contains("add them to the room directly"), "{e:#}");
}

#[tokio::test]
async fn the_proof_joins_the_person_once_and_marks_the_part_done() {
    let r = Rig::new().await;
    flag_on(&r);
    let room = room_of(&r, &["house"]);
    let (invite, token) = accepted(&r, json!({"house": "member"}), std::slice::from_ref(&room)).await;
    reconcile(r.hub.clone()).await;
    assert!(people(&r, &room).is_empty(), "accepted is not proved: nobody joins yet");
    prove(&r, "sam-phone", &invite, &token).await;
    assert_eq!(people(&r, &room), vec!["sam".to_owned()]);
    assert!(r.hub.store.invite_room_done(&invite, &room));
    let before = r.hub.store.room_gen(&room);
    let listed = owner(&r, "invites", json!({})).await.unwrap();
    assert_eq!(listed["invites"][0]["rooms"], json!([{"room": room, "done": true}]));
    reconcile(r.hub.clone()).await;
    assert_eq!(r.hub.store.room_gen(&room), before, "a repeat takes no second gen");
}

/// invite-nomarker: Sam proves on the phone and joins, Kai takes him out, the Mac proves later.
#[tokio::test]
async fn a_removed_persons_second_device_never_re_joins() {
    let r = Rig::new().await;
    flag_on(&r);
    let room = room_of(&r, &["house"]);
    let (invite, token) = accepted(&r, json!({"house": "member"}), std::slice::from_ref(&room)).await;
    prove(&r, "sam-phone", &invite, &token).await;
    owner(&r, "removeRoomPerson", json!({"room": room, "person": "sam"})).await.unwrap();
    assert_eq!(people(&r, &room), Vec::<String>::new());
    prove(&r, "sam-mac", &invite, &token).await;
    reconcile(r.hub.clone()).await;
    assert!(people(&r, &room).is_empty(), "the done mark keeps Kai's removal");
    assert_eq!(r.hub.store.room_stays(&room).len(), 1, "no second stay");
}

/// invite-edge: the host went down between the proof and the join. The proof is on the device row; nothing else
/// remembers it, so only a pass that looks at the rows can join him.
#[tokio::test]
async fn a_restart_between_the_proof_and_the_join_still_joins() {
    let r = Rig::new().await;
    flag_on(&r);
    let room = room_of(&r, &["house"]);
    let (invite, _) = accepted(&r, json!({"house": "member"}), std::slice::from_ref(&room)).await;
    r.hub.store.put_device(&sam_device("sam-phone")).unwrap();
    r.hub.store.set_device_proven("sam-phone").unwrap();
    assert!(people(&r, &room).is_empty(), "the proof landed, the join did not");
    reconcile(r.hub.clone()).await; // the pass at host start
    assert_eq!(people(&r, &room), vec!["sam".to_owned()]);
    assert!(r.hub.store.invite_room_done(&invite, &room));
}

fn sam_device(key: &str) -> crate::store::Device {
    use crate::store::{Device, DeviceSource, Scope, now_ms};
    Device {
        key: key.into(),
        name: "Phone".into(),
        platform: "ios".into(),
        source: DeviceSource::Account,
        grant_id: Some(format!("grt_{key}")),
        scopes: vec![Scope::Control],
        lease_until: Some(now_ms() + 60_000),
        created_at: now_ms(),
        last_seen_at: None,
        person_id: Some("sam".into()),
    }
}

/// A cancelled invite (revoked before anyone accepted it) has no membership, so no device can prove it, and a pass that
/// looks at the rows finds nobody who did.
#[tokio::test]
async fn a_cancelled_invite_never_joins() {
    let r = Rig::new().await;
    flag_on(&r);
    let room = room_of(&r, &["house"]);
    let made = owner(&r, "createInvite", json!({"name": "Sam", "agents": {"house": "member"}, "rooms": [room]}))
        .await
        .unwrap();
    let invite = made["inviteId"].as_str().unwrap();
    assert_eq!(owner(&r, "revokeInvite", json!({"inviteId": invite})).await.unwrap()["revoked"], true);
    let listed = [Listed {
        membership_id: "mem_sam".into(),
        user_id: "user_sam".into(),
        invite_id: invite.to_owned(),
        ..Default::default()
    }];
    members::apply(&r.hub, Some(&r.bin), &listed, Some("user_owner")).await.unwrap();
    r.hub.store.put_device(&sam_device("sam-phone")).unwrap();
    r.hub.store.set_device_proven("sam-phone").unwrap();
    reconcile(r.hub.clone()).await;
    assert_eq!(people(&r, &room), Vec::<String>::new());
    assert!(!r.hub.store.invite_room_done(invite, &room));
}

#[tokio::test]
async fn a_revoked_person_does_not_join_and_an_undo_lets_the_waiting_part_go_on() {
    let r = Rig::new().await;
    flag_on(&r);
    let room = room_of(&r, &["house"]);
    let _ = accepted(&r, json!({"house": "member"}), std::slice::from_ref(&room)).await;
    r.hub.store.put_device(&sam_device("sam-phone")).unwrap();
    owner(&r, "revokePerson", json!({"personId": "sam"})).await.unwrap();
    reconcile(r.hub.clone()).await;
    assert_eq!(people(&r, &room), Vec::<String>::new());
    assert_eq!(r.hub.store.room_stays(&room), vec![], "a revoke writes and keeps no room row");
    owner(&r, "unrevokePerson", json!({"personId": "sam"})).await.unwrap();
    r.hub.store.set_device_proven("sam-phone").unwrap();
    reconcile(r.hub.clone()).await;
    assert_eq!(people(&r, &room), vec!["sam".to_owned()], "the part was not done, so it goes on");
}

#[tokio::test]
async fn a_room_deleted_before_the_join_is_marked_done_with_no_join() {
    let r = Rig::new().await;
    flag_on(&r);
    let room = room_of(&r, &["house"]);
    let (invite, token) = accepted(&r, json!({"house": "member"}), std::slice::from_ref(&room)).await;
    owner(&r, "deleteBot", json!({"botId": room})).await.unwrap();
    prove(&r, "sam-phone", &invite, &token).await;
    assert_eq!(people(&r, &room), Vec::<String>::new());
    assert!(r.hub.store.invite_room_done(&invite, &room));
}

#[tokio::test]
async fn an_agent_added_to_the_room_after_the_invite_holds_the_join_until_the_person_has_it() {
    let r = Rig::new().await;
    flag_on(&r);
    let room = room_of(&r, &["house"]);
    let (invite, token) = accepted(&r, json!({"house": "member"}), std::slice::from_ref(&room)).await;
    let house = r.hub.store.bot(&room).unwrap().unwrap().config.members[0].clone();
    let money = r.owner_bot("money");
    owner(&r, "updateBot", json!({"id": room, "members": [house, money]})).await.unwrap();
    prove(&r, "sam-phone", &invite, &token).await;
    assert!(people(&r, &room).is_empty(), "rule 5: not granted every agent in the room");
    assert!(!r.hub.store.invite_room_done(&invite, &room), "the part waits");
    owner(&r, "setPersonAccess", json!({"personId": "sam", "agents": {"house": "member", "money": "member"}}))
        .await
        .unwrap();
    reconcile(r.hub.clone()).await;
    assert_eq!(people(&r, &room), vec!["sam".to_owned()]);
}

#[tokio::test]
async fn the_setting_off_at_the_join_leaves_the_part_waiting() {
    let r = Rig::new().await;
    flag_on(&r);
    let room = room_of(&r, &["house"]);
    let (invite, token) = accepted(&r, json!({"house": "member"}), std::slice::from_ref(&room)).await;
    r.hub.store.kv_set(FLAG, "off").unwrap();
    prove(&r, "sam-phone", &invite, &token).await;
    assert!(people(&r, &room).is_empty() && !r.hub.store.invite_room_done(&invite, &room));
    flag_on(&r);
    reconcile(r.hub.clone()).await;
    assert_eq!(people(&r, &room), vec!["sam".to_owned()]);
}

/// Kai adds the person by hand before they accept, then takes them out: the invite's waiting part is done by the
/// removal, so the proof later joins no one (the architect's `InviteJoin2` trace).
#[tokio::test]
async fn a_removal_before_the_proof_marks_the_waiting_part_done() {
    let r = Rig::new().await;
    flag_on(&r);
    let room = room_of(&r, &["house"]);
    let (invite, token) = accepted(&r, json!({"house": "member"}), std::slice::from_ref(&room)).await;
    owner(&r, "addRoomPerson", json!({"room": room, "person": "sam"})).await.unwrap();
    owner(&r, "removeRoomPerson", json!({"room": room, "person": "sam"})).await.unwrap();
    assert!(r.hub.store.invite_room_done(&invite, &room));
    prove(&r, "sam-phone", &invite, &token).await;
    assert_eq!(people(&r, &room), Vec::<String>::new());
}

/// Two invites for the same person, only the first accepted: the second, naming a room, is made before the first is accepted. Proving the first joins
/// no one into the second's room, and the list says the second isn't accepted.
#[tokio::test]
async fn an_unaccepted_invite_beside_an_accepted_one_waits_and_says_why() {
    let r = Rig::new().await;
    flag_on(&r);
    let room = room_of(&r, &["house"]);
    let first = owner(&r, "createInvite", json!({"name": "Sam", "agents": {"house": "member"}})).await.unwrap();
    let second = owner(&r, "createInvite", json!({"name": "Sam", "agents": {"house": "member"}, "rooms": [room]}))
        .await
        .unwrap();
    let (first_id, second_id) =
        (first["inviteId"].as_str().unwrap().to_owned(), second["inviteId"].as_str().unwrap().to_owned());
    let token = first["link"].as_str().unwrap().rsplit("t=").next().unwrap().to_owned();
    let listed = [Listed {
        membership_id: "mem_sam".into(),
        user_id: "user_sam".into(),
        invite_id: first_id.clone(),
        ..Default::default()
    }];
    members::apply(&r.hub, Some(&r.bin), &listed, Some("user_owner")).await.unwrap();
    prove(&r, "sam-phone", &first_id, &token).await;
    reconcile(r.hub.clone()).await;
    assert_eq!(people(&r, &room), Vec::<String>::new());
    assert!(!r.hub.store.invite_room_done(&second_id, &room));
    let listed = owner(&r, "invites", json!({})).await.unwrap();
    let row = listed["invites"].as_array().unwrap().iter().find(|i| i["inviteId"] == second_id.as_str()).unwrap();
    assert_eq!(row["rooms"][0]["waiting"], "not yet joined: the invite isn't accepted", "{row}");
}
