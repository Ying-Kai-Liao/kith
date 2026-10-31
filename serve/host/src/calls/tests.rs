//! Calls from an agent to its person: ringing, answering, the missed line, busy, the rate limit and who hears it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::api::devices::Caller;
use crate::api::rig::Rig;
use crate::api::{dispatch, events_stream};
use crate::remote::members::{self, Listed};
use futures::StreamExt;

async fn rpc(r: &Rig, caller: &Caller, method: &str, b: Value) -> Result<Value> {
    dispatch(&r.hub, caller, method, b).await
}

/// `sam`, invited to `house`, with a phone and a chat of their own.
async fn sam(r: &Rig) -> (Caller, String) {
    let made =
        rpc(r, &Caller::Local, "createInvite", json!({"name": "Sam", "agents": {"house": "member"}})).await.unwrap();
    let listed = [Listed {
        membership_id: "mem_sam".into(),
        user_id: "user_sam".into(),
        invite_id: made["inviteId"].as_str().unwrap().into(),
        ..Default::default()
    }];
    members::apply(&r.hub, Some(&r.bin), &listed, Some("user_owner")).await.unwrap();
    let phone = r.device("sam-phone", Some("sam"));
    let chat =
        rpc(r, &phone, "startChat", json!({"agent": "house"})).await.unwrap()["bot"]["id"].as_str().unwrap().to_owned();
    (phone, chat)
}

async fn calls_seen(r: &Rig, caller: &Caller) -> Vec<Value> {
    let stream = events_stream(&r.hub, 0, None, caller).unwrap();
    Box::pin(stream)
        .take_until(tokio::time::sleep(Duration::from_millis(300)))
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .filter(|e| e["type"] == "call")
        .collect()
}

fn ring_for(r: &Rig, bot: &str, reason: &str) -> Value {
    ring(&r.hub, bot, reason).unwrap()
}

fn notices(r: &Rig, bot: &str) -> Vec<Value> {
    r.hub.store.history(bot, i64::MAX, 50).unwrap().into_iter().filter(|e| e.kind == "notice").map(|e| e.data).collect()
}

#[tokio::test]
async fn a_call_rings_once_and_the_first_answer_wins() {
    let r = Rig::new().await;
    let bot = r.owner_bot("house");
    let out = ring_for(&r, &bot, "  the\nbill is   due ");
    assert_eq!(out["status"], "ringing");
    let id = out["id"].as_str().unwrap().to_owned();

    let seen = calls_seen(&r, &Caller::Local).await;
    assert_eq!(seen.len(), 1, "an app that connects while it rings gets the call");
    assert_eq!(seen[0]["call"]["reason"], "the bill is due");
    assert_eq!(seen[0]["call"]["botId"], bot.as_str());
    assert_eq!(seen[0]["call"]["state"], "ringing");

    let owner = r.device("owner-phone", None);
    let accepted = rpc(&r, &owner, "callAccept", json!({"id": id})).await.unwrap();
    assert_eq!(accepted["botId"], bot.as_str());
    let late = rpc(&r, &Caller::Local, "callAccept", json!({"id": id})).await.unwrap_err();
    assert!(late.to_string().contains("already accepted"), "{late:#}");
    let late = rpc(&r, &Caller::Local, "callDecline", json!({"id": id})).await.unwrap_err();
    assert!(late.to_string().contains("already accepted"), "{late:#}");
    assert_eq!(calls_seen(&r, &Caller::Local).await, Vec::<Value>::new(), "nothing rings any more");
    assert_eq!(notices(&r, &bot), Vec::<Value>::new(), "an accepted call leaves its line once the call is live");

    // The agent speaks first, once, on a line that isn't the person's words.
    rpc(&r, &owner, "callGreet", json!({"id": id})).await.unwrap();
    assert!(rpc(&r, &owner, "callGreet", json!({"id": id})).await.is_err());
    assert_eq!(notices(&r, &bot)[0]["text"], "house called: the bill is due");
    // Its end frees the person for the next call.
    assert_eq!(ring_for(&r, &r.owner_bot("money"), "x")["status"], "busy");
    r.hub.calls.ended(&bot);
    assert_eq!(ring_for(&r, &r.owner_bot("money"), "x")["status"], "ringing");
}

