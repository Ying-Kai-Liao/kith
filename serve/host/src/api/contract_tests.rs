//! The host RPC contract (serve/contract/host/contract.json) against the host itself: an invited person calls the
//! member methods for real, and what they get back is the contract's (the producer check in `member_call`) and is what
//! the shared fixtures in serve/contract/host/fixtures/ are made of. The apps read those fixtures in their own tests,
//! so a change to what the host sends is seen there.
//!
//! `KITH_WRITE_FIXTURES=1 cargo test api::contract_tests` writes the fixtures again from today's answers; otherwise the
//! test checks that each answer still has the shape of its fixture (the same fields, at every depth).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::devices::Caller;
use super::rig::Rig;
use super::{dispatch, roles};
use crate::remote::members::{self, Listed};
use crate::store::{EntryKind, Lane};
use kith_wire::check::Mode;
use kith_wire::rpc::{self, SCHEMA};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;

async fn call(r: &Rig, caller: &Caller, method: &str, b: Value) -> Value {
    dispatch(&r.hub, caller, method, b).await.unwrap_or_else(|e| panic!("{method}: {e:#}"))
}

/// `sam`, invited with `house` as a member, bound to `user_sam`, on one phone.
async fn sam(r: &Rig) -> Caller {
    let made = dispatch(&r.hub, &Caller::Local, "createInvite", json!({"name": "Sam", "agents": {"house": "member"}}))
        .await
        .unwrap();
    let listed = [Listed {
        membership_id: "mem_sam".into(),
        user_id: "user_sam".into(),
        invite_id: made["inviteId"].as_str().unwrap().to_owned(),
        ..Default::default()
    }];
    members::apply(&r.hub, Some(&r.bin), &listed, Some("user_owner")).await.unwrap();
    r.device("sam-phone", Some("sam"))
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../contract/host/fixtures")
}

/// What a value is made of, without its values: an object's fields and theirs, a list's first item.
fn shape(v: &Value) -> Value {
    match v {
        Value::Object(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), shape(v))).collect()),
        Value::Array(xs) => json!([xs.first().map(shape)]),
        Value::String(_) => json!("string"),
        Value::Number(_) => json!("number"),
        Value::Bool(_) => json!("bool"),
        Value::Null => Value::Null,
    }
}

