//! One-bot-model C5, the safety net: the host never launches a host-native row (an agent row with no kith agent behind
//! it), flags it, and can export it. Groups are exempt everywhere.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::rig::Rig;
use crate::agent::bot::Cmd;
use crate::api::{devices::Caller, dispatch};
use crate::store::{BotConfig, EntryKind, Lane, native_ids};
use serde_json::{Value, json};
use std::time::Duration;

const WORDS: &str = "kith agent";

fn cfg(v: Value) -> BotConfig {
    serde_json::from_value(v).unwrap()
}

/// A host-native row, as an old release left it.
fn native(r: &Rig, name: &str) -> String {
    let cfg = cfg(json!({
        "id": "", "name": name, "backend": "claude", "cwd": r.dir.to_string_lossy(),
        "description": format!("{name} helps"), "model": "sonnet", "modelOptions": {"effort": "high"},
        "tasks": false, "connectors": [], "skills": [],
    }));
    r.hub.create_bot(cfg).unwrap()["id"].as_str().unwrap().to_owned()
}

async fn call(r: &Rig, method: &str, b: Value) -> anyhow::Result<Value> {
    dispatch(&r.hub, &Caller::Local, method, b).await
}

/// Waits (a few seconds) for `ok`.
async fn until(what: &str, mut ok: impl FnMut() -> bool) {
    for _ in 0..200 {
        if ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("never saw: {what}");
}

fn says(r: &Rig, lane: &Lane, words: &str) -> bool {
    r.hub
        .store
        .history(&lane.chat, i64::MAX, 100)
        .unwrap()
        .iter()
        .any(|e| e.data["text"].as_str().is_some_and(|t| t.contains(words)))
}

#[test]
fn a_native_row_has_no_hq_agent_and_is_no_group() {
    let is = |v: Value| cfg(v).is_native();
    let base = json!({"id": "a", "name": "A", "backend": "claude", "cwd": "/"});
    assert!(is(base.clone()));
    assert!(is(json!({"id": "a", "name": "A", "backend": "claude", "cwd": "/", "hqAgent": ""})));
    assert!(is(json!({"id": "a", "name": "A", "backend": "claude", "cwd": "/", "hqAgent": "  "})));
    assert!(!is(json!({"id": "a", "name": "A", "backend": "claude", "cwd": "/", "hqAgent": "house"})));
    let room = json!({"id": "r", "name": "R", "kind": "group", "members": []});
    assert!(!is(room), "a room has no agent and is not native");
}

#[tokio::test]
async fn only_native_rows_are_counted_and_flagged_and_a_group_or_agent_is_not() {
    let r = Rig::new().await;
    let a = native(&r, "Old");
    let house = r.owner_bot("house");
    let room: BotConfig = cfg(json!({"id": "", "kind": "group", "name": "Crew", "members": [house]}));
    let room = r.hub.create_bot(room).unwrap()["id"].as_str().unwrap().to_owned();
    let rows = r.hub.store.bots().unwrap();
    assert_eq!(native_ids(&rows), vec![a.clone()], "the count names only the native row");
    let flag = |id: &str| r.hub.bot_json(&r.hub.store.bot(id).unwrap().unwrap());
    assert_eq!(flag(&a)["needsKithAgent"], true);
    assert!(flag(&house).get("needsKithAgent").is_none(), "left out for a kith agent");
    assert!(flag(&room).get("needsKithAgent").is_none(), "left out for a room");
    // The row stays listed and readable.
    assert!(!r.hub.store.bot(&a).unwrap().unwrap().deleted);
}

#[tokio::test]
async fn a_message_to_a_native_bot_is_refused_and_nothing_is_saved() {
    let r = Rig::new().await;
    let a = native(&r, "Old");
    let e = call(&r, "send", json!({"botId": a, "text": "hi", "clientNonce": "n1"})).await.unwrap_err();
    assert!(format!("{e:#}").contains(WORDS), "{e:#}");
    assert!(r.hub.store.history(&a, i64::MAX, 10).unwrap().is_empty());
}

#[tokio::test]
async fn createbot_makes_rooms_only() {
    let r = Rig::new().await;
    let house = r.owner_bot("house");
    let e = call(&r, "createBot", json!({"name": "New", "backend": "claude", "cwd": ""})).await.unwrap_err();
    assert!(format!("{e:#}").contains(WORDS), "{e:#}");
    let room = call(&r, "createBot", json!({"name": "Crew", "kind": "group", "members": [house]})).await.unwrap();
    assert_eq!(room["bot"]["kind"], "group");
}

#[tokio::test]
async fn a_turn_the_actor_is_given_is_refused_with_the_one_error() {
    let r = Rig::new().await;
    let a = native(&r, "Old");
    let lane = Lane::main(&a);
    let e = r.hub.add_entry(&lane, EntryKind::User, 1, &json!({"text": "hi", "status": "queued"})).unwrap();
    r.hub.send_cmd(&a, Cmd::Send { lane: lane.clone(), entry_id: e.id, text: "hi".into(), route: false }).unwrap();
    until("the refusal in the chat", || says(&r, &lane, WORDS)).await;
}

#[tokio::test]
async fn a_routine_of_a_native_bot_fails_to_start_and_says_why() {
    let r = Rig::new().await;
    let a = native(&r, "Old");
    let saved = r.hub.routines.save(
        &r.hub,
        &a,
        &json!({"name": "daily", "instruction": "do it", "triggers": [{"type": "interval", "seconds": 3600}]}),
    );
    let saved = saved.unwrap();
    let id = saved["routine"]["id"].as_str().unwrap().to_owned();
    let run = r.hub.routines.enqueue(&r.hub.store, &a, &id, json!({}), None, true).unwrap();
    let run_id = run["run"]["id"].as_str().unwrap().to_owned();
    r.hub.send_cmd(&a, Cmd::Routine(run_id.clone())).unwrap();
    until("the run to fail", || {
        let list = r.hub.routines.list(&a);
        list["runs"].as_array().unwrap().iter().any(|run| {
            run["id"] == run_id.as_str()
                && run["status"] == "failed"
                && run["detail"].as_str().is_some_and(|d| d.contains(WORDS))
        })
    })
    .await;
}

#[tokio::test]
async fn a_native_bot_fails_the_guard_and_a_group_passes_it() {
    let r = Rig::new().await;
    let a = native(&r, "Old");
    let row = r.hub.store.bot(&a).unwrap().unwrap().config;
    let e = row.refuse_native().unwrap_err();
    assert!(format!("{e:#}").contains(WORDS));
    let room = cfg(json!({"id": "r", "name": "R", "kind": "group", "members": []}));
    assert!(room.refuse_native().is_ok(), "a room passes the guard");
    assert!(!room.is_native());
    assert!(row.is_native(), "compaction and idle upkeep read this: nothing of a native bot is compacted");
}

#[tokio::test]
async fn a_room_turn_skips_a_native_member_with_a_notice_and_goes_on() {
    let r = Rig::new().await;
    let a = native(&r, "Alpha");
    let b = native(&r, "Beta");
    let room = cfg(json!({"id": "", "kind": "group", "name": "Crew", "members": [a, b]}));
    let room = r.hub.create_bot(room).unwrap()["id"].as_str().unwrap().to_owned();
    let lane = Lane::main(&room);
    let post = json!({"text": "@Alpha @Beta hello", "status": "sent"});
    let e = r.hub.add_owner_post(&lane, 1, &post, true).unwrap();
    crate::chat::group::start(&r.hub, &lane, 10, "owner", &e.id).unwrap();
    until("a notice for each native member", || {
        let all = r.hub.store.history(&room, i64::MAX, 100).unwrap();
        ["Alpha", "Beta"].iter().all(|n| {
            all.iter().any(|e| {
                e.data["notice"] == "nativeSkipped" && e.data["text"].as_str().is_some_and(|t| t.starts_with(n))
            })
        })
    })
    .await;
    // Said once per conversation, however many rounds there are.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let all = r.hub.store.history(&room, i64::MAX, 100).unwrap();
    assert_eq!(all.iter().filter(|e| e.data["notice"] == "nativeSkipped").count(), 2, "one per native member");
}

// MARK: export

fn bundle(path: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(path)
}

fn all_text(dir: &std::path::Path) -> String {
    let mut out = String::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let p = entry.unwrap().path();
        out += &p.to_string_lossy();
        if p.is_dir() {
            out += &all_text(&p);
        } else {
            out += &std::fs::read_to_string(&p).unwrap();
        }
    }
    out
}

