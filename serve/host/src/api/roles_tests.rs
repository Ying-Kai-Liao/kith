//! Invited people against a real kith-auth in a temp folder, a temp store and a fake cloud: who a caller is, what is
//! denied by default, per-person filtering, inviting, binding memberships, revoking and the audit log.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::devices::{Caller, NotAllowed, Principal, principal};
use super::rig::Rig;
use super::{dispatch, events_stream, roles};
use crate::remote::cloud;
use crate::remote::crypto::{self, b64};
use crate::remote::members::{self, Listed};
use crate::store::{Invite, Scope, now_ms};
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

async fn call(r: &Rig, caller: &Caller, method: &str, b: Value) -> anyhow::Result<Value> {
    dispatch(&r.hub, caller, method, b).await
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |s, b| s + &format!("{b:02x}"))
}

fn denied(e: &anyhow::Error) -> bool {
    e.is::<NotAllowed>() || e.is::<super::devices::Forbidden>()
}

/// `sam` made by an invite, bound to `user_sam`, with `house` and one phone.
async fn joined(r: &Rig, roles: Value) -> (Caller, String) {
    let made = call(r, &Caller::Local, "createInvite", json!({"name": "Sam", "agents": roles})).await.unwrap();
    let invite = made["inviteId"].as_str().unwrap().to_owned();
    let listed = [Listed {
        membership_id: "mem_sam".into(),
        user_id: "user_sam".into(),
        invite_id: invite,
        ..Default::default()
    }];
    members::apply(&r.hub, Some(&r.bin), &listed, Some("user_owner")).await.unwrap();
    (r.device("sam-phone", Some("sam")), "mem_sam".into())
}

async fn events(r: &Rig, caller: &Caller) -> Vec<Value> {
    let s = events_stream(&r.hub, 0, None, caller).unwrap();
    Box::pin(s).take_until(tokio::time::sleep(Duration::from_millis(300))).collect().await
}

#[tokio::test]
async fn a_caller_is_the_owner_or_a_person() {
    let r = Rig::new().await;
    r.auth(&["person", "sam"]);
    let owner = r.device("owner-phone", None);
    let sam = r.device("sam-phone", Some("sam"));
    let ghost = Caller::Device { key: "ghost".into(), scopes: vec![Scope::Control] };
    let store = &r.hub.store;
    assert_eq!(principal(store, &Caller::Local), Principal::Owner);
    assert_eq!(principal(store, &owner), Principal::Owner, "old rows and the owner's devices have no person");
    assert_eq!(principal(store, &sam), Principal::Member("sam".into()));
    assert_eq!(principal(store, &ghost), Principal::Nobody);
    assert_eq!(principal(store, &Caller::Pairing { key: "p".into() }), Principal::Nobody);

    // linkCode is for the caller's own person.
    let code_of = |v: &Value| v["code"].as_str().unwrap().to_owned();
    let sam_code = code_of(&call(&r, &sam, "linkCode", json!({})).await.unwrap());
    let code = code_of(&call(&r, &owner, "linkCode", json!({})).await.unwrap());
    let file: Value = serde_json::from_str(&std::fs::read_to_string(r.dir.join("auth.json")).unwrap()).unwrap();
    assert_eq!(file["invites"][sam_code]["person"], "sam", "an invitee's code is for their own person");
    assert_eq!(file["invites"][code]["person"], "kai", "the owner's code is the owner's");
    assert!(call(&r, &ghost, "hello", json!({})).await.is_err());
}

#[tokio::test]
async fn schedule_messages_are_on_until_the_person_turns_their_own_off() {
    let r = Rig::new().await;
    r.auth(&["person", "sam"]);
    let owner = r.device("owner-phone", None);
    let sam = r.device("sam-phone", Some("sam"));
    let on = |v: anyhow::Result<Value>| v.unwrap()["on"].as_bool().unwrap();
    assert!(on(call(&r, &sam, "scheduleMessages", json!({})).await));
    assert!(!on(call(&r, &sam, "scheduleMessages", json!({"on": false})).await));
    assert!(!on(call(&r, &sam, "scheduleMessages", json!({})).await), "it stays off");
    assert!(on(call(&r, &owner, "scheduleMessages", json!({})).await), "sam's switch isn't the owner's");
    assert!(!crate::run_outbox::schedule_messages(&r.hub.store, Some("sam")));
    assert!(crate::run_outbox::schedule_messages(&r.hub.store, None));
    assert!(on(call(&r, &sam, "scheduleMessages", json!({"on": true})).await));
    let ghost = Caller::Device { key: "ghost".into(), scopes: vec![Scope::Control] };
    assert!(call(&r, &ghost, "scheduleMessages", json!({"on": false})).await.is_err());
}

/// Every method name the router has an arm for, read from the api sources.
pub(super) fn router_methods() -> BTreeSet<String> {
    let sources = [
        include_str!("bots.rs"),
        include_str!("calls.rs"),
        include_str!("cards.rs"),
        include_str!("host.rs"),
        include_str!("folders.rs"),
        include_str!("backups.rs"),
        include_str!("kith_info.rs"),
        include_str!("marketplace.rs"),
        include_str!("remote.rs"),
        include_str!("people.rs"),
        include_str!("botguests.rs"),
        include_str!("peers.rs"),
        include_str!("threads.rs"),
    ];
    let mut out = BTreeSet::new();
    for src in sources {
        let mut open = false;
        for line in src.lines().map(str::trim) {
            let starts = line.starts_with('"') || (open && line.starts_with("| \""));
            if !starts {
                open = false;
                continue;
            }
            let head = line.split("=>").next().unwrap_or(line);
            let arm = line.contains("=>");
            if !arm && !head.trim_end().ends_with('|') {
                open = false;
                continue;
            }
            out.extend(
                head.split('"')
                    .skip(1)
                    .step_by(2)
                    .filter(|t| !t.is_empty() && t.chars().all(|c| c.is_ascii_alphanumeric()))
                    .map(str::to_owned),
            );
            open = !arm;
        }
    }
    out.extend(crate::chat::memory::HQ_METHODS.iter().map(|m| (*m).to_owned()));
    out.extend(crate::chat::topics::METHODS.iter().map(|m| (*m).to_owned()));
    out.insert("readFile".into());
    out
}

