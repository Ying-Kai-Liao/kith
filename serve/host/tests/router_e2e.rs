//! A real host routing messages sent while a bot works, against a fake Jev: "stop" ends the turn,
//! a correction restarts it with the correction merged in, another subject gets its own turn, and
//! anything else (or a router that can't be reached) joins the queue.

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
    async fn start(jev: &str) -> Self {
        let home = std::env::temp_dir().join(format!("kith-router-e2e-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        let (child, base) = common::serve_env(&home, &[("JEV_API_KEY", "test-key"), ("JEV_BASE_URL", jev)]).await;
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

    async fn bot(&self) -> String {
        let agent = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/slow_agent.py");
        let bot = json!({
            "name": "Slow", "backend": "custom", "tasks": false, "command": common::python_agent(&agent),
            "cwd": self.home, "permission": "auto", "notify": false,
        });
        self.call("createBot", bot).await["bot"]["id"].as_str().unwrap().to_owned()
    }

    async fn send(&self, bot: &str, text: &str) -> String {
        let body = json!({"botId": bot, "text": text, "clientNonce": uuid::Uuid::new_v4().to_string()});
        self.call("send", body).await["entry"]["id"].as_str().unwrap().to_owned()
    }

    async fn entries(&self, bot: &str) -> Vec<Value> {
        self.call("history", json!({"botId": bot})).await["entries"].as_array().unwrap().clone()
    }

    /// Polls the history until `pred` matches an entry.
    async fn wait_for(&self, bot: &str, pred: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(e) = self.entries(bot).await.into_iter().find(|e| pred(e)) {
                return e;
            }
            assert!(Instant::now() < deadline, "timed out; history: {:#?}", self.entries(bot).await);
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// The agent's replies, oldest first (each starts with "got: <its prompt>").
    async fn replies(&self, bot: &str) -> Vec<String> {
        self.entries(bot)
            .await
            .into_iter()
            .filter(|e| e["kind"] == "agent")
            .filter_map(|e| e["data"]["text"].as_str().map(str::to_owned))
            .collect()
    }
}

fn text(e: &Value) -> &str {
    e["data"]["text"].as_str().unwrap_or_default()
}

/// A Jev that scores by keywords in `latest`; its base URL.
async fn fake_jev() -> String {
    async fn answer(axum::Json(body): axum::Json<Value>) -> axum::Json<Value> {
        let latest = body["state"]["latest"].as_str().unwrap_or_default();
        let (stop, change, add, other) = if latest.starts_with("stop") {
            (0.97, 0.1, 0.05, 0.1)
        } else if latest.starts_with("actually") {
            (0.8, 0.96, 0.2, 0.3)
        } else if latest.starts_with("unrelated") {
            (0.05, 0.1, 0.1, 0.95)
        } else {
            (0.05, 0.1, 0.9, 0.1)
        };
        let s = |v: f64| json!({"noul": v});
        axum::Json(json!({"answers": {"stop": s(stop), "change": s(change), "add": s(add), "other": s(other)}}))
    }
    let app = axum::Router::new().route("/v1/systemone", axum::routing::post(answer));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

fn python_missing() -> bool {
    let missing = Command::new(common::python()).arg("--version").output().is_err();
    if missing {
        eprintln!("skipping: Python not available");
    }
    missing
}

#[tokio::test]
async fn stop_typed_while_working_stops_the_turn() {
    if python_missing() {
        return;
    }
    let host = Host::start(&fake_jev().await).await;
    let bot = host.bot().await;
    host.send(&bot, "task A: write the report").await;
    host.wait_for(&bot, |e| text(e).contains("step 1.")).await;
    let stop = host.send(&bot, "stop please").await;

    host.wait_for(&bot, |e| e["kind"] == "notice" && text(e) == "Stopped.").await;
    let e = host.entries(&bot).await.into_iter().find(|e| e["id"] == stop.as_str()).unwrap();
    assert_eq!(e["data"]["status"], "sent", "the stop message was acted on, not left queued: {e}");
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(
        !host.replies(&bot).await.iter().any(|r| r.contains("stop please")),
        "the agent never gets the stop message as a task"
    );
}

#[tokio::test]
async fn a_correction_restarts_the_work_with_it() {
    if python_missing() {
        return;
    }
    let host = Host::start(&fake_jev().await).await;
    let bot = host.bot().await;
    host.send(&bot, "task B: plan the July trip").await;
    host.wait_for(&bot, |e| text(e).contains("step 1.")).await;
    host.send(&bot, "actually make it August").await;

    host.wait_for(&bot, |e| text(e) == "Stopping to start again with your correction.").await;
    let redo = host.wait_for(&bot, |e| e["kind"] == "agent" && text(e).contains("stopped part-way")).await;
    let redo = text(&redo);
    assert!(redo.contains("task B: plan the July trip") && redo.contains("actually make it August"), "{redo}");
}

#[tokio::test]
async fn another_subject_gets_its_own_turn_and_more_detail_merges() {
    if python_missing() {
        return;
    }
    let host = Host::start(&fake_jev().await).await;
    let bot = host.bot().await;
    host.send(&bot, "task C: write the release notes").await;
    host.wait_for(&bot, |e| text(e).contains("step 1.")).await;
    host.send(&bot, "unrelated: what's the capital of Peru").await;
    host.send(&bot, "add the bug fixes too").await;
    host.send(&bot, "and thank the testers").await;

    host.wait_for(&bot, |e| e["kind"] == "agent" && text(e).contains("add the bug fixes too")).await;
    let replies = host.replies(&bot).await;
    let peru = replies.iter().find(|r| r.contains("capital of Peru")).unwrap();
    assert!(!peru.contains("bug fixes"), "the other subject went alone: {peru}");
    let more = replies.iter().find(|r| r.contains("add the bug fixes too")).unwrap();
    assert!(more.contains("thank the testers"), "details for the same work merged: {more}");
}

#[tokio::test]
async fn without_a_router_messages_queue_and_merge() {
    if python_missing() {
        return;
    }
    // Nothing listens there: every routing attempt fails, and messages queue as before.
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let host = Host::start(&format!("http://127.0.0.1:{port}")).await;
    let bot = host.bot().await;
    host.send(&bot, "task D: tidy the docs").await;
    host.wait_for(&bot, |e| text(e).contains("step 1.")).await;
    host.send(&bot, "stop please").await;
    host.send(&bot, "unrelated: lunch ideas").await;

    let next = host.wait_for(&bot, |e| e["kind"] == "agent" && text(e).contains("stop please")).await;
    assert!(text(&next).contains("lunch ideas"), "both waited and went together: {}", text(&next));
}
