//! Rooms with people on a switched Mac after a rename (R-rename, design 2026-11-16-rooms-rename, `RoomsRename.tla`),
//! end to end: the rig's auth.json is stepped for real into a core daemon, the opt-in (`KITH_CORE_AUTHORITY`) is left
//! off as the runbook leaves it, and the core renames sam to samuel while the host keeps its rows under `sam`. A person
//! is one identity across every id the core lists for them: Kai's add by the new id finds the stay under the old one,
//! Kai's removal by the new id ends it and keeps a later prove out, a device under either id sees and leaves the same
//! stay, and a revoke by the new id stops the room turn whose basis names the old one.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::access_core_tests::Core;
use super::devices::Caller;
use super::dispatch;
use super::rig::Rig;
use crate::chat::room_people::{self, FLAG};
use crate::chat::room_turn::room_person_id;
use crate::remote::members::{self, Listed};
use crate::store::Lane;
use serde_json::{Value, json};

async fn call(r: &Rig, caller: &Caller, method: &str, b: Value) -> anyhow::Result<Value> {
    dispatch(&r.hub, caller, method, b).await
}

async fn owner(r: &Rig, method: &str, b: Value) -> anyhow::Result<Value> {
    call(r, &Caller::Local, method, b).await
}

/// A group room with the owner's `house` bot in it: the room's id and the bot's.
fn room_of(r: &Rig) -> (String, String) {
    let bot = r.owner_bot("house");
    let cfg = serde_json::from_value(json!({"id": "", "kind": "group", "name": "Crew", "members": [bot]})).unwrap();
    (r.hub.create_bot(cfg).unwrap()["id"].as_str().unwrap().to_owned(), bot)
}

/// On the file (before the step): Sam invited to `house` with a part for `room`, accepted but proven by no device yet.
/// Then the real step, the opt-in off, and the core renames sam to samuel. Answers the invite id and its token.
async fn renamed_after_the_step(r: &Rig, room: &str) -> (Core, String, String) {
    r.hub.store.kv_set(FLAG, "on").unwrap();
    let made =
        owner(r, "createInvite", json!({"name": "Sam", "agents": {"house": "member"}, "rooms": [room]})).await.unwrap();
    let invite = made["inviteId"].as_str().unwrap().to_owned();
    let token = made["link"].as_str().unwrap().rsplit("t=").next().unwrap().to_owned();
    let listed = [Listed {
        membership_id: "mem_sam".into(),
        user_id: "user_sam".into(),
        invite_id: invite.clone(),
        ..Default::default()
    }];
    members::apply(&r.hub, Some(&r.bin), &listed, Some("user_owner")).await.unwrap();
    let core = Core::start_stepped(r);
    // The runbook's configuration: switched by the tombstone, the opt-in off.
    super::authority::TEST_OPT_IN.store(false, std::sync::atomic::Ordering::SeqCst);
    assert!(super::authority::switched_now() && !super::authority::opted_in());
    core.ask("people.renamePerson", json!({"id": "sam", "to": "samuel"})).expect("the core renames");
    assert_eq!(crate::remote::core_revokes::held_under("samuel").await, ["samuel", "sam"]);
    (core, invite, token)
}

fn open(r: &Rig, room: &str) -> Vec<String> {
    r.hub.store.room_open_people(room)
}

