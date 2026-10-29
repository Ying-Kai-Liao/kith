//! The bot detail page's host methods (`kithInfo`, `kithMemoryFile`, `kithMemorySearch`) against a stub `kith`.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::host::Host;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::{Duration, Instant};

struct Setup {
    host: Host,
    home: PathBuf,
    id: String,
}

async fn start() -> Setup {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let home = std::env::temp_dir().join(format!("kith-info-e2e-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(home.join("memory")).unwrap();
    std::fs::write(home.join("memory/notes.md"), "the pantry is full\n").unwrap();
    let home = home.canonicalize().unwrap();
    let stub = fixtures.join("stub_hq.py");
    let host =
        Host::start_env(&[("KITH_KERNEL_BIN", &stub.to_string_lossy()), ("STUB_HQ_HOME", &home.to_string_lossy())])
            .await;
    let deadline = Instant::now() + Duration::from_secs(20);
    let id = loop {
        if let Some(bot) = host.roster().await.into_iter().find(|b| b["hqAgent"] == "house") {
            break bot["id"].as_str().unwrap().to_owned();
        }
        assert!(Instant::now() < deadline, "house never appeared");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    Setup { host, home, id }
}

impl Drop for Setup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

impl Setup {
    fn info_runs(&self) -> usize {
        let text = std::fs::read_to_string(self.home.join("calls.jsonl")).unwrap_or_default();
        text.lines().filter(|l| serde_json::from_str::<Value>(l).unwrap()["args"][1] == "--info-json").count()
    }

    async fn file(&self, path: &str) -> (bool, Value) {
        self.host.post("kithMemoryFile", &json!({"botId": self.id, "path": path})).await
    }
}

#[tokio::test]
async fn info_passes_kith_json_through_and_is_cached() {
    let s = start().await;
    let (a, b) =
        tokio::join!(s.host.call("kithInfo", json!({"botId": s.id})), s.host.call("kithInfo", json!({"botId": s.id})));
    assert_eq!(a["info"]["agent"], "house");
    assert_eq!(a["info"]["role"]["sections"][0]["title"], "Not yours");
    assert_eq!(a["info"]["future_key"], true, "unknown keys pass through");
    assert_eq!(a["info"]["memory"]["files"][0]["path"], "notes.md");
    assert!(a["fetchedAt"].as_u64().unwrap() > 0);
    assert_eq!(a["info"], b["info"]);
    assert_eq!(s.info_runs(), 1, "two clients, one kith run");
    s.host.call("kithInfo", json!({"botId": s.id})).await;
    assert_eq!(s.info_runs(), 1, "cached");
    s.host.call("kithInfo", json!({"botId": s.id, "refresh": true})).await;
    assert_eq!(s.info_runs(), 2, "refresh forces a run");
}

#[tokio::test]
async fn a_non_kith_bot_has_no_info_and_no_memory_to_read() {
    let s = start().await;
    let plain = s.host.bot("plain", "ask").await;
    let got = s.host.call("kithInfo", json!({"botId": plain})).await;
    assert_eq!(got["info"], Value::Null);
    assert!(got.get("error").is_none());
    let (ok, body) = s.host.post("kithMemoryFile", &json!({"botId": plain, "path": "notes.md"})).await;
    assert!(!ok, "{body}");
    let (ok, _) = s.host.post("kithMemoryFile", &json!({"botId": "nope", "path": "notes.md"})).await;
    assert!(!ok);
    let (ok, _) = s.host.post("kithMemorySearch", &json!({"botId": plain, "query": "x"})).await;
    assert!(!ok);
}

/// The Schedule card's buttons: kith-sched does the change as the person the job runs for (kith-info's word, not the
/// caller's), and the answer is the job read back.
#[tokio::test]
async fn a_schedule_button_runs_kith_sched_as_the_jobs_person() {
    let s = start().await;
    let jobs =
        json!([{"id": "news", "person": "sam", "paused": false}, {"id": "mine", "person": null, "paused": false}]);
    std::fs::write(s.home.join("schedules.json"), jobs.to_string()).unwrap();
    let got = s.host.call("kithSchedule", json!({"botId": s.id, "id": "news", "op": "pause", "person": "kai"})).await;
    assert_eq!(got["info"]["schedules"][0]["paused"], true, "the job as it is now: {got}");
    let got = s.host.call("kithSchedule", json!({"botId": s.id, "id": "mine", "op": "remove"})).await;
    assert_eq!(got["info"]["schedules"].as_array().unwrap().len(), 1);

    let text = std::fs::read_to_string(s.home.join("calls.jsonl")).unwrap();
    let calls: Vec<Value> = text
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|c| c["args"][0] == "schedule")
        .collect();
    assert_eq!(calls[0]["args"], json!(["schedule", "pause", "house", "news"]));
    assert_eq!(calls[0]["person"], "sam", "as the job's person");
    assert_eq!(calls[1]["args"], json!(["schedule", "rm", "house", "mine"]));
    assert_eq!(calls[1]["person"], Value::Null, "a job of the owner's runs as the owner");

    let (ok, body) = s.host.post("kithSchedule", &json!({"botId": s.id, "id": "gone", "op": "run"})).await;
    assert!(!ok && body.to_string().contains("no scheduled job gone"), "{body}");
    let (ok, body) = s.host.post("kithSchedule", &json!({"botId": s.id, "id": "news", "op": "delete"})).await;
    assert!(!ok && body.to_string().contains("op must be"), "{body}");
    let plain = s.host.bot("plain", "ask").await;
    let (ok, _) = s.host.post("kithSchedule", &json!({"botId": plain, "id": "news", "op": "run"})).await;
    assert!(!ok, "not a kith agent");
}

#[tokio::test]
async fn kith_failing_is_an_error_field_not_an_error_status() {
    let s = start().await;
    std::fs::write(s.home.join("info_fail"), "kith is unwell").unwrap();
    let got = s.host.call("kithInfo", json!({"botId": s.id})).await;
    assert_eq!(got["info"], Value::Null);
    assert!(got["error"].as_str().unwrap().contains("kith is unwell"), "{got}");
    std::fs::remove_file(s.home.join("info_fail")).unwrap();
    tokio::time::sleep(Duration::from_millis(3200)).await;
    let healed = s.host.call("kithInfo", json!({"botId": s.id})).await;
    assert_eq!(healed["info"]["agent"], "house", "a failure is not kept for long: {healed}");
    std::fs::write(s.home.join("search_fail"), "").unwrap();
    let found = s.host.call("kithMemorySearch", json!({"botId": s.id, "query": "pantry"})).await;
    assert_eq!(found["hits"], json!([]));
    assert!(found["error"].as_str().unwrap().contains("search broke"), "{found}");
}

#[tokio::test]
async fn memory_files_are_read_inside_the_memory_folder_only() {
    let s = start().await;
    let (ok, body) = s.file("notes.md").await;
    assert!(ok, "{body}");
    assert_eq!(body["text"], "the pantry is full\n");
    assert_eq!(body["bytes"], 19);
    assert_eq!(body["truncated"], false);

    std::fs::write(s.home.join("secret.txt"), "secret").unwrap();
    std::os::unix::fs::symlink(s.home.join("secret.txt"), s.home.join("memory/link.md")).unwrap();
    std::fs::create_dir(s.home.join("memory/sub")).unwrap();
    let fifo = std::ffi::CString::new(s.home.join("memory/pipe.md").to_string_lossy().into_owned()).unwrap();
    // SAFETY: a valid NUL-terminated path owned by `fifo` for the call.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    let outside = s.home.join("secret.txt").to_string_lossy().into_owned();
    for path in
        ["../secret.txt", "sub/../../secret.txt", outside.as_str(), "link.md", "sub", "pipe.md", "", "missing.md"]
    {
        let (ok, body) = s.file(path).await;
        assert!(!ok, "{path:?} should be refused: {body}");
        assert!(body["error"].as_str().unwrap().contains('`'), "a clear message: {body}");
    }
}

#[tokio::test]
async fn a_big_memory_file_is_cut_at_512_kb() {
    let s = start().await;
    std::fs::write(s.home.join("memory/big.md"), "a".repeat(600 * 1024)).unwrap();
    let (ok, body) = s.file("big.md").await;
    assert!(ok, "{body}");
    assert_eq!(body["text"].as_str().unwrap().len(), 512 * 1024);
    assert_eq!(body["bytes"], 600 * 1024);
    assert_eq!(body["truncated"], true);
}

#[tokio::test]
async fn memory_search_passes_hits_through_with_a_limit() {
    let s = start().await;
    let found = s.host.call("kithMemorySearch", json!({"botId": s.id, "query": "pantry", "limit": 2})).await;
    assert_eq!(found["hits"].as_array().unwrap().len(), 2);
    assert_eq!(found["hits"][0]["path"], "notes.md");
    assert_eq!(found["hits"][0]["openable"], true);
    assert_eq!(found["hits"][0]["agent"], "house");
    assert!(found["hits"][0]["snippet"].as_str().unwrap().contains("agent=house"));
    let empty = s.host.call("kithMemorySearch", json!({"botId": s.id, "query": "  "})).await;
    assert_eq!(empty["hits"], json!([]));
}
