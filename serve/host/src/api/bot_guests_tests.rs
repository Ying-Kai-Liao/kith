//! Bot guests against a real kith-auth in a temp folder, a temp store and a fake cloud: invite, accept (as the cloud
//! lists it), prove, approve, ask, revoke; what a bot may call; the cascades; limits and the seven-day silence.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::devices::{Caller, NotAllowed, Principal, principal};
use super::rig::Rig;
use super::{botguests::BotError, dispatch, roles};
use crate::remote::botsync::{self, Listed};
use crate::remote::crypto::{self, b64};
use crate::remote::members::{self, Listed as Member};
use crate::store::{BotState, EntryKind, Lane, Scope, now_ms};
use serde_json::{Value, json};
use std::collections::BTreeSet;

async fn call(r: &Rig, caller: &Caller, method: &str, b: Value) -> anyhow::Result<Value> {
    dispatch(&r.hub, caller, method, b).await
}

fn code(e: &anyhow::Error) -> Option<&'static str> {
    super::error_code(e)
}

/// `sam` made by an invite, bound to `user_sam`, with `house` and one phone.
async fn sam(r: &Rig) -> Caller {
    let made =
        call(r, &Caller::Local, "createInvite", json!({"name": "Sam", "agents": {"house": "member"}})).await.unwrap();
    let invite = made["inviteId"].as_str().unwrap().to_owned();
    let listed = [Member {
        membership_id: "mem_sam".into(),
        user_id: "user_sam".into(),
        invite_id: invite,
        ..Default::default()
    }];
    members::apply(&r.hub, Some(&r.bin), &listed, Some("user_owner")).await.unwrap();
    r.device("sam-phone", Some("sam"))
}

fn token_of(link: &str) -> String {
    link.split("#t=").nth(1).unwrap().split('&').next().unwrap().to_owned()
}

/// A bot joined the way the cloud reports it: the invite made, the key listed. Not yet proven.
struct Joined {
    caller: Caller,
    bot: String,
    invite: String,
    token: String,
}

async fn join(r: &Rig, owner: &Caller, body: Value) -> Joined {
    let made = call(r, owner, "createBotInvite", body).await.unwrap();
    let key = b64(&crypto::random::<32>());
    let invite = made["inviteId"].as_str().unwrap().to_owned();
    let listed = Listed {
        bot_device_id: format!("bd_{}", &key[..8]),
        invite_id: invite.clone(),
        device_key: key.clone(),
        ..Listed::default()
    };
    // The cloud lists every bot device it has, so the ones already admitted stay in the list.
    let mut all: Vec<Listed> = r
        .hub
        .store
        .bot_guests()
        .unwrap()
        .into_iter()
        .filter(|g| g.state.live())
        .filter_map(|g| g.cloud_device)
        .map(|bot_device_id| Listed {
            bot_device_id,
            invite_id: String::new(),
            device_key: String::new(),
            ..Default::default()
        })
        .collect();
    all.push(listed);
    botsync::apply(&r.hub, &all).await.unwrap();
    Joined {
        caller: Caller::Device { key, scopes: vec![Scope::Control] },
        bot: made["botId"].as_str().unwrap().to_owned(),
        invite,
        token: token_of(made["link"].as_str().unwrap()),
    }
}

async fn prove(r: &Rig, j: &Joined) -> anyhow::Result<Value> {
    call(r, &j.caller, "proveBotInvite", json!({"inviteId": j.invite, "token": j.token})).await
}

/// A bot of Kai's, joined and let in.
async fn active_bot(r: &Rig) -> Joined {
    let j = join(r, &Caller::Local, json!({"name": "Rex", "provider": "grok", "agents": ["house"]})).await;
    assert_eq!(prove(r, &j).await.unwrap()["state"], "active");
    j
}

fn ask(text: &str, nonce: &str) -> Value {
    json!({"agent": "house", "text": text, "clientNonce": nonce})
}

