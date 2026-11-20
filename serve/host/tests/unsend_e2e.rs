//! A real host taking back messages the user sent while the bot worked: an unsent message leaves the
//! queue as "cancelled" and never reaches the agent, the rest still go, and a message whose turn
//! already started can't be taken back.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[allow(dead_code)]
mod common;

use common::host::Host;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// A host whose router can't be reached, so messages sent while the bot works simply queue.
async fn host() -> Host {
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let jev = format!("http://127.0.0.1:{port}");
    Host::start_env(&[("JEV_API_KEY", "test-key"), ("JEV_BASE_URL", &jev)]).await
}

async fn slow_bot(host: &Host, tasks: bool) -> String {
    if tasks {
        host.task_agent("slow", "slow_agent.py", "").await
    } else {
        host.agent("slow", "slow_agent.py", "").await
    }
}

async fn send(host: &Host, bot: &str, text: &str) -> Value {
    let body = json!({"botId": bot, "text": text, "clientNonce": uuid::Uuid::new_v4().to_string()});
    host.call("send", body).await["entry"].clone()
}

/// The chat (`root` None) or a thread, oldest first.
async fn entries(host: &Host, bot: &str, root: Option<&str>) -> Vec<Value> {
    let page = match root {
        Some(root) => host.call("thread", json!({"botId": bot, "rootId": root})).await,
        None => host.call("history", json!({"botId": bot})).await,
    };
    page["entries"].as_array().unwrap().clone()
}

/// Waits for an agent reply in the chat or thread that contains `text`.
async fn reply(host: &Host, bot: &str, root: Option<&str>, text: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let found = entries(host, bot, root).await.into_iter().filter(|e| e["kind"] == "agent").find_map(|e| {
            let t = e["data"]["text"].as_str().unwrap_or_default().to_owned();
            t.contains(text).then_some(t)
        });
        if let Some(t) = found {
            return t;
        }
        assert!(Instant::now() < deadline, "no reply with {text:?}: {:#?}", entries(host, bot, root).await);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn id(e: &Value) -> &str {
    e["id"].as_str().unwrap()
}

fn python_missing() -> bool {
    let missing = std::process::Command::new(common::python()).arg("--version").output().is_err();
    if missing {
        eprintln!("skipping: Python not available");
    }
    missing
}

#[tokio::test]
async fn an_unsent_message_never_reaches_the_agent_and_the_next_still_goes() {
    if python_missing() {
        return;
    }
    let host = host().await;
    let bot = slow_bot(&host, false).await;
    let running = send(&host, &bot, "task A: write the report").await;
    reply(&host, &bot, None, "step 1.").await;
    let a = send(&host, &bot, "first: the old figures").await;
    let b = send(&host, &bot, "second: the new figures").await;
    assert_eq!(a["data"]["status"], "queued", "{a}");

    let unsent = host.call("unsend", json!({"botId": bot, "entryId": id(&a)})).await;
    assert_eq!(unsent["entry"]["data"]["status"], "cancelled", "{unsent}");
    let again = host.call("unsend", json!({"botId": bot, "entryId": id(&a)})).await;
    assert_eq!(again["entry"]["data"]["status"], "cancelled", "taking it back twice is fine: {again}");
    let error = host.refused("unsend", json!({"botId": bot, "entryId": id(&running)})).await;
    assert!(error.contains("already"), "the running message can't be unsent: {error}");

    let next = reply(&host, &bot, None, "second: the new figures").await;
    assert!(!next.contains("first: the old figures"), "the unsent message stayed out of the turn: {next}");
    let error = host.refused("unsend", json!({"botId": bot, "entryId": id(&b)})).await;
    assert!(error.contains("already"), "a message whose turn started can't be unsent: {error}");
    reply(&host, &bot, None, "step 20.").await;
    let history = entries(&host, &bot, None).await;
    let replies: Vec<&str> =
        history.iter().filter(|e| e["kind"] == "agent").filter_map(|e| e["data"]["text"].as_str()).collect();
    assert!(replies.iter().all(|r| !r.contains("first: the old figures")), "the agent never got it: {replies:#?}");
    let a = history.iter().find(|e| e["id"] == id(&a)).unwrap();
    assert_eq!(a["data"]["status"], "cancelled", "{a}");
}

#[tokio::test]
async fn a_message_waiting_in_a_task_can_be_unsent() {
    if python_missing() {
        return;
    }
    let host = host().await;
    let bot = slow_bot(&host, true).await;
    let root = send(&host, &bot, "task A: tidy the docs").await;
    let root = id(&root).to_owned();
    reply(&host, &bot, Some(&root), "step 1.").await;
    let more = send(&host, &bot, "also the README").await;
    assert_eq!(more["threadId"], root.as_str(), "it goes on with the task: {more}");

    let unsent = host.call("unsend", json!({"botId": bot, "entryId": id(&more)})).await;
    assert_eq!(unsent["entry"]["data"]["status"], "cancelled", "{unsent}");
    reply(&host, &bot, Some(&root), "step 20.").await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let thread = entries(&host, &bot, Some(&root)).await;
    assert!(
        thread
            .iter()
            .filter(|e| e["kind"] == "agent")
            .all(|e| !e["data"]["text"].as_str().unwrap_or_default().contains("README")),
        "the agent never got it: {thread:#?}"
    );
}

#[tokio::test]
async fn only_the_users_own_messages_can_be_unsent() {
    let host = host().await;
    let bot = host.agent("idle", "slow_agent.py", "").await;
    let error = host.refused("unsend", json!({"botId": bot, "entryId": "nope"})).await;
    assert!(error.contains("unknown message"), "{error}");
}
