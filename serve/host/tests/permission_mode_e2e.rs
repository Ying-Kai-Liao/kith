//! The mode a Claude session starts in: the adapter starts every session in "default", so the host asks for the mode
//! `kith <agent> --launch-json` reports (`session/set_mode`). An invited person's run keeps the confined mode and is
//! never asked into bypass. Against the stub kith (its `permission` file is launch-json's `permission_mode`) and the
//! scripted ACP agent that lists the modes the way claude-agent-acp does.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::host::Host;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A host over the `house` stub agent, whose launch-json reports `permission` as its permission mode.
async fn start(permission: &str) -> (Host, PathBuf) {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let home = std::env::temp_dir().join(format!("kith-mode-e2e-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("permission"), permission).unwrap();
    let acp = format!("{} claude", common::python_agent(&fixtures.join("instructions_agent.py")));
    let host = Host::start_env(&[
        ("KITH_KERNEL_BIN", &fixtures.join("stub_hq.py").to_string_lossy()),
        ("STUB_HQ_HOME", &home.to_string_lossy()),
        ("KITH_SERVE_HQ_ACP_COMMAND", &acp),
        ("KITH_AUTH", &home.join("auth.json").to_string_lossy()),
        ("KITH_STATE", &home.join("state").to_string_lossy()),
        ("KITH_HOME", &home.join("kith-home").to_string_lossy()),
    ])
    .await;
    (host, home)
}

async fn house(host: &Host) -> String {
    for _ in 0..200 {
        if let Some(bot) = host.roster().await.into_iter().find(|b| b["hqAgent"] == "house") {
            return bot["id"].as_str().unwrap().to_owned();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("house never appeared");
}

/// Sends `go` and returns the ACP agent's requests once it has answered a prompt (so the session exists and its mode
/// is settled). A send that lands while a restarted host is still settling can be lost, so the same message (the same
/// clientNonce, which the host takes once) is sent again until a turn really reaches the agent: the test never passes
/// without a `session/prompt`. When none comes, the failure shows the chat's entries (the host's own notice, if any).
async fn requests_after_a_turn(host: &Host, bot: &str, home: &Path) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut next_send = Instant::now();
    loop {
        if Instant::now() >= next_send {
            host.call("send", json!({"botId": bot, "text": "go", "clientNonce": "n1"})).await;
            next_send = Instant::now() + Duration::from_secs(10);
        }
        let text = std::fs::read_to_string(home.join("requests.jsonl")).unwrap_or_default();
        let all: Vec<Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        if all.iter().any(|r| r["method"] == "session/prompt") {
            return all;
        }
        if Instant::now() >= deadline {
            let history = host.call("history", json!({"botId": bot})).await;
            panic!("the agent never got a prompt; requests {all:?}; chat {history}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn set_modes(requests: &[Value]) -> Vec<String> {
    requests
        .iter()
        .filter(|r| r["method"] == "session/set_mode")
        .map(|r| r["params"]["modeId"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn an_owners_run_starts_in_the_mode_launch_json_reports() {
    let (host, home) = start("acceptEdits").await;
    let bot = house(&host).await;
    let requests = requests_after_a_turn(&host, &bot, &home).await;
    assert_eq!(set_modes(&requests), ["acceptEdits"], "{requests:?}");
    // The mode is asked for before the first prompt, not after.
    let order: Vec<&str> = requests.iter().map(|r| r["method"].as_str().unwrap()).collect();
    let set = order.iter().position(|m| *m == "session/set_mode").unwrap();
    let prompt = order.iter().position(|m| *m == "session/prompt").unwrap();
    assert!(set < prompt, "{order:?}");
    let _ = std::fs::remove_dir_all(&home);
}

#[tokio::test]
async fn an_invitees_run_never_starts_in_bypass() {
    // The kernel's mode for the agent is a free shell; for sam's run it is confined to "default", which is where the
    // adapter starts anyway, so nothing is set.
    let (mut host, home) = start("bypassPermissions").await;
    let _ = house(&host).await;
    let mut chat = String::new();
    host.restart(|dir| {
        let db = rusqlite::Connection::open(dir.join("serve.db")).unwrap();
        let n = db.execute("UPDATE bots SET config = json_set(config, '$.person', 'sam')", []).unwrap();
        assert_eq!(n, 1, "one bot, the house");
        chat = db.query_row("SELECT id FROM bots", [], |r| r.get(0)).unwrap();
    })
    .await;
    // The restarted host has the bot as sam's before anything is sent.
    for _ in 0..200 {
        if host.roster().await.iter().any(|b| b["id"] == chat.as_str()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let requests = requests_after_a_turn(&host, &chat, &home).await;
    assert!(requests.iter().any(|r| r["method"] == "session/new" || r["method"] == "session/load"), "{requests:?}");
    assert!(set_modes(&requests).is_empty(), "{requests:?}");
    let _ = std::fs::remove_dir_all(&home);
}
