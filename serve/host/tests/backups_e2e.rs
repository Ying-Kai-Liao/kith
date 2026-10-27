//! Backup subscriptions end to end: an hq agent whose account runs out of usage moves its turns to the next entry of
//! the user's chain and comes back when the limit lifts. The kernel is a stub `kith` that prints a chain and a launch
//! per entry; the agent is `limit_agent.py`, which fails on cue the way the adapters do. No real channels: the host
//! runs on a temporary `KITH_SERVE_HOME`.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::host::Host;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Setup {
    host: Host,
    /// The agent's folder: the stub's files and the agent's logs are in it.
    home: PathBuf,
    bot: String,
}

fn now() -> i64 {
    i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()).unwrap()
}

fn entry(id: &str) -> Value {
    let (harness, account) = id.split_once(':').map_or((id, None), |(h, a)| (h, Some(a)));
    json!({"id": id, "harness": harness, "account": account, "kind": "subscription", "ready": true})
}

async fn start(chain: &[&str]) -> Setup {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let home = std::env::temp_dir().join(format!("kith-backups-e2e-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    let acp = common::python_agent(&fixtures.join("limit_agent.py"));
    let host = Host::start_env(&[
        ("KITH_KERNEL_BIN", &fixtures.join("stub_hq.py").to_string_lossy()),
        ("STUB_HQ_HOME", &home.to_string_lossy()),
        ("KITH_SERVE_HQ_ACP_COMMAND", &acp),
    ])
    .await;
    let mut s = Setup { host, home, bot: String::new() };
    s.set_chain(chain);
    let deadline = Instant::now() + Duration::from_secs(20);
    s.bot = loop {
        if let Some(bot) = s.host.roster().await.into_iter().find(|b| b["hqAgent"] == "house") {
            break bot["id"].as_str().unwrap().to_owned();
        }
        assert!(Instant::now() < deadline, "house never appeared");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    s.host.call("updateBot", json!({"id": s.bot, "tasks": false})).await;
    s
}

impl Drop for Setup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

impl Setup {
    /// The backups after the agent's own login, as `kith house --backups-json` prints them. The host asks again
    /// within half a minute, so a test sets its chain before the first message.
    fn set_chain(&self, chain: &[&str]) {
        let chain: Vec<Value> = chain.iter().map(|id| entry(id)).collect();
        std::fs::write(self.home.join("chain.json"), json!(chain).to_string()).unwrap();
    }

    /// `account` (`primary`, `claude:work`…) fails its next prompts like this; `None` lets it answer again.
    fn limit(&self, account: &str, limit: Option<Value>) {
        let path = self.home.join("limits.json");
        let mut all: serde_json::Map<String, Value> =
            std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        match limit {
            Some(limit) => all.insert(account.to_owned(), limit),
            None => all.remove(account),
        };
        std::fs::write(path, Value::Object(all).to_string()).unwrap();
    }

    fn requests(&self, account: &str, method: &str) -> Vec<Value> {
        let text = std::fs::read_to_string(self.home.join("requests.jsonl")).unwrap_or_default();
        let all = text.lines().map(|l| serde_json::from_str::<Value>(l).unwrap());
        all.filter(|r| r["account"] == account && r["method"] == method).collect()
    }

    /// The prompts `account` got, oldest first.
    fn prompts(&self, account: &str) -> Vec<String> {
        let text = |r: &Value| {
            let blocks = r["params"]["prompt"].as_array().unwrap();
            blocks.iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n")
        };
        self.requests(account, "session/prompt").iter().map(text).collect()
    }

    fn backups_file(&self) -> Value {
        let text = std::fs::read_to_string(self.host.home.join("backups.json")).unwrap_or_else(|_| "{}".into());
        serde_json::from_str(&text).unwrap()
    }

    /// The `--launch-json` calls that named a backup entry.
    fn backup_launches(&self) -> Vec<String> {
        let text = std::fs::read_to_string(self.home.join("calls.jsonl")).unwrap_or_default();
        text.lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .filter_map(|c| {
                let args = c["args"].as_array().unwrap();
                let at = args.iter().position(|a| a == "--backup")?;
                Some(args[at + 1].as_str().unwrap().to_owned())
            })
            .collect()
    }

    async fn ask(&self, text: &str) {
        self.host.reply(&self.bot, text).await;
    }

    /// Every final reply of the chat, oldest first: its text and who answered.
    async fn answers(&self) -> Vec<(String, Value)> {
        let history = self.host.call("history", json!({"botId": self.bot})).await;
        history["entries"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["kind"] == "agent" && e["data"]["final"] == true)
            .map(|e| (e["data"]["text"].as_str().unwrap().to_owned(), e["data"]["answeredBy"].clone()))
            .collect()
    }

    /// The notices of the chat containing `needle`, oldest first.
    async fn notices(&self, needle: &str) -> Vec<String> {
        let history = self.host.call("history", json!({"botId": self.bot})).await;
        let all = history["entries"].as_array().unwrap().iter();
        all.filter(|e| e["kind"] == "notice")
            .filter_map(|e| e["data"]["text"].as_str().filter(|t| t.contains(needle)).map(str::to_owned))
            .collect()
    }

    /// Sends `text` and waits for one more notice containing `needle`.
    async fn ask_for_notice(&self, text: &str, needle: &str) -> String {
        let before = self.notices(needle).await.len();
        let nonce = uuid::Uuid::new_v4().to_string();
        self.host.call("send", json!({"botId": self.bot, "text": text, "clientNonce": nonce})).await;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let mut found = self.notices(needle).await;
            if found.len() > before {
                return found.pop().unwrap();
            }
            assert!(
                Instant::now() < deadline,
                "no notice with {needle:?}: {}",
                self.host.call("history", json!({"botId": self.bot})).await
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

fn limited(kind: &str, resets_in: i64) -> Value {
    json!({"kind": kind, "resetsAt": now() + resets_in, "via": "error"})
}

#[tokio::test]
async fn a_turn_that_ran_out_of_usage_is_answered_by_the_next_entry_and_the_next_one_starts_there() {
    let s = start(&["claude:work"]).await;
    s.ask("first, on the usual account").await;
    s.limit("primary", Some(limited("rate_limit", 3600)));

    s.ask("second, when the usual account is out").await;

    let answers = s.answers().await;
    assert_eq!(answers[0], ("answered by primary".to_owned(), Value::Null), "null: the agent's own");
    assert_eq!(answers[1], ("answered by claude:work".to_owned(), json!("claude:work")));
    assert_eq!(s.prompts("primary").len(), 2, "the usual account was tried once");
    assert_eq!(s.prompts("claude:work").len(), 1, "and the turn retried once");
    let backup_prompt = &s.prompts("claude:work")[0];
    assert!(backup_prompt.contains("second, when the usual account is out"), "{backup_prompt}");
    assert!(
        backup_prompt.contains("first, on the usual account") && backup_prompt.contains("answered by primary"),
        "the backup is told the conversation so far: {backup_prompt}"
    );
    let marked = &s.backups_file()["claude"];
    assert_eq!(marked["reason"], "rate_limit");
    assert!((marked["out_until"].as_i64().unwrap() - (now() + 3600)).abs() < 30, "{marked}");

    // The next turn starts on the backup without trying the usual account again, in the session it opened.
    s.ask("third").await;
    assert_eq!(s.prompts("primary").len(), 2);
    assert_eq!(s.prompts("claude:work").len(), 2);
    assert!(!s.prompts("claude:work")[1].contains("The latest messages"), "a live session needs no recap");
    assert_eq!(s.requests("claude:work", "session/new").len(), 1);
    assert_eq!(s.answers().await[2].1, json!("claude:work"));
    assert_eq!(s.backup_launches(), vec!["claude:work"; 1], "hq was asked for the entry once");
}

#[tokio::test]
async fn a_rejected_rate_limit_update_counts_even_when_the_error_has_no_category() {
    let s = start(&["claude:work"]).await;
    s.limit("primary", Some(json!({"via": "rejected", "resetsAt": now() + 600})));
    s.ask("hello").await;
    assert_eq!(s.answers().await[0].1, json!("claude:work"));
    let out = s.backups_file()["claude"]["out_until"].as_i64().unwrap();
    assert!((out - (now() + 600)).abs() < 30, "the update's reset time marks the entry: {out}");
}

#[tokio::test]
async fn a_busy_service_is_not_depletion() {
    let s = start(&["claude:work"]).await;
    s.limit("primary", Some(limited("overloaded", 600)));
    let notice = s.ask_for_notice("hello", "The agent failed").await;
    assert!(notice.contains("Internal error"), "{notice}");
    assert_eq!(s.prompts("claude:work"), Vec::<String>::new());
    assert_eq!(s.backups_file(), json!({}), "nothing marked out");
    assert_eq!(s.backup_launches(), Vec::<String>::new());
}

#[tokio::test]
async fn without_a_chain_a_limit_is_the_error_it_always_was() {
    let s = start(&[]).await;
    s.limit("primary", Some(limited("rate_limit", 600)));
    s.ask_for_notice("hello", "The agent failed").await;
    assert_eq!(s.prompts("primary").len(), 1);
    assert_eq!(s.backup_launches(), Vec::<String>::new());
    assert_eq!(s.backups_file(), json!({}), "no file of state either");
    assert!(!s.host.home.join("backups.json").exists());
}

#[tokio::test]
async fn a_turn_moves_on_once_per_entry_and_never_loops() {
    let s = start(&["claude:work"]).await;
    s.limit("primary", Some(limited("rate_limit", 7200)));
    s.limit("claude:work", Some(limited("rate_limit", 1800)));

    let notice = s.ask_for_notice("hello", "out of usage").await;

    assert!(notice.contains("claude (back in") && notice.contains("claude:work (back in"), "{notice}");
    assert!(notice.contains("The first is back in 30m") || notice.contains("The first is back in 29m"), "{notice}");
    assert_eq!(s.prompts("primary").len(), 1);
    assert_eq!(s.prompts("claude:work").len(), 1, "the retry that hit a limit marked its entry and stopped");
    let file = s.backups_file();
    assert!(file["claude"].is_object() && file["claude:work"].is_object());

    // With every entry out nothing is started, and the user is told again.
    s.ask_for_notice("anyone?", "Every account").await;
    assert_eq!(s.prompts("primary").len(), 1);
    assert_eq!(s.prompts("claude:work").len(), 1);
}

#[tokio::test]
async fn usage_at_95_percent_moves_the_next_turn_before_anything_fails() {
    let s = start(&["claude:work"]).await;
    s.ask("FILL95 please").await;
    assert_eq!(s.answers().await[0].1, Value::Null, "the turn that reported it was the agent's own");

    s.ask("next").await;

    assert_eq!(s.answers().await[1].1, json!("claude:work"));
    assert_eq!(s.prompts("primary").len(), 1, "the usual account wasn't asked again");
    let marked = &s.backups_file()["claude"];
    assert_eq!(marked["reason"], "usage_95");
    assert!((marked["out_until"].as_i64().unwrap() - (now() + 1800)).abs() < 30, "until the window resets: {marked}");
}

#[tokio::test]
async fn after_the_limit_lifts_the_agents_own_account_comes_back_in_its_old_session() {
    let s = start(&["claude:work"]).await;
    s.ask("one").await;
    s.limit("primary", Some(limited("rate_limit", 3)));
    s.ask("two").await;
    assert_eq!(s.answers().await[1].1, json!("claude:work"));

    s.limit("primary", None);
    tokio::time::sleep(Duration::from_secs(4)).await;
    s.ask("three").await;

    let answers = s.answers().await;
    assert_eq!(answers[2], ("answered by primary".to_owned(), Value::Null));
    let loads = s.requests("primary", "session/load");
    assert_eq!(loads.len(), 1, "the agent's stored session is resumed");
    assert_eq!(loads[0]["params"]["sessionId"], "primary-s1");
    let prompt = s.prompts("primary").pop().unwrap();
    assert!(prompt.contains("two") && prompt.contains("answered by claude:work"), "told what it missed: {prompt}");
    assert_eq!(s.backups_file(), json!({}), "the entry is out no more");
}

#[tokio::test]
async fn the_state_survives_a_restart_of_the_host() {
    let mut s = start(&["claude:work"]).await;
    s.limit("primary", Some(limited("rate_limit", 3600)));
    s.ask("hello").await;
    s.host.restart(|_| {}).await;
    let deadline = Instant::now() + Duration::from_secs(20);
    while !s.host.roster().await.iter().any(|b| b["hqAgent"] == "house") {
        assert!(Instant::now() < deadline, "house never came back");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // The startup sync turns tasks on once for a bot that has them off (see `hub::hq`); this test reads the main chat.
    s.host.wait_tasks_on(&s.bot).await;
    s.host.call("updateBot", json!({"id": s.bot, "tasks": false})).await;
    s.ask("after the restart").await;

    assert_eq!(s.prompts("primary").len(), 1, "still out: not asked again");
    assert_eq!(s.answers().await[1].1, json!("claude:work"));
}

#[tokio::test]
async fn an_entry_hq_will_not_run_is_skipped() {
    let s = start(&["codex", "claude:work"]).await;
    std::fs::write(s.home.join("launch_fail_codex"), "").unwrap();
    s.limit("primary", Some(limited("rate_limit", 3600)));

    s.ask("hello").await;

    assert_eq!(s.answers().await[0].1, json!("claude:work"));
    assert_eq!(s.prompts("codex"), Vec::<String>::new());
    assert_eq!(s.backup_launches(), vec!["codex", "claude:work"]);
}

#[tokio::test]
async fn a_codex_backup_gets_acps_own_session_parameters() {
    let s = start(&["codex"]).await;
    s.limit("primary", Some(limited("rate_limit", 3600)));

    s.ask("hello").await;

    assert_eq!(s.answers().await[0].1, json!("codex"));
    let new = &s.requests("codex", "session/new")[0]["params"];
    assert_eq!(new["mcpServers"][0]["name"], "pantry", "servers as ACP's list");
    assert!(new.get("_meta").is_none(), "nothing of Claude's: {new}");
    let modes = s.requests("codex", "session/set_mode");
    assert_eq!(modes.len(), 1, "the agent's permission mode reaches the backup");
    assert_eq!(modes[0]["params"]["modeId"], "read-only", "hq's `default` is Codex's read-only");
    assert_eq!(s.requests("primary", "session/set_mode"), Vec::<Value>::new());
    let prompt = &s.prompts("codex")[0];
    assert!(
        prompt.contains("You are house, the household keeper."),
        "the agent's instructions reach a Codex that can't take them as system prompt: {prompt}"
    );
}

#[tokio::test]
async fn a_turn_that_already_acted_is_not_run_again_on_the_next_entry() {
    let s = start(&["claude:work"]).await;
    s.limit("primary", Some(limited("rate_limit", 3600)));

    s.ask_for_notice("TOOL edit the file", "The agent failed").await;

    assert_eq!(s.prompts("claude:work"), Vec::<String>::new(), "its edit would have been done twice");
    assert_eq!(s.backups_file()["claude"]["reason"], "rate_limit", "but the entry is out for the next turn");
    s.ask("next").await;
    assert_eq!(s.answers().await[0].1, json!("claude:work"));
}

impl Setup {
    /// Waits until the host has asked kith for house's chain at least `n` times.
    async fn chain_asked(&self, n: usize) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let text = std::fs::read_to_string(self.home.join("calls.jsonl")).unwrap_or_default();
            if text.lines().filter(|l| l.contains("--backups-json")).count() >= n {
                return;
            }
            assert!(Instant::now() < deadline, "the chain was never asked for");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

#[tokio::test]
async fn on_a_main_account_of_its_own_a_turn_that_ran_out_still_moves_to_the_next_entry() {
    // The agent's main account is claude:work; the default login is its backup.
    let s = start(&["claude"]).await;
    std::fs::write(s.home.join("main"), "claude:work").unwrap();
    s.ask("first, on the main account").await;
    assert_eq!(s.answers().await[0], ("answered by claude:work".to_owned(), Value::Null), "null: the agent's own");
    s.chain_asked(1).await;
    s.limit("claude:work", Some(limited("rate_limit", 3)));

    s.ask("second, when it is out").await;

    assert_eq!(s.answers().await[1], ("answered by claude".to_owned(), json!("claude")));
    assert_eq!(s.prompts("claude:work").len(), 2, "the main account was tried once");
    assert_eq!(s.prompts("primary"), Vec::<String>::new(), "nothing ran on the default login as the main account");
    assert_eq!(s.backups_file()["claude:work"]["reason"], "rate_limit", "the main account is what ran out");
    assert_eq!(s.backup_launches(), vec!["claude"]);
    let backup_prompt = &s.prompts("claude")[0];
    assert!(backup_prompt.contains("first, on the main account"), "told the conversation: {backup_prompt}");

    // Back from its limit, the main account loads the session it made.
    s.limit("claude:work", None);
    tokio::time::sleep(Duration::from_secs(4)).await;
    s.ask("third").await;
    assert_eq!(s.answers().await[2], ("answered by claude:work".to_owned(), Value::Null));
    let loads = s.requests("claude:work", "session/load");
    assert_eq!(loads.len(), 1, "its stored session is resumed");
    assert_eq!(loads[0]["params"]["sessionId"], "claude:work-s1");
}

#[tokio::test]
async fn a_session_made_under_another_account_is_not_loaded_the_agent_starts_fresh_and_is_told() {
    let s = start(&[]).await;
    s.ask("one, on the default login").await;
    s.chain_asked(1).await;

    let got = s.host.call("accountSet", json!({"agent": "house", "account": "claude:home"})).await;
    assert_eq!(got["main"]["agents"]["house"]["id"], "claude:home", "{got}");
    assert_eq!(got["main"]["agents"]["house"]["source"], "agent");
    s.ask("two, on the new main account").await;

    assert_eq!(s.answers().await[1], ("answered by claude:home".to_owned(), Value::Null));
    assert_eq!(s.requests("claude:home", "session/load"), Vec::<Value>::new(), "no load of another login's session");
    assert_eq!(s.requests("claude:home", "session/new").len(), 1);
    let prompt = &s.prompts("claude:home")[0];
    assert!(
        prompt.contains("main account changed") && prompt.contains("one, on the default login"),
        "the new session is told the conversation: {prompt}"
    );
    assert!(prompt.contains("two, on the new main account"), "{prompt}");
    assert_eq!(s.notices("main account changed").await.len(), 1);
    assert_eq!(s.notices("Couldn't resume").await, Vec::<String>::new(), "not a failed load");
    assert_eq!(s.backup_launches(), Vec::<String>::new(), "the main account is no backup");

    // The next turn stays in the session the new account made.
    s.ask("three").await;
    assert_eq!(s.requests("claude:home", "session/new").len(), 1);
    assert!(!s.prompts("claude:home")[1].contains("main account changed"));
}