#[tokio::test]
async fn declining_and_ringing_out_leave_a_line_in_the_chat() {
    let r = Rig::new().await;
    let bot = r.owner_bot("house");
    let id = ring_for(&r, &bot, "lunch?")["id"].as_str().unwrap().to_owned();
    rpc(&r, &Caller::Local, "callDecline", json!({"id": id})).await.unwrap();
    let lines = notices(&r, &bot);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["text"], "house tried to call: lunch?");
    assert_eq!(lines[0]["missedCall"], true);
    assert_eq!(lines[0]["callState"], "declined");
    assert!(rpc(&r, &Caller::Local, "callDecline", json!({"id": id})).await.is_err(), "decline after the fact");

    // Nobody answers: it rings out. A different agent, so the rate limit isn't what is tested.
    r.hub.calls.set_ring_ms(40);
    let money = r.owner_bot("money");
    let out = ring_and_wait(&r.hub, &money, "late", true).await.unwrap();
    assert_eq!(out["status"], "missed");
    assert_eq!(notices(&r, &money)[0]["text"], "money tried to call: late");
    assert_eq!(notices(&r, &money)[0]["callState"], "missed");
    assert!(rpc(&r, &Caller::Local, "callAccept", json!({"id": out["id"]})).await.is_err(), "accept after the timeout");
}

#[tokio::test]
async fn a_person_is_on_one_call_and_an_agent_rings_rarely() {
    let r = Rig::new().await;
    let (house, money) = (r.owner_bot("house"), r.owner_bot("money"));
    let id = ring_for(&r, &house, "one")["id"].as_str().unwrap().to_owned();
    assert_eq!(ring_for(&r, &money, "two")["status"], "busy", "another agent while one rings");
    assert_eq!(ring_for(&r, &house, "again")["status"], "busy", "the same agent twice quickly");
    decline_now(&r, &id);

    // Declined, so nobody is busy, but `house` rang a minute ago.
    let out = ring_for(&r, &house, "pester");
    assert_eq!(out["status"], "rate-limited");
    assert!(out["retryAfterSeconds"].as_i64().unwrap() > 500);
    let calm = ring_for(&r, &money, "different agent");
    assert_eq!(calm["status"], "ringing");
    decline_now(&r, calm["id"].as_str().unwrap());

    // Ten minutes on it may ring again; the sixth in a day is the last.
    let now = now_ms();
    let hours = |h: i64| now - h * 60 * 60 * 1000;
    r.hub.calls.inner.locked().rung.insert(rate_key(&house, None), (1..=5).rev().map(hours).collect());
    let sixth = ring_for(&r, &house, "sixth");
    assert_eq!(sixth["status"], "ringing");
    decline_now(&r, sixth["id"].as_str().unwrap());
    r.hub
        .calls
        .inner
        .locked()
        .rung
        .get_mut(&rate_key(&house, None))
        .unwrap()
        .iter_mut()
        .for_each(|t| *t -= 11 * 60 * 1000);
    assert_eq!(ring_for(&r, &house, "seventh")["status"], "rate-limited", "six in the last day");
    assert_eq!(ring(&r.hub, "unknown", "x").unwrap(), json!({"status": "not-allowed"}));
}

fn decline_now(r: &Rig, id: &str) {
    settle(&r.hub, id, State::Declined, None).unwrap();
}

