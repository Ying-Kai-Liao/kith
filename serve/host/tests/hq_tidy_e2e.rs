//! An hq agent's bot is tidied: after a user turn and a quiet spell the host reads the conversation with a scripted
//! one-shot model (no agent slot, no agent launch), writes what survives the audit into the agent's memory files,
//! logs the run, records its cost, and the apps' memory calls can read, restore and undo it. Temporary
//! `KITH_SERVE_HOME`, `KITH_STATE` and `KITH_HOME`; channels stay off.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::host::Host;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::{Duration, Instant};

struct Setup {
    host: Host,
    root: PathBuf,
    /// The agent's folder, `<KITH_HOME>/agents/house`.
    house: PathBuf,
    state: PathBuf,
}

impl Drop for Setup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

async fn start() -> Setup {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let root = std::env::temp_dir().join(format!("kith-hq-tidy-e2e-{}", uuid::Uuid::new_v4()));
    let (state, kith_home) = (root.join("state"), root.join("home"));
    let house = kith_home.join("agents/house");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&house).unwrap();
    let root = root.canonicalize().unwrap();
    let (state, house, kith_home) = (root.join("state"), root.join("home/agents/house"), root.join("home"));
    let acp = common::python_agent(&fixtures.join("hq_tidy_agent.py"));
    let host = Host::start_env(&[
        ("KITH_KERNEL_BIN", &fixtures.join("stub_hq.py").to_string_lossy()),
        ("STUB_HQ_HOME", &house.to_string_lossy()),
        ("KITH_SERVE_HQ_ACP_COMMAND", &acp),
        ("KITH_STATE", &state.to_string_lossy()),
        ("KITH_HOME", &kith_home.to_string_lossy()),
        ("KITH_SERVE_TIDY_IDLE_MS", "400"),
    ])
    .await;
    Setup { host, root, house, state }
}

