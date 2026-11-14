//! An invited person in a group room (rooms with people, slice B), through the real `dispatch` and `events_stream`
//! against a real kith-auth in a temp folder: what they are shown (from their stay on, never anything unstamped),
//! what they can do (read, post, react, leave), and that every one of those follows the live check.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::devices::{Caller, NotAllowed};
use super::dispatch;
use super::rig::Rig;
use crate::chat::room_people::FLAG;
use crate::remote::members::{self, Listed};
use crate::store::{Entry, EntryKind, Lane};
use futures::StreamExt;
use serde_json::{Value, json};

async fn call(r: &Rig, who: &Caller, method: &str, b: Value) -> anyhow::Result<Value> {
    dispatch(&r.hub, who, method, b).await
}

/// Sam, let in with `agents` (agent -> role).
async fn sam_with(r: &Rig, agents: Value) {
    let made = call(r, &Caller::Local, "createInvite", json!({"name": "Sam", "agents": agents})).await.unwrap();
    let listed = [Listed {
        membership_id: "mem_sam".into(),
        user_id: "user_sam".into(),
        invite_id: made["inviteId"].as_str().unwrap().to_owned(),
        ..Default::default()
    }];
    members::apply(&r.hub, Some(&r.bin), &listed, Some("user_owner")).await.unwrap();
}

fn room_of(r: &Rig, agents: &[&str]) -> String {
    let members: Vec<String> = agents.iter().map(|a| r.owner_bot(a)).collect();
    let cfg = serde_json::from_value(json!({"id": "", "kind": "group", "name": "Crew", "members": members})).unwrap();
    r.hub.create_bot(cfg).unwrap()["id"].as_str().unwrap().to_owned()
}

/// A rig with the setting on, Sam granted `house`, and a room of `house` he is not yet in.
async fn rig() -> (Rig, String, Caller) {
    let r = Rig::new().await;
    r.hub.store.kv_set(FLAG, "on").unwrap();
    sam_with(&r, json!({"house": "member"})).await;
    let room = room_of(&r, &["house"]);
    let phone = r.device("sam-phone", Some("sam"));
    (r, room, phone)
}

async fn add(r: &Rig, room: &str) -> i64 {
    let v = call(r, &Caller::Local, "addRoomPerson", json!({"room": room, "person": "sam"})).await.unwrap();
    v["gen"].as_i64().unwrap()
}

async fn remove(r: &Rig, room: &str) -> i64 {
    let v = call(r, &Caller::Local, "removeRoomPerson", json!({"room": room, "person": "sam"})).await.unwrap();
    v["gen"].as_i64().unwrap()
}

/// A line of the room stamped with `gen` (what a writer that names its gen leaves), by the user or an agent.
fn stamped(r: &Rig, room: &str, kind: EntryKind, text: &str, origin: i64) -> Entry {
    let data = json!({"text": text, "author": "agentbot", "final": true, "attachments": [{"path": "/secret/file"}]});
    r.hub.store.insert_entry_from_gen(&Lane::main(room), kind, 1, &data, 1, origin).unwrap()
}

/// A line of the room that never got a gen (what every writer but a person's post leaves until the room-person turns).
fn unstamped(r: &Rig, room: &str, kind: EntryKind, text: &str) -> Entry {
    r.hub.add_entry(&Lane::main(room), kind, 1, &json!({"text": text, "final": true})).unwrap()
}

async fn texts(r: &Rig, phone: &Caller, room: &str) -> Vec<String> {
    let v = call(r, phone, "history", json!({"botId": room})).await.unwrap();
    v["entries"].as_array().unwrap().iter().map(|e| e["data"]["text"].as_str().unwrap().to_owned()).collect()
}

fn denied(e: &anyhow::Error) -> bool {
    e.is::<NotAllowed>()
}

