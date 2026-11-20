//! A real host whose agent is slow to start a session: Stop still works meanwhile, and a `send_file` the agent gave
//! up on never shows up afterwards.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[allow(dead_code)]
mod common;

use common::host::Host;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

fn python_missing() -> bool {
    let missing = std::process::Command::new(common::python()).arg("--version").output().is_err();
    if missing {
        eprintln!("skipping: Python not available");
    }
    missing
}

async fn history(host: &Host, bot: &str) -> Vec<Value> {
    host.call("history", json!({"botId": bot})).await["entries"].as_array().unwrap().clone()
}

async fn wait_until(what: &str, timeout: Duration, mut done: impl AsyncFnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !done().await {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn status(host: &Host, bot: &str) -> String {
    let roster = host.roster().await;
    roster.iter().find(|b| b["id"] == bot).map(|b| b["status"].as_str().unwrap_or_default().to_owned()).unwrap()
}

#[tokio::test]
async fn stop_ends_a_turn_that_is_still_starting_and_the_next_message_starts_afresh() {
    if python_missing() {
        return;
    }
    let host = Host::start().await;
    // session/new takes 60 s the first time: the turn is stuck starting.
    let bot = host.fixture_bot("Stalled", "stall_agent.py", "60 0").await;
    host.call("send", json!({"botId": bot, "text": "first", "clientNonce": "n1"})).await;
    wait_until("the turn to start", Duration::from_secs(10), async || status(&host, &bot).await == "working").await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    host.call("stop", json!({"botId": bot})).await;
    wait_until("the stop to end the start", Duration::from_secs(10), async || status(&host, &bot).await == "idle")
        .await;
    assert!(history(&host, &bot).await.iter().any(|e| e["kind"] == "notice" && e["data"]["text"] == "Stopped."));

    // A fresh agent process answers at once.
    host.reply(&bot, "second").await;
    let echoed =
        history(&host, &bot).await.iter().any(|e| e["data"]["text"].as_str().is_some_and(|t| t.contains("second")));
    assert!(echoed);
}

/// A `send_file` the agent makes while the session is still starting; `abandon`: its tool call is dropped (its own
/// timeout) before the host takes the share. Whether a file message was posted in the end.
async fn share_during_a_slow_start(abandon: bool) -> bool {
    let host = Host::start().await;
    // The session takes 13 s, so the host's actor can't take the share until the turn has started. The turn then
    // runs 4 s more, so a late share would still find a turn to post into.
    let bot = host.fixture_bot("Slow start", "stall_agent.py", "13 4").await;
    // In the agent's own folder: a share outside the run's folders is refused, which would pass the test for free.
    let file = host.agent_dir("Slow start").join("late.txt");
    std::fs::write(&file, "late file").unwrap();
    host.call("send", json!({"botId": bot, "text": "go", "clientNonce": "n1"})).await;
    wait_until("the turn to start", Duration::from_secs(10), async || status(&host, &bot).await == "working").await;
    let body = json!({"botId": bot, "name": "send_file", "arguments": {"path": file.to_string_lossy()}});
    if abandon {
        let call = tokio::time::timeout(Duration::from_secs(2), host.post("chatCall", &body)).await;
        assert!(call.is_err(), "the share should still be waiting for the slow start");
    } else {
        let (ok, answer) = host.post("chatCall", &body).await;
        assert!(ok, "the share should go through once the turn runs: {answer}");
    }
    wait_until("the turn to finish", Duration::from_secs(30), async || {
        history(&host, &bot).await.iter().any(|e| e["data"]["text"].as_str().is_some_and(|t| t.starts_with("Echo: ")))
    })
    .await;
    wait_until("the bot to be idle", Duration::from_secs(10), async || status(&host, &bot).await == "idle").await;
    history(&host, &bot).await.iter().any(|e| e["data"]["files"].is_array())
}

#[tokio::test]
async fn a_file_share_the_agent_gave_up_on_is_not_posted_later() {
    if python_missing() {
        return;
    }
    assert!(!share_during_a_slow_start(true).await, "the share the agent gave up on was posted");
}

/// The control: the same share, not abandoned, is posted, so the test above can fail.
#[tokio::test]
async fn the_same_file_share_not_given_up_on_is_posted() {
    if python_missing() {
        return;
    }
    assert!(share_during_a_slow_start(false).await, "the share was not posted");
}