#[tokio::test]
async fn a_bot_joins_proves_asks_and_is_revoked() {
    let r = Rig::new().await;
    r.owner_bot("house");
    let made =
        call(&r, &Caller::Local, "createBotInvite", json!({"name": "Rex", "provider": "grok", "agents": ["house"]}))
            .await
            .unwrap();
    let link = made["link"].as_str().unwrap();
    let cid = r.hub.identity.computer_id();
    assert!(link.contains(&format!("/b/{cid}/{}#t=", made["inviteId"].as_str().unwrap())), "{link}");
    assert!(link.ends_with(&format!("&k={}", r.hub.identity.sign_pub_b64())), "the host key is pinned in the link");
    assert!(made["share"].as_str().unwrap().contains("kith join"));
    let posts = r.posts();
    let posted = posts.iter().find(|(p, _)| p == "/v1/host/bot-invites").expect("the cloud was told");
    assert_eq!(posted.1["inviteId"], made["inviteId"]);
    // The cloud never sees the token, only a hash of its hash.
    assert!(!posted.1.to_string().contains(&token_of(link)));

    // A key the cloud lists for an invite this host never made adds nothing.
    let stranger = Listed {
        bot_device_id: "bd_x".into(),
        invite_id: "bi_unknown".into(),
        device_key: b64(&crypto::random::<32>()),
        ..Default::default()
    };
    botsync::apply(&r.hub, &[stranger]).await.unwrap();
    assert_eq!(r.hub.store.devices().unwrap().len(), 0);

    let key = b64(&crypto::random::<32>());
    let inv = made["inviteId"].as_str().unwrap().to_owned();
    let listed =
        Listed { bot_device_id: "bd_1".into(), invite_id: inv.clone(), device_key: key.clone(), ..Default::default() };
    botsync::apply(&r.hub, std::slice::from_ref(&listed)).await.unwrap();
    botsync::apply(&r.hub, &[listed]).await.unwrap();
    let bot = Caller::Device { key: key.clone(), scopes: vec![Scope::Control] };
    let id = made["botId"].as_str().unwrap().to_owned();
    let row = r.hub.store.bot_guest(&id).unwrap();
    assert_eq!((row.state, row.cloud_device.as_deref()), (BotState::Joining, Some("bd_1")));
    assert_eq!(principal(&r.hub.store, &bot), Principal::BotPending(id.clone()));
    let device = r.hub.store.device(&key).unwrap();
    assert_eq!(device.grant_id.as_deref(), Some("bot:bd_1"));

    // Before the proof: status only; an ask says why.
    assert_eq!(call(&r, &bot, "botStatus", json!({})).await.unwrap()["state"], "joining");
    let e = call(&r, &bot, "botAsk", ask("hi", "n0")).await.unwrap_err();
    assert_eq!(code(&e), Some("pending"));
    let e = call(&r, &bot, "proveBotInvite", json!({"inviteId": inv, "token": "wrong"})).await.unwrap_err();
    assert!(e.is::<NotAllowed>());
    let token = token_of(link);
    let proven = call(&r, &bot, "proveBotInvite", json!({"inviteId": inv, "token": token})).await.unwrap();
    assert_eq!(proven["state"], "active", "the owner's own bot needs no approval");
    let again = call(&r, &bot, "proveBotInvite", json!({"inviteId": inv, "token": token})).await.unwrap();
    assert_eq!(again["state"], "active", "proving twice is fine");
    assert!(matches!(principal(&r.hub.store, &bot), Principal::Bot { .. }));

    let status = call(&r, &bot, "botStatus", json!({})).await.unwrap();
    assert_eq!(status["agents"][0]["agent"], "house");
    let person = r.hub.store.bot_guest(&id).unwrap().person_id;
    assert!(r.allowed(&person, "house"));
    assert!(!r.allowed(&person, "money"), "only what the owner picked");
    let hello = call(&r, &bot, "hello", json!({})).await.unwrap();
    assert!(hello.get("grantedAgents").is_some() && hello.get("usage").is_none(), "{hello}");

    // Ask: a conversation, the run is the bot's person, the text is marked pasted.
    let first = call(&r, &bot, "botAsk", ask("what's on the list?", "n1")).await.unwrap();
    let conv = first["conversation"].as_str().unwrap().to_owned();
    assert!(conv.starts_with("cv_") && first["duplicate"].is_null());
    let chat = r.hub.store.bot_conversation_by_id(&conv).unwrap();
    let chat_row = r.hub.store.bot(&chat.chat_bot_id).unwrap().unwrap();
    assert_eq!(chat_row.config.person.as_deref(), Some(person.as_str()), "the run's person is the bot's");
    let user = r.hub.store.entry(first["entryId"].as_str().unwrap()).unwrap();
    assert_eq!(user.data["pasted"][0], "what's on the list?");
    assert_eq!(user.data["sender"]["botId"], id.as_str());
    assert_eq!(user.thread_id.as_deref(), Some(chat.thread_root.as_str()));
    let root = r.hub.store.entry(&chat.thread_root).unwrap();
    assert_eq!(root.data["thread"]["origin"], "bot");
    assert!(root.data["thread"]["originLabel"].as_str().unwrap().starts_with("from Grok · "));
    assert!(root.data["thread"]["originLabel"].as_str().unwrap().ends_with("'s Rex"));
    assert_eq!(root.data["thread"]["originBot"]["name"], "Rex");

    // A retry with the same nonce sends nothing new.
    let retry = call(&r, &bot, "botAsk", ask("what's on the list?", "n1")).await.unwrap();
    assert_eq!(
        (&retry["conversation"], &retry["entryId"], &retry["duplicate"]),
        (&first["conversation"], &first["entryId"], &json!(true))
    );
    let users = r.hub.store.thread(&chat.chat_bot_id, &chat.thread_root, 50).unwrap();
    assert_eq!(users.iter().filter(|e| e.kind == "user").count(), 1);

    // Continue it, and find it by the provider's own id.
    let more = call(&r, &bot, "botAsk", json!({"conversation": conv, "text": "and the pantry?", "clientNonce": "n2"}))
        .await
        .unwrap();
    assert_eq!(more["conversation"], conv.as_str());
    let by_ref =
        call(&r, &bot, "botAsk", json!({"agent": "house", "ref": "grok:8841", "text": "x", "clientNonce": "n3"}))
            .await
            .unwrap();
    let by_ref2 =
        call(&r, &bot, "botAsk", json!({"agent": "house", "ref": "grok:8841", "text": "y", "clientNonce": "n4"}))
            .await
            .unwrap();
    assert_eq!(by_ref["conversation"], by_ref2["conversation"]);
    assert_ne!(by_ref["conversation"], conv.as_str());

    // What a bot reads back is only words, shaped.
    let t = call(&r, &bot, "botThread", json!({"conversation": conv})).await.unwrap();
    assert!(
        t["entries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| ["user", "agent", "notice", "waiting"].contains(&e["kind"].as_str().unwrap()))
    );
    assert_eq!(t["entries"][0]["text"], "what's on the list?");
    let list = call(&r, &bot, "botConversations", json!({})).await.unwrap();
    assert_eq!(list["conversations"].as_array().unwrap().len(), 2);

    // Revoke: everything stops at once.
    let mut events = r.hub.events.subscribe();
    let out = call(&r, &Caller::Local, "revokeBot", json!({"botId": id})).await.unwrap();
    assert_eq!(out["revoked"], true);
    assert_eq!(
        call(&r, &Caller::Local, "revokeBot", json!({"botId": id})).await.unwrap()["revoked"],
        false,
        "twice is fine"
    );
    assert_eq!(principal(&r.hub.store, &bot), Principal::Nobody);
    assert!(r.hub.store.device(&key).is_none());
    assert!(call(&r, &bot, "botAsk", ask("hi", "n9")).await.is_err());
    assert!(!r.allowed(&person, "house"), "its person is gone from kith-auth");
    let row = r.hub.store.bot_guest(&id).unwrap();
    assert_eq!((row.state, row.revoked_why.as_deref()), (BotState::Revoked, Some("kai")));
    assert!(r.posts().iter().any(|(p, _)| p == "/v1/host/bot-devices/bd_1/revoke"), "the cloud is told");
    let mut saw = false;
    while let Ok(ev) = events.try_recv() {
        saw |= ev["type"] == "botUpdated" && ev["bot"]["state"] == "revoked";
    }
    assert!(saw, "the apps hear of it");
    assert!(r.hub.store.audit_list(0, None).unwrap().iter().any(|a| a.method == "bot.revoke"));
    // Its conversations stay for the owner, marked ended.
    let thread = r.hub.store.thread(&chat.chat_bot_id, &chat.thread_root, 50).unwrap();
    assert!(thread.iter().any(|e| e.data["ended"].is_string()));
}

#[tokio::test]
async fn an_invitees_bot_waits_for_the_owner_who_may_always_allow() {
    let r = Rig::new().await;
    r.owner_bot("house");
    r.owner_bot("money");
    let sam = sam(&r).await;

    // Sam can give his bot only what he holds, up to ten bots, and sees only his own.
    let e = call(&r, &sam, "createBotInvite", json!({"name": "Rex", "agents": ["money"]})).await.unwrap_err();
    assert!(e.is::<NotAllowed>(), "{e:#}");
    let j = join(&r, &sam, json!({"name": "Rex", "provider": "grok", "agents": ["house"]})).await;
    let mut events = r.hub.events.subscribe();
    assert_eq!(prove(&r, &j).await.unwrap()["state"], "pending");
    assert_eq!(principal(&r.hub.store, &j.caller), Principal::BotPending(j.bot.clone()));
    let e = call(&r, &j.caller, "botAsk", ask("hi", "a")).await.unwrap_err();
    assert_eq!(code(&e), Some("pending"));
    let mut request = None;
    while let Ok(ev) = events.try_recv() {
        if ev["type"] == "botRequest" {
            request = Some(ev);
        }
    }
    let request = request.expect("the computer's owner is asked");
    assert_eq!((request["bot"]["ownerPerson"].as_str(), request["bot"]["name"].as_str()), (Some("sam"), Some("Rex")));
    // The request goes to the owner's devices only; a bot update to the owner and Sam.
    let ev = |t: &str| json!({"type": t, "bot": {"ownerPerson": "sam"}});
    assert!(roles::event_for(&r.hub.store, None, ev("botRequest")).is_some());
    assert!(roles::event_for(&r.hub.store, Some("sam"), ev("botRequest")).is_none());
    assert!(roles::event_for(&r.hub.store, Some("sam"), ev("botUpdated")).is_some());
    assert!(roles::event_for(&r.hub.store, Some("kim"), ev("botUpdated")).is_none());

    // Neither Sam nor the bot decides.
    for who in [&sam, &j.caller] {
        let e = call(&r, who, "decideBot", json!({"botId": j.bot, "decision": "allow"})).await.unwrap_err();
        assert!(e.is::<NotAllowed>() || e.is::<super::devices::Forbidden>());
    }
    let mine = call(&r, &sam, "myBots", json!({})).await.unwrap();
    assert_eq!(mine["bots"][0]["state"], "pending");
    assert_eq!(call(&r, &Caller::Local, "bots", json!({})).await.unwrap()["bots"].as_array().unwrap().len(), 1);

    let allow = json!({"botId": j.bot, "decision": "allow", "alwaysForOwner": true});
    call(&r, &Caller::Local, "decideBot", allow.clone()).await.unwrap();
    call(&r, &Caller::Local, "decideBot", allow).await.unwrap();
    assert!(matches!(principal(&r.hub.store, &j.caller), Principal::Bot { .. }));
    assert!(r.hub.store.person("sam").unwrap().auto_bots);
    assert_eq!(call(&r, &j.caller, "botAsk", ask("hi", "b")).await.unwrap()["state"], "running");

    // With the toggle on, Sam's next bot is in at once.
    let j2 = join(&r, &sam, json!({"name": "Scout", "agents": ["house"]})).await;
    assert_eq!(prove(&r, &j2).await.unwrap()["state"], "active");

    // Sam loses house: the bot's next ask is refused and the cascade holds.
    call(&r, &Caller::Local, "setPersonAccess", json!({"personId": "sam", "agents": {}})).await.unwrap();
    let e = call(&r, &j.caller, "botAsk", ask("again", "c")).await.unwrap_err();
    assert_eq!(code(&e), Some("notGranted"));
    assert_eq!(
        call(&r, &j.caller, "botStatus", json!({})).await.unwrap()["agents"].as_array().unwrap().as_slice(),
        &[] as &[Value]
    );

    // Revoking Sam puts every bot of his to sleep, not to an end: each is refused everything, and back on undo.
    call(&r, &Caller::Local, "setPersonAccess", json!({"personId": "sam", "agents": {"house": "member"}}))
        .await
        .unwrap();
    assert_eq!(call(&r, &j2.caller, "botAsk", ask("before", "d")).await.unwrap()["state"], "running");
    call(&r, &Caller::Local, "revokePerson", json!({"personId": "sam"})).await.unwrap();
    for b in [&j, &j2] {
        assert_eq!(r.hub.store.bot_guest(&b.bot).unwrap().state, BotState::Active);
        let e = call(&r, &b.caller, "botAsk", ask("while revoked", "e")).await.unwrap_err();
        assert_eq!(code(&e), Some("notGranted"));
    }
    call(&r, &Caller::Local, "unrevokePerson", json!({"personId": "sam"})).await.unwrap();
    assert_eq!(call(&r, &j2.caller, "botAsk", ask("after", "f")).await.unwrap()["state"], "running");
}

#[tokio::test]
async fn deny_ends_a_request_and_silence_for_seven_days_is_a_no() {
    let r = Rig::new().await;
    r.owner_bot("house");
    let sam = sam(&r).await;
    let denied = join(&r, &sam, json!({"name": "A", "agents": ["house"]})).await;
    prove(&r, &denied).await.unwrap();
    call(&r, &Caller::Local, "decideBot", json!({"botId": denied.bot, "decision": "deny"})).await.unwrap();
    call(&r, &Caller::Local, "decideBot", json!({"botId": denied.bot, "decision": "deny"})).await.unwrap();
    assert_eq!(r.hub.store.bot_guest(&denied.bot).unwrap().state, BotState::Denied);
    assert_eq!(principal(&r.hub.store, &denied.caller), Principal::Nobody);

    let quiet = join(&r, &sam, json!({"name": "B", "agents": ["house"]})).await;
    prove(&r, &quiet).await.unwrap();
    botsync::sweep_at(&r.hub, now_ms() + botsync::PENDING_MS - 60_000).await;
    assert_eq!(r.hub.store.bot_guest(&quiet.bot).unwrap().state, BotState::Pending, "not yet");
    botsync::sweep_at(&r.hub, now_ms() + botsync::PENDING_MS + 60_000).await;
    let row = r.hub.store.bot_guest(&quiet.bot).unwrap();
    assert_eq!((row.state, row.revoked_why.as_deref()), (BotState::Denied, Some("noAnswer")));
    assert_eq!(principal(&r.hub.store, &quiet.caller), Principal::Nobody);
}

#[tokio::test]
async fn a_key_that_never_proves_is_dropped_after_ten_minutes() {
    let r = Rig::new().await;
    r.owner_bot("house");
    let j = join(&r, &Caller::Local, json!({"name": "Rex", "agents": ["house"]})).await;
    botsync::sweep_at(&r.hub, now_ms() + 60_000).await;
    assert_eq!(r.hub.store.bot_guest(&j.bot).unwrap().state, BotState::Joining);
    botsync::sweep_at(&r.hub, now_ms() + botsync::JOIN_MS + 60_000).await;
    assert_eq!(r.hub.store.bot_guest(&j.bot).unwrap().state, BotState::Revoked);
    assert!(
        r.hub
            .store
            .device(match &j.caller {
                Caller::Device { key, .. } => key,
                _ => unreachable!(),
            })
            .is_none()
    );
}

#[tokio::test]
async fn limits_and_ownership_hold() {
    let r = Rig::new().await;
    r.owner_bot("house");
    let j = active_bot(&r).await;
    // 16000 characters, no more; another bot's conversation is a 404.
    let long = "x".repeat(16_001);
    let e = call(&r, &j.caller, "botAsk", ask(&long, "l")).await.unwrap_err();
    assert_eq!(code(&e), Some("tooLong"));
    let first = call(&r, &j.caller, "botAsk", ask("one", "m1")).await.unwrap();
    let other = active_bot(&r).await;
    let e = call(
        &r,
        &other.caller,
        "botAsk",
        json!({"conversation": first["conversation"], "text": "mine now", "clientNonce": "z"}),
    )
    .await
    .unwrap_err();
    assert_eq!(code(&e), Some("notFound"));
    let e = call(&r, &other.caller, "botThread", json!({"conversation": first["conversation"]})).await.unwrap_err();
    assert!(e.downcast_ref::<BotError>().is_some_and(|b| b.status == 404));

    // The hourly limit, lowered by the owner: counted from the audit log, retries not counted.
    call(&r, &Caller::Local, "setBotLimits", json!({"botId": j.bot, "ratePerHour": 2})).await.unwrap();
    call(&r, &j.caller, "botAsk", ask("two", "m2")).await.unwrap();
    call(&r, &j.caller, "botAsk", ask("two", "m2")).await.unwrap();
    let e = call(&r, &j.caller, "botAsk", ask("three", "m3")).await.unwrap_err();
    assert_eq!(code(&e), Some("rateLimited"));
    // A member's bot owner can lower the limits but not raise them past the defaults.
    let e = call(&r, &Caller::Local, "setBotLimits", json!({"botId": j.bot, "ratePerHour": 0})).await.unwrap_err();
    assert!(format!("{e:#}").contains("from 1 to"));

    // A busy conversation queues; with noQueue it refuses; at five waiting it is busy.
    let busy = call(&r, &other.caller, "botAsk", ask("a", "q1")).await.unwrap();
    let conv = busy["conversation"].as_str().unwrap().to_owned();
    // The rig has no agent process, so nothing keeps the first turn running; a message waiting in the thread is what
    // makes the conversation busy, whatever the timing.
    let cv = r.hub.store.bot_conversation_by_id(&conv).unwrap();
    let lane = Lane::in_thread(&cv.chat_bot_id, &cv.thread_root);
    let wait = |n: usize| {
        let waiting = json!({"text": "waiting", "clientNonce": format!("w{n}"), "status": "queued", "attachments": []});
        r.hub.add_entry(&lane, EntryKind::User, 99, &waiting).expect("a waiting message");
    };
    wait(0);
    let e = call(
        &r,
        &other.caller,
        "botAsk",
        json!({"conversation": conv, "text": "b", "clientNonce": "q2", "noQueue": true}),
    )
    .await
    .unwrap_err();
    assert_eq!(code(&e), Some("busy"));
    // Under five waiting, a message queues behind them.
    for n in 1..4 {
        wait(n);
    }
    let more = |nonce: &str| json!({"conversation": conv, "text": "more", "clientNonce": nonce});
    let v = call(&r, &other.caller, "botAsk", more("q3")).await.unwrap();
    assert_eq!(v["state"], "queued");
    // At five waiting it is busy. (The rig has no agent, so the count is set up here and not by timing.)
    wait(4);
    let e = call(&r, &other.caller, "botAsk", more("q4")).await.unwrap_err();
    assert_eq!(code(&e), Some("busy"));

    // Ten live bots per owner.
    for i in 0..8 {
        call(&r, &Caller::Local, "createBotInvite", json!({"name": format!("B{i}"), "agents": []})).await.unwrap();
    }
    let e = call(&r, &Caller::Local, "createBotInvite", json!({"name": "Eleven", "agents": []})).await.unwrap_err();
    assert_eq!(code(&e), Some("tooMany"));
}

#[tokio::test]
async fn a_refused_invite_leaves_nothing_and_a_rekey_replaces_the_old_key() {
    let r = Rig::new().await;
    r.owner_bot("house");
    // A cloud that can't be reached refuses the invite.
    r.hub.store.kv_set("test_cloud_url", "http://127.0.0.1:1").unwrap();
    assert!(call(&r, &Caller::Local, "createBotInvite", json!({"name": "Rex", "agents": ["house"]})).await.is_err());
    assert!(r.hub.store.bot_guests().unwrap().is_empty(), "no bot row without its invite");
    assert!(!r.auth(&["people"]).contains("bot-"), "nor a kith-auth person");
}

#[tokio::test]
async fn rotation_replaces_the_key_in_one_step() {
    let r = Rig::new().await;
    r.owner_bot("house");
    let j = active_bot(&r).await;
    let old = match &j.caller {
        Caller::Device { key, .. } => key.clone(),
        _ => unreachable!(),
    };
    let made = call(&r, &Caller::Local, "createBotInvite", json!({"botId": j.bot})).await.unwrap();
    assert_eq!(made["botId"], j.bot.as_str());
    let key = b64(&crypto::random::<32>());
    let inv = made["inviteId"].as_str().unwrap().to_owned();
    botsync::apply(
        &r.hub,
        &[Listed {
            bot_device_id: "bd_new".into(),
            invite_id: inv.clone(),
            device_key: key.clone(),
            ..Default::default()
        }],
    )
    .await
    .unwrap();
    assert!(r.hub.store.device(&old).is_none(), "the old key stops working");
    let fresh = Caller::Device { key, scopes: vec![Scope::Control] };
    assert_eq!(
        principal(&r.hub.store, &fresh),
        Principal::BotPending(j.bot.clone()),
        "unproven until it shows the token"
    );
    let token = token_of(made["link"].as_str().unwrap());
    let proven = call(&r, &fresh, "proveBotInvite", json!({"inviteId": inv, "token": token})).await.unwrap();
    assert_eq!(proven["state"], "active", "a bot already let in stays in");
}

/// Every method name the router has an arm for, as the member walk reads them from the sources.
fn router_methods() -> BTreeSet<String> {
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
    out.insert("readFile".into());
    out
}

#[tokio::test]
async fn every_router_method_is_denied_to_bots_unless_listed() {
    let r = Rig::new().await;
    r.owner_bot("house");
    let active = active_bot(&r).await;
    let sam = sam(&r).await;
    let pending = join(&r, &sam, json!({"name": "P", "agents": ["house"]})).await;
    prove(&r, &pending).await.unwrap();
    let known = router_methods();
    assert!(known.len() > 100, "the walk found {} methods", known.len());
    for listed in roles::BOT_METHODS {
        assert!(known.contains(*listed), "{listed} is on the bot list but nothing answers it");
    }
    let mut refused = 0;
    for m in &known {
        if roles::bot_may(m) {
            continue;
        }
        for who in [&active.caller, &pending.caller] {
            let e = call(&r, who, m, json!({})).await.expect_err(m);
            assert!(e.is::<NotAllowed>() || e.is::<super::devices::Forbidden>(), "{m}: {e:#}");
        }
        refused += 1;
    }
    assert!(refused > 100);
    for m in [
        "respondPermission",
        "send",
        "startChat",
        "thread",
        "history",
        "upload",
        "projects",
        "linkCode",
        "createBotInvite",
        "decideBot",
        "revokeBot",
    ] {
        assert!(!roles::bot_may(m), "{m} is not for bots");
    }
    // Every allowed method really is answered for an active bot (a validation error, never "unknown method").
    for m in roles::BOT_METHODS.iter().filter(|m| **m != "botLeave" && **m != "proveBotInvite") {
        if let Err(e) = call(&r, &active.caller, m, json!({})).await {
            assert!(!e.is::<super::UnknownMethod>() && !e.is::<NotAllowed>(), "{m}: {e:#}");
        }
    }
    // A pending bot gets only the three.
    for m in roles::BOT_METHODS {
        let r = call(&r, &pending.caller, m, json!({})).await;
        if !roles::BOT_PENDING_METHODS.contains(m) {
            assert_eq!(code(&r.unwrap_err()), Some("pending"), "{m}");
        }
    }
    // Members hold the bot methods that are theirs and nothing of the owner's decisions.
    for m in ["decideBot", "setAutoAllowBots", "bots"] {
        assert!(!roles::member_may(m));
        assert!(call(&r, &sam, m, json!({})).await.unwrap_err().is::<NotAllowed>());
    }
    assert!(known.contains("createBotInvite") && roles::member_may("createBotInvite"));
    let rows = r.hub.store.audit_list(0, Some(&r.hub.store.bot_guest(&active.bot).unwrap().person_id)).unwrap();
    assert!(
        !rows.is_empty()
            && rows.iter().all(|row| !row.allowed || roles::bot_may(&row.method) || row.method.starts_with("bot."))
    );
}

#[tokio::test]
async fn a_bots_stream_carries_only_its_own_words() {
    use futures::StreamExt;
    let r = Rig::new().await;
    r.owner_bot("house");
    let j = active_bot(&r).await;
    let other = active_bot(&r).await;
    let first = call(&r, &j.caller, "botAsk", ask("mine", "s1")).await.unwrap();
    call(&r, &other.caller, "botAsk", ask("theirs", "s2")).await.unwrap();
    // The agent's thought and tool call are never sent.
    let conv = r.hub.store.bot_conversation_by_id(first["conversation"].as_str().unwrap()).unwrap();
    let lane = Lane::in_thread(&conv.chat_bot_id, &conv.thread_root);
    r.hub.add_entry(&lane, EntryKind::Thought, 1, &json!({"text": "secret thought"}));
    r.hub.add_entry(&lane, EntryKind::Agent, 1, &json!({"text": "answer", "final": true}));
    let stream = super::events_stream(&r.hub, 0, None, &j.caller).unwrap();
    let evs: Vec<Value> =
        Box::pin(stream).take_until(tokio::time::sleep(std::time::Duration::from_millis(300))).collect().await;
    assert_eq!(evs[0]["type"], "hello");
    let texts: Vec<&str> =
        evs.iter().filter(|e| e["type"] == "entry").map(|e| e["entry"]["text"].as_str().unwrap()).collect();
    assert!(texts.contains(&"mine") && texts.contains(&"answer"), "{texts:?}");
    assert!(!texts.contains(&"theirs") && !texts.contains(&"secret thought"), "{texts:?}");
    assert!(evs.iter().all(|e| e["type"] != "bot" && e["type"] != "usage"));
    let t = call(&r, &j.caller, "botThread", json!({"conversation": first["conversation"]})).await.unwrap();
    assert_eq!(t["done"], true);
}

#[tokio::test]
async fn a_terminal_holding_the_thread_refuses_an_ask_and_a_retry_still_answers() {
    use crate::terminal::{Env, claim};
    let r = Rig::new().await;
    r.owner_bot("house");
    r.hub.terminal.set_env(Env { state: r.dir.join("state"), sessions: Some(r.dir.join("sessions")) });
    let state = r.hub.terminal.env().state;
    let j = active_bot(&r).await;
    let first = call(&r, &j.caller, "botAsk", ask("one", "n1")).await.unwrap();
    let conv = r.hub.store.bot_conversation_by_id(first["conversation"].as_str().unwrap()).unwrap();
    let lane = Lane::in_thread(&conv.chat_bot_id, &conv.thread_root);
    r.hub.store.kv_set(&crate::store::lane_key("session", &conv.chat_bot_id, &lane), "sbot1").unwrap();
    // Asks only: the first turn has no agent here and may add a notice at any time.
    let sent = || {
        r.hub
            .store
            .thread(&conv.chat_bot_id, &conv.thread_root, 50)
            .unwrap()
            .iter()
            .filter(|e| e.kind == "user")
            .count()
    };
    let before = sent();

    // A terminal holds the thread (this test process stands in for it).
    let me = std::process::id();
    assert!(claim::take(&state, "sbot1", claim::Kind::Terminal, me, false).unwrap().is_ok());
    let more = json!({"conversation": conv.conversation_id, "text": "two", "clientNonce": "n2"});
    let e = call(&r, &j.caller, "botAsk", more.clone()).await.unwrap_err();
    assert_eq!(code(&e), Some("heldByOtherWriter"));
    assert_eq!(sent(), before, "nothing was sent");
    // A retry of an ask already sent answers the same, ahead of the gate, and takes no claim.
    let retry = call(&r, &j.caller, "botAsk", ask("one", "n1")).await.unwrap();
    assert_eq!(retry["duplicate"], true);
    assert_eq!(claim::holder(&state, "sbot1").unwrap().kind, claim::Kind::Terminal);

    claim::release(&state, "sbot1", me, false);
    call(&r, &j.caller, "botAsk", more).await.unwrap();
    assert!(claim::holder(&state, "sbot1").is_none(), "the claim is taken when the turn starts, never by the ask");
}

#[tokio::test]
async fn the_ask_cursor_catches_up_the_answer_and_a_failed_turn_is_marked_an_error() {
    let r = Rig::new().await;
    r.owner_bot("house");
    let j = active_bot(&r).await;
    let first = call(&r, &j.caller, "botAsk", ask("go", "c1")).await.unwrap();
    let conv = r.hub.store.bot_conversation_by_id(first["conversation"].as_str().unwrap()).unwrap();
    let user = r.hub.store.entry(first["entryId"].as_str().unwrap()).unwrap();
    assert_eq!(first["seq"], user.rev, "the cursor botAsk hands out is the entry's rev");
    let lane = Lane::in_thread(&conv.chat_bot_id, &conv.thread_root);
    let answer = r.hub.add_entry(&lane, EntryKind::Agent, user.turn, &json!({"text": "done", "final": true})).unwrap();
    let failed = r
        .hub
        .add_entry(&lane, EntryKind::Notice, user.turn + 1, &json!({"text": "The agent crashed", "style": "error"}))
        .unwrap();

    // Catching up from the ask's cursor returns the answer, whose seq is on the same scale.
    let after = first["seq"].as_i64().unwrap();
    let t = call(&r, &j.caller, "botThread", json!({"conversation": conv.conversation_id, "afterSeq": after}))
        .await
        .unwrap();
    let got = t["entries"].as_array().unwrap();
    let agent = got.iter().find(|e| e["kind"] == "agent").expect("the answer is past the cursor");
    assert_eq!((agent["text"].as_str(), agent["seq"].as_i64()), (Some("done"), Some(answer.rev)));
    assert!(got.iter().all(|e| e["seq"].as_i64().unwrap() > after));
    // The failed turn's notice says so; the client ends the turn on it.
    let notice = got.iter().find(|e| e["kind"] == "notice").expect("the error notice is shown");
    assert_eq!((notice["error"].as_bool(), notice["seq"].as_i64()), (Some(true), Some(failed.rev)));
    assert!(agent.get("error").is_none());
}

/// What `caller`'s event stream carries on connect (the catch-up), as the apps read it.
async fn catch_up(r: &Rig, caller: &Caller) -> Vec<Value> {
    use futures::StreamExt;
    let stream = super::events_stream(&r.hub, 0, None, caller).unwrap();
    let evs: Vec<Value> =
        Box::pin(stream).take_until(tokio::time::sleep(std::time::Duration::from_millis(300))).collect().await;
    // The running turns go on writing while we listen: keep what was there on connect.
    let at = evs[0]["rev"].as_i64().unwrap();
    evs.into_iter().filter(|e| e["rev"].as_i64().is_none_or(|r| r <= at)).collect()
}

fn chat_rows<'a>(evs: &'a [Value], chat: &str) -> Vec<&'a Value> {
    evs.iter().filter(|e| e["type"] == "bot" && e["bot"]["id"] == chat).collect()
}

