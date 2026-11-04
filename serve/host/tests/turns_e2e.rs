//! A real host driving scripted ACP agents: Stop on an agent that ignores it, room turns
//! that a newer group message replaced, and Fast mode across session resumes.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

struct Host {
    child: Child,
    base: String,
    token: String,
    home: PathBuf,
}

impl Drop for Host {
    fn drop(&mut self) {
        common::stop(&mut self.child);
        let _ = std::fs::remove_dir_all(&self.home);
        common::forget_home(&self.home);
    }
}

impl Host {
    async fn start() -> Self {
        let home = std::env::temp_dir().join(format!("kith-turns-e2e-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        let (child, base) = common::serve(&home).await;
        let token = common::read_token(&home).trim().to_owned();
        Self { child, base, token, home }
    }

    async fn call(&self, method: &str, body: Value) -> Value {
        let res = reqwest::Client::new()
            .post(format!("{}/api/{method}", self.base))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = res.status();
        let v: Value = res.json().await.unwrap();
        assert!(status.is_success(), "{method} failed: {v}");
        v
    }

    /// A bot running `fixture` in its own folder (`home/<name>`).
    async fn bot(&self, name: &str, fixture: &str) -> String {
        let cwd = self.home.join(name);
        std::fs::create_dir_all(&cwd).unwrap();
        let agent = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(fixture);
        let bot = json!({
            "name": name, "backend": "custom", "tasks": false, "command": common::python_agent(&agent),
            "cwd": cwd, "permission": "ask", "notify": false,
        });
        self.call("createBot", bot).await["bot"]["id"].as_str().unwrap().to_owned()
    }

    async fn entries(&self, chat: &str) -> Vec<Value> {
        self.call("history", json!({"botId": chat})).await["entries"].as_array().unwrap().clone()
    }

    /// Polls `chat`'s history until `pred` matches an entry.
    async fn wait_for(&self, chat: &str, within: Duration, pred: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + within;
        loop {
            if let Some(e) = self.entries(chat).await.into_iter().rev().find(|e| pred(e)) {
                return e;
            }
            assert!(Instant::now() < deadline, "timed out; history: {:?}", self.entries(chat).await);
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

fn python_missing() -> bool {
    let missing = Command::new(common::python()).arg("--version").output().is_err();
    if missing {
        eprintln!("skipping: Python not available");
    }
    missing
}

#[tokio::test]
async fn stop_ends_a_turn_whose_agent_ignores_the_cancel() {
    if python_missing() {
        return;
    }
    let host = Host::start().await;
    let bot = host.bot("Stuck", "tests/fixtures/stubborn_agent.py").await;
    host.call("send", json!({"botId": bot, "text": "go"})).await;
    host.wait_for(&bot, Duration::from_secs(60), |e| e["data"]["text"] == "Working on it").await;
    host.call("stop", json!({"botId": bot})).await;
    // The agent never answers: after the grace period its process is stopped and the turn ends.
    host.wait_for(&bot, Duration::from_secs(40), |e| e["kind"] == "notice" && e["data"]["text"] == "Stopped.").await;
    let sync = common::snapshot(&host.base, &host.token).await;
    assert_eq!(sync["bots"][0]["status"], "idle");
}

#[tokio::test]
async fn a_member_busy_in_its_own_chat_still_answers_the_room() {
    if python_missing() {
        return;
    }
    let host = Host::start().await;
    let member = host.bot("Member", "tests/fixtures/team_agent.py").await;
    let folder = host.home.join("Member");
    let group =
        host.call("createBot", json!({"name": "Room", "kind": "group", "members": [member]})).await["bot"]["id"]
            .as_str()
            .unwrap()
            .to_owned();
    // Busy in its own chat until `release` exists.
    host.call("send", json!({"botId": member, "text": "BLOCK"})).await;
    let started = Instant::now();
    while !std::fs::read_to_string(folder.join("prompts.jsonl")).is_ok_and(|p| p.contains("BLOCK")) {
        assert!(started.elapsed() < Duration::from_secs(60), "the member never started");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    host.call("send", json!({"botId": group, "text": "first"})).await;

    // The room has its own session (and agent): it answers while the member's own chat still waits.
    let reply =
        host.wait_for(&group, Duration::from_secs(30), |e| e["kind"] == "agent" && e["data"]["final"] == true).await;
    assert_eq!(reply["data"]["text"], "reply: group");
    assert!(!folder.join("release").exists());
    let prompts = std::fs::read_to_string(folder.join("prompts.jsonl")).unwrap();
    let room = prompts.lines().find(|p| p.contains("[Group chat")).unwrap();
    assert!(!room.contains("BLOCK"), "nothing from the member's own chat reaches the room's session: {room}");
    std::fs::write(folder.join("release"), "").unwrap();
}

/// A final reply that starts with `speed` and quotes `word` from its prompt.
fn speed_reply(speed: &'static str, word: &'static str) -> impl Fn(&Value) -> bool {
    move |e: &Value| {
        e["kind"] == "agent"
            && e["data"]["final"] == true
            && e["data"]["text"].as_str().is_some_and(|t| t.starts_with(speed) && t.contains(word))
    }
}

const FAST_ON: &str = r#""configId": "fast", "value": "on""#;
const FAST_OFF: &str = r#""configId": "fast", "value": "off""#;

#[tokio::test]
async fn fast_mode_switches_live_sessions_without_a_restart() {
    if python_missing() {
        return;
    }
    let host = Host::start().await;
    let bot = host.bot("Quick", "tests/fixtures/fast_mode_agent.py").await;
    let calls = host.home.join("Quick").join("calls.jsonl");
    let log = || std::fs::read_to_string(&calls).unwrap();

    host.call("updateBot", json!({"id": bot, "fastMode": true})).await;
    host.call("send", json!({"botId": bot, "text": "one"})).await;
    host.wait_for(&bot, Duration::from_secs(60), speed_reply("fast on", "one")).await;
    assert_eq!(log().matches(FAST_ON).count(), 1);
    let sync = common::snapshot(&host.base, &host.token).await;
    assert_eq!(sync["bots"][0]["fastModeSupport"], "offered");

    // Off and on again: the same agent process switches the live session before each turn.
    host.call("updateBot", json!({"id": bot, "fastMode": false})).await;
    host.call("send", json!({"botId": bot, "text": "two"})).await;
    host.wait_for(&bot, Duration::from_secs(60), speed_reply("fast off", "two")).await;
    host.call("updateBot", json!({"id": bot, "fastMode": true})).await;
    host.call("send", json!({"botId": bot, "text": "three"})).await;
    host.wait_for(&bot, Duration::from_secs(60), speed_reply("fast on", "three")).await;
    let log = log();
    assert!(!log.contains("session/load"), "{log}");
    assert_eq!((log.matches(FAST_ON).count(), log.matches(FAST_OFF).count()), (2, 1), "{log}");
}

#[tokio::test]
async fn an_agent_without_config_options_has_no_fast_mode() {
    if python_missing() {
        return;
    }
    let host = Host::start().await;
    let bot = host.bot("Plain", "tests/fixtures/fast_mode_agent.py").await;
    std::fs::write(host.home.join("Plain").join("plain"), "").unwrap();
    let support = || async { common::snapshot(&host.base, &host.token).await["bots"][0]["fastModeSupport"].clone() };
    assert_eq!(support().await, "unknown", "clients offer /fast until a session says");

    host.call("send", json!({"botId": bot, "text": "one"})).await;
    host.wait_for(&bot, Duration::from_secs(60), speed_reply("fast off", "one")).await;
    assert_eq!(support().await, "unsupported");
}

#[tokio::test]
async fn a_refused_fast_mode_is_reported_and_not_asked_for_again() {
    if python_missing() {
        return;
    }
    let host = Host::start().await;
    let bot = host.bot("Refused", "tests/fixtures/fast_mode_agent.py").await;
    let folder = host.home.join("Refused");
    std::fs::write(folder.join("refuse"), "").unwrap();
    let log = || std::fs::read_to_string(folder.join("calls.jsonl")).unwrap();

    host.call("updateBot", json!({"id": bot, "fastMode": true})).await;
    host.call("send", json!({"botId": bot, "text": "one"})).await;
    host.wait_for(&bot, Duration::from_secs(60), speed_reply("fast off", "one")).await;
    let state = &common::snapshot(&host.base, &host.token).await["bots"][0];
    assert_eq!(state["fastModeSupport"], "refused");
    assert_eq!(state["fastModeIssue"], "requires extra usage");
    assert_eq!(state["fastMode"], true, "the user's choice stays");

    host.call("send", json!({"botId": bot, "text": "two"})).await;
    host.wait_for(&bot, Duration::from_secs(60), speed_reply("fast off", "two")).await;
    assert_eq!(log().matches(FAST_ON).count(), 1, "asked again after a refusal");

    // Changing the setting asks again.
    host.call("updateBot", json!({"id": bot, "fastMode": false})).await;
    host.call("updateBot", json!({"id": bot, "fastMode": true})).await;
    host.call("send", json!({"botId": bot, "text": "three"})).await;
    host.wait_for(&bot, Duration::from_secs(60), speed_reply("fast off", "three")).await;
    assert_eq!(log().matches(FAST_ON).count(), 2);
}

#[tokio::test]
async fn a_session_that_ended_in_the_agent_is_closed_and_loaded_again() {
    if python_missing() {
        return;
    }
    let host = Host::start().await;
    let bot = host.bot("Ended", "tests/fixtures/ended_session_agent.py").await;
    host.call("send", json!({"botId": bot, "text": "hello"})).await;
    host.wait_for(&bot, Duration::from_secs(60), |e| e["kind"] == "agent" && e["data"]["text"] == "pong").await;
    host.call("send", json!({"botId": bot, "text": "end-this-session"})).await;
    host.wait_for(&bot, Duration::from_secs(30), |e| {
        e["kind"] == "notice" && e["data"]["text"].as_str().is_some_and(|t| t.contains("session has ended"))
    })
    .await;
    // Left open, the ended session would refuse this one too.
    host.call("send", json!({"botId": bot, "text": "still there?"})).await;
    host.wait_for(&bot, Duration::from_secs(30), |e| e["kind"] == "agent" && e["data"]["text"] == "still here").await;
}