#[tokio::test]
async fn a_person_is_shown_the_room_from_their_stay_on_and_never_an_unstamped_line() {
    let (r, room, phone) = rig().await;
    let before = stamped(&r, &room, EntryKind::User, "before the join", 0);
    unstamped(&r, &room, EntryKind::Agent, "an agent reply nobody stamped");
    let joined = add(&r, &room).await;
    assert!(before.seq < joined);
    let at = stamped(&r, &room, EntryKind::Agent, "from a turn that started at the join", joined);
    stamped(&r, &room, EntryKind::Agent, "from a turn that started before the join", joined - 1);
    stamped(&r, &room, EntryKind::Agent, "a thought", joined + 5);
    // A tool line is never shown, whatever its gen.
    r.hub
        .store
        .insert_entry_from_gen(&Lane::main(&room), EntryKind::Thought, 1, &json!({"text": "thinking"}), 1, joined + 5)
        .unwrap();
    assert_eq!(texts(&r, &phone, &room).await, ["from a turn that started at the join", "a thought"]);

    // Fields outside the list stay home.
    let v = call(&r, &phone, "history", json!({"botId": room})).await.unwrap();
    let first = &v["entries"][0];
    assert_eq!(first["id"], at.id);
    assert!(first["data"]["attachments"].is_null(), "{first}");
    assert!(v["folder"].is_null() && v["conversation"].is_null(), "{v}");
}

#[tokio::test]
async fn kai_sees_everything_and_each_line_a_person_cannot_see_says_so() {
    let (r, room, _phone) = rig().await;
    stamped(&r, &room, EntryKind::User, "old", 0);
    let joined = add(&r, &room).await;
    stamped(&r, &room, EntryKind::User, "new", joined);
    let v = call(&r, &Caller::Local, "history", json!({"botId": room})).await.unwrap();
    let by_text = |t: &str| v["entries"].as_array().unwrap().iter().find(|e| e["data"]["text"] == t).unwrap().clone();
    assert_eq!(by_text("old")["data"]["hiddenFrom"], json!(["sam"]));
    assert!(by_text("new")["data"]["hiddenFrom"].is_null());
    assert!(
        v["entries"].as_array().unwrap().iter().any(|e| e["data"]["notice"] == "personJoined"),
        "the owner sees notices"
    );
}

#[tokio::test]
async fn a_post_is_stamped_with_the_gen_at_its_append_and_a_repeat_writes_one_line() {
    let (r, room, phone) = rig().await;
    let joined = add(&r, &room).await;
    let body = json!({"botId": room, "text": "hello", "clientNonce": "n1", "stay": joined});
    let first = call(&r, &phone, "send", body.clone()).await.unwrap();
    let again = call(&r, &phone, "send", body).await.unwrap();
    assert_eq!(first["entry"]["id"], again["entry"]["id"], "a repeat of the nonce is the same line");
    let id = first["entry"]["id"].as_str().unwrap();
    assert_eq!(r.hub.store.origin_gen(id), Some(joined), "the room's gen at the append");
    assert_eq!(first["entry"]["data"]["sender"], json!({"person": "sam", "name": "Sam"}));
    assert_eq!(texts(&r, &phone, &room).await, ["hello"]);
    // It opened one conversation, the person's own (PR C), and with no @-mention it starts no agent.
    let conv = r.hub.store.newest_conversation(&Lane::main(&room)).expect("the post opens a conversation");
    assert_eq!(conv.opened_by, "sam");
    // Someone else's nonce is not theirs to read back.
    let owner = call(&r, &Caller::Local, "history", json!({"botId": room})).await.unwrap();
    assert!(owner["entries"].as_array().unwrap().iter().any(|e| e["data"]["text"] == "hello"));
}

#[tokio::test]
async fn a_post_that_asks_for_more_than_text_is_refused_and_writes_nothing() {
    let (r, room, phone) = rig().await;
    let joined = add(&r, &room).await;
    for extra in [
        json!({"attachments": ["u1"]}),
        json!({"threadId": "x"}),
        json!({"newTask": true}),
        json!({"folder": "/tmp"}),
        json!({"project": "p"}),
        json!({"clear": true}),
        json!({"via": "call"}),
    ] {
        let mut b = json!({"botId": room, "text": "hi", "stay": joined});
        b.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        assert!(call(&r, &phone, "send", b).await.is_err(), "{extra}");
    }
    assert!(call(&r, &phone, "send", json!({"botId": room, "text": "   "})).await.is_err());
    assert!(call(&r, &phone, "send", json!({"botId": room, "text": "x".repeat(20_000)})).await.is_err());
    assert_eq!(texts(&r, &phone, &room).await, Vec::<String>::new());
}