async fn guard_sam(r: &Rig, m: &str, chat: &str) -> anyhow::Result<()> {
    roles::guard(&r.hub, "sam", true, m, &json!({"botId": chat, "text": "x"})).await
}

#[tokio::test]
async fn a_bot_guests_chat_reaches_the_owner_and_the_bots_owner_tagged() {
    let r = Rig::new().await;
    r.owner_bot("house");
    let sam = sam(&r).await;
    let mut live = r.hub.events.subscribe();

    // Sam's bot (let in by the owner) and Kai's own bot each ask once.
    let mine = join(&r, &sam, json!({"name": "Rex", "provider": "grok", "agents": ["house"]})).await;
    prove(&r, &mine).await.unwrap();
    call(&r, &Caller::Local, "decideBot", json!({"botId": mine.bot, "decision": "allow"})).await.unwrap();
    let kais = active_bot(&r).await;
    let a = call(&r, &mine.caller, "botAsk", ask("sam's question", "m1")).await.unwrap();
    let b = call(&r, &kais.caller, "botAsk", ask("kai's question", "k1")).await.unwrap();
    let chat_of =
        |a: &Value| r.hub.store.bot_conversation_by_id(a["conversation"].as_str().unwrap()).unwrap().chat_bot_id;
    let (sam_chat, kai_chat) = (chat_of(&a), chat_of(&b));

    // Live: every event of the chat reaches the owner; Sam gets his bot's chat and not Kai's bot's; a stranger none.
    let mut seen = vec![];
    while let Ok(ev) = live.try_recv() {
        seen.push(ev);
    }
    let about = |ev: &Value, chat: &str| ev["bot"]["id"] == chat || ev["entry"]["botId"] == chat;
    let live_for = |who: Option<&str>, chat: &str| {
        seen.iter()
            .filter(|ev| about(ev, chat))
            .filter_map(|ev| roles::event_for(&r.hub.store, who, ev.clone()))
            .count()
    };
    let total = |chat: &str| seen.iter().filter(|ev| about(ev, chat)).count();
    assert!(total(&sam_chat) > 0 && total(&kai_chat) > 0);
    for chat in [&sam_chat, &kai_chat] {
        assert_eq!(live_for(None, chat), total(chat), "the owner misses events of a bot guest's chat");
    }
    assert_eq!(live_for(Some("sam"), &sam_chat), total(&sam_chat));
    assert_eq!(live_for(Some("sam"), &kai_chat), 0);
    assert_eq!(live_for(Some("kim"), &sam_chat), 0);

    // Catch-up agrees, with the tag on the row and no duplicates.
    let owner = catch_up(&r, &Caller::Local).await;
    for (chat, who) in [(&sam_chat, "Rex"), (&kai_chat, "Rex")] {
        let rows = chat_rows(&owner, chat);
        assert_eq!(rows.len(), 1, "one row per chat");
        let tag = &rows[0]["bot"]["botGuest"];
        assert_eq!((tag["name"].as_str(), tag["provider"].as_str()), (Some(who), Some("grok")), "{tag}");
        assert!(owner.iter().any(|e| e["type"] == "entry" && e["entry"]["botId"] == chat.as_str()));
    }
    assert_eq!(chat_rows(&owner, &sam_chat)[0]["bot"]["botGuest"]["ownerPerson"], "sam");
    assert_eq!(chat_rows(&owner, &sam_chat)[0]["bot"]["botGuest"]["ownerName"], "Sam");
    assert!(chat_rows(&owner, &kai_chat)[0]["bot"]["botGuest"]["ownerPerson"].is_null());
    // A row that is not a bot guest's has no tag.
    assert!(owner.iter().filter(|e| e["type"] == "bot").all(|e| e["bot"]["id"] == sam_chat.as_str()
        || e["bot"]["id"] == kai_chat.as_str()
        || e["bot"].get("botGuest").is_none()));

    let sams = catch_up(&r, &sam).await;
    assert_eq!(chat_rows(&sams, &sam_chat).len(), 1);
    assert_eq!(chat_rows(&sams, &kai_chat).len(), 0);
    assert!(sams.iter().all(|e| e["entry"]["botId"] != kai_chat.as_str()));
    assert!(!json!(sams).to_string().contains("/Users/"), "nothing of where the agent works");

    // Sam reads his bot's chat and cannot write to it, nor read Kai's bot's.
    let g = |m: &'static str, chat: &str| {
        let chat = chat.to_owned();
        let r = &r;
        async move { guard_sam(r, m, &chat).await }
    };
    for m in ["history", "thread", "kithInfo", "threadProjects"] {
        g(m, &sam_chat).await.unwrap_or_else(|e| panic!("{m}: {e:#}"));
        assert!(g(m, &kai_chat).await.is_err(), "{m} on Kai's bot's chat");
    }
    for m in ["send", "editMessage", "deleteBot", "setSettings", "takeOver", "respondPermission"] {
        assert!(g(m, &sam_chat).await.is_err(), "{m} on a bot's chat");
    }

    // Revoked, the chat stays readable to both.
    call(&r, &Caller::Local, "revokeBot", json!({"botId": mine.bot})).await.unwrap();
    let after = catch_up(&r, &Caller::Local).await;
    assert_eq!(chat_rows(&after, &sam_chat).len(), 1);
    assert_eq!(chat_rows(&catch_up(&r, &sam).await, &sam_chat).len(), 1);
    g("history", &sam_chat).await.unwrap();
}