#[tokio::test]
async fn a_member_cannot_make_rename_or_unfold_a_topic() {
    let r = Rig::new().await;
    r.auth(&["person", "sam"]);
    let sam = r.device("sam-phone", Some("sam"));
    for m in crate::chat::topics::METHODS {
        let e = call(&r, &sam, m, json!({"botId": "house", "entryId": "e", "title": "t"})).await.unwrap_err();
        assert!(denied(&e), "{m}: {e:#}");
    }
}

#[tokio::test]
async fn every_router_method_is_denied_to_members_unless_listed() {
    let r = Rig::new().await;
    r.auth(&["person", "sam"]);
    let sam = r.device("sam-phone", Some("sam"));
    let known = router_methods();
    assert!(known.len() > 100, "the walk found {} methods: did the router's layout change?", known.len());
    for m in ["createBot", "updateBot", "deleteBot", "listDirs", "registerProject", "memoryCall", "routines", "skills"]
    {
        assert!(known.contains(m), "{m} is a router method");
    }
    for listed in roles::MEMBER_METHODS {
        assert!(known.contains(*listed), "{listed} is on the allow-list but the router has no such method");
    }
    let mut refused = 0;
    for m in &known {
        if roles::member_may(m) {
            continue;
        }
        let e = call(&r, &sam, m, json!({})).await.expect_err(m);
        assert!(denied(&e), "{m}: {e:#}");
        refused += 1;
    }
    for m in super::devices::LOCAL_ONLY {
        assert!(!roles::member_may(m), "{m} is local-only and must not be on the member list");
        assert!(call(&r, &sam, m, json!({})).await.is_err());
    }
    for m in ["createInvite", "invites", "revokeInvite", "people", "setPersonAccess", "revokePerson", "audit"] {
        assert!(!roles::member_may(m), "{m} is the owner's");
        assert!(call(&r, &sam, m, json!({})).await.unwrap_err().is::<NotAllowed>());
    }
    let rows = r.hub.store.audit_list(0, Some("sam")).unwrap();
    assert!(rows.len() >= refused && rows.iter().all(|row| !row.allowed || roles::member_may(&row.method)));
}

#[tokio::test]
async fn an_invitee_sees_and_touches_only_their_own_chats() {
    let r = Rig::new().await;
    let owner_house = r.owner_bot("house");
    let owner_money = r.owner_bot("money");
    let (sam, _) = joined(&r, json!({"house": "member"})).await;

    let started = call(&r, &sam, "startChat", json!({"agent": "house"})).await.unwrap();
    let chat = started["bot"]["id"].as_str().unwrap().to_owned();
    assert_eq!(started["bot"]["person"], "sam");
    assert!(started["bot"]["cwd"].is_null(), "the agent's folder on this computer isn't shown");
    let again = call(&r, &sam, "startChat", json!({"agent": "house"})).await.unwrap();
    assert_eq!(again["bot"]["id"], chat.as_str(), "one chat per agent");
    let e = call(&r, &sam, "startChat", json!({"agent": "money"})).await.unwrap_err();
    assert!(e.is::<NotAllowed>(), "not granted: {e:#}");
    assert!(call(&r, &Caller::Local, "startChat", json!({"agent": "house"})).await.is_err());

    // Their chat works; the owner's chats and entries are not theirs.
    call(&r, &sam, "history", json!({"botId": chat})).await.unwrap();
    for bot in [&owner_house, &owner_money] {
        let e = call(&r, &sam, "history", json!({"botId": bot})).await.unwrap_err();
        assert!(e.is::<NotAllowed>(), "{e:#}");
        assert!(call(&r, &sam, "send", json!({"botId": bot, "text": "hi"})).await.is_err());
    }
    let entry = r
        .hub
        .add_entry(
            &crate::store::Lane::main(&owner_house),
            crate::store::EntryKind::Agent,
            1,
            &json!({"text": "secret"}),
        )
        .unwrap();
    assert!(call(&r, &sam, "react", json!({"entryId": entry.id, "emoji": "x"})).await.is_err());
    assert!(call(&r, &sam, "respondPermission", json!({"entryId": entry.id})).await.is_err());
    // Folders on this Mac, and projects nobody shared.
    let e = call(&r, &sam, "send", json!({"botId": chat, "text": "hi", "folder": "/etc"})).await.unwrap_err();
    assert!(e.is::<NotAllowed>());
    let e = call(&r, &sam, "send", json!({"botId": chat, "text": "hi", "project": "secret-site"})).await.unwrap_err();
    assert!(e.is::<NotAllowed>());

    // Lists: the owner never sees their chat, they never see the owner's.
    let seen = |evs: &[Value]| -> BTreeSet<String> {
        evs.iter().filter(|e| e["type"] == "bot").map(|e| e["bot"]["id"].as_str().unwrap().to_owned()).collect()
    };
    let owner_view = events(&r, &Caller::Local).await;
    assert_eq!(seen(&owner_view), BTreeSet::from([owner_house.clone(), owner_money.clone()]));
    let sam_view = events(&r, &sam).await;
    assert_eq!(seen(&sam_view), BTreeSet::from([chat.clone()]));
    assert!(sam_view.iter().all(|e| e["type"] != "entry" || e["entry"]["botId"] == chat.as_str()));
    assert!(sam_view[0]["usage"].is_null() && sam_view[0]["screen"].is_null(), "hello is trimmed");
    assert!(sam_view.iter().all(|e| e["type"] != "usage"));

    // Hello hides this computer's paths and addresses.
    let hello = call(&r, &sam, "hello", json!({})).await.unwrap();
    assert!(hello["home"].is_null() && hello["urls"].is_null() && hello["hostId"].is_string());
    // Only the agents they hold, with their role; `money` is set up here but not theirs.
    assert_eq!(hello["grantedAgents"], json!([{"agent": "house", "name": "house", "role": "member"}]));
    r.auth(&["member", "money", "sam", "preset=guest"]);
    let again = call(&r, &sam, "hello", json!({})).await.unwrap();
    assert_eq!(
        again["grantedAgents"],
        json!([{"agent": "house", "name": "house", "role": "member"}, {"agent": "money", "name": "money", "role": "guest"}])
    );
    r.auth(&["member", "money", "sam", "--rm"]);
    let owner_hello = call(&r, &Caller::Local, "hello", json!({})).await.unwrap();
    assert!(owner_hello["home"].is_string() || owner_hello["urls"].is_array());

    // Projects: only those shared with them.
    r.hub.store.set_person_projects("sam", &["site".to_owned()]).unwrap();
    let all = json!({"projects": [{"id": "site"}, {"id": "other"}], "error": "no hq at /Users/x"});
    let shaped = roles::shape(&r.hub, "sam", "projects", all);
    assert_eq!(shaped, json!({"projects": [{"id": "site", "name": "site"}]}));

    // Taking the agent away cuts the next request.
    r.auth(&["member", "house", "sam", "--rm"]);
    let e = call(&r, &sam, "history", json!({"botId": chat})).await.unwrap_err();
    assert!(e.is::<NotAllowed>(), "{e:#}");

    let audit = call(&r, &Caller::Local, "audit", json!({"personId": "sam"})).await.unwrap();
    let rows = audit["audit"].as_array().unwrap();
    assert!(rows.iter().any(|a| a["method"] == "history" && a["allowed"] == true && a["target"] == chat.as_str()));
    assert!(rows.iter().any(|a| a["method"] == "history" && a["allowed"] == false));
    assert!(rows.iter().all(|a| a["deviceKey"] == "sam-phone"));
}