#[tokio::test]
async fn the_export_has_the_row_memory_and_routines_and_no_secret_or_webhook_key() {
    let r = Rig::new().await;
    let a = native(&r, "Old");
    // Host chat-memory files of the bot.
    let memory = crate::chat::memory::files::folder(Some(&a));
    std::fs::create_dir_all(memory.join("notes")).unwrap();
    std::fs::write(memory.join("likes.md"), "likes tea").unwrap();
    std::fs::write(memory.join("notes/deep.md"), "deeper").unwrap();
    // A kith-mem fact of the bot's own layer, which lives in serve.db.
    r.hub
        .store
        .memory_write(|c, now, rev| {
            let new = kith_mem::store::New { bot: Some(&a), text: "Likes oolong", room: None, evidence: &[] };
            kith_mem::store::add(c, kith_mem::store::Writer::User, new, now, rev)
        })
        .unwrap();
    // A routine with a webhook trigger and a schedule, one of them paused; the engine's saved key.
    let saved = r
        .hub
        .routines
        .save(&r.hub, &a, &json!({"name": "hook", "instruction": "react", "triggers": [{"type": "webhook"}, {"type": "interval", "seconds": 600}]}))
        .unwrap();
    let id = saved["routine"]["id"].as_str().unwrap().to_owned();
    let key = r.hub.routines.credentials(&r.hub, &a, &id, false).unwrap()["key"].as_str().unwrap().to_owned();
    assert!(key.len() > 20);
    r.hub.routines.set_enabled(&r.hub.store, &a, &id, false).unwrap();
    r.hub.store.vault.mark_open();
    let secret = "sk-ant-SECRETVALUE-1234";
    r.hub
        .store
        .kv_set(&crate::agent::auth::env_slot("claude"), &json!({"ANTHROPIC_API_KEY": secret}).to_string())
        .unwrap();

    let out = call(&r, "exportHostBot", json!({"botId": a})).await.unwrap();
    let dir = bundle(out["path"].as_str().unwrap());
    let read = |p: &str| serde_json::from_str::<Value>(&std::fs::read_to_string(dir.join(p)).unwrap()).unwrap();
    let bot = read("bot.json");
    assert_eq!((bot["name"].as_str(), bot["description"].as_str()), (Some("Old"), Some("Old helps")));
    assert_eq!(
        (bot["engine"].as_str(), bot["model"].as_str(), bot["effort"].as_str()),
        (Some("claude"), Some("sonnet"), Some("high"))
    );
    assert_eq!(std::fs::read_to_string(dir.join("memory/likes.md")).unwrap(), "likes tea");
    assert_eq!(std::fs::read_to_string(dir.join("memory/notes/deep.md")).unwrap(), "deeper");
    assert_eq!(read("entries.json")["entries"][0]["text"], "Likes oolong");
    let routines = read("routines.json");
    let first = &routines["routines"][0];
    assert_eq!(
        (first["name"].as_str(), first["prompt"].as_str(), first["paused"].as_bool()),
        (Some("hook"), Some("react"), Some(true))
    );
    assert_eq!(first["triggers"][0]["type"], "webhook");
    assert_eq!(read("vault.json")["keyNames"], json!(["ANTHROPIC_API_KEY"]), "names only");
    assert!(bot["usedSharedMemory"].is_boolean());
    // Nothing that authorizes anything.
    let everything = all_text(&dir);
    assert!(!everything.contains(secret), "a vault value leaked");
    assert!(!everything.contains(&key), "a webhook key leaked");
    assert!(!out.to_string().contains(secret) && !out.to_string().contains(&key));

    // Again: rebuilt in place, an earlier file that is gone from the memory is gone from the bundle.
    std::fs::remove_file(memory.join("likes.md")).unwrap();
    let again = call(&r, "exportHostBot", json!({"botId": a})).await.unwrap();
    assert_eq!(again["path"], out["path"]);
    assert!(!dir.join("memory/likes.md").exists());
    assert!(dir.join("memory/notes/deep.md").exists());
}