#[tokio::test]
async fn nobody_outside_the_stay_reads_posts_or_reacts_and_a_leave_cuts_the_next_call() {
    let (r, room, phone) = rig().await;
    let body = json!({"botId": room, "text": "hi"});
    // Granted the agent but never let in.
    for (m, b) in
        [("history", json!({"botId": room})), ("send", body.clone()), ("thread", json!({"botId": room, "rootId": "x"}))]
    {
        assert!(denied(&call(&r, &phone, m, b).await.unwrap_err()), "{m} before joining");
    }
    assert_eq!(call(&r, &phone, "myRooms", json!({})).await.unwrap(), json!({"rooms": []}));

    let joined = add(&r, &room).await;
    let body = json!({"botId": room, "text": "hi", "stay": joined});
    let sent = call(&r, &phone, "send", body.clone()).await.unwrap();
    let id = sent["entry"]["id"].as_str().unwrap();
    assert_eq!(
        call(&r, &phone, "myRooms", json!({})).await.unwrap(),
        json!({"rooms": [{"id": room, "name": "Crew", "agents": ["house"], "stay": joined}]})
    );
    let reacted = call(&r, &phone, "react", json!({"entryId": id, "emoji": "👍"})).await.unwrap();
    assert_eq!(reacted["entry"]["data"]["reactions"], json!(["👍"]));

    assert_eq!(call(&r, &phone, "leaveRoom", json!({"room": room})).await.unwrap(), json!({"left": true}));
    for (m, b) in [
        ("history", json!({"botId": room})),
        ("send", body),
        ("react", json!({"entryId": id, "emoji": "🎉"})),
        ("leaveRoom", json!({"room": room})),
    ] {
        assert!(denied(&call(&r, &phone, m, b).await.unwrap_err()), "{m} after leaving");
    }
    assert_eq!(call(&r, &phone, "myRooms", json!({})).await.unwrap(), json!({"rooms": []}));
}

#[tokio::test]
async fn a_room_is_that_room_alone_another_room_with_the_same_agent_is_not_theirs() {
    let (r, room, phone) = rig().await;
    let other = room_of(&r, &["house"]);
    add(&r, &room).await;
    stamped(&r, &other, EntryKind::User, "elsewhere", 10_000);
    assert!(denied(&call(&r, &phone, "history", json!({"botId": other})).await.unwrap_err()));
    assert!(denied(&call(&r, &phone, "send", json!({"botId": other, "text": "hi"})).await.unwrap_err()));
    // Nor the owner's own chat of the agent, which the guard treats as it always did.
    let bot = r.owner_bot("house");
    assert!(denied(&call(&r, &phone, "history", json!({"botId": bot})).await.unwrap_err()));
}

#[tokio::test]
async fn a_removed_person_who_is_added_again_sees_only_what_came_after_the_re_add() {
    let (r, room, phone) = rig().await;
    let first = add(&r, &room).await;
    call(&r, &phone, "send", json!({"botId": room, "text": "first stay", "stay": first})).await.unwrap();
    stamped(&r, &room, EntryKind::Agent, "reply in the first stay", first);
    let left = remove(&r, &room).await;
    stamped(&r, &room, EntryKind::User, "while out", left);
    assert!(denied(&call(&r, &phone, "history", json!({"botId": room})).await.unwrap_err()));
    let again = add(&r, &room).await;
    assert!(again > left);
    assert_eq!(texts(&r, &phone, &room).await, Vec::<String>::new(), "a new stay starts empty");
    stamped(&r, &room, EntryKind::Agent, "after the re-add", again);
    assert_eq!(texts(&r, &phone, &room).await, ["after the re-add"]);
}

#[tokio::test]
async fn a_revoke_and_its_undo_cut_and_give_back_the_room_without_touching_a_row() {
    let (r, room, phone) = rig().await;
    let joined = add(&r, &room).await;
    stamped(&r, &room, EntryKind::User, "seen", joined);
    let rows = r.hub.store.room_stays(&room);
    call(&r, &Caller::Local, "revokePerson", json!({"personId": "sam"})).await.unwrap();
    assert!(call(&r, &phone, "history", json!({"botId": room})).await.is_err());
    assert!(call(&r, &phone, "send", json!({"botId": room, "text": "hi"})).await.is_err());
    assert_eq!(r.hub.store.room_stays(&room), rows);
    call(&r, &Caller::Local, "unrevokePerson", json!({"personId": "sam"})).await.unwrap();
    assert_eq!(texts(&r, &phone, &room).await, ["seen"]);
}

