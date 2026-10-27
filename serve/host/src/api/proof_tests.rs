//! The invite proof, the run gate, answer shaping and what the host takes back, against a real kith-auth in a temp folder.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::devices::{Caller, NotAllowed};
use super::rig::Rig;
use super::{dispatch, events_stream, roles};
use crate::remote::members::{self, Listed};
use serde_json::{Value, json};

async fn call(r: &Rig, caller: &Caller, method: &str, b: Value) -> anyhow::Result<Value> {
    dispatch(&r.hub, caller, method, b).await
}

fn denied(e: &anyhow::Error) -> bool {
    e.is::<NotAllowed>() || e.is::<super::devices::Forbidden>()
}

async fn joined(r: &Rig, roles: Value) -> Caller {
    let made = call(r, &Caller::Local, "createInvite", json!({"name": "Sam", "agents": roles})).await.unwrap();
    let listed = [Listed {
        membership_id: "mem_sam".into(),
        user_id: "user_sam".into(),
        invite_id: made["inviteId"].as_str().unwrap().to_owned(),
        ..Default::default()
    }];
    members::apply(&r.hub, Some(&r.bin), &listed, Some("user_owner")).await.unwrap();
    r.device("sam-phone", Some("sam"))
}

#[tokio::test]
async fn an_unproven_device_can_only_prove_and_only_a_few_times() {
    let r = Rig::new().await;
    let made =
        call(&r, &Caller::Local, "createInvite", json!({"name": "Sam", "agents": {"house": "member"}})).await.unwrap();
    let invite = made["inviteId"].as_str().unwrap().to_owned();
    let t = made["link"].as_str().unwrap().rsplit("t=").next().unwrap().to_owned();
    let listed = [Listed {
        membership_id: "mem_sam".into(),
        user_id: "user_sam".into(),
        invite_id: invite.clone(),
        ..Default::default()
    }];
    members::apply(&r.hub, Some(&r.bin), &listed, Some("user_owner")).await.unwrap();
    let phone = r.unproven_device("new-phone", Some("sam"));
    for m in ["hello", "history", "startChat", "linkCode", "people", "settings"] {
        let e = call(&r, &phone, m, json!({})).await.expect_err(m);
        assert!(denied(&e), "{m}: {e:#}");
    }
    assert!(events_stream(&r.hub, 0, None, &phone).is_err(), "no event stream either");
    for _ in 0..5 {
        let e = call(&r, &phone, "proveInvite", json!({"inviteId": invite, "token": "wrong"})).await.unwrap_err();
        assert!(e.is::<NotAllowed>());
    }
    let e = call(&r, &phone, "proveInvite", json!({"inviteId": invite, "token": t})).await.unwrap_err();
    assert!(format!("{e:#}").contains("Too many"), "the right token is refused once the tries are used: {e:#}");
    let rows = r.hub.store.audit_list(0, Some("sam")).unwrap();
    assert!(rows.iter().filter(|a| a.method == "proveInvite" && !a.allowed).count() >= 5);
    // Another device has its own tries, and an invite id that isn't theirs fails.
    let other = r.unproven_device("other-phone", Some("sam"));
    assert!(call(&r, &other, "proveInvite", json!({"inviteId": "inv_nope", "token": t})).await.is_err());
    call(&r, &other, "proveInvite", json!({"inviteId": invite, "token": t})).await.unwrap();
    assert!(call(&r, &other, "proveInvite", json!({"inviteId": invite, "token": t})).await.is_err(), "once is enough");
}

#[tokio::test]
async fn permission_prompts_go_to_the_owner_for_a_guest_and_to_a_member_for_their_own_run() {
    let r = Rig::new().await;
    r.owner_bot("house");
    let sam = joined(&r, json!({"house": "member"})).await;
    let chat =
        call(&r, &sam, "startChat", json!({"agent": "house"})).await.unwrap()["bot"]["id"].as_str().unwrap().to_owned();
    let lane = crate::store::Lane::main(&chat);
    let card = |kind| {
        r.hub.add_entry(&lane, kind, 1, &json!({"title": "Run ls", "status": "pending", "author": chat})).unwrap()
    };
    let ask = card(crate::store::EntryKind::Permission);
    let said = card(crate::store::EntryKind::Agent);
    let guard = |m: &'static str, b: Value| {
        let hub = r.hub.clone();
        async move { roles::guard(&hub, "sam", true, m, &b).await }
    };

    // Running is open to both roles; a member answers their own run's prompt, and only a prompt.
    for m in ["send", "newSession"] {
        guard(m, json!({"botId": chat, "text": "hi"})).await.expect(m);
    }
    guard("respondPermission", json!({"entryId": ask.id})).await.expect("a member answers their own prompt");
    let e = guard("respondPermission", json!({"entryId": said.id})).await.unwrap_err();
    assert!(e.is::<NotAllowed>(), "{e:#}");
    // Someone else's chat is never theirs to answer.
    let other = r.owner_bot("house");
    let theirs =
        r.hub.add_entry(&crate::store::Lane::main(&other), crate::store::EntryKind::Permission, 1, &json!({})).unwrap();
    assert!(guard("respondPermission", json!({"entryId": theirs.id})).await.unwrap_err().is::<NotAllowed>());

    // The owner sees the prompt (and nothing else of the chat), live and on catch-up.
    let for_owner = |e: &crate::store::Entry| {
        roles::event_for(&r.hub.store, None, json!({"type": "entry", "rev": e.rev, "entry": e}))
    };
    assert!(for_owner(&ask).is_some(), "the owner's devices get the prompt");
    assert!(for_owner(&said).is_none(), "but not the rest of the invitee's chat");

    // The same person as a guest: sending still works, the answer is the owner's.
    r.auth(&["member", "house", "sam", "preset=guest"]);
    guard("send", json!({"botId": chat, "text": "hi"})).await.expect("a guest chats");
    let e = guard("respondPermission", json!({"entryId": ask.id})).await.unwrap_err();
    assert!(e.is::<NotAllowed>(), "{e:#}");
    // The owner can always answer: the guard is for invitees only, the owner's calls never reach it.
    assert!(roles::approves_own("sam", "house", "Edit", "edit").await.eq(&false));
    call(&r, &sam, "history", json!({"botId": chat})).await.unwrap();
    call(&r, &sam, "stop", json!({"botId": chat})).await.unwrap();
}