#[tokio::test]
async fn the_ring_history_survives_a_restart() {
    let dir = std::env::temp_dir().join(format!("kith-calls-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("calls.json");
    let first = Calls::open(Some(path.clone()), RING_MS);
    let rung = HashMap::from([("house".to_owned(), vec![now_ms() - 1000])]);
    first.save(&rung);
    let again = Calls::open(Some(path), RING_MS);
    assert_eq!(again.inner.locked().rung["house"].len(), 1);
    assert!(Calls::open(None, RING_MS).inner.locked().rung.is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn a_call_reaches_only_the_person_whose_chat_it_is() {
    let r = Rig::new().await;
    let mine = r.owner_bot("house");
    let (phone, theirs) = sam(&r).await;

    // The owner's call: the owner hears it, Sam does not and cannot answer it.
    let owners = ring_for(&r, &mine, "owner only");
    let id = owners["id"].as_str().unwrap().to_owned();
    assert_eq!(calls_seen(&r, &Caller::Local).await.len(), 1);
    assert_eq!(calls_seen(&r, &phone).await, Vec::<Value>::new());
    for method in ["callAccept", "callDecline", "callGreet"] {
        let e = rpc(&r, &phone, method, json!({"id": id})).await.unwrap_err();
        assert!(e.to_string().contains("gone"), "{method}: {e:#}");
    }
    decline_now(&r, &id);

    r.hub.calls.inner.locked().rung.clear();
    // Sam's call: only Sam's devices; the owner's stream and answers are shut out. Sam being called is not busy for the owner.
    let sams = ring_for(&r, &theirs, "sam only");
    assert_eq!(sams["status"], "ringing");
    let sid = sams["id"].as_str().unwrap().to_owned();
    assert_eq!(calls_seen(&r, &phone).await.len(), 1);
    assert_eq!(calls_seen(&r, &Caller::Local).await, Vec::<Value>::new(), "the owner never hears an invitee's call");
    let e = rpc(&r, &Caller::Local, "callAccept", json!({"id": sid})).await.unwrap_err();
    assert!(e.to_string().contains("gone"), "{e:#}");
    rpc(&r, &phone, "callAccept", json!({"id": sid})).await.unwrap();

    // The loopback call is the agent's alone: no device, member or not, may place one.
    assert!(rpc(&r, &phone, "callPerson", json!({"agent": "house", "reason": "x"})).await.is_err());
    let owner = r.device("owner-phone", None);
    assert!(rpc(&r, &owner, "callPerson", json!({"agent": "house", "reason": "x"})).await.is_err());
}

fn env_of(env: &[(String, String)]) -> (String, String) {
    let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).unwrap_or_default();
    (get("KITH_CALL_CHAT"), get("KITH_CALL_KEY"))
}

async fn cli(r: &Rig, env: &[(String, String)], reason: &str) -> Value {
    let (chat, key) = env_of(env);
    rpc(r, &Caller::Local, "callPerson", json!({"chat": chat, "key": key, "reason": reason})).await.unwrap()
}

#[tokio::test]
async fn every_kind_of_run_rings_only_the_person_it_serves() {
    let r = Rig::new().await;
    let mine = r.owner_bot("house");
    let (phone, theirs) = sam(&r).await;
    let owner_phone = r.device("owner-phone", None);

    // An app chat's run (a routine runs on its chat's actor, so it gets the same): the owner's chat rings the owner.
    let app = env_for(&r.hub, &mine);
    assert_eq!(cli(&r, &app, "from the app").await["status"], "ringing");
    assert_eq!(calls_seen(&r, &owner_phone).await.len(), 1);
    assert_eq!(calls_seen(&r, &phone).await, Vec::<Value>::new());
    r.hub.calls.inner.locked().calls.clear();
    r.hub.calls.inner.locked().rung.clear();

    // A channel run for the owner gets the owner's chat; for an invitee, that invitee's.
    let owner_channel = run_env(&r.hub, "house", None);
    assert_eq!(env_of(&owner_channel).0, mine);
    assert_eq!(cli(&r, &owner_channel, "owner channel").await["status"], "ringing");
    assert_eq!(calls_seen(&r, &Caller::Local).await.len(), 1);
    r.hub.calls.inner.locked().calls.clear();
    r.hub.calls.inner.locked().rung.clear();

    let sam_channel = run_env(&r.hub, "house", Some("sam"));
    assert_eq!(env_of(&sam_channel).0, theirs);
    assert_eq!(cli(&r, &sam_channel, "sam channel").await["status"], "ringing");
    assert_eq!(calls_seen(&r, &phone).await.len(), 1);
    assert_eq!(
        calls_seen(&r, &Caller::Local).await,
        Vec::<Value>::new(),
        "an invitee's channel run never reaches the owner"
    );
    r.hub.calls.inner.locked().calls.clear();
    r.hub.calls.inner.locked().rung.clear();

    // A person with no app chat with the agent, or an agent they don't have: no key at all.
    assert_eq!(run_env(&r.hub, "house", Some("kim")), Vec::<(String, String)>::new());
    assert_eq!(run_env(&r.hub, "money", None), Vec::<(String, String)>::new());
}

#[tokio::test]
async fn a_forged_or_mismatched_key_rings_nobody() {
    let r = Rig::new().await;
    let mine = r.owner_bot("house");
    let (_, theirs) = sam(&r).await;
    let (_, sam_key) = env_of(&env_for(&r.hub, &theirs));
    let (_, owner_key) = env_of(&env_for(&r.hub, &mine));
    let body = |chat: &str, key: Option<&str>| match key {
        Some(k) => json!({"chat": chat, "key": k, "reason": "hi"}),
        None => json!({"chat": chat, "reason": "hi"}),
    };
    // Sam's key on the owner's chat (the forged `KITH_PERSON=` case), the owner's on Sam's, a made-up one, a key
    // for the right chat but another person, none at all, an unknown chat.
    let forged = key(&r.hub.token, &mine, Some("sam"));
    for (chat, k) in [
        (&mine, Some(sam_key.as_str())),
        (&theirs, Some(owner_key.as_str())),
        (&mine, Some("deadbeef")),
        (&mine, Some(forged.as_str())),
        (&mine, None),
        (&"nope".to_owned(), Some(owner_key.as_str())),
    ] {
        let out = rpc(&r, &Caller::Local, "callPerson", body(chat, k)).await.unwrap();
        assert_eq!(out["status"], "not-allowed", "{chat} {k:?}");
    }
    assert_eq!(r.hub.calls.ringing(&r.hub), Vec::<Value>::new());
    let empty = rpc(&r, &Caller::Local, "callPerson", json!({"chat": mine, "key": owner_key, "reason": "  "})).await;
    assert!(empty.is_err());
}

#[tokio::test]
async fn a_run_naming_another_agent_is_shown_but_not_counted_separately() {
    let r = Rig::new().await;
    let mine = r.owner_bot("house");
    let (key_chat, key) = env_of(&env_for(&r.hub, &mine));
    let body = |from: &str| json!({"chat": key_chat, "key": key, "reason": "bug", "from": from});
    let first = rpc(&r, &Caller::Local, "callPerson", body("coder")).await.unwrap();
    assert_eq!(first["status"], "ringing");
    let name = r.hub.calls.ringing(&r.hub)[0]["call"]["name"].as_str().unwrap().to_owned();
    assert!(name.starts_with("coder (for "), "{name}");
    decline_now(&r, first["id"].as_str().unwrap());
    // A different `from` buys no extra call: the chat rang a minute ago.
    for from in ["architect", "x", "house"] {
        let out = rpc(&r, &Caller::Local, "callPerson", body(from)).await.unwrap();
        assert_eq!(out["status"], "rate-limited", "{from}");
    }
}

#[tokio::test]
async fn run_env_is_the_owners_and_mints_only_for_the_person_asked() {
    let r = Rig::new().await;
    let mine = r.owner_bot("house");
    let (phone, theirs) = sam(&r).await;
    let ask = |person: Value| json!({"agent": "house", "person": person});

    // The owner's own process: the owner's chat for no person, Sam's for Sam, nothing for a stranger.
    let own = rpc(&r, &Caller::Local, "runEnv", ask(Value::Null)).await.unwrap();
    assert_eq!(own["chat"], mine);
    assert_eq!(own["env"]["KITH_CALL_CHAT"], mine);
    assert_eq!(own["env"]["KITH_CALL_KEY"], key(&r.hub.token, &mine, None));
    assert!(own["env"].get("KITH_RUN_OUTBOX").is_none(), "no outbox unless asked");
    let sams = rpc(&r, &Caller::Local, "runEnv", ask(json!("sam"))).await.unwrap();
    assert_eq!(sams["env"]["KITH_CALL_CHAT"], theirs);
    assert_eq!(sams["env"]["KITH_CALL_KEY"], key(&r.hub.token, &theirs, Some("sam")));
    let none = rpc(&r, &Caller::Local, "runEnv", ask(json!("kim"))).await.unwrap();
    assert_eq!(none, json!({"chat": null, "env": {}}));
    let other = rpc(&r, &Caller::Local, "runEnv", json!({"agent": "money", "person": "sam"})).await.unwrap();
    assert!(other["chat"].is_null(), "an agent the person has no chat with");

    // Not a device, not an invitee's, not a bot guest's.
    assert!(rpc(&r, &phone, "runEnv", ask(json!("sam"))).await.is_err());
    assert!(rpc(&r, &r.device("owner-phone", None), "runEnv", ask(Value::Null)).await.is_err());
}

#[tokio::test]
async fn an_accepted_call_that_never_starts_frees_the_person() {
    let r = Rig::new().await;
    let (house, money) = (r.owner_bot("house"), r.owner_bot("money"));
    let id = ring_for(&r, &house, "x")["id"].as_str().unwrap().to_owned();
    accept(&r.hub, None, &id).unwrap();
    assert_eq!(ring_for(&r, &money, "y")["status"], "busy");
    r.hub.calls.inner.locked().calls.get_mut(&id).unwrap().settled -= 2 * GREET_MS;
    assert_eq!(ring_for(&r, &money, "y")["status"], "ringing", "no callGreet came");
}

#[tokio::test]
async fn a_voice_call_line_is_written_at_the_start_and_filled_in_at_the_end() {
    let r = Rig::new().await;
    let bot = r.owner_bot("house");
    let started = rpc(&r, &Caller::Local, "startCall", json!({"botId": bot})).await.unwrap();
    let id = started["entry"]["id"].as_str().unwrap().to_owned();
    assert_eq!(started["entry"]["data"]["callLive"], true);
    // What is said during the call lands after the line.
    let said = r.hub.add_entry(&Lane::main(&bot), EntryKind::Notice, 1, &json!({"text": "during"})).unwrap();

    let ended = rpc(&r, &Caller::Local, "endCall", json!({"botId": bot, "entryId": id, "seconds": 16})).await.unwrap();
    assert_eq!(ended["entry"]["data"]["callSeconds"], 16);
    assert!(ended["entry"]["data"]["callLive"].is_null());
    assert!(ended["entry"]["seq"].as_i64().unwrap() < said.seq, "the record keeps its place");
    // A retry changes nothing.
    let again = rpc(&r, &Caller::Local, "endCall", json!({"botId": bot, "entryId": id, "seconds": 99})).await.unwrap();
    assert_eq!(again["entry"]["data"]["callSeconds"], 16);
    assert_eq!(notices(&r, &bot).len(), 2);
    // No entry (a call that never connected) writes nothing; a foreign id is refused.
    rpc(&r, &Caller::Local, "endCall", json!({"botId": bot, "seconds": 3})).await.unwrap();
    assert!(rpc(&r, &Caller::Local, "endCall", json!({"botId": bot, "entryId": "nope"})).await.is_err());
    assert_eq!(notices(&r, &bot).len(), 2);
    // A notice that is not a call line is not stamped with a length.
    let plain = r.hub.add_entry(&Lane::main(&bot), EntryKind::Notice, 1, &json!({"text": "hello"})).unwrap();
    let stamp = rpc(&r, &Caller::Local, "endCall", json!({"botId": bot, "entryId": plain.id, "seconds": 5})).await;
    assert!(stamp.is_err());
    assert!(r.hub.store.entry(&plain.id).unwrap().data["callSeconds"].is_null());
}
