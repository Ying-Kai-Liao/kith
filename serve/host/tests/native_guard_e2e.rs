//! One-bot-model C5: a spawned host refuses a host-native row (an agent row with no kith agent behind it), flags it,
//! will not make another, and exports it. The row is one an old release left in serve.db: the test writes it while the
//! host is stopped, as nothing can make one now.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::assert_is_empty)]

#[allow(dead_code)]
mod common;

use common::host::Host;
use serde_json::json;

#[tokio::test]
async fn a_host_refuses_flags_and_exports_a_native_row() {
    let mut host = Host::start().await;
    let old = host.native_row("Oldbot").await;

    // `send` is refused with the shared message and saves nothing.
    let said = host.refused("send", json!({"botId": old, "text": "hi", "clientNonce": "n1"})).await;
    assert!(said.contains("kith agent"), "{said}");
    assert!(host.call("history", json!({"botId": old})).await["entries"].as_array().unwrap().is_empty());

    // No new agent is made through createBot; a room still is.
    let made = host.refused("createBot", json!({"name": "New", "backend": "claude", "cwd": ""})).await;
    assert!(made.contains("kith agent"), "{made}");
    host.call("createBot", json!({"name": "Room", "kind": "group", "members": [old]})).await;

    // The row is listed, flagged, and counted in the hello.
    let row = host.roster().await.into_iter().find(|b| b["id"] == old.as_str()).expect("the row stays listed");
    assert_eq!(row["needsKithAgent"], true);
    assert_eq!(host.call("hello", json!({})).await["nativeBots"], 1);

    // The export writes the bundle.
    let out = host.call("exportHostBot", json!({"botId": old})).await;
    let dir = std::path::PathBuf::from(out["path"].as_str().unwrap());
    let bot: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("bot.json")).unwrap()).unwrap();
    assert_eq!(bot["name"], "Oldbot");
    assert!(dir.join("routines.json").exists() && dir.join("vault.json").exists() && dir.join("memory").is_dir());
}