#[tokio::test]
async fn create_invite_makes_the_person_the_grants_and_the_cloud_record() {
    let r = Rig::new().await;
    let made = call(&r, &Caller::Local, "createInvite", json!({"name": "Sam Lee", "agents": {"house": "member"}}))
        .await
        .unwrap();
    assert_eq!(made["personId"], "sam-lee");
    let link = made["appLink"].as_str().unwrap();
    let scheme = crate::environment::Environment::current().url_scheme();
    let query = link.strip_prefix(&format!("{scheme}://invite?")).unwrap();
    // The shared link is the web one whenever the build has a web origin.
    assert_eq!(made["link"], if made["webLink"].is_null() { made["appLink"].clone() } else { made["webLink"].clone() });
    match made["webLink"].as_str() {
        Some(web) => {
            let frag = web.split_once("/join#").unwrap().1;
            assert!(frag.contains("t=") && !frag.contains("r="), "{web}");
            assert!(!web.contains('?'), "secrets belong in the fragment: {web}");
        }
        None => assert!(crate::environment::Environment::current().web_origin().is_none()),
    }
    let part = |k: &str| query.split('&').find_map(|p| p.strip_prefix(&format!("{k}="))).unwrap().to_owned();
    assert_eq!(part("c"), r.hub.identity.computer_id());
    assert_eq!(part("i"), made["inviteId"].as_str().unwrap());
    let resolved: Value = serde_json::from_str(&r.auth(&["resolve", "house", "--person", "sam-lee"])).unwrap();
    assert_eq!(
        (resolved["allowed"] == true, resolved["preset"].as_str(), resolved["budget"].as_str()),
        (true, Some("member"), Some("20"))
    );
    assert!(!r.allowed("sam-lee", "money"));
    // The cloud only gets the hash.
    let posted = r.posts().into_iter().find(|(p, _)| p == "/v1/host/invites").unwrap().1;
    assert_eq!(posted["inviteId"], made["inviteId"]);
    let proof = hex(&crypto::sha256(&[part("t").as_bytes()]));
    assert_eq!(
        posted["tokenHash"],
        hex(&crypto::sha256(&[proof.as_bytes()])).as_str(),
        "the cloud gets the hash of the proof"
    );
    assert_eq!(posted["expiresAt"], made["expiresAt"]);
    let stored = r.hub.store.invite(made["inviteId"].as_str().unwrap()).unwrap();
    assert!(stored.open(now_ms()) && stored.token_hash == proof, "the host keeps the proof the cloud never saw");
    assert!(
        !serde_json::to_string(&call(&r, &Caller::Local, "invites", json!({})).await.unwrap())
            .unwrap()
            .contains(&part("t"))
    );

    // The same name again: another invite for the same person. Someone else's id isn't taken over.
    let again = call(&r, &Caller::Local, "createInvite", json!({"name": "Sam Lee", "agents": {"money": "guest"}}))
        .await
        .unwrap();
    assert_eq!(again["personId"], "sam-lee");
    assert_ne!(again["inviteId"], made["inviteId"]);
    r.auth(&["person", "bob"]);
    let bob =
        call(&r, &Caller::Local, "createInvite", json!({"name": "Bob", "agents": {"house": "guest"}})).await.unwrap();
    assert_eq!(bob["personId"], "bob-2");
    let people = call(&r, &Caller::Local, "people", json!({})).await.unwrap();
    let ids: Vec<&str> = people["people"].as_array().unwrap().iter().map(|p| p["personId"].as_str().unwrap()).collect();
    assert_eq!(ids, ["sam-lee", "bob-2"]);
    assert_eq!(people["people"][0]["agents"], json!({"house": "member", "money": "guest"}));
}

