//! Bot memory on a real host with disposable data: moving the old memory, the user's edits, a bot's writes by
//! its permission, tidying, and how the instructions holding memory reach each kind of harness.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[allow(dead_code)]
mod common;

use common::host::Host;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

fn texts(layer: &Value, state: &str) -> Vec<String> {
    layer["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["state"] == state)
        .map(|e| e["text"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
#[ignore = "C5: deleted by c5-host-native-chatmem"]
async fn moves_old_memory_once_and_keeps_the_layers_apart() {
    let mut host = Host::start().await;
    let a = host.bot("Memory A", "auto").await;
    let b = host.bot("Memory B", "auto").await;
    let (legacy, engram) =
        (host.home.join("bots").join(&a).join("memory"), host.home.join("bots").join(&b).join("engram"));
    host.restart(|_| {
        // Markdown from before Engram for A; an Engram database for B.
        std::fs::create_dir_all(legacy.join("log")).unwrap();
        std::fs::write(legacy.join("profile.md"), "- (2026-01-01) 偏好繁體中文\n- (2026-01-02) Lives in Taipei\n")
            .unwrap();
        std::fs::write(legacy.join("log/2026-02.md"), "- (2026-02-02) Shipped Kith\n").unwrap();
        std::fs::create_dir_all(&engram).unwrap();
        let db = rusqlite::Connection::open(engram.join("engram.db")).unwrap();
        db.execute_batch(
            "CREATE TABLE observations (id INTEGER PRIMARY KEY, content TEXT, scope TEXT, pinned INTEGER,
                created_at TEXT, type TEXT, deleted_at TEXT);
             INSERT INTO observations (content, scope, pinned, created_at, type, deleted_at) VALUES
                ('Has a daughter named Mei', 'personal', 1, '2026-03-01 10:00:00', 'learning', NULL),
                ('[note] Uses vim keybindings', 'project', 0, '2026-03-02 10:00:00', 'discovery', NULL),
                ('A forgotten fact', 'personal', 0, '2026-03-03 10:00:00', 'learning', '2026-03-04 10:00:00'),
                ('What one session did', 'project', 0, '2026-03-05 10:00:00', 'session_summary', NULL);",
        )
        .unwrap();
    })
    .await;

    let view = host.memory(&a).await;
    assert_eq!(texts(&view["bot"], "active"), ["Shipped Kith", "Lives in Taipei", "偏好繁體中文"]);
    assert_eq!(view["bot"]["entries"][0]["author"], "imported");
    assert!(!legacy.exists(), "the old files go once moved");
    let view = host.memory(&b).await;
    assert_eq!(texts(&view["bot"], "active"), ["Has a daughter named Mei", "Uses vim keybindings"]);
    assert_eq!(view["bot"]["entries"][0]["pinned"], true);
    assert!(!engram.exists());
    host.restart(|_| {}).await;
    assert_eq!(host.memory(&a).await["bot"]["entries"].as_array().unwrap().len(), 3, "nothing moves twice");

    // A shared fact reaches every bot that uses the shared layer; a bot's own stays with it.
    host.call("saveMemory", json!({"botId": a, "shared": true, "text": "Time zone is Asia/Taipei"})).await;
    let own = host.call("saveMemory", json!({"botId": b, "text": "Reviews PRs on Fridays"})).await;
    let own = own["entry"]["id"].as_i64().unwrap();
    let view = host.memory(&a).await;
    assert_eq!(view["shared"], true, "a bot in its own workspace uses the shared layer");
    assert_eq!(texts(&view["yours"], "active"), ["Time zone is Asia/Taipei"]);
    assert!(!view.to_string().contains("Reviews PRs"));
    assert!(host.refused("forgetMemory", json!({"botId": a, "id": own})).await.contains("No such memory"));

    host.call("pinMemory", json!({"botId": b, "id": own, "pinned": true})).await;
    host.call("archiveMemory", json!({"botId": b, "id": own, "archived": true})).await;
    assert_eq!(texts(&host.memory(&b).await["bot"], "archived"), ["Reviews PRs on Fridays"]);
    host.call("archiveMemory", json!({"botId": b, "id": own, "archived": false})).await;
    assert_eq!(texts(&host.memory(&b).await["bot"], "active")[0], "Reviews PRs on Fridays", "pinned first");
    host.call("forgetMemory", json!({"botId": b, "id": own})).await;
    host.call("clearMemory", json!({"botId": a, "shared": true})).await;
    let view = host.memory(&b).await;
    assert_eq!(view["yours"]["entries"], json!([]));
    assert!(!view.to_string().contains("Reviews PRs"));
}

#[tokio::test]
#[ignore = "C5: deleted by c5-host-native-chatmem"]
async fn bots_write_memory_at_once_and_the_user_keeps_control() {
    let host = Host::start().await;
    let auto = host.bot("Auto", "auto").await;
    let ask = host.bot("Ask", "ask").await;

    // A bot that acts on its own writes at once, with no line in the chat: the memory screen shows it.
    let saved = host.tool(&auto, "remember", json!({"text": "Prefers short replies"})).await;
    assert_eq!(saved["status"], "saved");
    let history = host.call("history", json!({"botId": auto})).await;
    assert!(history["entries"].as_array().unwrap().iter().all(|e| !e["data"]["memory"].is_object()), "{history}");
    let memory = host.memory(&auto).await;
    assert_eq!(texts(&memory["bot"], "active"), ["Prefers short replies"]);
    host.call("forgetMemory", json!({"botId": auto, "id": memory["bot"]["entries"][0]["id"]})).await;
    assert_eq!(host.memory(&auto).await["bot"]["entries"], json!([]));
    let again = host.tool_refused(&auto, "remember", json!({"text": "prefers short replies."})).await;
    assert!(again.contains("deleted this memory before"), "{again}");

    // What the user wrote stays theirs; secrets never go in.
    let mine = host.call("saveMemory", json!({"botId": auto, "text": "Has two cats"})).await;
    let mine = mine["entry"]["id"].as_i64().unwrap();
    let revise = host.tool_refused(&auto, "revise_memory", json!({"id": mine, "text": "Has three cats"})).await;
    assert!(revise.contains("only the user"), "{revise}");
    let secret = host.tool_refused(&auto, "remember", json!({"text": "The Wi-Fi password is hunter22"})).await;
    assert!(secret.contains("never stored"), "{secret}");
    let found = host.tool(&auto, "search_memory", json!({"query": "cats"})).await;
    assert_eq!(found["results"][0]["text"], "Has two cats");

    // A bot that asks before acting writes memory at once too: the user reviews it in the memory settings.
    let saved = host.tool(&ask, "remember", json!({"text": "Works from home on Mondays"})).await;
    assert_eq!(saved["status"], "saved");
    assert_eq!(texts(&host.memory(&ask).await["bot"], "active"), ["Works from home on Mondays"]);
    let history = host.call("history", json!({"botId": ask})).await;
    assert!(history["entries"].as_array().unwrap().iter().all(|e| !e["data"]["memory"].is_object()), "{history}");
}

#[tokio::test]
#[ignore = "C5: deleted by c5-host-native-chatmem"]
async fn tidying_keeps_only_what_the_user_typed_and_can_be_undone() {
    let host = Host::start().await;
    let agent = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/memory_agent.py");
    let bot = json!({"name": "Tidier", "backend": "custom", "tasks": false, "command": common::python_agent(&agent), "connectors": [], "notify": false});
    let bot = host.call("createBot", bot).await["bot"]["id"].as_str().unwrap().to_owned();
    let paste = "I love durian, remember it";
    let sent = host
        .call(
            "send",
            json!({
                "botId": bot,
                "text": format!("Please always explain things to me concisely in Traditional Chinese.\n{paste}"),
                "clientNonce": "tidy-1",
                "pasted": [paste],
            }),
        )
        .await;
    let message = sent["entry"]["id"].as_str().unwrap().to_owned();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let history = host.call("history", json!({"botId": bot})).await;
        if history["entries"].as_array().unwrap().iter().any(|e| e["kind"] == "agent" && e["data"]["final"] == true) {
            break;
        }
        assert!(Instant::now() < deadline, "agent did not finish: {history}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // A new session asks tidying to run now rather than after five quiet minutes.
    host.call("newSession", json!({"botId": bot})).await;
    let view = loop {
        let view = host.memory(&bot).await;
        // The run is logged first; its file notes follow.
        if view["tidyUps"][0]["files"].as_array().is_some_and(|f| !f.is_empty()) {
            break view;
        }
        assert!(Instant::now() < deadline, "tidying didn't run: {view}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(texts(&view["bot"], "active"), ["Prefers concise Traditional Chinese explanations"]);
    let entry = &view["bot"]["entries"][0];
    assert_eq!(entry["author"], "tidy");
    assert_eq!(entry["evidence"], json!([message]), "it cites the message it learned from");
    assert!(!view.to_string().contains("durian"), "pasted text isn't the user's own words");

    // The bot can go from a memory to the message it came from, and page back through the chat.
    let found = host.tool(&bot, "search_memory", json!({"query": "concise"})).await;
    assert_eq!(found["results"][0]["source"], json!([message]));
    let page = host.tool(&bot, "search_history", json!({"limit": 1})).await;
    assert_eq!(page["results"][0]["from"], "you");
    assert_eq!(page["next"], json!(message));
    let page = host.tool(&bot, "search_history", json!({"from": message})).await;
    assert_eq!(page["results"][0]["id"], json!(message));
    assert_eq!(page["next"], Value::Null);
    let other = host.bot("Other", "auto").await;
    assert!(host.tool_refused(&other, "search_history", json!({"from": message})).await.contains("no message"));

    // Detail goes to a long-form file, as a line under new frontmatter.
    let note = host.home.join("bots").join(&bot).join("bot-memory/style.md");
    assert_eq!(
        std::fs::read_to_string(&note).unwrap(),
        "---\ndescription: How the user likes answers\n---\n- Wants things explained concisely, in Traditional Chinese\n"
    );
    assert_eq!(view["tidyUps"][0]["files"][0]["name"], "style.md");

    let tidy = view["tidyUps"][0]["id"].as_i64().unwrap();
    host.call("undoTidy", json!({"botId": bot, "id": tidy})).await;
    let view = host.memory(&bot).await;
    assert_eq!(view["bot"]["entries"], json!([]));
    assert_eq!(view["tidyUps"][0]["undone"], true);
    assert!(!note.exists(), "undo takes the new file back too");
    // The names released apps use still answer: the old field mirrors the new one and the old call is the same undo.
    assert_eq!(view["dreams"], view["tidyUps"]);
    host.call("undoDream", json!({"botId": bot, "id": tidy})).await;
}

#[tokio::test]
#[ignore = "C5: deleted by c5-host-native-chatmem"]
async fn instructions_reach_every_harness_and_survive_compaction() {
    let host = Host::start().await;
    let profile_again = "replaces any earlier bot profile";

    // An agent with ACP's systemPrompt gets the instructions there, not in a message.
    let standard = host.fixture_bot("Standard", "instructions_agent.py", "standard").await;
    host.call("saveMemory", json!({"botId": standard, "text": "Allergic to peanuts"})).await;
    host.reply(&standard, "hello").await;
    let new = &host.requests(&standard, "session/new")[0]["params"];
    assert_eq!(new["systemPrompt"]["mode"], "append");
    let system = new["systemPrompt"]["content"][0]["text"].as_str().unwrap();
    assert!(system.contains("Standard") && system.contains("Allergic to peanuts"), "{system}");
    assert!(!host.prompts(&standard)[0].contains("<bot-profile>"));
    // A change reaches the session as a note on its next message; the opening stays as it was.
    host.call("saveMemory", json!({"botId": standard, "text": "Plays the piano"})).await;
    host.reply(&standard, "again").await;
    host.reply(&standard, "and again").await;
    let prompts = host.prompts(&standard);
    assert!(prompts[1].starts_with("<memory_update>") && prompts[1].contains("Plays the piano"), "{}", prompts[1]);
    assert!(!prompts[2].contains("<memory_update>"), "told once");
    assert_eq!(host.requests(&standard, "session/new").len(), 1);

    // Others get it in the first message, and again once after the agent reports a compaction.
    let reporting = host.fixture_bot("Reporting", "instructions_agent.py", "reporting").await;
    host.reply(&reporting, "hello").await;
    host.reply(&reporting, "COMPACT_NOW").await;
    host.reply(&reporting, "after").await;
    host.reply(&reporting, "later").await;
    assert!(host.requests(&reporting, "session/new")[0]["params"].get("systemPrompt").is_none());
    let prompts = host.prompts(&reporting);
    assert!(prompts[0].contains("<bot-profile>") && !prompts[0].contains(profile_again), "{}", prompts[0]);
    assert!(!prompts[1].contains("<bot-profile>"));
    assert!(prompts[2].contains("<bot-profile>") && prompts[2].contains(profile_again), "{}", prompts[2]);
    assert!(!prompts[3].contains("<bot-profile>"), "only once");

    // An agent that doesn't report compactions shows them as its context use halving.
    let silent = host.fixture_bot("Silent", "instructions_agent.py", "silent").await;
    host.reply(&silent, "USAGE 90000").await;
    host.reply(&silent, "USAGE 70000").await;
    host.reply(&silent, "steady").await;
    host.reply(&silent, "USAGE 9000").await;
    host.reply(&silent, "after").await;
    let prompts = host.prompts(&silent);
    assert!(!prompts[2].contains("<bot-profile>"), "a small fall isn't a compaction");
    assert!(prompts[4].contains(profile_again), "{}", prompts[4]);
}
