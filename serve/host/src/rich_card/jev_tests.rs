//! Jev reply cards: the splitter and prefilter, what Jev's answers must clear, that only the reply's own words reach a
//! card, each gate, and Jev missing or down. Jev here is a fake on a local port; the real API is never called.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::LockExt;
use crate::store::{EntryKind, Lane};
use axum::{Json, extract::State, routing::post};
use std::sync::Mutex;

const REPLY: &str = "Disk check on the Mac mini\n\n- Free space: 340 GB of 500 GB\n- Memory: 12 GB in use\n- Last backup: today 03:10\n- Full report: [Open report](https://example.com/report/1).\n\nNothing else needs attention right now, the machine looks healthy overall.";

/// Jev's answers for `REPLY`'s blocks: a result, the first block its title, the closing remark its verdict, items 2
/// and 3 needed rows (item 4 not needed, item 5 the report link), block 5 also a link.
fn answers() -> Value {
    json!({
        "kind": {"choice": "info", "probabilities": {"info": 0.95, "plain": 0.03, "digest": 0.01, "draft": 0.01}},
        "title": {"choice": "1", "probabilities": {"1": 0.9}},
        "headline": {"choice": "6", "probabilities": {"6": 0.8, "none": 0.1}},
        "need_2": {"noul": 0.9}, "need_3": {"noul": 0.8}, "need_4": {"noul": 0.2}, "need_5": {"noul": 0.4},
        "skip_2": {"noul": 0.05}, "skip_3": {"noul": 0.05}, "skip_4": {"noul": 0.05}, "skip_5": {"noul": 0.1},
        "link_5": {"noul": 0.9},
    })
}

/// A real reply (the attachment of the v2 brief): a heading, a bold verdict line, a table with a header row, `Label:
/// value` items, a log-style monitor list and suggestions.
const DEVOPS: &str = include_str!("jev_fixture_devops.md");

/// What Jev answers for `DEVOPS` (block numbers as `split` numbers them): the heading as the title, the bold line
/// (2) as the verdict; the five table rows and the list items rated by need and exclusion.
fn devops_answers() -> Value {
    let mut a = json!({
        "kind": {"choice": "info", "probabilities": {"info": 0.97}},
        "title": {"choice": "1", "probabilities": {"1": 0.9, "2": 0.05}},
        "headline": {"choice": "2", "probabilities": {"2": 0.88, "none": 0.05}},
    });
    // (block, need, skip)
    for (n, need, skip) in [
        (3, 0.95, 0.02), // Disk (internal)
        (4, 0.2, 0.05),  // Disk (external): fine
        (5, 0.4, 0.05),  // Memory: no pressure
        (6, 0.9, 0.03),  // Swap
        (7, 0.6, 0.05),  // CPU
        (9, 0.35, 0.05),
        (10, 0.2, 0.05),
        (11, 0.2, 0.05),
        (12, 0.1, 0.05),
        (18, 0.7, 0.1), // Two problems are open
    ] {
        a[format!("need_{n}")] = json!({"noul": need});
        a[format!("skip_{n}")] = json!({"noul": skip});
    }
    a
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
    // The table's header row is never a block.
    assert_eq!(
        shape,
        [
            (1, Kind::Heading, "Plan"),
            (2, Kind::Para, "First we look. Then we go."),
            (3, Kind::Item, "one more of one"),
            (4, Kind::Item, "two"),
            (5, Kind::Row, "1 | 2"),
            (6, Kind::Kv, "Status: green"),
            (7, Kind::Code, "code here"),
            (8, Kind::Quote, "quoted"),
            (9, Kind::Heading, "Done"),
        ]
    );
    assert_eq!(b[4].pair, Some(("1".into(), "2".into())));
    assert_eq!(b[5].pair, Some(("Status".into(), "green".into())));
    assert_eq!(b[2].pair, None);
}