#[tokio::test]
async fn create_invite_refuses_and_leaves_nothing_half_made() {
    let r = Rig::new().await;
    let invite = |name: &str, agents: Value| json!({"name": name, "agents": agents});
    // Unclaimed.
    cloud::update_status(&r.hub, |s| s.owner = None);
    let e = call(&r, &Caller::Local, "createInvite", invite("Kim", json!({"house": "guest"}))).await.unwrap_err();
    assert!(format!("{e:#}").contains("Sign in"), "{e:#}");
    cloud::update_status(&r.hub, |s| s.owner = Some(cloud::Owner { user_id: "user_owner".into(), email: None }));
    // Bad roles and unknown agents.
    assert!(call(&r, &Caller::Local, "createInvite", invite("Kim", json!({"house": "owner"}))).await.is_err());
    assert!(call(&r, &Caller::Local, "createInvite", invite("Kim", json!({"nope": "guest"}))).await.is_err());
    assert!(!r.auth_ok(&["people", "kim"]), "a failed grant takes the new person back");
    // The cloud says no.
    r.fake.refuse_invites.store(true, std::sync::atomic::Ordering::Relaxed);
    let e = call(&r, &Caller::Local, "createInvite", invite("Kim", json!({"house": "guest"}))).await.unwrap_err();
    assert!(format!("{e:#}").contains("cloud"), "{e:#}");
    assert!(!r.auth_ok(&["people", "kim"]));
    assert!(r.hub.store.invites().unwrap().is_empty() && r.hub.store.person("kim").is_none());
    assert!(!r.auth_ok(&["retired", "kim"]), "no link went out, so the id is free again");
    // An existing person keeps what they had when a new grant is rolled back.
    r.fake.refuse_invites.store(false, std::sync::atomic::Ordering::Relaxed);
    let made = call(&r, &Caller::Local, "createInvite", invite("Kim", json!({"house": "guest"}))).await.unwrap();
    assert_eq!(made["personId"], "kim");
    r.fake.refuse_invites.store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(call(&r, &Caller::Local, "createInvite", invite("Kim", json!({"money": "member"}))).await.is_err());
    assert!(r.allowed("kim", "house") && !r.allowed("kim", "money"));
    assert_eq!(r.hub.store.person("kim").unwrap().agents.keys().collect::<Vec<_>>(), ["house"]);
    assert_eq!(r.hub.store.invites().unwrap().len(), 1);
    // No owner person in kith-auth.
    r.auth(&["person", "kai", "--rm"]);
    let e = call(&r, &Caller::Local, "createInvite", invite("Lee", json!({"house": "guest"}))).await.unwrap_err();
    assert!(format!("{e:#}").contains("owner"), "{e:#}");
}

#[tokio::test]
async fn a_removed_persons_id_never_goes_to_a_new_invite_and_their_old_link_binds_nobody() {
    let r = Rig::new().await;
    let invite = |name: &str| json!({"name": name, "agents": {"house": "guest"}});
    let made = call(&r, &Caller::Local, "createInvite", invite("Sam")).await.unwrap();
    assert_eq!(made["personId"], "sam");
    r.auth(&["person", "sam", "--rm"]);
    // The same name again is someone new: another id, so none of sam's memory, files or logins.
    let again = call(&r, &Caller::Local, "createInvite", invite("Sam")).await.unwrap();
    assert_eq!(again["personId"], "sam-2");
    // Sam's link, accepted after the removal, binds nobody and doesn't bring sam back.
    let late = Listed {
        membership_id: "mem_late".into(),
        user_id: "user_late".into(),
        invite_id: made["inviteId"].as_str().unwrap().into(),
        ..Default::default()
    };
    members::apply(&r.hub, Some(r.bin.as_path()), &[late], Some("user_owner")).await.unwrap();
    assert!(r.who("user_late")["person"].is_null() && !r.auth_ok(&["people", "sam"]));
    assert_eq!(r.hub.store.memberships().unwrap(), []);
}

#[tokio::test]
async fn memberships_bind_only_to_our_open_invites_and_leave_with_the_cloud() {
    let r = Rig::new().await;
    let made =
        call(&r, &Caller::Local, "createInvite", json!({"name": "Sam", "agents": {"house": "guest"}})).await.unwrap();
    let invite = made["inviteId"].as_str().unwrap().to_owned();
    let listed = |id: &str, user: &str, inv: &str| Listed {
        membership_id: id.into(),
        user_id: user.into(),
        invite_id: inv.into(),
        ..Default::default()
    };
    let bin = Some(r.bin.as_path());

    // An invite we never made, and one that has expired, bind nothing.
    let old = Invite {
        invite_id: "inv_old".into(),
        person_id: "sam".into(),
        token_hash: "h".into(),
        agents: std::collections::BTreeMap::default(),
        created_at: 1,
        expires_at: now_ms() - 1,
        used_at: None,
        revoked_at: None,
    };
    r.hub.store.put_invite(&old).unwrap();
    let strangers = [listed("mem_a", "user_a", "inv_unknown"), listed("mem_b", "user_b", "inv_old")];
    members::apply(&r.hub, bin, &strangers, Some("user_owner")).await.unwrap();
    // Nor does an account that isn't an account id, or the owner's own.
    let odd = [listed("mem_c", "not a user id", &invite), listed("mem_d", "user_owner", &invite)];
    members::apply(&r.hub, bin, &odd, Some("user_owner")).await.unwrap();
    assert_eq!(r.hub.store.memberships().unwrap(), []);
    assert!(r.who("user_a")["person"].is_null() && r.who("user_b")["person"].is_null());
    assert_eq!(r.hub.store.memberships().unwrap(), []);

    // Ours binds, once, and is the same on every pull.
    let sam = listed("mem_sam", "user_sam", &invite);
    members::apply(&r.hub, bin, std::slice::from_ref(&sam), Some("user_owner")).await.unwrap();
    members::apply(&r.hub, bin, std::slice::from_ref(&sam), Some("user_owner")).await.unwrap();
    assert_eq!(r.who("user_sam")["person"], "sam");
    assert_eq!(r.hub.store.memberships().unwrap().len(), 1);
    assert!(r.hub.store.invite(&invite).unwrap().used_at.is_some());
    // A second account can't ride the same invite.
    members::apply(&r.hub, bin, &[sam.clone(), listed("mem_x", "user_x", &invite)], Some("user_owner")).await.unwrap();
    assert!(r.who("user_x")["person"].is_null());

    // The membership disappears from the cloud: devices go, the person and their memory stay.
    let phone = r.device("sam-phone", Some("sam"));
    r.device("owner-phone", None);
    members::apply(&r.hub, bin, &[], Some("user_owner")).await.unwrap();
    assert!(r.hub.store.device("sam-phone").is_none() && r.hub.store.device("owner-phone").is_some());
    assert!(r.who("user_sam")["person"].is_null() && r.auth_ok(&["people", "sam"]));
    assert_eq!(r.hub.store.memberships().unwrap(), []);
    assert!(call(&r, &phone, "hello", json!({})).await.is_err(), "its channel is refused at once");
    members::apply(&r.hub, bin, &[], Some("user_owner")).await.unwrap();
}