/// The rig's fresh ids and keys swapped for fixed stand-ins: a fixture keeps their shape, and secret scanners flag
/// the real ones.
fn with_stand_in_keys(body: &Value) -> Value {
    let mut body = body.clone();
    for (field, stand_in) in [
        ("computerId", "AAAAAAAAAAAAAAAAAAAAAA"),
        ("signKey", "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
        ("boxKey", "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE"),
    ] {
        if let Some(v) = body.get_mut(field) {
            *v = json!(stand_in);
        }
    }
    body
}

/// Each member method an invited person calls, with what it answered (one answer per method).
async fn member_answers(r: &Rig, sam: &Caller) -> BTreeMap<&'static str, Value> {
    let mut got = BTreeMap::new();
    got.insert("hello", call(r, sam, "hello", json!({})).await);
    let started = call(r, sam, "startChat", json!({"agent": "house"})).await;
    let chat = started["bot"]["id"].as_str().unwrap().to_owned();
    got.insert("startChat", started);
    let lane = Lane::main(&chat);
    let said = r.hub.add_entry(&lane, EntryKind::User, 1, &json!({"text": "hi", "author": "sam"})).unwrap();
    let answer =
        r.hub.add_entry(&lane, EntryKind::Agent, 1, &json!({"text": "hello", "final": true, "author": chat})).unwrap();
    got.insert("history", call(r, sam, "history", json!({"botId": chat})).await);
    got.insert("thread", call(r, sam, "thread", json!({"botId": chat, "rootId": said.id})).await);
    got.insert("react", call(r, sam, "react", json!({"entryId": answer.id, "emoji": "👍"})).await);
    got.insert("markRead", call(r, sam, "markRead", json!({"botId": chat, "all": true})).await);
    got.insert("stop", call(r, sam, "stop", json!({"botId": chat})).await);
    got.insert("kithInfo", call(r, sam, "kithInfo", json!({"botId": chat})).await);
    got.insert("projects", call(r, sam, "projects", json!({})).await);
    got.insert("threadProjects", call(r, sam, "threadProjects", json!({"botId": chat})).await);
    got.insert("linkCode", call(r, sam, "linkCode", json!({})).await);
    got.insert("myBots", call(r, sam, "myBots", json!({})).await);
    got.insert("logCall", call(r, sam, "logCall", json!({"botId": chat, "seconds": 42})).await);
    let upload = json!({"botId": chat, "uploadId": "6f1c0e2a-5b7d-4c1e-9a3f-2d8b4e6a0c11", "name": "note.txt", "data": "aGk=", "offset": 0, "done": true});
    got.insert("upload", call(r, sam, "upload", upload).await);
    got.insert(
        "readUpload",
        call(r, sam, "readUpload", json!({"botId": chat, "uploadId": "6f1c0e2a-5b7d-4c1e-9a3f-2d8b4e6a0c11"})).await,
    );
    got.insert(
        "resolveCitation",
        call(r, sam, "resolveCitation", json!({"citation": "kith://thread/none#entry=1"})).await,
    );
    // A message stopped before the bot took it.
    let held =
        r.hub.add_entry(&lane, EntryKind::User, 2, &json!({"text": "later", "status": "cancelled", "author": "sam"}));
    let unsend = json!({"botId": chat, "entryId": held.unwrap().id});
    got.insert("unsend", call(r, sam, "unsend", unsend).await);
    got.insert("newSession", call(r, sam, "newSession", json!({"botId": chat})).await);
    got
}