#[tokio::test]
async fn deleting_a_native_row_removes_its_memory_files_and_export_bundle() {
    let r = Rig::new().await;
    let a = native(&r, "Old");
    let memory = crate::chat::memory::files::folder(Some(&a));
    std::fs::create_dir_all(&memory).unwrap();
    std::fs::write(memory.join("likes.md"), "likes tea").unwrap();
    let out = call(&r, "exportHostBot", json!({"botId": a})).await.unwrap();
    let dir = bundle(out["path"].as_str().unwrap());
    assert!(dir.exists() && memory.exists(), "kept until the owner deletes the row");
    r.hub.delete_bot(&a).unwrap();
    until("the folders to go", || !memory.exists() && !dir.exists()).await;
}

#[tokio::test]
async fn the_export_refuses_a_kith_agent_a_room_and_an_unknown_bot() {
    let r = Rig::new().await;
    let house = r.owner_bot("house");
    let room = r.hub.create_bot(cfg(json!({"id": "", "kind": "group", "name": "Crew", "members": [house]}))).unwrap();
    let room = room["id"].as_str().unwrap().to_owned();
    for id in [house, room, "nobody".to_owned()] {
        assert!(call(&r, "exportHostBot", json!({"botId": id})).await.is_err(), "{id}");
    }
}

#[tokio::test]
async fn the_export_is_the_owners_alone() {
    let r = Rig::new().await;
    r.auth(&["person", "sam"]);
    let sam = r.device("sam-phone", Some("sam"));
    let e = dispatch(&r.hub, &sam, "exportHostBot", json!({"botId": "x"})).await.unwrap_err();
    assert!(e.is::<super::devices::NotAllowed>() || e.is::<super::devices::Forbidden>(), "{e:#}");
    assert!(!super::roles::member_may("exportHostBot") && !super::roles::bot_may("exportHostBot"));
}