#[test]
fn the_devops_reply_splits_into_a_verdict_and_rows_with_labels_and_values() {
    let b = split(DEVOPS);
    let at = |i: usize| &b[i - 1];
    assert_eq!((at(1).kind, at(1).text.as_str()), (Kind::Heading, "Machine health, 2026-11-01 00:25"));
    // The bold lead line stays a candidate for the headline, its marks gone from the block text.
    assert_eq!(
        (at(2).kind, at(2).text.as_str()),
        (Kind::Heading, "Overall: OK, but the disk is trending down and swap is in use.")
    );
    assert_eq!(at(2).pair, None, "a verdict is not a row");
    // The table's header (`| | Now | Note |`) is gone; each body row is a block with its first cell and the next
    // non-empty one.
    assert!(b.iter().all(|b| !b.text.contains("Now |")));
    for (i, label, value) in [
        (3, "Disk (internal)", "22.7 GB free (10%) of 228 GB"),
        (6, "Swap", "2.9 of 4.0 GB used"),
        (7, "CPU", "Load 9.9, 9.0, 5.8 on 10 cores"),
    ] {
        assert_eq!(at(i).kind, Kind::Row);
        assert_eq!(at(i).pair, Some((label.into(), value.into())), "block {i}");
    }
    // `Label: value` items: bold and code marks and the closing full stop come off.
    assert_eq!(at(9).pair, Some(("Claude".into(), "3.8 GB across 11 processes".into())));
    assert_eq!(at(10).pair, Some(("rustc".into(), "1.3 GB".into())));
    // Prose items are not rows.
    assert_eq!(at(15).pair, None);
    assert_eq!(at(20).pair, None);
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
fn only_a_clear_result_with_a_title_and_rows_becomes_a_card() {
    let b = blocks();
    let card = card_from(&b, &answers()).unwrap();
    assert_eq!(card["type"], "result");
    assert_eq!(card["v"], 2);
    let snap = &card["snapshot"];
    assert_eq!(snap["title"], "Disk check on the Mac mini");
    assert_eq!(snap["sub"], "Nothing else needs attention right now, the machine looks healthy overall.");
    assert_eq!(
        snap["rows"],
        json!([{"label": "Free space", "value": "340 GB of 500 GB"}, {"label": "Memory", "value": "12 GB in use"}])
    );
    assert_eq!(snap["links"], json!([{"label": "Open report", "url": "https://example.com/report/1"}]));

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
    // An unsure, absent or title-repeating verdict only drops `sub`; the card stands.
    for card in [
        with(&["headline", "probabilities", "6"], json!(0.59)),
        with(&["headline", "choice"], json!("none")),
        with(&["headline", "choice"], json!("1")),
        with(&["headline"], json!(null)),
    ] {
        assert!(card.is_some_and(|c| c["snapshot"].get("sub").is_none()));
    }
    // Exclusion vetoes; a row that isn't needed enough is not shown.
    assert!(
        with(&["skip_3", "noul"], json!(0.7)).is_some_and(|c| c["snapshot"]["rows"].as_array().unwrap().len() == 1)
    );
    assert!(with(&["need_2", "noul"], json!(0.59)).is_some_and(|c| c["snapshot"]["rows"][0]["label"] == "Memory"));
    assert!(with(&["need_4", "noul"], json!(0.6)).is_some_and(|c| c["snapshot"]["rows"][2]["label"] == "Last backup"));
    // Without its exclusion answer a row is not shown.
    let mut missing = answers();
    missing.as_object_mut().unwrap().remove("skip_3");
    assert!(card_from(&b, &missing).is_some_and(|c| c["snapshot"]["rows"][0]["label"] == "Free space"));
    // No row at all is no card.
    let mut none = answers();
    for n in 2..=5 {
        none[format!("need_{n}")] = json!({"noul": 0.2});
    }
    assert!(card_from(&b, &none).is_none());
    // A title that names no block, or a block that can't be one.
    assert!(with(&["title", "choice"], json!("99")).is_none());
    assert!(with(&["title", "choice"], json!("not a number")).is_none());
    assert!(card_from(&b, &json!({})).is_none());
}

#[test]
fn the_devops_reply_becomes_a_card_with_its_headline_and_the_numbers_that_matter() {
    let blocks = split(DEVOPS);
    let card = card_from(&blocks, &devops_answers()).unwrap();
    assert_eq!(
        card,
        json!({"type": "result", "v": 2, "snapshot": {
            "title": "Machine health, 2026-11-01 00:25",
            "sub": "Overall: OK, but the disk is trending down and swap is in use.",
            "rows": [
                {"label": "Disk (internal)", "value": "22.7 GB free (10%) of 228 GB"},
                {"label": "Swap", "value": "2.9 of 4.0 GB used"},
                {"label": "CPU", "value": "Load 9.9, 9.0, 5.8 on 10 cores"},
                {"label": "Two problems are open", "value": "swap (since 18:11) and bridge (since 10:26)"},
            ],
            "links": [],
        }})
    );
}

#[test]
fn log_lines_and_suggestions_are_vetoed_however_important_jev_finds_them() {
    let blocks = split(
        "# Notes\n\nLog: I appended the line to the file\nSuggestion: you could free space\nDisk: 10 GB free\nSwap: 2 GB used\n\nAll fine here, nothing to do at all right now for you.",
    );
    let mut a = json!({
        "kind": {"choice": "info", "probabilities": {"info": 0.9}},
        "title": {"choice": "1", "probabilities": {"1": 0.9}},
        "headline": {"choice": "none", "probabilities": {"none": 0.9}},
    });
    // Jev rates the log line and the suggestion important too, but excluded.
    for (n, need, skip) in [(2, 0.9, 0.95), (3, 0.9, 0.9), (4, 0.9, 0.0), (5, 0.8, 0.0)] {
        a[format!("need_{n}")] = json!({"noul": need});
        a[format!("skip_{n}")] = json!({"noul": skip});
    }
    let rows = card_from(&blocks, &a).map(|c| c["snapshot"]["rows"].clone());
    assert_eq!(rows, Some(json!([{"label": "Disk", "value": "10 GB free"}, {"label": "Swap", "value": "2 GB used"}])));
}

#[test]
fn the_rows_are_the_top_five_by_need_in_reply_order() {
    let items = (1..=8).map(|i| format!("- Metric {i}: {i} GB\n")).collect::<Vec<_>>().concat();
    let blocks = split(&format!("# Report\n\n{items}\nThat is everything measured on the machine this time round."));
    let mut a = json!({
        "kind": {"choice": "info", "probabilities": {"info": 0.9}},
        "title": {"choice": "1", "probabilities": {"1": 0.9}},
    });
    for n in 2..=9_u8 {
        a[format!("need_{n}")] = json!({"noul": 0.6 + f64::from(n) / 100.0});
        a[format!("skip_{n}")] = json!({"noul": 0.0});
    }
    let card = card_from(&blocks, &a).unwrap();
    let labels: Vec<&str> =
        card["snapshot"]["rows"].as_array().unwrap().iter().map(|r| r["label"].as_str().unwrap()).collect();
    assert_eq!(labels, ["Metric 4", "Metric 5", "Metric 6", "Metric 7", "Metric 8"]);
}

/// Lab's wrong rows (reply d8e31bd0d9): a table whose header names the unit, and a table whose first column is the figure.
const DISK_TABLES: &str = "I mapped about 155 of the 203 GB in the data volume.\n\n**Whole internal disk (245 GB)**\n\n| Part | GB |\n|---|---|\n| Data volume (everything below) | 202.7 |\n| Free | about 5.3 |\n\n**Inside the data volume**\n\n| GB | What | Kind |\n|---|---|---|\n| **31.1** | `/tmp/caches`, your temporary files | probably cache |\n| 22.6 | `~/Workspace` | your code |\n";

#[test]
fn a_figure_in_a_table_keeps_its_unit_and_the_words_label_it() {
    let pairs: Vec<_> = split(DISK_TABLES).into_iter().filter_map(|b| b.pair).collect();
    let p = |l: &str, v: &str| (l.to_owned(), v.to_owned());
    assert_eq!(
        pairs,
        [
            p("Data volume (everything below)", "202.7 GB"),
            p("Free", "about 5.3 GB"),
            p("/tmp/caches, your temporary files", "31.1 GB"),
            p("~/Workspace", "22.6 GB"),
        ]
    );
    // A header that isn't a unit adds nothing, and a value that has its unit already keeps it as is.
    let rows: Vec<_> = split("| Item | Size |\n|---|---|\n| Cache | 5 GB |\n| Logs | 2 |\n")
        .into_iter()
        .filter_map(|b| b.pair)
        .collect();
    assert_eq!(rows, [("Cache".to_owned(), "5 GB".to_owned()), ("Logs".to_owned(), "2".to_owned())]);
    let rows: Vec<_> = split("| Item | Size (GB) |\n|---|---|\n| Cache | 5 GB |\n| Logs | 2 |\n")
        .into_iter()
        .filter_map(|b| b.pair)
        .collect();
    assert_eq!(rows, [("Cache".to_owned(), "5 GB".to_owned()), ("Logs".to_owned(), "2 GB".to_owned())]);
    // The unit doesn't leak onto a later table without a header.
    let rows: Vec<_> = split("| Part | GB |\n|---|---|\n| A | 1 |\n\ntext\n\n| B | 2 |\n")
        .into_iter()
        .filter_map(|b| b.pair)
        .collect();
    assert_eq!(rows[1], ("B".to_owned(), "2".to_owned()));
}

#[test]
fn a_long_opening_paragraph_offers_its_first_sentence_as_the_title() {
    let long = "Free space on the data volume fell from 6.7 to 5.3 GB while I scanned, so something is still writing to the disk, and I have not deleted or changed anything yet because the biggest folders need your say first. Details follow.";
    let text = format!("{long}\n\n- Free: 5.3 GB\n- Swap: 11.8 GB\n");
    let blocks = split(&text);
    assert!(plain(&blocks[0].text).chars().count() > TITLE_CHARS);
    let (_, q) = ask_for(&blocks);
    assert!(q.get("title").is_none(), "a long first sentence is no title");
    // With a sentence short enough, the sentence is the candidate, cut at the sentence end and not at "6.7".
    let text = "Free space fell from 6.7 to 5.3 GB while I scanned. The biggest folders need your say before I delete anything, so I stopped here and did not change a single file on the disk today.\n\n- Free: 5.3 GB\n- Swap: 11.8 GB\n";
    let blocks = split(text);
    assert!(plain(&blocks[0].text).chars().count() > TITLE_CHARS);
    let (_, q) = ask_for(&blocks);
    assert_eq!(q["title"]["criteria"]["1"], "Free space fell from 6.7 to 5.3 GB while I scanned.");
    let a = json!({
        "kind": {"choice": "info", "probabilities": {"info": 0.95}},
        "title": {"choice": "1", "probabilities": {"1": 0.9}},
        "need_2": {"noul": 0.9}, "skip_2": {"noul": 0.05},
    });
    let card = card_from(&blocks, &a).unwrap();
    assert_eq!(card["snapshot"]["title"], "Free space fell from 6.7 to 5.3 GB while I scanned.");
    // A short paragraph stays whole; a later long paragraph offers nothing.
    assert_eq!(first_sentence("Use e.g. a cache. Then stop."), "Use e.g. a cache.");
    assert_eq!(first_sentence("No full stop here"), "No full stop here");
    assert_eq!(first_sentence("Really? Yes."), "Really?");
}

#[test]
fn a_reply_with_no_label_and_value_lines_gets_no_card() {
    let blocks = split(
        "# Plan\n\nWe will first read the code and then change it carefully, step by step.\n\n- Read the router\n- Change the router\n\nThat should do it for this round of work, I think.",
    );
    let (_, q) = ask_for(&blocks);
    assert!(q.keys().all(|k| !k.starts_with("need_")));
    let a = json!({
        "kind": {"choice": "info", "probabilities": {"info": 0.99}},
        "title": {"choice": "1", "probabilities": {"1": 0.99}},
    });
    assert!(card_from(&blocks, &a).is_none());
}

#[test]
fn every_string_on_a_card_comes_from_the_reply() {
    let card = card_from(&split(DEVOPS), &devops_answers()).unwrap();
    let snap = &card["snapshot"];
    let strings = snap["rows"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|r| [r["label"].clone(), r["value"].clone()])
        .chain([snap["title"].clone(), snap["sub"].clone()]);
    let bare = plain(DEVOPS);
    for s in strings {
        let s = s.as_str().unwrap();
        assert!(s.split_whitespace().all(|w| bare.contains(w)), "{s}");
    }
}

#[test]
fn what_jev_is_sent_is_one_capped_call_with_every_decision_and_leaves_code_out() {
    let long = (1..=100).map(|i| format!("- Item {i}: {i} GB")).collect::<Vec<_>>().join("\n");
    let (state, q) =
        ask_for(&split(&format!("Title\n\nA closing sentence comes first here.\n\n```\nsecret = 1\n```\n\n{long}")));
    assert_eq!(state["reply"].as_array().unwrap().len(), MAX_BLOCKS);
    assert!(q.contains_key("kind") && q.contains_key("title") && q.contains_key("headline"));
    assert!(!q.contains_key("need_3"), "a code block is never a row");
    assert!(!q.contains_key("need_61"));
    assert!(q.contains_key("need_4"));
    assert!(q.contains_key("need_60") && q.contains_key("skip_60"));
}

#[test]
fn the_question_set_for_the_devops_reply() {
    let (state, q) = ask_for(&split(DEVOPS));
    assert_eq!(state["reply"].as_array().unwrap().len(), 26);
    // Title and headline choose among prose and headings only (rows are never offered); the headline can be none.
    assert!(q["title"]["criteria"].get("1").is_some() && q["title"]["criteria"].get("2").is_some());
    assert!(q["title"]["criteria"].get("3").is_none(), "a table row is not a title");
    assert!(q["title"]["criteria"].get("none").is_some());
    assert_eq!(q["headline"]["criteria"]["2"], "Overall: OK, but the disk is trending down and swap is in use.");
    assert!(q["headline"]["criteria"].get("none").is_some());
    // Each row candidate (five table rows, five label/value items) has a need and an exclusion question.
    for n in [3, 4, 5, 6, 7, 9, 10, 11, 12, 18] {
        assert_eq!(q[&format!("need_{n}")]["type"], "noul");
        assert!(q[&format!("skip_{n}")]["instructions"].as_str().unwrap().contains(&format!("block {n}")));
    }
    for n in [1, 2, 8, 13, 15, 20] {
        assert!(!q.contains_key(&format!("need_{n}")), "block {n} is not a row candidate");
    }
    assert_eq!(q.keys().filter(|k| k.starts_with("need_")).count(), 10);
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
    assert_eq!(e.data["richCard"]["v"], 2);

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

/// The eval harness: the real `classify_traced` on live Jev over a labelled set, for the lab. Reads JSONL
/// `{"id","text"}` lines from `JEV_EVAL_IN`, writes `{"id","card","answers","ms"}` (plus `error` when Jev failed) to
/// `JEV_EVAL_OUT`, one reply at a time. Needs `JEV_API_KEY`; `JEV_EVAL_ONLY=<n>` caps the replies run. Run it from
/// `serve/host` with `--bin kith-serve rich_card::jev::tests::eval_live -- --ignored --nocapture`.
#[tokio::test]
#[ignore = "calls the real Jev API; run by hand"]
async fn eval_live() {
    use std::io::Write;
    let input = std::env::var("JEV_EVAL_IN").expect("JEV_EVAL_IN: a JSONL of {id, text}");
    let output = std::env::var("JEV_EVAL_OUT").expect("JEV_EVAL_OUT: where to write the JSONL");
    let only: usize = std::env::var("JEV_EVAL_ONLY").ok().and_then(|n| n.parse().ok()).unwrap_or(usize::MAX);
    let jev = Jev::from_key(std::env::var("JEV_API_KEY").expect("JEV_API_KEY"));
    let mut out = std::fs::File::create(output).unwrap();
    for line in std::fs::read_to_string(input).unwrap().lines().filter(|l| !l.trim().is_empty()).take(only) {
        let row: Value = serde_json::from_str(line).unwrap();
        let started = std::time::Instant::now();
        let traced = classify_traced(&jev, row["text"].as_str().unwrap_or_default()).await;
        let ms = started.elapsed().as_millis();
        let result = match traced {
            Ok(t) => json!({"id": row["id"], "card": t.card, "answers": t.answers, "ms": ms}),
            Err(e) => json!({"id": row["id"], "card": null, "answers": null, "ms": ms, "error": format!("{e:#}")}),
        };
        writeln!(out, "{result}").unwrap();
    }
}