#[tokio::test]
async fn kais_add_and_removal_by_the_new_id_act_on_the_one_stay_and_a_later_prove_does_not_re_join() {
    let r = Rig::new().await;
    let (room, _) = room_of(&r);
    let (_core, invite, token) = renamed_after_the_step(&r, &room).await;
    assert!(room_people::enabled(&r.hub.store), "rooms with people are on on a switched Mac");

    // Kai adds the person the app shows: the stay is written under the first id, and a second add under either id is a
    // repeat (one open stay per person).
    let added = owner(&r, "addRoomPerson", json!({"room": room, "person": "samuel"})).await.unwrap();
    assert_eq!(added["changed"], true, "{added}");
    assert_eq!(open(&r, &room), ["sam"], "under the first id");
    for again in ["samuel", "sam"] {
        let repeat = owner(&r, "addRoomPerson", json!({"room": room, "person": again})).await.unwrap();
        assert_eq!(repeat["changed"], false, "{again}: {repeat}");
    }
    assert!(room_people::room_member_may(&r.hub.store, "samuel", &room).await, "asked by the new id");
    assert!(room_people::room_member_may(&r.hub.store, "sam", &room).await, "asked by the old id");

    // Kai's removal by the new id ends the stay under the old one, and marks the invite's part (under the old id) done.
    let removed = owner(&r, "removeRoomPerson", json!({"room": room, "person": "samuel"})).await.unwrap();
    assert_eq!(removed["changed"], true, "{removed}");
    assert!(open(&r, &room).is_empty(), "removing samuel closed sam's stay");
    assert!(r.hub.store.invite_room_done(&invite, &room), "the waiting part is done");
    assert!(!room_people::room_member_may(&r.hub.store, "sam", &room).await);

    // A device of theirs proves the invite later: the done mark keeps Kai's removal.
    let phone = r.unproven_device("sam-phone", Some("sam"));
    call(&r, &phone, "proveInvite", json!({"inviteId": invite, "token": token})).await.unwrap();
    crate::chat::invite_rooms::reconcile(r.hub.clone()).await;
    assert!(open(&r, &room).is_empty(), "a later prove doesn't re-join");
    assert_eq!(r.hub.store.room_stays(&room).len(), 1, "no second stay");
}

#[tokio::test]
async fn a_device_under_the_new_id_sees_and_leaves_the_stay_under_the_old_one() {
    let r = Rig::new().await;
    let (room, _) = room_of(&r);
    let (_core, _, _) = renamed_after_the_step(&r, &room).await;
    owner(&r, "addRoomPerson", json!({"room": room, "person": "samuel"})).await.unwrap();
    // A device paired after the rename is held under the new id; the stay is under the old one.
    let mac = r.device("samuel-mac", Some("samuel"));
    let mine = call(&r, &mac, "myRooms", json!({})).await.unwrap();
    let stay = r.hub.store.room_open_stay(&room, &["sam".to_owned()]).unwrap().1;
    assert_eq!(mine["rooms"][0]["id"], room, "{mine}");
    assert_eq!(mine["rooms"][0]["stay"], stay);
    let left = call(&r, &mac, "leaveRoom", json!({"room": room})).await.unwrap();
    assert_eq!(left["left"], true, "{left}");
    assert!(open(&r, &room).is_empty(), "a member's leave by the new id ends the old stay");
    assert!(call(&r, &mac, "leaveRoom", json!({"room": room})).await.is_err(), "nothing left to leave");
}

#[tokio::test]
async fn a_revoke_by_the_new_id_stops_the_room_turn_whose_basis_names_the_old_one() {
    let r = Rig::new().await;
    let (room, bot) = room_of(&r);
    let (_core, _, _) = renamed_after_the_step(&r, &room).await;
    let added = owner(&r, "addRoomPerson", json!({"room": room, "person": "samuel"})).await.unwrap();
    let joined = added["gen"].as_i64().unwrap();
    // The room turn running for the people in the room: its basis names the stay's id, `sam`.
    let key = crate::agent::bot::actor_key_as(&bot, &Lane::main(&room), Some(&room_person_id(&room, joined)));
    r.hub.room_turn_begins(&key, &["sam".into()]);
    owner(&r, "revokePerson", json!({"personId": "samuel"})).await.unwrap();
    assert!(!r.hub.room_turn_registered(&key), "the revoke stopped the room turn");
    assert!(!room_people::room_member_may(&r.hub.store, "sam", &room).await, "and the old id may not read the room");
    assert_eq!(open(&r, &room), ["sam"], "a revoke writes no room row: an undo gives the stay back");
}