/// revoke-person R7, the bot door: the cloud says something changed (the owner revoked Sam on another Mac), and no ask
/// of Sam's bot starts a turn until the pull that applies it; after it the bot is dormant, and an undo from another Mac
/// wakes it.
#[tokio::test]
async fn a_bot_ask_waits_for_the_cloud_and_a_revoke_from_another_mac_puts_the_bot_to_sleep() {
    use crate::remote::{cloud, sync_lease};
    let r = Rig::new().await;
    r.owner_bot("house");
    let sam = sam(&r).await;
    let j = join(&r, &sam, json!({"name": "Scout", "agents": ["house"]})).await;
    prove(&r, &j).await.unwrap();
    call(&r, &Caller::Local, "decideBot", json!({"botId": j.bot, "decision": "allow"})).await.unwrap();
    let first = call(&r, &j.caller, "botAsk", ask("hi", "a")).await.unwrap();
    assert_eq!(first["state"], "running");
    let thread = json!({"conversation": first["conversation"]});

    sync_lease::changed(&r.hub);
    let e = call(&r, &j.caller, "botAsk", ask("now", "b")).await.unwrap_err();
    assert_eq!(code(&e), Some("syncing"), "no turn on access the host hasn't caught up with: {e:#}");
    let e = call(&r, &j.caller, "botThread", thread.clone()).await.unwrap_err();
    assert_eq!(code(&e), Some("syncing"), "and no answer either: {e:#}");

    let pull = |revoked: Value| {
        r.set_cloud_state(
            json!({"owner": {"userId": "user_owner"}, "memberships": [], "grants": [], "requests": [], "revoked": revoked}),
        );
        async {
            let mark = sync_lease::mark(&r.hub);
            cloud::pull(&r.hub, &cloud::url(&r.hub.store).unwrap()).await.unwrap();
            sync_lease::pulled(&r.hub, mark);
        }
    };
    pull(json!(["user_sam"])).await;
    assert_eq!(r.who("user_sam")["revoked"], true);
    assert_eq!(r.hub.store.bot_guest(&j.bot).unwrap().state, BotState::Active, "dormant, not ended");
    let e = call(&r, &j.caller, "botAsk", ask("now", "c")).await.unwrap_err();
    assert_eq!(code(&e), Some("notGranted"));
    let e = call(&r, &j.caller, "botThread", thread.clone()).await.unwrap_err();
    assert_eq!(code(&e), Some("notGranted"), "a turn that outlived the revoke delivers nothing (R5)");

    // Undone on another Mac: the bot answers again (its owner's rows were kept).
    pull(json!([])).await;
    assert_eq!(r.who("user_sam")["revoked"], false);
    assert_eq!(call(&r, &j.caller, "botAsk", ask("again", "d")).await.unwrap()["state"], "running");
    assert!(call(&r, &j.caller, "botThread", thread).await.is_ok());
}

