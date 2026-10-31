//! Jev reply cards: the splitter and prefilter, what Jev's answers must clear, that only the reply's own words reach a
//! card, each gate, and Jev missing or down. Jev here is a fake on a local port; the real API is never called.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::LockExt;
use crate::store::{EntryKind, Lane};
use axum::{Json, extract::State, routing::post};
use std::sync::Mutex;

const REPLY: &str = "Disk check on the Mac mini\n\n- Free space: 340 GB of 500 GB\n- Memory: 12 GB in use\n- Last backup: today 03:10\n- Full report: [Open report](https://example.com/report/1).\n\nNothing else needs attention right now, the machine looks healthy overall.";

/// Jev's answers for `REPLY`'s blocks: a result, the first block its title, items 2 and 3 facts, item 5 a link.
fn answers() -> Value {
    json!({
        "kind": {"choice": "info", "probabilities": {"info": 0.95, "plain": 0.03, "digest": 0.01, "draft": 0.01}},
        "title": {"choice": "1", "probabilities": {"1": 0.9}},
        "fact_2": {"noul": 0.9}, "fact_3": {"noul": 0.8}, "fact_4": {"noul": 0.2},
        "link_5": {"noul": 0.9},
    })
}

fn blocks() -> Vec<Block> {
    split(REPLY)
}