#[tokio::test]
async fn a_pull_binds_the_member_and_their_device_waits_for_the_owner_to_compare_its_code() {
    let r = Rig::new().await;
    let made =
        call(&r, &Caller::Local, "createInvite", json!({"name": "Sam", "agents": {"house": "member"}})).await.unwrap();
    let invite = made["inviteId"].as_str().unwrap().to_owned();
    let base = cloud::url(&r.hub.store).unwrap();
    let (dk, nd): ([u8; 32], [u8; 32]) = (crypto::random(), crypto::random());
    let stranger_dk: [u8; 32] = crypto::random();
    let request = |id: &str, user: &str, host_nonce: Option<String>, device_nonce: Option<[u8; 32]>| {
        json!({"requestId": id, "deviceKey": b64(if user == "user_sam" { &dk } else { &stranger_dk }), "userId": user, "deviceName": "Sam's phone", "platform": "ios",
            "commit": b64(&crypto::sas_commit(crypto::Labels::Kith, if user == "user_sam" { &dk } else { &stranger_dk }, &nd)),
            "hostNonce": host_nonce, "deviceNonce": device_nonce.map(|n| b64(&n)), "createdAt": 1, "expiresAt": 2})
    };
    let memberships =
        json!([{"membershipId": "mem_sam", "userId": "user_sam", "inviteId": invite, "email": "s@example.com"}]);
    r.set_cloud_state(json!({"owner": {"userId": "user_owner"}, "memberships": memberships, "grants": [],
        "requests": [request("req_sam", "user_sam", None, None), request("req_x", "user_stranger", None, None)]}));
    cloud::pull(&r.hub, &base).await.unwrap();
    assert_eq!(r.who("user_sam")["person"], "sam");
    let posts = r.posts();
    assert!(
        posts.iter().any(|(p, b)| p.ends_with("req_x/decision") && b["decision"] == "deny"),
        "a stranger is denied"
    );
    let nonce =
        posts.iter().find(|(p, _)| p.ends_with("req_sam/nonce")).expect("the member's request is answered").1["nonce"]
            .as_str()
            .unwrap()
            .to_owned();
    assert_eq!(r.hub.cloud.requests_json()[0]["personId"], "sam");

    // Approval mode stays "code": the matching reveal alone doesn't let the member's device in, the owner's compare does.
    assert_eq!(cloud::approval(&r.hub.store), cloud::Approval::Code);
    r.set_cloud_state(json!({"owner": {"userId": "user_owner"}, "memberships": memberships, "grants": [],
        "requests": [request("req_sam", "user_sam", Some(nonce), Some(nd))]}));
    cloud::pull(&r.hub, &base).await.unwrap();
    assert!(r.hub.store.device(&b64(&dk)).is_none(), "waits for the owner");
    cloud::decide(&r.hub, "req_sam", true).await.unwrap();
    let device = r.hub.store.device(&b64(&dk)).expect("approved");
    assert_eq!((device.person_id.as_deref(), device.grant_id.as_deref()), (Some("sam"), Some("grt_new")));
    let caller = Caller::Device { key: device.key.clone(), scopes: device.scopes.clone() };
    // The owner compared its code on this Mac: that stands in for the invite token, which a new browser no longer has.
    assert_eq!(principal(&r.hub.store, &caller), Principal::Member("sam".into()), "the owner's approval proves it");
    let again = call(&r, &caller, "proveInvite", json!({"inviteId": invite, "token": "nope"})).await.unwrap_err();
    assert!(format!("{again:#}").contains("already shown its invite"));
    call(&r, &caller, "hello", json!({})).await.unwrap();

    // Grants: renewed for the bound account, removed for any other.
    let grant = |user: Option<&str>| kith_wire::cloud::HostGrant {
        grant_id: "grt_new".into(),
        device_key: device.key.clone(),
        user_id: user.map(str::to_owned),
        ..kith_wire::cloud::HostGrant::default()
    };
    cloud::apply_grants(&r.hub, &[grant(Some("user_sam"))], Some("user_owner")).unwrap();
    assert!(r.hub.store.device(&device.key).is_some());
    cloud::apply_grants(&r.hub, &[grant(Some("user_owner"))], Some("user_owner")).unwrap();
    assert!(r.hub.store.device(&device.key).is_none(), "a grant naming another account isn't honoured");
    // An owner's device needs the owner's account (or a cloud that doesn't say).
    let owner_phone = r.device("owner-phone", None);
    let og = |user: Option<&str>| kith_wire::cloud::HostGrant {
        grant_id: "grt_owner-phone".into(),
        device_key: "owner-phone".into(),
        user_id: user.map(str::to_owned),
        ..kith_wire::cloud::HostGrant::default()
    };
    cloud::apply_grants(&r.hub, &[og(None)], Some("user_owner")).unwrap();
    cloud::apply_grants(&r.hub, &[og(Some("user_owner"))], Some("user_owner")).unwrap();
    assert!(r.hub.store.device("owner-phone").is_some());
    cloud::apply_grants(&r.hub, &[og(Some("user_sam"))], Some("user_owner")).unwrap();
    assert!(r.hub.store.device("owner-phone").is_none());
    let _ = owner_phone;
}