impl Setup {
    async fn house_bot(&self) -> String {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(bot) = self.host.roster().await.into_iter().find(|b| b["hqAgent"] == "house") {
                return bot["id"].as_str().unwrap().to_owned();
            }
            assert!(Instant::now() < deadline, "house never appeared");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    fn memory_file(&self) -> PathBuf {
        self.house.join("memory/MEMORY.md")
    }

    fn lines(&self, file: &str) -> Vec<Value> {
        let text = std::fs::read_to_string(self.house.join(file)).unwrap_or_default();
        text.lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    async fn tidy_ups(&self, bot: &str) -> Vec<Value> {
        self.host.call("kithMemoryTidyUps", json!({"botId": bot})).await["tidyUps"].as_array().unwrap().clone()
    }

    /// The first tidy-up run that wrote files, once the idle spell has passed.
    async fn first_tidy(&self, bot: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(run) = self.tidy_ups(bot).await.into_iter().next() {
                return run;
            }
            assert!(Instant::now() < deadline, "no tidy ran");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

#[tokio::test]
async fn an_hq_bot_is_tidied_into_its_files_without_launching_the_agent_and_it_can_be_undone() {
    let s = start().await;
    let bot = s.house_bot().await;
    s.host.call("updateBot", json!({"id": bot, "tasks": false})).await;
    s.host.reply(&bot, "remember I prefer tea over coffee").await;

    let run = s.first_tidy(&bot).await;
    let memory = std::fs::read_to_string(s.memory_file()).unwrap();
    assert_eq!(memory, "# Memory\n\n## From conversations\n- Prefers tea over coffee\n");
    assert_eq!(run["changes"][0]["kind"], "added");
    assert_eq!(run["changes"][0]["text"], "Prefers tea over coffee");
    assert_eq!(run["files"][0]["name"], "MEMORY.md");
    assert_eq!(run["undone"], false);

    // The chat launched the agent once; the tidy took none: it ran on plain Claude (haiku, no tools, no settings,
    // no saved session) in the host's background folder.
    let launches = s.lines("calls.jsonl").into_iter().filter(|c| c["args"][1] == "--launch-json").count();
    assert_eq!(launches, 1);
    let background = s.host.home.join("background/sessions.jsonl");
    let jobs: Vec<Value> =
        std::fs::read_to_string(background).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(jobs.len(), 1, "one tidy-up, one job");
    let options = &jobs[0]["params"]["_meta"]["claudeCode"]["options"];
    assert_eq!(options["model"], "haiku");
    assert_eq!(options["tools"], json!([]));
    assert_eq!(options["settingSources"], json!([]));
    assert_eq!(options["persistSession"], false);
    assert_eq!(jobs[0]["params"]["mcpServers"], json!([]));

    // Its cost is in the agent's ledger as its tidy-up.
    let deadline = Instant::now() + Duration::from_secs(10);
    let cost = loop {
        if let Some(row) = s.lines("cost.jsonl").into_iter().next() {
            break row;
        }
        assert!(Instant::now() < deadline, "no cost row");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!((cost["agent"].as_str(), cost["via"].as_str()), (Some("house"), Some("tidy")));
    assert_eq!(cost["row"]["input"], 120);
    assert_eq!(cost["row"]["output"], 30);
    assert_eq!(cost["row"]["cache_read"], 10);
    assert_eq!(cost["row"]["model"], "haiku");

    // The apps list the files and read the history: the tidy-up's version is the newest.
    let files = s.host.call("kithMemoryList", json!({"botId": bot})).await;
    assert_eq!(files["agent"][0]["name"], "MEMORY.md");
    let history = s.host.call("kithMemoryHistory", json!({"botId": bot, "name": "MEMORY.md"})).await;
    assert_eq!(history["versions"][0]["author"], "tidy");
    assert_eq!(history["versions"][0]["authorBot"], "house");

    // Undo puts the file back: it did not exist before, so it goes.
    let undone = s.host.call("kithMemoryUndoTidy", json!({"botId": bot, "id": run["id"]})).await;
    assert_eq!(undone["undone"], true);
    assert!(!s.memory_file().exists());
    assert_eq!(s.tidy_ups(&bot).await[0]["undone"], true);
    // The old call names (released apps) answer too, with the list under both keys.
    let old = s.host.call("kithMemoryDreams", json!({"botId": bot})).await;
    assert_eq!(old["dreams"], old["tidyUps"]);
    assert_eq!(s.host.call("kithMemoryUndoDream", json!({"botId": bot, "id": run["id"]})).await["undone"], true);

    // Kai's edit is never refused, is a version of his, and restoring brings the tidy-up's version back.
    let saved = s
        .host
        .call("kithMemorySave", json!({"botId": bot, "name": "MEMORY.md", "text": "# Memory\n- Kai wrote this\n"}))
        .await;
    assert_eq!(saved["version"]["author"], "kai");
    let history = s.host.call("kithMemoryHistory", json!({"botId": bot, "name": "MEMORY.md"})).await;
    let versions = history["versions"].as_array().unwrap();
    let tidy_version = versions.iter().find(|v| v["author"] == "tidy").unwrap()["id"].clone();
    let diff = s.host.call("kithMemoryDiff", json!({"botId": bot, "name": "MEMORY.md", "from": tidy_version})).await;
    assert!(diff["diff"].as_str().unwrap().contains("+ - Kai wrote this"), "{diff}");
    s.host.call("kithMemoryRestore", json!({"botId": bot, "name": "MEMORY.md", "version": tidy_version})).await;
    assert!(std::fs::read_to_string(s.memory_file()).unwrap().contains("Prefers tea over coffee"));

    // Restoring a file of another agent's layer, or a path outside memory, is refused.
    let refused = s.host.refused("kithMemoryRead", json!({"botId": bot, "name": "../role/agent.md"})).await;
    assert!(refused.contains("isn't a memory file"), "{refused}");
    assert!(s.state.join("memory.db").exists());
}

#[tokio::test]
async fn kai_deletes_a_memory_file_and_forgets_a_fact_from_the_apps() {
    let s = start().await;
    let bot = s.house_bot().await;
    let notes = s.house.join("memory/notes.md");
    std::fs::create_dir_all(notes.parent().unwrap()).unwrap();
    std::fs::write(&notes, "- likes zebras\n").unwrap();

    let gone = s.host.call("kithMemoryDelete", json!({"botId": bot, "name": "notes.md"})).await;
    assert_eq!((gone["version"]["cause"].as_str(), gone["version"]["deleted"].as_bool()), (Some("delete"), Some(true)));
    assert!(!notes.exists());
    let history = s.host.call("kithMemoryHistory", json!({"botId": bot, "name": "notes.md"})).await;
    let versions = history["versions"].as_array().unwrap();
    assert_eq!(versions[0]["deleted"], true);
    assert_eq!(versions[1]["cause"], "baseline");
    s.host.call("kithMemoryRestore", json!({"botId": bot, "name": "notes.md", "version": versions[1]["id"]})).await;
    assert!(std::fs::read_to_string(&notes).unwrap().contains("zebras"));

    let missing = s.host.refused("kithMemoryDelete", json!({"botId": bot, "name": "nope.md"})).await;
    assert!(missing.contains("no such file"), "{missing}");
    std::fs::write(s.memory_file(), "# Memory\n").unwrap();
    let index = s.host.refused("kithMemoryDelete", json!({"botId": bot, "name": "MEMORY.md"})).await;
    assert!(index.contains("index"), "{index}");
    assert!(s.memory_file().exists());
    let outside = s.host.refused("kithMemoryDelete", json!({"botId": bot, "name": "../role/agent.md"})).await;
    assert!(outside.contains("isn't a memory file"), "{outside}");

    let forgot = s.host.call("kithMemoryForget", json!({"botId": bot, "text": "Likes zebras"})).await;
    assert_eq!((forgot["forgotten"].as_bool(), forgot["text"].as_str()), (Some(true), Some("Likes zebras")));
    assert_eq!(s.host.call("kithMemoryForget", json!({"botId": bot, "text": "Likes zebras"})).await["forgotten"], true);
    let empty = s.host.refused("kithMemoryForget", json!({"botId": bot, "text": "  "})).await;
    assert!(empty.contains("text required"), "{empty}");
}

#[tokio::test]
async fn a_plan_the_user_never_said_changes_nothing_and_is_not_retried() {
    let s = start().await;
    let bot = s.house_bot().await;
    s.host.call("updateBot", json!({"id": bot, "tasks": false})).await;
    s.host.reply(&bot, "hello there, nothing worth remembering").await;
    // The tidy-up runs (the scripted model answers) and its one line quotes words the user never typed: the audit
    // drops it, so there is no file and no logged run, and the conversation counts as read.
    let jobs = s.host.home.join("background/sessions.jsonl");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !jobs.exists() {
        assert!(Instant::now() < deadline, "the tidy never ran");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(!s.memory_file().exists());
    assert_eq!(s.tidy_ups(&bot).await, Vec::<Value>::new());
    assert_eq!(std::fs::read_to_string(&jobs).unwrap().lines().count(), 1, "not asked again");
}
