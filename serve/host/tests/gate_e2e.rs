//! The resource gate on a real host: five bots written to at once run two turns at a time, the other three
//! show "Waiting for room" and start as the first ones finish.

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
    async fn start(env: &[(&str, &str)]) -> Self {
        let home = std::env::temp_dir().join(format!("kith-gate-e2e-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        let (child, base) = common::serve_env(&home, env).await;
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

    async fn send(&self, bot: &str, text: &str) -> String {
        let body = json!({"botId": bot, "text": text, "clientNonce": uuid::Uuid::new_v4().to_string()});
        self.call("send", body).await["entry"]["id"].as_str().unwrap().to_owned()
    }

    async fn gate(&self) -> Value {
        let health: Value = reqwest::get(format!("{}/health", self.base)).await.unwrap().json().await.unwrap();
        health["gate"].clone()
    }

    /// Each bot's latest `(status, activity)` from the event stream.
    async fn states(&self) -> Vec<(String, String, String)> {
        let mut res = reqwest::Client::new()
            .get(format!("{}/events?since=0", self.base))
            .bearer_auth(&self.token)
            .send()
            .await
            .unwrap();
        let mut buf = String::new();
        let deadline = tokio::time::Instant::now() + Duration::from_millis(600);
        while let Ok(Ok(Some(chunk))) = tokio::time::timeout_at(deadline, res.chunk()).await {
            buf.push_str(&String::from_utf8_lossy(&chunk));
        }
        let mut latest: Vec<(String, String, String)> = Vec::new();
        for ev in buf
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .filter_map(|d| serde_json::from_str::<Value>(d.trim()).ok())
            .filter(|ev| ev["type"] == "bot")
        {
            let bot = &ev["bot"];
            let row = (
                bot["id"].as_str().unwrap_or_default().to_owned(),
                bot["status"].as_str().unwrap_or_default().to_owned(),
                bot["activity"].as_str().unwrap_or_default().to_owned(),
            );
            latest.retain(|r| r.0 != row.0);
            latest.push(row);
        }
        latest
    }

    async fn finished(&self, bot: &str) -> bool {
        let h = self.call("history", json!({"botId": bot})).await;
        h["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "agent" && e["data"]["text"].as_str().is_some_and(|t| t.contains("step 20.")))
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
async fn five_bots_at_once_run_two_and_queue_three() {
    if python_missing() {
        return;
    }
    let host = Host::start(&[("KITH_SERVE_MAX_RUNNING_AGENTS", "2")]).await;
    let mut bots = Vec::new();
    for i in 0..5 {
        bots.push(host.bot(&format!("Bot{i}")).await);
    }
    for (i, bot) in bots.iter().enumerate() {
        host.send(bot, &format!("job {i}")).await;
    }

    let gate = host.gate().await;
    assert_eq!((gate["running"].clone(), gate["waiting"].clone()), (json!(2), json!(3)), "gate: {gate}");
    assert_eq!(gate["maxRunning"], 2);
    assert_eq!(gate["blocked"], "slots");
    let states = host.states().await;
    let waiting: Vec<_> = states
        .iter()
        .filter(|(id, _, activity)| bots.contains(id) && activity.starts_with("Waiting for room"))
        .collect();
    assert_eq!(waiting.len(), 3, "three bots show they wait: {states:?}");
    assert!(waiting.iter().all(|(_, status, activity)| status == "working" && activity.contains("(2 agents running)")));

    // Never more than two at once, and everything finishes.
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let gate = host.gate().await;
        assert!(gate["running"].as_u64().unwrap() <= 2, "gate: {gate}");
        let mut done = 0;
        for bot in &bots {
            done += usize::from(host.finished(bot).await);
        }
        if done == 5 {
            break;
        }
        assert!(Instant::now() < deadline, "not all finished: {gate}");
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    // The slot goes back a beat after the last reply is written, so give it a moment before calling it stuck.
    let settle = Instant::now() + Duration::from_secs(5);
    let mut gate = host.gate().await;
    while (gate["running"].clone(), gate["waiting"].clone()) != (json!(0), json!(0)) && Instant::now() < settle {
        tokio::time::sleep(Duration::from_millis(100)).await;
        gate = host.gate().await;
    }
    assert_eq!((gate["running"].clone(), gate["waiting"].clone()), (json!(0), json!(0)), "gate: {gate}");
}

#[tokio::test]
async fn stopping_a_waiting_bot_takes_it_out_of_the_line() {
    if python_missing() {
        return;
    }
    let host = Host::start(&[("KITH_SERVE_MAX_RUNNING_AGENTS", "1")]).await;
    let (first, second) = (host.bot("First").await, host.bot("Second").await);
    host.send(&first, "job a").await;
    host.send(&second, "job b").await;
    assert_eq!(host.gate().await["waiting"], 1);

    host.call("stop", json!({"botId": second})).await;
    let deadline = Instant::now() + Duration::from_secs(10);
    while host.gate().await["waiting"] != 0 {
        assert!(Instant::now() < deadline, "still waiting: {}", host.gate().await);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let state = host.states().await.into_iter().find(|(id, ..)| *id == second).unwrap();
    assert_eq!((state.1.as_str(), state.2.as_str()), ("idle", ""), "the wait is cleared: {state:?}");
}