#[tokio::test]
async fn the_member_methods_answer_as_the_contract_and_its_fixtures_say() {
    let r = Rig::new().await;
    r.owner_bot("house");
    let sam = sam(&r).await;
    let answers = member_answers(&r, &sam).await;
    let write = std::env::var_os("KITH_WRITE_FIXTURES").is_some();
    let dir = fixtures_dir();
    for (method, body) in &answers {
        let m = SCHEMA.method(method).unwrap();
        assert!(m.who.contains(&"member"), "{method}");
        // `member_call` already ran the producer check; this one names the method when it fails here.
        let problems = SCHEMA.check(m.result, body, Mode::Producer);
        assert_eq!(problems, Vec::<String>::new(), "{method}");
        let path = dir.join(format!("{method}.json"));
        if write {
            let fixture = json!({"method": method, "type": m.result, "body": with_stand_in_keys(body)});
            std::fs::write(&path, serde_json::to_string_pretty(&fixture).unwrap() + "\n").unwrap();
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap_or_else(|_| {
            panic!("no fixture for {method}: KITH_WRITE_FIXTURES=1 cargo test api::contract_tests")
        });
        let fixture: Value = serde_json::from_str(&text).unwrap();
        assert_eq!((fixture["method"].as_str(), fixture["type"].as_str()), (Some(*method), Some(m.result)));
        assert_eq!(
            shape(&fixture["body"]),
            shape(body),
            "{method} answers in a new shape: KITH_WRITE_FIXTURES=1 cargo test api::contract_tests, and see that every \
             app still reads it"
        );
    }
}

#[test]
fn the_member_lists_are_the_contracts() {
    for m in rpc::MEMBER_METHODS {
        assert!(SCHEMA.method(m).is_some_and(|s| s.who.contains(&"member")), "{m}");
        assert!(roles::member_may(m), "{m}");
    }
    for m in rpc::UNPROVEN_METHODS {
        assert!(SCHEMA.method(m).is_some_and(|s| s.who.contains(&"unproven")), "{m}");
    }
    for m in SCHEMA.methods {
        assert_eq!(m.who.contains(&"member"), rpc::MEMBER_METHODS.contains(&m.name), "{}", m.name);
    }
}

/// The host's own enums and the contract's words agree: what the host writes is a word the contract has, and every
/// word the contract has is one the host writes. Each list is run through a `match` on its enum with no wildcard, so a
/// new variant fails to build here until it is in the list, and then the test until the contract has its word.
#[test]
fn the_hosts_words_are_the_contracts() {
    fn words(name: &str) -> Vec<String> {
        SCHEMA.enum_schema(name).unwrap_or_else(|| panic!("{name}")).values.iter().map(|w| (*w).to_owned()).collect()
    }
    fn said<T: serde::Serialize>(all: &[T]) -> Vec<String> {
        all.iter().map(|x| serde_json::to_value(x).unwrap().as_str().unwrap().to_owned()).collect()
    }
    use crate::hub::BotStatus;
    use crate::service::Device;
    use crate::store::{BotKind, BotState, Permission};

    let every = |p: &Permission| match p {
        Permission::Ask
        | Permission::AcceptEdits
        | Permission::Plan
        | Permission::Auto
        | Permission::BypassPermissions => {}
    };
    let permissions =
        [Permission::Ask, Permission::AcceptEdits, Permission::Plan, Permission::Auto, Permission::BypassPermissions];
    permissions.iter().for_each(every);
    assert_eq!(said(&permissions), words("Permission"));

    let every = |s: &BotStatus| match s {
        BotStatus::Idle | BotStatus::Working | BotStatus::NeedsInput | BotStatus::Error => {}
    };
    let statuses = [BotStatus::Idle, BotStatus::Working, BotStatus::NeedsInput, BotStatus::Error];
    statuses.iter().for_each(every);
    assert_eq!(said(&statuses), words("BotStatus"));

    let every = |s: &BotState| match s {
        BotState::Invited
        | BotState::Joining
        | BotState::Pending
        | BotState::Active
        | BotState::Denied
        | BotState::Revoked => {}
    };
    let states = [
        BotState::Invited,
        BotState::Joining,
        BotState::Pending,
        BotState::Active,
        BotState::Denied,
        BotState::Revoked,
    ];
    states.iter().for_each(every);
    assert_eq!(said(&states), words("BotGuestState"));

    let every = |k: &EntryKind| match k {
        EntryKind::User
        | EntryKind::Agent
        | EntryKind::Thought
        | EntryKind::Tool
        | EntryKind::Plan
        | EntryKind::Permission
        | EntryKind::Notice => {}
    };
    let kinds = [
        EntryKind::User,
        EntryKind::Agent,
        EntryKind::Thought,
        EntryKind::Tool,
        EntryKind::Plan,
        EntryKind::Permission,
        EntryKind::Notice,
    ];
    kinds.iter().for_each(every);
    assert_eq!(kinds.iter().map(|k| k.as_str().to_owned()).collect::<Vec<_>>(), words("EntryKind"));

    let every = |k: &BotKind| match k {
        BotKind::Agent | BotKind::Group => {}
    };
    let bot_kinds = [BotKind::Agent, BotKind::Group];
    bot_kinds.iter().for_each(every);
    assert_eq!(said(&bot_kinds), words("BotKind"));

    let every = |d: &Device| match d {
        Device::Laptop
        | Device::MacMini
        | Device::MacStudio
        | Device::IMac
        | Device::MacPro
        | Device::Desktop
        | Device::Linux => {}
    };
    let devices = [
        Device::Laptop,
        Device::MacMini,
        Device::MacStudio,
        Device::IMac,
        Device::MacPro,
        Device::Desktop,
        Device::Linux,
    ];
    devices.iter().for_each(every);
    assert_eq!(said(&devices), words("Device"));
}
