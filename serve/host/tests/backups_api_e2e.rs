//! The apps' view of the backup chain (`backups`, `backupsSet`) against a stub `kith`. The host runs on a temporary
//! `KITH_SERVE_HOME` and connects no channel; the stub keeps the chain in a file, so nothing real is read or written.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::host::Host;
use serde_json::{Value, json};
use std::path::PathBuf;

struct Setup {
    host: Host,
    home: PathBuf,
}

async fn start() -> Setup {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let home = std::env::temp_dir().join(format!("kith-backups-api-e2e-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    let home = home.canonicalize().unwrap();
    let host = Host::start_env(&[
        ("KITH_KERNEL_BIN", &fixtures.join("stub_hq.py").to_string_lossy()),
        ("STUB_HQ_HOME", &home.to_string_lossy()),
    ])
    .await;
    Setup { host, home }
}

impl Drop for Setup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn entry<'a>(got: &'a Value, id: &str) -> &'a Value {
    got["entries"].as_array().unwrap().iter().find(|e| e["id"] == id).unwrap_or_else(|| panic!("no entry {id}"))
}

#[tokio::test]
async fn an_empty_chain_reads_as_empty() {
    let s = start().await;
    let got = s.host.call("backups", json!({})).await;
    assert_eq!(got["global"], json!([]));
    assert_eq!(got["agents"], json!({}));
    assert_eq!(got["entries"], json!([]));
}

#[tokio::test]
async fn set_replaces_the_list_and_reads_back_with_state() {
    let s = start().await;
    std::fs::write(s.home.join("not_ready.json"), r#"{"codex": "not logged in"}"#).unwrap();
    std::fs::write(
        s.home.join("accounts.json"),
        r#"[{"id": "claude:work", "harness": "claude", "name": "work", "dir": "/secret/dir", "logged_in": true}]"#,
    )
    .unwrap();
    let until = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() + 3600;
    std::fs::write(
        s.host.home.join("backups.json"),
        json!({"claude:work": {"out_until": until, "reason": "rate_limit", "since": 1}}).to_string(),
    )
    .unwrap();

    let got = s.host.call("backupsSet", json!({"agent": null, "chain": ["codex", "claude:work"]})).await;
    assert_eq!(got["global"], json!(["codex", "claude:work"]));
    let codex = entry(&got, "codex");
    assert_eq!((codex["ready"].clone(), codex["why"].clone()), (json!(false), json!("not logged in")));
    assert_eq!(codex["fix"], "kith account add codex");
    assert_eq!(codex["outUntil"], Value::Null);
    let work = entry(&got, "claude:work");
    assert_eq!(
        (work["harness"].clone(), work["account"].clone(), work["ready"].clone()),
        (json!("claude"), json!("work"), json!(true))
    );
    assert_eq!(work["outUntil"], json!(until * 1000));
    assert_eq!(work["reason"], "rate_limit");
    assert!(!got.to_string().contains("/secret/dir"), "no account folder or env leaves the host");

    // A per-agent order, then back to the global one.
    let got = s.host.call("backupsSet", json!({"agent": "house", "chain": ["claude:work"]})).await;
    assert_eq!(got["agents"], json!({"house": ["claude:work"]}));
    assert_eq!(got["global"], json!(["codex", "claude:work"]));
    let got = s.host.call("backupsSet", json!({"agent": "house", "chain": null})).await;
    assert_eq!(got["agents"], json!({}));

    // An explicit empty list is "no backups" for the agent, not "use the global order".
    let got = s.host.call("backupsSet", json!({"agent": "house", "chain": []})).await;
    assert_eq!(got["agents"], json!({"house": []}));
}

#[tokio::test]
async fn an_account_in_no_chain_is_offered() {
    let s = start().await;
    std::fs::write(
        s.home.join("accounts.json"),
        r#"[{"id": "codex:deep", "harness": "codex", "name": "deep", "logged_in": false, "why": "not logged in"}]"#,
    )
    .unwrap();
    let got = s.host.call("backups", json!({})).await;
    assert_eq!(got["global"], json!([]));
    let deep = entry(&got, "codex:deep");
    assert_eq!((deep["ready"].clone(), deep["fix"].clone()), (json!(false), json!("kith account add codex deep")));
}

#[tokio::test]
async fn set_refuses_what_kith_would_not_take() {
    let s = start().await;
    for body in [
        json!({"agent": null, "chain": ["--agent"]}),
        json!({"agent": null, "chain": ["Bad id"]}),
        json!({"agent": null, "chain": "codex"}),
        json!({"agent": "-x", "chain": []}),
        json!({"agent": null, "chain": null}),
    ] {
        let (ok, _) = s.host.post("backupsSet", &body).await;
        assert!(!ok, "{body}");
    }
    assert!(!s.home.join("chain_settings.json").exists(), "nothing was written");
}

#[tokio::test]
async fn two_quick_sets_leave_the_last_one() {
    let s = start().await;
    let a = s.host.call("backupsSet", json!({"agent": null, "chain": ["codex"]}));
    let b = s.host.call("backupsSet", json!({"agent": null, "chain": ["codex", "claude"]}));
    let _ = tokio::join!(a, b);
    let got = s.host.call("backupsSet", json!({"agent": null, "chain": ["claude", "codex"]})).await;
    assert_eq!(got["global"], json!(["claude", "codex"]));
}

#[tokio::test]
async fn each_agents_main_account_reads_above_the_chain_and_account_set_chooses_it() {
    let s = start().await;
    let got = s.host.call("backups", json!({})).await;
    assert_eq!(got["main"]["global"], Value::Null, "no global choice");
    let house = &got["main"]["agents"]["house"];
    assert_eq!((house["id"].clone(), house["source"].clone()), (json!("claude"), json!("default")), "{got}");
    assert_eq!((house["ready"].clone(), house["harness"].clone()), (json!(true), json!("claude")));

    let got = s.host.call("accountSet", json!({"agent": "house", "account": "claude:work"})).await;
    let house = &got["main"]["agents"]["house"];
    assert_eq!((house["id"].clone(), house["source"].clone()), (json!("claude:work"), json!("agent")));
    assert_eq!(std::fs::read_to_string(s.home.join("main")).unwrap(), "claude:work");

    let got = s.host.call("accountSet", json!({"agent": "house", "account": null})).await;
    assert_eq!(got["main"]["agents"]["house"]["source"], "default");
    assert!(!s.home.join("main").exists(), "back to the default login");

    for body in [
        json!({"agent": "house", "account": "--reset"}),
        json!({"agent": "house", "account": "Bad id"}),
        json!({"agent": "house", "account": 3}),
        json!({"agent": "-x", "account": "claude"}),
    ] {
        let (ok, _) = s.host.post("accountSet", &body).await;
        assert!(!ok, "{body}");
    }
    // kith's own refusal (the id doesn't fit the agent's harness) reaches the app.
    let (ok, _) = s.host.post("accountSet", &json!({"agent": "house", "account": "codex"})).await;
    assert!(!ok);
    assert!(!s.home.join("main").exists());
}