#[tokio::test]
async fn changing_and_revoking_a_persons_access() {
    let r = Rig::new().await;
    let (sam, membership) = joined(&r, json!({"house": "guest"})).await;
    // More agents, a role change, a project.
    call(
        &r,
        &Caller::Local,
        "setPersonAccess",
        json!({"personId": "sam", "agents": {"house": "member", "money": "guest"}, "projects": ["site"]}),
    )
    .await
    .unwrap();
    let house: Value = serde_json::from_str(&r.auth(&["resolve", "house", "--person", "sam"])).unwrap();
    assert_eq!((house["preset"].as_str(), house["budget"].as_str()), (Some("member"), Some("20")));
    assert!(r.allowed("sam", "money"));
    assert_eq!(r.hub.store.person("sam").unwrap().projects, ["site"]);
    call(&r, &Caller::Local, "setPersonAccess", json!({"personId": "sam", "agents": {"money": "guest"}}))
        .await
        .unwrap();
    assert!(!r.allowed("sam", "house") && r.allowed("sam", "money"));
    assert_eq!(r.hub.store.person("sam").unwrap().projects, ["site"], "left alone when not named");
    assert!(
        call(&r, &Caller::Local, "setPersonAccess", json!({"personId": "kai", "agents": {}})).await.is_err(),
        "not someone invited"
    );

    // An unused invite revokes locally and in the cloud; twice is fine.
    let made = call(&r, &Caller::Local, "createInvite", json!({"personId": "sam", "agents": {"money": "guest"}}))
        .await
        .unwrap();
    let id = made["inviteId"].as_str().unwrap();
    assert_eq!(call(&r, &Caller::Local, "revokeInvite", json!({"inviteId": id})).await.unwrap()["revoked"], true);
    assert_eq!(call(&r, &Caller::Local, "revokeInvite", json!({"inviteId": id})).await.unwrap()["revoked"], false);
    assert!(r.posts().iter().any(|(p, _)| p == &format!("/v1/host/invites/{id}/revoke")));

    // Revoking the person: their devices and membership stop granting anything (kept for the undo, the cloud cuts the
    // membership itself) and kith-auth refuses them; the person, their rows, their memory and their account handle
    // stay (the bridge tells them, and an undo finds them).
    let other = r.device("sam-tablet", Some("sam"));
    call(&r, &Caller::Local, "revokePerson", json!({"personId": "sam"})).await.unwrap();
    call(&r, &Caller::Local, "revokePerson", json!({"personId": "sam"})).await.unwrap();
    assert!(!r.allowed("sam", "money") && r.auth_ok(&["people", "sam"]));
    assert!(r.hub.store.devices_of("sam").unwrap().is_empty());
    assert_eq!(r.who("user_sam")["person"], "sam");
    assert_eq!(r.who("user_sam")["revoked"], true);
    assert_eq!(r.hub.store.memberships().unwrap(), []);
    assert_eq!(r.hub.store.suspended_of("sam").unwrap().memberships, 1, "kept for the undo");
    assert!(r.posts().iter().all(|(p, _)| p != &format!("/v1/host/memberships/{membership}/revoke")));
    assert!(call(&r, &sam, "hello", json!({})).await.is_err() && call(&r, &other, "hello", json!({})).await.is_err());
    assert!(call(&r, &Caller::Local, "revokePerson", json!({"personId": "nobody"})).await.is_err());
    // An invite that was open cannot be accepted meanwhile.
    assert!(r.hub.store.invites().unwrap().iter().all(|i| !i.open(now_ms())));
    let _ = Arc::strong_count(&r.hub);
}

#[tokio::test]
async fn an_invitee_never_reaches_a_terminal_thread() {
    let r = Rig::new().await;
    let owner_house = r.owner_bot("house");
    let (sam, _) = joined(&r, json!({"house": "member"})).await;
    let started = call(&r, &sam, "startChat", json!({"agent": "house"})).await.unwrap();
    let theirs = started["bot"]["id"].as_str().unwrap().to_owned();

    // A terminal session of the owner's `house` agent becomes a thread in the owner's bot.
    let state = r.dir.join("term-state");
    let reg_dir = state.join("live").join("terminal");
    std::fs::create_dir_all(&reg_dir).unwrap();
    let transcript = r.dir.join("tsess-1.jsonl");
    let user = json!({"type": "user", "timestamp": "2026-10-23T01:00:00Z", "message": {"content": "terminal secret"}});
    std::fs::write(&transcript, format!("{user}\n")).unwrap();
    let reg = json!({
        "v": 1, "sessionId": "tsess-1", "agent": "house", "cwd": r.dir.to_string_lossy(),
        "transcriptPath": transcript.to_string_lossy(), "pid": 2_000_000_000u32, "project": null,
        "source": "startup", "resumedFrom": null, "startedAt": "2026-10-23T01:00:00Z",
    });
    std::fs::write(reg_dir.join("tsess-1.json"), reg.to_string()).unwrap();
    r.hub.terminal.set_env(crate::terminal::Env { state, sessions: Some(r.dir.join("sessions")) });
    crate::terminal::scan(&r.hub);
    let root = r.hub.store.history(&owner_house, i64::MAX, 10).unwrap().pop().expect("the terminal thread");
    assert_eq!(root.data["text"], "terminal secret");
    assert!(r.hub.store.history(&theirs, i64::MAX, 10).unwrap().is_empty(), "nothing lands in their chat");

    // The owner reaches it; the invitee reaches none of it.
    let seen = call(&r, &Caller::Local, "resolveThread", json!({"thread": root.id})).await.unwrap();
    assert_eq!(seen["sessionId"], "tsess-1");
    for (method, body) in [
        ("history", json!({"botId": owner_house})),
        ("thread", json!({"botId": owner_house, "rootId": root.id})),
        ("send", json!({"botId": owner_house, "threadId": root.id, "text": "hi"})),
        ("resolveThread", json!({"thread": root.id})),
        ("resolveThread", json!({"thread": "tsess-1"})),
        ("takeOverThread", json!({"botId": owner_house, "threadId": root.id})),
    ] {
        let e = call(&r, &sam, method, body.clone()).await.expect_err(method);
        assert!(denied(&e), "{method} {body}: {e:#}");
    }
    // Naming the owner's thread from their own chat reaches nothing either.
    let e = call(&r, &sam, "send", json!({"botId": theirs, "threadId": root.id, "text": "hi"})).await.unwrap_err();
    // A plain-string refusal is hidden from a guest; the owner's same call keeps the text.
    assert!(e.to_string().starts_with("Something went wrong on the host."), "{e:#}");
    let own = call(&r, &Caller::Local, "send", json!({"botId": theirs, "threadId": root.id, "text": "hi"}))
        .await
        .unwrap_err();
    assert!(own.to_string().contains("threads start from a message"), "{own:#}");
    assert!(!roles::member_may("resolveThread") && !roles::member_may("takeOverThread"));

    // No event of it reaches them; the owner's stream has it.
    let sam_view = events(&r, &sam).await;
    assert!(sam_view.iter().all(|e| e["type"] != "entry" || e["entry"]["botId"] == theirs.as_str()));
    assert!(!serde_json::to_string(&sam_view).unwrap().contains("terminal secret"));
    let entry_event = json!({"type": "entry", "rev": 1, "entry": root});
    assert!(roles::event_for(&r.hub.store, Some("sam"), entry_event.clone()).is_none());
    assert!(roles::event_for(&r.hub.store, None, entry_event).is_some());
    let owner_view = events(&r, &Caller::Local).await;
    assert!(serde_json::to_string(&owner_view).unwrap().contains("terminal secret"));
}