#[tokio::test]
async fn an_agent_taken_from_a_person_closes_the_room_for_them_but_they_can_still_leave() {
    let r = Rig::new().await;
    r.hub.store.kv_set(FLAG, "on").unwrap();
    sam_with(&r, json!({"house": "member", "money": "member"})).await;
    let room = room_of(&r, &["house", "money"]);
    let phone = r.device("sam-phone", Some("sam"));
    let joined = add(&r, &room).await;
    stamped(&r, &room, EntryKind::User, "seen", joined);
    assert_eq!(texts(&r, &phone, &room).await, ["seen"]);
    call(&r, &Caller::Local, "setPersonAccess", json!({"personId": "sam", "agents": {"house": "member"}}))
        .await
        .unwrap();
    assert!(denied(&call(&r, &phone, "history", json!({"botId": room})).await.unwrap_err()));
    assert!(denied(&call(&r, &phone, "send", json!({"botId": room, "text": "hi"})).await.unwrap_err()));
    assert_eq!(call(&r, &phone, "myRooms", json!({})).await.unwrap(), json!({"rooms": []}));
    assert_eq!(call(&r, &phone, "leaveRoom", json!({"room": room})).await.unwrap(), json!({"left": true}));
}

#[tokio::test]
async fn a_post_checked_before_a_removal_or_a_re_add_does_not_land_after_it() {
    let (r, room, _phone) = rig().await;
    add(&r, &room).await;
    let seat = crate::chat::room_people::live_seat(&r.hub.store, "sam", &room).await.unwrap();
    let data = json!({"text": "late", "sender": {"person": "sam", "name": "Sam"}});
    // The removal comes between the check and the append.
    remove(&r, &room).await;
    let e = r.hub.commit_room_post(&room, "sam", &seat, "", &data).unwrap_err();
    assert_eq!(e.downcast_ref::<crate::hub::PostRefused>(), Some(&crate::hub::PostRefused::NotInRoom));
    // So does a re-add: the stay is a new one, with its own joined gen (R4).
    add(&r, &room).await;
    let e = r.hub.commit_room_post(&room, "sam", &seat, "", &data).unwrap_err();
    assert_eq!(e.downcast_ref::<crate::hub::PostRefused>(), Some(&crate::hub::PostRefused::NotInRoom));
    let fresh = crate::chat::room_people::live_seat(&r.hub.store, "sam", &room).await.unwrap();
    r.hub.commit_room_post(&room, "sam", &fresh, "", &data).unwrap();
    // The agents of the room moved under the check: the post is refused, not written against the old set.
    let money = r.owner_bot("money");
    let house = r.hub.store.bot(&room).unwrap().unwrap().config.members[0].clone();
    let both = json!({"personId": "sam", "agents": {"house": "member", "money": "member"}});
    call(&r, &Caller::Local, "setPersonAccess", both).await.unwrap();
    call(&r, &Caller::Local, "updateBot", json!({"id": room, "members": [house, money]})).await.unwrap();
    let e = r.hub.commit_room_post(&room, "sam", &fresh, "", &data).unwrap_err();
    assert_eq!(e.downcast_ref::<crate::hub::PostRefused>(), Some(&crate::hub::PostRefused::RoomChanged));
}

#[tokio::test]
async fn a_client_cannot_name_itself_a_room_person() {
    let (r, room, _phone) = rig().await;
    add(&r, &room).await;
    unstamped(&r, &room, EntryKind::Agent, "owner level");
    // The owner naming itself Sam still gets the owner's whole view: the name is dropped before anything reads it.
    let v = call(&r, &Caller::Local, "history", json!({"botId": room, "asRoomPerson": "sam"})).await.unwrap();
    let all: Vec<&str> = v["entries"].as_array().unwrap().iter().filter_map(|e| e["data"]["text"].as_str()).collect();
    assert_eq!(all, ["Sam joined · sees this room from now on", "owner level"]);
    assert!(v.get("folder").is_some(), "the owner's answer, with its folder: {v}");
}

