#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::api::devices::Caller;
use crate::remote::identity::Identity;
use crate::store::{Entry, Store};

fn hub() -> Arc<Hub> {
    let dir = std::env::temp_dir().join(format!("kith-samples-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(std::path::Path::new(":memory:")).unwrap();
    Hub::new(store, "t".into(), Identity::load_or_create(&dir).unwrap(), "t".into(), 0)
}

fn entries(hub: &Hub, chat: &str) -> Vec<Entry> {
    let mut all = hub.store.history(chat, i64::MAX, 1000).unwrap();
    all.sort_by_key(|e| e.seq);
    all
}

fn live_samples(hub: &Hub) -> Vec<String> {
    let rows = hub.store.bots().unwrap().into_iter().filter(|b| !b.deleted);
    rows.filter(|b| b.config.name == "Samples").map(|b| b.config.id).collect()
}

#[tokio::test]
async fn every_card_is_posted_once_into_one_samples_chat() {
    let hub = hub();
    let first = post(&hub).unwrap()["botId"].as_str().unwrap().to_owned();
    let got = post(&hub).unwrap();
    let chat = got["botId"].as_str().unwrap().to_owned();
    assert_ne!(first, chat, "a second run replaces the chat");
    assert_eq!(live_samples(&hub), vec![chat.clone()], "one Samples chat after two runs");

    let posted = entries(&hub, &chat);
    assert_eq!(posted.len(), cards(&chat).len());
    assert_eq!(got["cards"], posted.len());
    assert!(posted.iter().all(|e| e.data["sample"] == true));
    for (i, what) in [(0, "schedule"), (1, "routine"), (2, "news")] {
        assert!(posted[i].data["richCard"].is_object(), "the {what} sample has its card");
    }
    let permissions = posted.iter().filter(|e| e.kind == "permission").count();
    assert_eq!(permissions, 6, "approval, decision, access, key and two limits");
}

#[tokio::test]
async fn sample_buttons_do_nothing() {
    let hub = hub();
    let chat = post(&hub).unwrap()["botId"].as_str().unwrap().to_owned();
    let before = entries(&hub, &chat);
    let by_kind = |kind: &str| before.iter().filter(|e| e.kind == kind).map(|e| e.id.clone()).collect::<Vec<_>>();
    let mut calls = vec![];
    for id in by_kind("permission") {
        calls.push(("respondPermission", json!({"entryId": id, "optionId": "allow"})));
        calls.push(("respondPermission", json!({"entryId": id, "optionId": "raise"})));
        calls.push(("provideSecret", json!({"entryId": id, "account": "work", "values": {"GITHUB_TOKEN": "x"}})));
    }
    for id in by_kind("notice") {
        calls.push(("editHandoff", json!({"entryId": id, "text": "changed"})));
        calls.push(("connectorRequestFinish", json!({"entryId": id})));
        calls.push(("keepConversation", json!({"entryId": id})));
    }
    for m in BY_CHAT.iter().filter(|m| **m != "updateBot") {
        calls.push((m, json!({"botId": chat, "text": "hi", "id": "sample-routine", "enabled": false})));
    }
    calls.push(("updateBot", json!({"id": chat, "name": "Mine now"})));
    for (method, b) in calls {
        let err = crate::api::dispatch(&hub, &Caller::Local, method, b).await.expect_err(method);
        assert!(format!("{err:#}").contains("sample card"), "{method}: {err:#}");
    }
    let after = entries(&hub, &chat);
    assert_eq!(
        before.iter().map(|e| (&e.id, &e.data)).collect::<Vec<_>>(),
        after.iter().map(|e| (&e.id, &e.data)).collect::<Vec<_>>(),
        "no sample entry changed and none was added"
    );
}

#[test]
fn other_chats_are_not_refused() {
    let hub = hub();
    post(&hub).unwrap();
    assert!(refuse(&hub, "routines", &json!({"botId": "another"})).is_ok());
    assert!(refuse(&hub, "respondPermission", &json!({"entryId": "missing"})).is_ok());
}

#[test]
fn only_the_owner_may_post_samples() {
    assert!(!crate::api::roles::MEMBER_METHODS.contains(&"sampleCards"));
    assert!(!crate::api::roles::BOT_METHODS.contains(&"sampleCards"));
}