#[tokio::test]
async fn a_thread_project_is_corrected_only_in_own_chats_and_to_shared_projects() {
    let r = Rig::new().await;
    let owner_house = r.owner_bot("house");
    let owner_money = r.owner_bot("money");
    let (sam, _) = joined(&r, json!({"house": "member"})).await;
    let started = call(&r, &sam, "startChat", json!({"agent": "house"})).await.unwrap();
    let chat = started["bot"]["id"].as_str().unwrap().to_owned();

    // Not the owner's chat; not a project nobody shared with them (checked before anything is looked up).
    let e = call(&r, &sam, "setThreadProjects", json!({"botId": owner_house, "projects": []})).await.unwrap_err();
    assert!(e.is::<NotAllowed>(), "{e:#}");
    let e = call(&r, &sam, "threadProjects", json!({"botId": owner_house})).await.unwrap_err();
    assert!(e.is::<NotAllowed>(), "{e:#}");
    let e = call(&r, &sam, "setThreadProjects", json!({"botId": chat, "projects": ["secret-site"]})).await.unwrap_err();
    assert!(e.is::<NotAllowed>(), "{e:#}");
    // Their own chat reads, and an empty correction is theirs to make.
    let got = call(&r, &sam, "threadProjects", json!({"botId": chat})).await.unwrap();
    assert_eq!((got["projects"].clone(), got["pinned"].clone()), (json!([]), json!(false)));
    let set = call(&r, &sam, "setThreadProjects", json!({"botId": chat, "projects": []})).await.unwrap();
    assert_eq!((set["by"].clone(), set["changed"].clone()), (json!("person"), json!(false)));

    // A thread root of another bot is no thread of this one.
    let lane = crate::store::Lane::main(&owner_house);
    let root = r.hub.add_entry(&lane, crate::store::EntryKind::User, 1, &json!({"text": "hi"})).unwrap();
    let e = call(&r, &Caller::Local, "threadProjects", json!({"botId": owner_money, "threadId": root.id}))
        .await
        .unwrap_err();
    assert!(format!("{e:#}").contains("unknown message"), "{e:#}");
}

fn recap(audience: &str) -> kith_mem::summary::Summary {
    kith_mem::summary::Summary {
        thread_id: "t".into(),
        room: "r".into(),
        agent: "house".into(),
        audience: audience.into(),
        projects: vec![],
        text: "Planned the move.".into(),
        links: vec![],
        covered_seq: 1,
        reason: kith_mem::summary::Reason::End,
        written_at: 77,
        cost_via: "summary".into(),
    }
}

/// A thread root in `bot`'s chat whose row carries a summary for `audience`.
fn root_with_summary(r: &Rig, bot: &str, audience: &str) -> crate::store::Entry {
    use crate::store::{EntryKind, Lane};
    let mut data = json!({"text": "hi", "thread": {"count": 1}});
    crate::chat::memory::summary::set_row_summary(&mut data, &recap(audience));
    r.hub.store.insert_entry(&Lane::main(bot), EntryKind::User, 1, &data).unwrap()
}