#[tokio::test]
async fn a_used_nonce_replays_the_same_ask_and_refuses_anything_else() {
    let r = Rig::new().await;
    r.owner_bot("house");
    r.owner_bot("money");
    let j = join(&r, &Caller::Local, json!({"name": "Rex", "provider": "grok", "agents": ["house", "money"]})).await;
    assert_eq!(prove(&r, &j).await.unwrap()["state"], "active");
    let first = call(&r, &j.caller, "botAsk", ask("one", "n1")).await.unwrap();
    let conv = first["conversation"].as_str().unwrap().to_owned();
    let users = || {
        let c = r.hub.store.bot_conversation_by_id(&conv).unwrap();
        r.hub.store.thread(&c.chat_bot_id, &c.thread_root, 50).unwrap().iter().filter(|e| e.kind == "user").count()
    };
    let person = r.hub.store.bot_guest(&j.bot).unwrap().person_id;

    // Same text (the trimmed one), same agent: the replay, also when it names the conversation it made.
    let again = call(&r, &j.caller, "botAsk", ask("  one ", "n1")).await.unwrap();
    assert_eq!((&again["entryId"], &again["duplicate"]), (&first["entryId"], &json!(true)));
    let named = call(&r, &j.caller, "botAsk", json!({"conversation": conv, "text": "one", "clientNonce": "n1"})).await;
    assert_eq!(named.unwrap()["duplicate"], true);

    // Other text, other agent, other conversation: refused with nothing stored, sent or counted.
    let audits = r.hub.store.audit_count_since(&person, "botAsk", 0);
    let refused = [
        ask("two", "n1"),
        json!({"agent": "money", "text": "one", "clientNonce": "n1"}),
        json!({"agent": "house", "ref": "grok:1", "text": "one", "clientNonce": "n1"}),
    ];
    for body in refused {
        let e = call(&r, &j.caller, "botAsk", body.clone()).await.unwrap_err();
        assert_eq!(code(&e), Some("nonceReused"), "{body}");
        assert!(e.downcast_ref::<BotError>().is_some_and(|b| b.status == 409));
    }
    assert_eq!(users(), 1);
    assert_eq!(r.hub.store.audit_count_since(&person, "botAsk", 0), audits);
}
