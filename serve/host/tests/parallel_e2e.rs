//! A real host running a bot in several places at once, like the hq bridge's per-thread workers:
//! its own chat and a thread work side by side, a group turn never waits behind the bot's own
//! chat (and has a session of its own), and Stop ends only the place it was pressed in.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[allow(dead_code)]
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
        let home = std::env::temp_dir().join(format!("kith-parallel-e2e-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        let (child, base) = common::serve_env(&home, &[]).await;
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

    async fn bot(&self, name: &str) -> String {
        let agent = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/slow_agent.py");
        let bot = json!({
            "name": name, "backend": "custom", "tasks": false, "command": common::python_agent(&agent),
            "cwd": self.home, "permission": "auto", "notify": false,
        });
        self.call("createBot", bot).await["bot"]["id"].as_str().unwrap().to_owned()
    }

    async fn send(&self, bot: &str, thread: Option<&str>, text: &str) -> String {
        let body = json!({
            "botId": bot, "threadId": thread, "text": text, "clientNonce": uuid::Uuid::new_v4().to_string(),
        });
        self.call("send", body).await["entry"]["id"].as_str().unwrap().to_owned()
    }

    /// The main chat's entries, or a thread's.
    async fn entries(&self, bot: &str, thread: Option<&str>) -> Vec<Value> {
        let v = match thread {
            Some(root) => self.call("thread", json!({"botId": bot, "rootId": root})).await,
            None => self.call("history", json!({"botId": bot})).await,
        };
        v["entries"].as_array().unwrap().clone()
    }

    /// Polls until `pred` matches an entry there.
    async fn wait_for(&self, bot: &str, thread: Option<&str>, pred: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(e) = self.entries(bot, thread).await.into_iter().find(|e| pred(e)) {
                return e;
            }
            assert!(Instant::now() < deadline, "timed out; entries: {:#?}", self.entries(bot, thread).await);
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// The agent's reply there so far ("[session] got: <prompt>" then its steps).
    async fn reply(&self, bot: &str, thread: Option<&str>, task: &str) -> String {
        self.entries(bot, thread)
            .await
            .into_iter()
            .filter(|e| e["kind"] == "agent")
            .filter_map(|e| e["data"]["text"].as_str().map(str::to_owned))
            .find(|t| t.contains(task))
            .unwrap_or_default()
    }

    /// The bot's latest state from the event stream.
    async fn bot_state(&self, bot: &str) -> Value {
        let mut res = reqwest::Client::new()
            .get(format!("{}/events?since=0", self.base))
            .bearer_auth(&self.token)
            .send()
            .await
            .unwrap();
        let mut buf = String::new();
        let deadline = tokio::time::Instant::now() + Duration::from_millis(700);
        while let Ok(Ok(Some(chunk))) = tokio::time::timeout_at(deadline, res.chunk()).await {
            buf.push_str(&String::from_utf8_lossy(&chunk));
        }
        buf.lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .filter_map(|d| serde_json::from_str::<Value>(d.trim()).ok())
            .rfind(|ev| ev["type"] == "bot" && ev["bot"]["id"] == bot)
            .map(|ev| ev["bot"].clone())
            .unwrap()
    }
}

fn text(e: &Value) -> &str {
    e["data"]["text"].as_str().unwrap_or_default()
}

/// The session a reply names: "[s123] got: …" -> "s123".
fn session(reply: &str) -> &str {
    reply.strip_prefix('[').and_then(|r| r.split(']').next()).unwrap_or_default()
}

fn python_missing() -> bool {
    let missing = Command::new(common::python()).arg("--version").output().is_err();
    if missing {
        eprintln!("skipping: Python not available");
    }
    missing
}

#[tokio::test]
async fn a_thread_works_while_the_chat_does_and_stops_alone() {
    if python_missing() {
        return;
    }
    let host = Host::start().await;
    let bot = host.bot("Slow").await;
    let root = host.send(&bot, None, "task A: write the report").await;
    host.wait_for(&bot, None, |e| text(e).contains("task A") && text(e).contains("step 1.")).await;

    host.send(&bot, Some(&root), "task T: check the numbers").await;
    host.wait_for(&bot, Some(&root), |e| text(e).contains("task T") && text(e).contains("step 1.")).await;
    let main = host.reply(&bot, None, "task A").await;
    assert!(!main.contains("step 20."), "the thread started while the chat still worked: {main}");
    let thread = host.reply(&bot, Some(&root), "task T").await;
    assert_ne!(session(&main), session(&thread), "each place has its own agent: {main} / {thread}");
    let state = host.bot_state(&bot).await;
    assert_eq!(state["workingLanes"].as_array().unwrap().len(), 2, "both places show as working: {state}");

    host.call("stop", json!({"botId": bot, "threadId": root})).await;
    host.wait_for(&bot, Some(&root), |e| e["kind"] == "notice" && text(e) == "Stopped.").await;
    let main = host.wait_for(&bot, None, |e| text(e).contains("task A") && text(e).contains("step 20.")).await;
    assert!(
        !host.entries(&bot, None).await.iter().any(|e| e["kind"] == "notice" && text(e) == "Stopped."),
        "stopping the thread left the chat running: {}",
        text(&main)
    );
}

#[tokio::test]
async fn stop_in_the_chat_leaves_its_threads_running() {
    if python_missing() {
        return;
    }
    let host = Host::start().await;
    let bot = host.bot("Slow").await;
    let root = host.send(&bot, None, "task B: tidy the docs").await;
    host.wait_for(&bot, None, |e| text(e).contains("task B") && text(e).contains("step 1.")).await;
    host.send(&bot, Some(&root), "task U: list the broken links").await;
    host.wait_for(&bot, Some(&root), |e| text(e).contains("task U") && text(e).contains("step 1.")).await;

    host.call("stop", json!({"botId": bot})).await;
    host.wait_for(&bot, None, |e| e["kind"] == "notice" && text(e) == "Stopped.").await;
    host.wait_for(&bot, Some(&root), |e| text(e).contains("task U") && text(e).contains("step 20.")).await;
}

#[tokio::test]
async fn a_group_turn_runs_while_the_member_works_in_its_own_chat() {
    if python_missing() {
        return;
    }
    let host = Host::start().await;
    let ann = host.bot("Ann").await;
    let group = host.call("createBot", json!({"name": "Team", "kind": "group", "members": [ann]})).await["bot"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    host.send(&ann, None, "task C: the long 1:1 job").await;
    host.wait_for(&ann, None, |e| text(e).contains("task C") && text(e).contains("step 1.")).await;

    host.send(&group, None, "team: plan the launch").await;
    host.wait_for(&group, None, |e| text(e).contains("plan the launch") && text(e).contains("step 1.")).await;
    let own = host.reply(&ann, None, "task C").await;
    assert!(!own.contains("step 20."), "the group didn't wait for Ann's own chat: {own}");
    let state = host.bot_state(&group).await;
    assert_eq!(state["status"], "working", "the group shows working (and so Stop): {state}");
    assert_eq!(state["workingLanes"][0]["chat"], group.as_str(), "{state}");

    let room = host.reply(&group, None, "plan the launch").await;
    assert_ne!(session(&own), session(&room), "the group has its own session, not Ann's 1:1: {own} / {room}");
    assert!(!room.contains("task C"), "nothing from the 1:1 reaches the group's session: {room}");
}

#[tokio::test]
async fn a_new_session_starts_the_member_afresh_in_its_groups_too() {
    if python_missing() {
        return;
    }
    let host = Host::start().await;
    let ann = host.bot("Ann").await;
    let group = host.call("createBot", json!({"name": "Team", "kind": "group", "members": [ann]})).await["bot"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    host.send(&group, None, "team: first plan").await;
    let first = host.wait_for(&group, None, |e| text(e).contains("first plan") && text(e).contains("step 20.")).await;

    host.call("newSession", json!({"botId": ann})).await;
    host.send(&group, None, "team: second plan").await;
    let second = host.wait_for(&group, None, |e| text(e).contains("second plan") && text(e).contains("step 1.")).await;
    assert_ne!(session(text(&first)), session(text(&second)), "{} / {}", text(&first), text(&second));
}

#[tokio::test]
async fn deleting_a_group_stops_its_members_there_and_nowhere_else() {
    if python_missing() {
        return;
    }
    let host = Host::start().await;
    let ann = host.bot("Ann").await;
    let group = host.call("createBot", json!({"name": "Team", "kind": "group", "members": [ann]})).await["bot"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    host.send(&ann, None, "task D: the long 1:1 job").await;
    host.send(&group, None, "team: draft the plan").await;
    host.wait_for(&group, None, |e| text(e).contains("draft the plan") && text(e).contains("step 1.")).await;
    host.wait_for(&ann, None, |e| text(e).contains("task D") && text(e).contains("step 1.")).await;

    host.call("deleteBot", json!({"botId": group})).await;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let lanes = host.bot_state(&ann).await["workingLanes"].clone();
        if lanes.as_array().unwrap().iter().all(|l| l["chat"] == ann.as_str()) {
            assert_eq!(lanes.as_array().unwrap().len(), 1, "Ann still works in her own chat: {lanes}");
            break;
        }
        assert!(Instant::now() < deadline, "Ann still works in the deleted group: {lanes}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    host.wait_for(&ann, None, |e| text(e).contains("task D") && text(e).contains("step 20.")).await;
}