#[tokio::test]
async fn answers_leave_out_this_computers_folders() {
    let r = Rig::new().await;
    let shaped = roles::shape(&r.hub, "sam", "history", json!({"entries": [], "folder": {"path": "/Users/x"}}));
    assert_eq!(shaped, json!({"entries": []}));
    let info = json!({"info": {"name": "house", "description": "d", "avatar": "a", "memory": {"dir": "/Users/x"}},
        "fetchedAt": 5, "error": "/Users/x"});
    assert_eq!(
        roles::shape(&r.hub, "sam", "kithInfo", info),
        json!({"info": {"name": "house", "description": "d", "avatar": "a"}, "fetchedAt": 5})
    );
}

#[tokio::test]
async fn person_ids_are_checked_and_only_our_own_grants_are_taken_back() {
    let r = Rig::new().await;
    for bad in ["-x", "Sam", "a b", "1x"] {
        let e = call(&r, &Caller::Local, "createInvite", json!({"personId": bad, "agents": {"house": "guest"}}))
            .await
            .unwrap_err();
        assert!(format!("{e:#}").contains("isn't a person id"), "{bad}: {e:#}");
    }
    // A person the owner already had, with a membership the owner gave by hand.
    r.auth(&["person", "kim"]);
    r.auth(&["member", "money", "kim", "preset=member"]);
    let both = json!({"personId": "kim", "agents": {"house": "guest", "money": "guest"}});
    call(&r, &Caller::Local, "createInvite", both).await.unwrap();
    assert_eq!(r.hub.store.person("kim").unwrap().made, ["house"], "money was already theirs");
    let only = |a: Value| json!({"personId": "kim", "agents": a});
    call(&r, &Caller::Local, "setPersonAccess", only(json!({"money": "guest"}))).await.unwrap();
    assert!(!r.allowed("kim", "house"), "the one this host made goes");
    call(&r, &Caller::Local, "setPersonAccess", only(json!({}))).await.unwrap();
    assert!(r.allowed("kim", "money"), "the owner's own membership is left in place");
    // A revoke beats the owner's own rows too (R2), and keeps them for the undo (R4).
    call(&r, &Caller::Local, "revokePerson", json!({"personId": "kim"})).await.unwrap();
    assert!(!r.allowed("kim", "money"));
    call(&r, &Caller::Local, "unrevokePerson", json!({"personId": "kim"})).await.unwrap();
    assert!(r.allowed("kim", "money"));
}

#[tokio::test]
async fn a_deleted_template_makes_no_chat_and_a_second_account_keeps_the_handle() {
    let r = Rig::new().await;
    let owner_bot = r.owner_bot("house");
    let sam = joined(&r, json!({"house": "member"})).await;
    r.hub.delete_bot(&owner_bot).unwrap();
    let e = call(&r, &sam, "startChat", json!({"agent": "house"})).await.unwrap_err();
    assert!(format!("{e:#}").starts_with("Something went wrong on the host."), "{e:#}");

    // Two accounts on one person: losing one leaves the other bound.
    let made = call(&r, &Caller::Local, "createInvite", json!({"personId": "sam", "agents": {"house": "member"}}))
        .await
        .unwrap();
    let second = Listed {
        membership_id: "mem_two".into(),
        user_id: "user_two".into(),
        invite_id: made["inviteId"].as_str().unwrap().to_owned(),
        ..Default::default()
    };
    let first = Listed {
        membership_id: "mem_sam".into(),
        user_id: "user_sam".into(),
        invite_id: "inv_x".into(),
        ..Default::default()
    };
    let bin = Some(r.bin.as_path());
    members::apply(&r.hub, bin, &[first, second.clone()], Some("user_owner")).await.unwrap();
    assert_eq!(r.who("user_two")["person"], "sam");
    members::apply(&r.hub, bin, &[second], Some("user_owner")).await.unwrap();
    assert_eq!(r.who("user_two")["person"], "sam", "the remaining account stays bound");
    assert!(r.hub.store.device("sam-phone").is_some(), "the person's devices stay while one account remains");
}