async fn chat_of(r: &Rig, caller: &Caller) -> String {
    let started = call(r, caller, "startChat", json!({"agent": "house"})).await.unwrap();
    started["bot"]["id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn a_thread_row_carries_its_summary_only_to_its_audience() {
    let r = Rig::new().await;
    let owner_house = r.owner_bot("house");
    let (sam, _) = joined(&r, json!({"house": "member"})).await;
    let theirs = chat_of(&r, &sam).await;
    let mine = root_with_summary(&r, &theirs, "person:sam");
    let shared = root_with_summary(&r, &theirs, "shared");
    let other = root_with_summary(&r, &theirs, "person:eve");
    let terminal = root_with_summary(&r, &theirs, "owner");

    let seen = call(&r, &sam, "history", json!({"botId": theirs})).await.unwrap();
    let row = |id: &str| {
        let all = seen["entries"].as_array().unwrap();
        all.iter().find(|e| e["id"] == id).unwrap()["data"]["thread"].clone()
    };
    assert_eq!(row(&mine.id)["summary"], json!({"text": "Planned the move.", "writtenAt": 77}));
    assert!(!row(&shared.id)["summary"].is_null());
    assert!(row(&other.id)["summary"].is_null(), "someone else's summary");
    assert!(row(&terminal.id)["summary"].is_null(), "terminal and group summaries are the owner's");
    assert!(!seen["entries"].to_string().contains("summaryAudience"), "the audience stays on the host");

    // The owner sees the summary on the row, in the same batch that carries the list.
    let all = root_with_summary(&r, &owner_house, "owner");
    let owner_seen = call(&r, &Caller::Local, "history", json!({"botId": owner_house})).await.unwrap();
    let got = owner_seen["entries"].as_array().unwrap().iter().find(|e| e["id"] == all.id).unwrap();
    assert_eq!(got["data"]["thread"]["summary"]["text"], "Planned the move.");
    assert!(!roles::may_see_summary(Some("sam"), "room:g1") && roles::may_see_summary(None, "room:g1"));
    // A row with no summary has no key at all.
    let plain = r.hub.store.insert_entry(
        &crate::store::Lane::main(&theirs),
        crate::store::EntryKind::User,
        2,
        &json!({"text": "x"}),
    );
    assert!(plain.unwrap().data["thread"].is_null());
}

#[tokio::test]
async fn showing_the_summary_as_the_row_is_an_owner_setting_off_by_default() {
    let r = Rig::new().await;
    let bot = r.owner_bot("house");
    let shows = |r: &Rig| {
        let row = r.hub.store.bot(&bot).unwrap().unwrap();
        r.hub.bot_json(&row)["rowShowsSummary"].clone()
    };
    assert_eq!(shows(&r), json!(false), "bots without the setting show the first message");
    for on in [true, true, false, true, false] {
        call(&r, &Caller::Local, "updateBot", json!({"id": bot, "rowShowsSummary": on})).await.unwrap();
        assert_eq!(shows(&r), json!(on));
    }
    let (sam, _) = joined(&r, json!({"house": "member"})).await;
    let e = call(&r, &sam, "updateBot", json!({"id": bot, "rowShowsSummary": true})).await.unwrap_err();
    assert!(denied(&e), "{e:#}");
}

#[tokio::test]
async fn a_citation_opens_for_its_audience_and_is_not_shared_for_anyone_else() {
    let r = Rig::new().await;
    let owner_house = r.owner_bot("house");
    let (sam, _) = joined(&r, json!({"house": "member"})).await;
    let theirs = chat_of(&r, &sam).await;
    let mine = root_with_summary(&r, &theirs, "person:sam");
    let owners = root_with_summary(&r, &owner_house, "owner");
    let link = |id: &str| json!({"citation": format!("kith://thread/{id}#entry=1")});

    let ok = call(&r, &sam, "resolveCitation", link(&mine.id)).await.unwrap();
    assert_eq!(ok["thread"]["rootId"], mine.id.as_str());
    assert!(ok.get("error").is_none());
    let owner_ok = call(&r, &Caller::Local, "resolveCitation", link(&owners.id)).await.unwrap();
    assert_eq!(owner_ok["thread"]["botId"], owner_house.as_str());

    // Someone else's thread and a thread that doesn't exist look the same, whatever the client claims to be.
    let stranger = call(&r, &sam, "resolveCitation", link(&owners.id)).await.unwrap();
    let missing = call(&r, &sam, "resolveCitation", link("no-such-thread")).await.unwrap();
    assert_eq!(stranger, json!({"error": "not_shared"}));
    assert_eq!(missing, stranger);
    let forged = json!({"citation": link(&owners.id)["citation"], "asPerson": null});
    assert_eq!(call(&r, &sam, "resolveCitation", forged).await.unwrap(), stranger);
    let malformed = call(&r, &sam, "resolveCitation", json!({"citation": "nonsense"})).await.unwrap();
    assert_eq!(malformed, stranger);
    assert_eq!(call(&r, &Caller::Local, "resolveCitation", link("no-such-thread")).await.unwrap(), stranger);

    // A forgotten thread is not shared with its own audience either.
    r.hub.store.with_db(|c| kith_mem::summary::forget(c, &mine.id, 1)).unwrap();
    assert_eq!(call(&r, &sam, "resolveCitation", link(&mine.id)).await.unwrap(), stranger);
}

#[tokio::test]
async fn resolving_a_main_chat_citation_hides_other_threads_summaries_from_a_member() {
    let r = Rig::new().await;
    r.owner_bot("house");
    let (sam, _) = joined(&r, json!({"house": "member"})).await;
    let theirs = chat_of(&r, &sam).await;
    root_with_summary(&r, &theirs, "person:sam");
    root_with_summary(&r, &theirs, "person:eve");
    root_with_summary(&r, &theirs, "owner");
    let link = json!({"citation": format!("kith://thread/main:{theirs}")});

    let seen = call(&r, &sam, "resolveCitation", link.clone()).await.unwrap();
    let rows: Vec<&Value> =
        seen["entries"].as_array().unwrap().iter().filter(|e| e["data"]["thread"]["count"] == 1).collect();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows.iter().filter(|e| !e["data"]["thread"]["summary"].is_null()).count(), 1, "only sam's own");
    assert!(rows.iter().all(|e| e["data"]["thread"].get("summaryAudience").is_none()));
    // The owner still sees all of them.
    let owner = call(&r, &Caller::Local, "resolveCitation", link).await.unwrap();
    let all = owner["entries"].as_array().unwrap().iter().filter(|e| !e["data"]["thread"]["summary"].is_null()).count();
    assert_eq!(all, 3);
}

#[tokio::test]
async fn forgetting_a_thread_takes_the_summary_off_its_row() {
    let r = Rig::new().await;
    r.owner_bot("house");
    let (sam, _) = joined(&r, json!({"house": "member"})).await;
    let theirs = chat_of(&r, &sam).await;
    let mine = root_with_summary(&r, &theirs, "person:sam");
    let older = root_with_summary(&r, &theirs, "person:sam");
    let mut events = r.hub.events.subscribe();

    // A member can't forget; the owner can, twice.
    assert!(call(&r, &sam, "forgetThread", json!({"thread": mine.id})).await.is_err());
    for _ in 0..2 {
        call(&r, &Caller::Local, "forgetThread", json!({"thread": mine.id})).await.unwrap();
    }
    let row = r.hub.store.entry(&mine.id).unwrap();
    assert!(row.data["thread"].get("summary").is_none() && row.data["thread"].get("summaryAudience").is_none());
    assert_eq!(row.data["thread"]["count"], 1);
    assert!(events.try_recv().is_ok_and(|e| e["type"] == "entry" && e["entry"]["id"] == mine.id.as_str()));

    // A thread forgotten without its row cleaned (an older forget) is served without the summary.
    r.hub.store.with_db(|c| kith_mem::summary::forget(c, &older.id, 1)).unwrap();
    for who in [&sam, &Caller::Local] {
        let seen = call(&r, who, "history", json!({"botId": theirs})).await.unwrap();
        let row = seen["entries"].as_array().unwrap().iter().find(|e| e["id"] == older.id.as_str()).unwrap();
        assert!(row["data"]["thread"].get("summary").is_none());
    }
}