#[tokio::test]
async fn events_and_the_catch_up_show_a_person_the_same_lines_history_does() {
    let (r, room, phone) = rig().await;
    stamped(&r, &room, EntryKind::User, "before", 0);
    let joined = add(&r, &room).await;
    let mine = stamped(&r, &room, EntryKind::User, "after", joined);
    let tool = r
        .hub
        .store
        .insert_entry_from_gen(&Lane::main(&room), EntryKind::Thought, 1, &json!({"text": "t"}), 1, joined)
        .unwrap();
    let bare = unstamped(&r, &room, EntryKind::Agent, "unstamped");

    // Catch-up on a fresh connection.
    let mut stream = Box::pin(super::events::events_stream(&r.hub, 0, None, &phone).unwrap());
    let mut shown: Vec<String> = vec![];
    while let Ok(Some(v)) = tokio::time::timeout(std::time::Duration::from_millis(200), stream.next()).await {
        if v["type"] == "entry" {
            shown.push(v["entry"]["data"]["text"].as_str().unwrap_or_default().to_owned());
        }
        assert!(v["type"] != "bot" && v["type"] != "conversation", "the room row is the owner's: {v}");
    }
    assert_eq!(shown, ["after"]);

    // A live event for each, judged by the same rule; the owner's copy of the first is marked.
    let ev = |e: &Entry| json!({"type": "entry", "rev": e.rev, "entry": e});
    let store = &r.hub.store;
    assert!(super::roles::event_for(store, Some("sam"), ev(&mine)).is_some());
    assert!(super::roles::event_for(store, Some("sam"), ev(&tool)).is_none(), "a tool line is never shown");
    assert!(super::roles::event_for(store, Some("sam"), ev(&bare)).is_none(), "unstamped is hidden");
    let owner = super::roles::event_for(store, None, ev(&bare)).unwrap();
    assert_eq!(owner["entry"]["data"]["hiddenFrom"], json!(["sam"]));
    // After the leave nothing reaches them, new or old.
    remove(&r, &room).await;
    assert!(super::roles::event_for(store, Some("sam"), ev(&mine)).is_none());
}

#[tokio::test]
async fn the_owner_calls_a_person_only_method_and_gets_nothing() {
    let (r, room, _phone) = rig().await;
    assert!(call(&r, &Caller::Local, "myRooms", json!({})).await.is_err());
    assert!(call(&r, &Caller::Local, "leaveRoom", json!({"room": room})).await.is_err());
}

#[tokio::test]
async fn a_post_from_an_earlier_stay_is_refused_after_a_re_add() {
    let (r, room, phone) = rig().await;
    let first = add(&r, &room).await;
    remove(&r, &room).await;
    let second = add(&r, &room).await;
    let body = |stay: i64| json!({"botId": room, "text": "queued", "stay": stay});
    assert!(denied(&call(&r, &phone, "send", body(first)).await.unwrap_err()), "the first stay's post");
    assert!(call(&r, &phone, "send", json!({"botId": room, "text": "no stay"})).await.is_err());
    assert!(call(&r, &phone, "send", body(second)).await.is_ok());
    let rooms = call(&r, &phone, "myRooms", json!({})).await.unwrap();
    assert_eq!(rooms["rooms"][0]["stay"], second);
}

#[tokio::test]
async fn a_revoke_recorded_between_the_check_and_the_append_refuses_the_post() {
    let (r, room, _phone) = rig().await;
    add(&r, &room).await;
    let seat = crate::chat::room_people::live_seat(&r.hub.store, "sam", &room).await.unwrap();
    call(&r, &Caller::Local, "revokePerson", json!({"personId": "sam"})).await.unwrap();
    let data = json!({"text": "late", "sender": {"person": "sam", "name": "Sam"}});
    let e = r.hub.commit_room_post(&room, "sam", &seat, "", &data).unwrap_err();
    assert_eq!(e.downcast_ref::<crate::hub::PostRefused>(), Some(&crate::hub::PostRefused::NotInRoom));
}

#[tokio::test]
async fn a_message_id_is_shown_only_to_its_sender() {
    let (r, room, phone) = rig().await;
    let joined = add(&r, &room).await;
    let body = json!({"botId": room, "text": "a", "clientNonce": "n", "stay": joined});
    let sent = call(&r, &phone, "send", body).await.unwrap();
    assert_eq!(sent["entry"]["data"]["clientNonce"], "n");
    let other = stamped(&r, &room, EntryKind::User, "from kai", joined);
    let mut data = other.data.clone();
    data["clientNonce"] = "k".into();
    r.hub.set_entry(&other.id, &data).unwrap();
    let v = call(&r, &phone, "history", json!({"botId": room})).await.unwrap();
    let kai = v["entries"].as_array().unwrap().iter().find(|e| e["data"]["text"] == "from kai").unwrap();
    assert!(kai["data"]["clientNonce"].is_null(), "{kai}");
}
