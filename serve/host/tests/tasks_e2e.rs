//! A real host turning what the user says to a bot into tasks: the first message is a task's root in the chat, the
//! bot answers in its thread, the next message goes on with it, and a new task (asked for, or after ten quiet
//! minutes) gets a thread and a fresh agent session of its own, running beside the others.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[allow(dead_code)]
mod common;

use common::host::Host;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// A bot as the apps create it (tasks on), running the slow fake agent.
async fn slow_bot(host: &Host) -> String {
    let agent = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/slow_agent.py");
    let bot = json!({"name": "Slow", "backend": "custom", "command": common::python_agent(&agent), "connectors": [], "notify": false});
    host.call("createBot", bot).await["bot"]["id"].as_str().unwrap().to_owned()
}

async fn send(host: &Host, bot: &str, text: &str, new_task: bool) -> Value {
    let body =
        json!({"botId": bot, "text": text, "clientNonce": uuid::Uuid::new_v4().to_string(), "newTask": new_task});
    host.call("send", body).await["entry"].clone()
}

async fn thread(host: &Host, bot: &str, root: &str) -> Vec<Value> {
    host.call("thread", json!({"botId": bot, "rootId": root})).await["entries"].as_array().unwrap().clone()
}

/// Waits for the bot's answer in `root`'s thread that mentions `task` and reached `step`: "[session] got: …".
async fn answer(host: &Host, bot: &str, root: &str, task: &str, step: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let found = thread(host, bot, root).await.into_iter().filter(|e| e["kind"] == "agent").find_map(|e| {
            let text = e["data"]["text"].as_str().unwrap_or_default().to_owned();
            (text.contains(task) && text.contains(step)).then_some(text)
        });
        if let Some(text) = found {
            return text;
        }
        assert!(Instant::now() < deadline, "no answer to {task:?} in {root}: {:#?}", thread(host, bot, root).await);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn session(reply: &str) -> &str {
    reply.strip_prefix('[').and_then(|r| r.split(']').next()).unwrap_or_default()
}

fn python_missing() -> bool {
    let missing = std::process::Command::new(common::python()).arg("--version").output().is_err();
    if missing {
        eprintln!("skipping: Python not available");
    }
    missing
}

#[tokio::test]
async fn a_message_starts_a_task_and_the_next_goes_on_with_it() {
    if python_missing() {
        return;
    }
    let host = Host::start().await;
    let bot = slow_bot(&host).await;
    let first = send(&host, &bot, "task A: tidy the docs", false).await;
    assert_eq!(first["threadId"], Value::Null, "a task's first message is a root in the chat: {first}");
    assert_eq!(first["data"]["task"], true, "{first}");
    let root = first["id"].as_str().unwrap().to_owned();
    let a = answer(&host, &bot, &root, "task A", "step 1.").await;
    assert!(!a.contains("[Thread]"), "a task starts fresh, with nothing from the chat before it: {a}");
    let history = host.call("history", json!({"botId": bot})).await;
    assert!(
        history["entries"].as_array().unwrap().iter().all(|e| e["kind"] != "agent"),
        "the bot answers in the task's thread, not the chat: {history}"
    );

    let second = send(&host, &bot, "also the README", false).await;
    assert_eq!(second["threadId"], root.as_str(), "a message while the task goes on joins it: {second}");
    let a = answer(&host, &bot, &root, "also the README", "step 20.").await;
    assert_eq!(session(&a), session(&answer(&host, &bot, &root, "task A", "step 1.").await), "same session");
}

#[tokio::test]
async fn a_new_task_runs_beside_the_last_in_a_session_of_its_own_and_stops_alone() {
    if python_missing() {
        return;
    }
    let host = Host::start().await;
    let bot = slow_bot(&host).await;
    let a = send(&host, &bot, "task A: the long build", false).await;
    let a = a["id"].as_str().unwrap().to_owned();
    let a_reply = answer(&host, &bot, &a, "task A", "step 1.").await;

    let b = send(&host, &bot, "task B: a quick question", true).await;
    assert_eq!(b["threadId"], Value::Null, "New task starts one even while the last goes on: {b}");
    assert_eq!(b["data"]["task"], true);
    let b = b["id"].as_str().unwrap().to_owned();
    let b_reply = answer(&host, &bot, &b, "task B", "step 1.").await;
    assert_ne!(session(&a_reply), session(&b_reply), "each task has its own session: {a_reply} / {b_reply}");
    let a_now = answer(&host, &bot, &a, "task A", "step 1.").await;
    assert!(!a_now.contains("step 20."), "task B didn't wait for task A: {a_now}");

    host.call("stop", json!({"botId": bot, "threadId": b})).await;
    let deadline = Instant::now() + Duration::from_secs(20);
    while !thread(&host, &bot, &b).await.iter().any(|e| e["kind"] == "notice" && e["data"]["text"] == "Stopped.") {
        assert!(Instant::now() < deadline, "task B didn't stop");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    answer(&host, &bot, &a, "task A", "step 20.").await;
    assert!(
        !thread(&host, &bot, &a).await.iter().any(|e| e["kind"] == "notice" && e["data"]["text"] == "Stopped."),
        "stopping task B left task A running"
    );
}

/// A card's Reply points the box at its task: the message carries the task's root as its thread, and lands there
/// (in that task's session) even while a newer task is the one the box would go on with.
#[tokio::test]
async fn a_reply_aimed_at_an_older_task_lands_in_its_thread() {
    if python_missing() {
        return;
    }
    let host = Host::start().await;
    let bot = slow_bot(&host).await;
    let a = send(&host, &bot, "task A: the long build", false).await;
    let a = a["id"].as_str().unwrap().to_owned();
    let a_reply = answer(&host, &bot, &a, "task A", "step 1.").await;
    let b = send(&host, &bot, "task B: a quick question", true).await;
    let b = b["id"].as_str().unwrap().to_owned();
    answer(&host, &bot, &b, "task B", "step 1.").await;

    let body = json!({"botId": bot, "text": "for A: use the cache", "threadId": a,
        "clientNonce": uuid::Uuid::new_v4().to_string()});
    let reply = host.call("send", body).await["entry"].clone();
    assert_eq!(reply["threadId"], a.as_str(), "the reply joins task A, not the newer B: {reply}");
    assert_ne!(reply["data"]["task"], true, "a reply starts no task: {reply}");
    let got = answer(&host, &bot, &a, "use the cache", "step").await;
    assert_eq!(session(&got), session(&a_reply), "task A's own session answers: {got}");
    assert!(
        !thread(&host, &bot, &b)
            .await
            .iter()
            .any(|e| e["data"]["text"].as_str().unwrap_or_default().contains("use the cache")),
        "nothing of it reaches task B"
    );
}
