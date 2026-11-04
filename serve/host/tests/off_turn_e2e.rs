//! An agent that wakes itself after its turn ended (a background task finished) still posts and sends files.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::Child;
use std::time::Duration;

struct Host {
    child: Child,
    home: PathBuf,
    /// The agent's folders, beside the host's data folder (a run folder inside it is the host's secrets, N1).
    work: PathBuf,
    base: String,
    token: String,
}

impl Drop for Host {
    fn drop(&mut self) {
        common::stop(&mut self.child);
        let _ = std::fs::remove_dir_all(&self.home);
        common::forget_home(&self.home);
        let _ = std::fs::remove_dir_all(&self.work);
    }
}

impl Host {
    async fn start() -> Self {
        let home = std::env::temp_dir().join(format!("kith-offturn-e2e-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        let work = std::env::temp_dir().join(format!("kith-offturn-e2e-work-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&work).unwrap();
        // A short quiet period so the off-turn turn ends within the test.
        let (child, base) = common::serve_env(&home, &[("KITH_SERVE_OFF_TURN_QUIET_MS", "800")]).await;
        let token = common::read_token(&home).trim().to_owned();
        Self { child, home, work, base, token }
    }

    async fn call(&self, method: &str, body: Value) -> Value {
        let response = reqwest::Client::new()
            .post(format!("{}/api/{method}", self.base))
            .bearer_auth(&self.token)
            .json(&body)
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .unwrap();
        let status = response.status();
        let value: Value = response.json().await.unwrap();
        assert!(status.is_success(), "{method}: {value}");
        value
    }

    async fn bot(&self) -> String {
        let cwd = self.work.join("workspace");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::write(cwd.join("report.pdf"), b"%PDF-example").unwrap();
        let agent = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/background_agent.py");
        let created = self.call("createBot", json!({"name":"Video", "backend":"custom", "tasks": false, "cwd":cwd, "command":common::python_agent(&agent), "connectors":[], "notify":false})).await;
        created["bot"]["id"].as_str().unwrap().to_owned()
    }

    async fn entries(&self, bot: &str) -> Vec<Value> {
        self.call("history", json!({"botId": bot})).await["entries"].as_array().unwrap().clone()
    }

    async fn idle(&self, bot: &str) -> bool {
        let sync = common::snapshot(&self.base, &self.token).await;
        sync["bots"].as_array().unwrap().iter().any(|b| b["id"] == bot && b["status"] == "idle")
    }
}

async fn until<F: Future<Output = bool>>(what: &str, mut check: impl FnMut() -> F) {
    let waited = tokio::time::timeout(Duration::from_secs(30), async {
        while !check().await {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    assert!(waited.is_ok(), "timed out waiting for {what}");
}

fn files(entries: &[Value]) -> usize {
    entries.iter().filter(|e| e["data"]["files"].is_array()).count()
}

#[tokio::test]
async fn a_woken_agent_posts_and_sends_in_the_chat_and_the_turn_ends_after_a_quiet_spell() {
    let host = Host::start().await;
    let bot = host.bot().await;
    host.call("send", json!({"botId": bot, "text": "background"})).await;
    until("the file", || async { files(&host.entries(&bot).await) == 1 }).await;
    let entries = host.entries(&bot).await;
    let kinds = |kind: &str| entries.iter().filter(|e| e["kind"] == kind).count();
    assert!(
        entries.iter().any(|e| e["kind"] == "tool" && e["data"]["title"] == "Render finished"),
        "the update after the turn ended is recorded: {entries:?}"
    );
    assert!(entries.iter().any(|e| e["kind"] == "agent" && e["data"]["text"] == "The render is done."));
    assert_eq!(kinds("notice"), 0, "nothing failed: {entries:?}");
    let path = host.work.join("workspace/offturn-results.json");
    until("the agent's results", || async {
        std::fs::read(&path).ok().and_then(|d| serde_json::from_slice::<Value>(&d).ok()).is_some()
    })
    .await;
    let results: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(results.as_array().unwrap().iter().all(|r| r["isError"] != true), "{results}");
    // The off-turn turn ends itself: the bot is idle again without anyone typing.
    until("idle", || async { host.idle(&bot).await }).await;
    // A second wake after the quiet finish opens a new one.
    host.call("send", json!({"botId": bot, "text": "background again"})).await;
    until("the second file", || async { files(&host.entries(&bot).await) == 2 }).await;
    until("idle again", || async { host.idle(&bot).await }).await;
}

#[tokio::test]
async fn an_off_turn_send_that_fails_leaves_a_notice() {
    let host = Host::start().await;
    let bot = host.bot().await;
    host.call("send", json!({"botId": bot, "text": "background bad"})).await;
    until("the notice", || async {
        host.entries(&bot).await.iter().any(|e| {
            e["kind"] == "notice"
                && e["data"]["text"].as_str().is_some_and(|t| t.contains("tried to send missing.mp4 but couldn't"))
        })
    })
    .await;
    assert_eq!(files(&host.entries(&bot).await), 0);
}

#[tokio::test]
async fn a_routine_is_not_adopted() {
    let host = Host::start().await;
    let bot = host.bot().await;
    // Another session keeps the agent process alive after the routine's run, as a background task would.
    host.call("send", json!({"botId": bot, "text": "hold"})).await;
    until("the first reply", || async { host.idle(&bot).await && !host.entries(&bot).await.is_empty() }).await;
    let routine = host
        .call(
            "saveRoutine",
            json!({"botId": bot, "name": "Wake", "instruction": "background", "triggers": [{"type": "webhook"}]}),
        )
        .await;
    host.call("runRoutine", json!({"botId": bot, "id": routine["routine"]["id"]})).await;
    let path = host.work.join("workspace/offturn-results.json");
    until("the agent's attempt", || async {
        std::fs::read(&path).ok().and_then(|d| serde_json::from_slice::<Value>(&d).ok()).is_some()
    })
    .await;
    let results: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(results.as_array().unwrap().iter().all(|r| r["isError"] == true), "{results}");
    let entries = host.entries(&bot).await;
    assert_eq!(files(&entries), 0, "{entries:?}");
    assert!(
        !entries
            .iter()
            .any(|e| e["kind"] == "notice" && e["data"]["text"].as_str().is_some_and(|t| t.contains("tried to")))
    );
}