/// A fake Jev that answers `reply` to anything and keeps the requests it got.
async fn fake_jev(reply: Value) -> (Jev, Arc<Mutex<Vec<Value>>>) {
    #[allow(clippy::unused_async)] // axum handlers are async
    async fn handler(
        State((reply, seen)): State<(Value, Arc<Mutex<Vec<Value>>>)>,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        seen.locked().push(body);
        Json(json!({"answers": reply}))
    }
    let seen = Arc::new(Mutex::new(Vec::new()));
    let app = axum::Router::new().route("/v1/systemone", post(handler)).with_state((reply, seen.clone()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    (Jev::at("jev-test-key".into(), &base, "jev-test".into()), seen)
}

fn hub() -> Arc<Hub> {
    let dir = std::env::temp_dir().join(format!("kith-jev-cards-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(std::path::Path::new(":memory:")).unwrap();
    let identity = crate::remote::identity::Identity::load_or_create(&dir).unwrap();
    Hub::new(store, "jev-cards-test".into(), identity, "test".into(), 0)
}

fn reply_entry(hub: &Arc<Hub>, text: &str) -> String {
    hub.add_entry(&Lane::main("bot"), EntryKind::Agent, 1, &json!({"text": text, "final": true})).unwrap().id
}

#[test]
fn a_reply_splits_into_numbered_blocks() {
    let b = split(
        "# Plan\n\nFirst we look.\nThen we go.\n\n1. one\n   more of one\n2) two\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\nStatus: green\n\n```\ncode here\n```\n> quoted\n**Done**",
    );
    let shape: Vec<(usize, Kind, &str)> = b.iter().map(|b| (b.i, b.kind, b.text.as_str())).collect();
    assert_eq!(
        shape,
        [
            (1, Kind::Heading, "Plan"),
            (2, Kind::Para, "First we look. Then we go."),
            (3, Kind::Item, "one more of one"),
            (4, Kind::Item, "two"),
            (5, Kind::Row, "a | b"),
            (6, Kind::Row, "1 | 2"),
            (7, Kind::Kv, "Status: green"),
            (8, Kind::Code, "code here"),
            (9, Kind::Quote, "quoted"),
            (10, Kind::Heading, "Done"),
        ]
    );
}

#[test]
fn a_short_or_single_block_reply_is_skipped_before_jev() {
    let one_long_block = "A".repeat(400);
    for text in ["Done, three files changed.", one_long_block.as_str()] {
        assert!(!prefilter(text, &split(text)), "{text}");
    }
    assert!(prefilter(REPLY, &blocks()));
}

#[test]
fn only_a_clear_result_with_a_title_and_a_fact_becomes_a_card() {
    let b = blocks();
    let card = card_from(&b, &answers()).unwrap();
    assert_eq!(card["type"], "result");
    assert_eq!(card["snapshot"]["title"], "Disk check on the Mac mini");
    assert_eq!(card["snapshot"]["facts"], json!(["Free space: 340 GB of 500 GB", "Memory: 12 GB in use"]));
    assert_eq!(card["snapshot"]["links"], json!([{"label": "Open report", "url": "https://example.com/report/1"}]));

    let with = |path: &[&str], v: Value| {
        let mut a = answers();
        let mut at = &mut a;
        for k in path {
            at = &mut at[*k];
        }
        *at = v;
        card_from(&b, &a)
    };
    // Each pick must clear its threshold.
    assert!(with(&["kind", "probabilities", "info"], json!(0.84)).is_none());
    assert!(with(&["kind", "choice"], json!("plain")).is_none());
    assert!(with(&["title", "probabilities", "1"], json!(0.59)).is_none());
    assert!(
        with(&["fact_2", "noul"], json!(0.49))
            .is_some_and(|c| c["snapshot"]["facts"] == json!(["Memory: 12 GB in use"]))
    );
    // No fact clears: the title alone isn't a card.
    let mut none = answers();
    none["fact_2"]["noul"] = json!(0.1);
    none["fact_3"]["noul"] = json!(0.1);
    assert!(card_from(&b, &none).is_none());
    // A title that names no block, or a block that can't be one.
    assert!(with(&["title", "choice"], json!("99")).is_none());
    assert!(with(&["title", "choice"], json!("not a number")).is_none());
    assert!(card_from(&b, &json!({})).is_none());
}

#[test]
fn every_string_on_a_card_comes_from_the_reply() {
    let b = split(
        "**Backup status**\n\n- **Disk:** `340 GB` free\n- Last run: [see log](https://example.com/log)\n- Next run: tomorrow 03:10\n\nThis paragraph is the reply's closing remark and is much longer than any fact line may be, so it can't fill a slot.",
    );
    let mut a = answers();
    a["fact_5"] = json!({"noul": 0.99});
    a["link_3"] = json!({"noul": 0.9});
    a["link_5"] = json!({"noul": 0.0});
    a["fact_2"] = json!({"noul": 0.9});
    a["fact_3"] = json!({"noul": 0.9});
    a["fact_4"] = json!({"noul": 0.9});
    a["title"] = json!({"choice": "1", "probabilities": {"1": 0.9}});
    let card = card_from(&b, &a).unwrap();
    let snap = &card["snapshot"];
    assert_eq!(snap["title"], "Backup status");
    // Marks removed, no word added; the long closing paragraph (block 5) never fills a slot.
    assert_eq!(snap["facts"], json!(["Disk: 340 GB free", "Last run: see log", "Next run: tomorrow 03:10"]));
    assert_eq!(snap["links"], json!([{"label": "see log", "url": "https://example.com/log"}]));
    for s in snap["facts"].as_array().unwrap().iter().chain([&snap["title"]]) {
        let s = s.as_str().unwrap();
        assert!(s.split_whitespace().all(|w| b.iter().any(|b| b.text.contains(w))), "{s}");
    }
}

#[test]
fn what_jev_is_sent_is_capped_and_leaves_code_out_of_the_slots() {
    let long = (1..=100).map(|i| format!("- Item {i}: {i} GB")).collect::<Vec<_>>().join("\n");
    let (state, q) = ask_for(&split(&format!("Title\n\n```\nsecret = 1\n```\n\n{long}")));
    assert_eq!(state["reply"].as_array().unwrap().len(), MAX_BLOCKS);
    assert!(q.contains_key("kind") && q.contains_key("title"));
    assert!(!q.contains_key("fact_2"), "a code block is never a fact");
    assert!(!q.contains_key("fact_61"));
    assert!(q.contains_key("fact_60"));
}

#[test]
fn each_gate_alone_keeps_the_reply_out_of_jev() {
    let store = Store::open(std::path::Path::new(":memory:")).unwrap();
    let ok = Run { owner: true, owner_only_room: true, touched_secrets: false };
    assert!(refusal(&store, ok).is_some(), "off by default");
    assert!(!enabled(&store));

    set_enabled(&store, true).unwrap();
    assert!(enabled(&store));
    assert_eq!(refusal(&store, ok), None);
    assert!(refusal(&store, Run { owner: false, ..ok }).is_some(), "a person's or bot guest's run");
    assert!(refusal(&store, Run { owner_only_room: false, ..ok }).is_some(), "a room with others");
    assert!(refusal(&store, Run { touched_secrets: true, ..ok }).is_some(), "secrets");

    set_enabled(&store, false).unwrap();
    assert!(refusal(&store, ok).is_some());
}

#[test]
fn a_key_or_access_card_in_the_turn_counts_as_touching_secrets() {
    let hub = hub();
    let lane = Lane::main("bot");
    let before = crate::store::now_ms();
    assert!(!raised_secret_card(&hub.store, "bot", before));
    hub.add_entry(&lane, EntryKind::Permission, 1, &json!({"card": "key", "status": "pending"})).unwrap();
    assert!(raised_secret_card(&hub.store, "bot", before));
    assert!(!raised_secret_card(&hub.store, "bot", before + 60_000), "a card from an earlier turn doesn't count");
    hub.add_entry(&Lane::main("other"), EntryKind::Permission, 1, &json!({"card": "account"})).unwrap();
    assert!(raised_secret_card(&hub.store, "other", before), "an Access card counts too");
    assert!(!raised_secret_card(&hub.store, "third", before), "another chat's card doesn't");
}

#[tokio::test]
async fn an_answered_reply_gets_its_card_once_and_keeps_its_text() {
    let hub = hub();
    let (jev, seen) = fake_jev(answers()).await;
    let id = reply_entry(&hub, REPLY);
    attach(hub.clone(), id.clone(), REPLY.into(), Some(jev)).await;
    let e = hub.store.entry(&id).unwrap();
    assert_eq!(e.data["text"], REPLY);
    assert_eq!(e.data["final"], true);
    assert_eq!(e.data["richCard"]["type"], "result");
    assert_eq!(seen.locked().len(), 1);
    // The state sent is the numbered blocks of the reply and nothing else.
    assert_eq!(
        seen.locked()[0]["state"]["reply"][0],
        json!({"block": 1, "kind": "para", "text": "Disk check on the Mac mini"})
    );

    // A card already there is never replaced.
    let mut own = e.data.clone();
    own["richCard"] = json!({"type": "schedule", "v": 1});
    hub.set_entry(&id, &own);
    let (jev, _) = fake_jev(answers()).await;
    attach(hub.clone(), id.clone(), REPLY.into(), Some(jev)).await;
    assert_eq!(hub.store.entry(&id).unwrap().data["richCard"]["type"], "schedule");
}

#[tokio::test]
async fn a_plain_reply_never_reaches_jev() {
    let hub = hub();
    let (jev, seen) = fake_jev(answers()).await;
    let id = reply_entry(&hub, "Sure, done.");
    attach(hub.clone(), id.clone(), "Sure, done.".into(), Some(jev)).await;
    assert!(seen.locked().is_empty());
    assert!(hub.store.entry(&id).unwrap().data["richCard"].is_null());
}

#[tokio::test]
async fn an_unsure_jev_leaves_the_reply_plain() {
    let hub = hub();
    let mut a = answers();
    a["kind"] = json!({"choice": "plain", "probabilities": {"info": 0.2, "plain": 0.8}});
    let (jev, seen) = fake_jev(a).await;
    let id = reply_entry(&hub, REPLY);
    attach(hub.clone(), id.clone(), REPLY.into(), Some(jev)).await;
    assert_eq!(seen.locked().len(), 1);
    assert!(hub.store.entry(&id).unwrap().data["richCard"].is_null());
}

#[tokio::test]
async fn jev_down_or_missing_is_no_card_and_no_error() {
    let hub = hub();
    // Nothing listens on this port.
    let down = Jev::at("jev-test-key".into(), "http://127.0.0.1:1", "jev-test".into());
    let id = reply_entry(&hub, REPLY);
    attach(hub.clone(), id.clone(), REPLY.into(), Some(down)).await;
    let e = hub.store.entry(&id).unwrap();
    assert!(e.data["richCard"].is_null());
    assert_eq!(e.data["text"], REPLY);
    // A body that isn't Jev's answer.
    let (jev, _) = fake_jev(json!("nonsense")).await;
    attach(hub.clone(), id.clone(), REPLY.into(), Some(jev)).await;
    assert!(hub.store.entry(&id).unwrap().data["richCard"].is_null());
}

#[tokio::test]
async fn a_reply_that_changed_while_jev_thought_gets_no_card() {
    let hub = hub();
    let (jev, _) = fake_jev(answers()).await;
    let id = reply_entry(&hub, "something else entirely");
    attach(hub.clone(), id.clone(), REPLY.into(), Some(jev)).await;
    assert!(hub.store.entry(&id).unwrap().data["richCard"].is_null());
}

#[test]
fn the_switch_is_off_until_the_owner_turns_it_on() {
    let store = Store::open(std::path::Path::new(":memory:")).unwrap();
    assert_eq!(status(&store), json!({"enabled": false}));
    assert_eq!(set_enabled(&store, true).unwrap(), json!({"enabled": true}));
    assert_eq!(set_enabled(&store, false).unwrap(), json!({"enabled": false}));
}
