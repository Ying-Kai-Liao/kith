//! The host as a client of the kernel's managed account set (`docs/accounts.md`): which account a turn starts on comes
//! from `kith <agent> --accounts-json`, a limit the host meets is told to the kernel (`kith account out`), and the apps
//! read and change the set through the `accounts*` methods. The kernel is `stub_hq.py` with a `kernel.json` (a small
//! stand-in for `kernel/lib/kith_accounts.py`); the agent is `limit_agent.py`, which fails on cue. The host runs on a
//! temporary `KITH_SERVE_HOME`.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::host::Host;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Setup {
    host: Host,
    /// The agent's folder: the stub kernel's state and the agent's logs are in it.
    home: PathBuf,
    bot: String,
}

fn now() -> i64 {
    i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()).unwrap()
}

async fn start(kernel: &Value) -> Setup {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let home = std::env::temp_dir().join(format!("kith-accounts-e2e-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    let home = home.canonicalize().unwrap();
    std::fs::write(home.join("kernel.json"), kernel.to_string()).unwrap();
    let acp = common::python_agent(&fixtures.join("limit_agent.py"));
    let host = Host::start_env(&[
        ("KITH_KERNEL_BIN", &fixtures.join("stub_hq.py").to_string_lossy()),
        ("STUB_HQ_HOME", &home.to_string_lossy()),
        ("KITH_SERVE_HQ_ACP_COMMAND", &acp),
    ])
    .await;
    let mut s = Setup { host, home, bot: String::new() };
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
    fn kernel(&self) -> Value {
        serde_json::from_str(&std::fs::read_to_string(self.home.join("kernel.json")).unwrap()).unwrap()
    }

    /// `account` (`primary`, `claude:work`…) fails its next prompts like this.
    fn limit(&self, account: &str, resets_in: i64) {
        let limit = json!({"kind": "rate_limit", "resetsAt": now() + resets_in, "via": "error"});
        std::fs::write(self.home.join("limits.json"), json!({ account: limit }).to_string()).unwrap();
    }

    /// The prompts `account` got, oldest first.
    fn prompts(&self, account: &str) -> Vec<String> {
        let text = std::fs::read_to_string(self.home.join("requests.jsonl")).unwrap_or_default();
        text.lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .filter(|r| r["account"] == account && r["method"] == "session/prompt")
            .map(|r| {
                let blocks = r["params"]["prompt"].as_array().unwrap();
                blocks.iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n")
            })
            .collect()
    }

    /// Every final reply of the house chat, oldest first: its text and who answered.
    async fn answers(&self) -> Vec<(String, Value)> {
        let history = self.host.call("history", json!({"botId": self.bot})).await;
        let all = history["entries"].as_array().unwrap().iter();
        all.filter(|e| e["kind"] == "agent" && e["data"]["final"] == true)
            .map(|e| (e["data"]["text"].as_str().unwrap().to_owned(), e["data"]["answeredBy"].clone()))
            .collect()
    }

    async fn notice_after(&self, text: &str, needle: &str) -> String {
        let nonce = uuid::Uuid::new_v4().to_string();
        self.host.call("send", json!({"botId": self.bot, "text": text, "clientNonce": nonce})).await;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let history = self.host.call("history", json!({"botId": self.bot})).await;
            let all = history["entries"].as_array().unwrap().iter();
            let found = all
                .filter(|e| e["kind"] == "notice")
                .filter_map(|e| e["data"]["text"].as_str().filter(|t| t.contains(needle)).map(str::to_owned))
                .next_back();
            if let Some(found) = found {
                return found;
            }
            assert!(Instant::now() < deadline, "no notice with {needle:?}: {history}");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

fn account<'a>(got: &'a Value, id: &str) -> &'a Value {
    got["accounts"].as_array().unwrap().iter().find(|a| a["id"] == id).unwrap_or_else(|| panic!("no account {id}"))
}

fn two() -> Value {
    json!({"set": ["claude", "claude:work"]})
}

#[tokio::test]
async fn an_agent_asked_by_another_agent_runs_on_its_next_account_when_the_kernel_has_its_first_out() {
    // Another run (a terminal, an `ask` from a third agent) hit the limit: the kernel has the account out, and this
    // host has never read `house`'s accounts. The ask arrives through the same actor queue as a message.
    let kernel = json!({"set": ["claude", "claude:work"], "out": {"claude": {"out_until": now() + 3600, "reason": "rate_limit"}}});
    let s = start(&kernel).await;
    let lead = s.host.fixture_bot("lead", "team_agent.py", "").await;

    s.host.reply(&lead, &format!("ASK_REPORT {}", s.bot)).await;

    // The lead got house's answer; an asked agent's own chat keeps it as a trace, not as a final reply.
    let trace = s.host.call("history", json!({"botId": s.bot})).await;
    let said = trace["entries"].as_array().unwrap().iter().find(|e| e["kind"] == "agent").unwrap().clone();
    assert_eq!(said["data"]["text"], "answered by claude:work");
    assert_eq!(said["data"]["answeredBy"], json!("claude:work"), "house answered on its next account");
    assert!(s.prompts("primary").is_empty(), "the account that is out was never started: {:?}", s.prompts("primary"));
    assert_eq!(s.prompts("claude:work").len(), 1);
}

#[tokio::test]
async fn a_limit_the_host_meets_is_told_to_the_kernel_and_the_next_turn_starts_on_the_next_account() {
    let s = start(&two()).await;
    s.host.reply(&s.bot, "first").await;
    s.limit("primary", 3600);

    s.host.reply(&s.bot, "second").await;

    let answers = s.answers().await;
    assert_eq!(answers[0].1, Value::Null, "the usual account answered the first");
    assert_eq!(answers[1].1, json!("claude:work"));
    // The kernel heard of it (a terminal run now skips the account too), in the background.
    let deadline = Instant::now() + Duration::from_secs(10);
    while s.kernel()["out"]["claude"].is_null() {
        assert!(Instant::now() < deadline, "the kernel was never told: {}", s.kernel());
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let out = &s.kernel()["out"]["claude"];
    assert_eq!(out["reason"], "rate_limit");
    assert!((out["out_until"].as_i64().unwrap() - (now() + 3600)).abs() < 30, "{out}");
    assert!(
        !s.host.home.join("backups.json").exists(),
        "the kernel keeps the marks: the host writes no file of its own"
    );

    // The next turn starts on the second account without asking the first.
    s.host.reply(&s.bot, "third").await;
    assert_eq!(s.prompts("primary").len(), 2);
    assert_eq!(s.answers().await[2].1, json!("claude:work"));
}

#[tokio::test]
async fn what_the_host_tells_the_kernel_never_shortens_a_longer_mark() {
    let s = start(&two()).await;
    s.host.reply(&s.bot, "first").await;
    // After the host last read the kernel: a terminal marks the account out for a week; the host's next turn still
    // starts there, meets an hour's limit and says so.
    let week = now() + 7 * 86400;
    let mut kernel = s.kernel();
    kernel["out"] = json!({"claude": {"out_until": week, "reason": "manual"}});
    std::fs::write(s.home.join("kernel.json"), kernel.to_string()).unwrap();
    s.limit("primary", 3600);

    s.host.reply(&s.bot, "second").await;

    assert_eq!(s.answers().await[1].1, json!("claude:work"));
    tokio::time::sleep(Duration::from_millis(800)).await;
    let out = &s.kernel()["out"]["claude"];
    assert_eq!(out["out_until"], json!(week), "the week stays: {out}");
}

#[tokio::test]
async fn every_account_out_gives_the_user_the_time_it_is_back_and_starts_nothing() {
    let until = now() + 5400;
    let out = json!({"out_until": until, "reason": "rate_limit"});
    let s = start(&json!({"set": ["claude", "claude:work"], "out": {"claude": out, "claude:work": out}})).await;

    let notice = s.notice_after("hello", "Every account").await;

    assert!(notice.contains("claude (back in") && notice.contains("claude:work (back in"), "{notice}");
    assert!(s.prompts("primary").is_empty() && s.prompts("claude:work").is_empty());
}

#[tokio::test]
async fn a_pin_that_is_out_falls_back_to_the_set_and_an_only_pin_runs_anyway() {
    let out = json!({"claude:work": {"out_until": now() + 3600, "reason": "rate_limit"}});
    let pin = json!({"id": "claude:work", "only": false});
    let s = start(&json!({"set": ["claude", "claude:work"], "agents": {"house": {"pin": pin}}, "out": out})).await;
    s.host.reply(&s.bot, "hello").await;
    assert_eq!(
        s.answers().await[0],
        ("answered by claude".to_owned(), json!("claude")),
        "the pin is out: the set's first"
    );
    drop(s);

    let pin = json!({"id": "claude:work", "only": true});
    let s = start(&json!({"set": ["claude", "claude:work"], "agents": {"house": {"pin": pin}}, "out": out})).await;
    // One account only and it is out: the kernel runs it anyway and the harness says what is wrong, so does the host.
    s.host.reply(&s.bot, "hello").await;
    assert_eq!(s.answers().await[0].0, "answered by claude:work");
}

#[tokio::test]
async fn accounts_reads_the_set_the_policy_and_each_agents_view_in_one_call() {
    let out_until = now() + 3600;
    let kernel = json!({
        "set": ["claude", "claude:work", "codex"],
        "agents": {"house": {"pin": {"id": "claude:work", "only": false}}},
        "out": {"claude:work": {"out_until": out_until, "reason": "usage_95"}},
        "signed_out": {"codex": "not logged in"},
        "usage": {"claude": {"read_at": now() - 60, "age_s": 60, "windows": [{"name": "five_hour", "percent": 71.5, "resets_at": now() + 600}]}},
    });
    let s = start(&kernel).await;

    let got = s.host.call("accounts", json!({})).await;

    assert_eq!((got["policy"].clone(), got["crossHarness"].clone()), (json!("fill"), json!(false)));
    let work = account(&got, "claude:work");
    assert_eq!(work["status"], "out");
    assert_eq!(work["outUntil"], json!(out_until * 1000), "milliseconds, like the rest of the API");
    assert_eq!(
        (work["reason"].clone(), work["pins"].clone()),
        (json!("usage_95"), json!([{"agent": "house", "only": false}]))
    );
    assert_eq!(account(&got, "codex")["status"], "signed_out");
    assert_eq!(account(&got, "codex")["fix"], "kith account login codex");
    let usage = &account(&got, "claude")["usage"];
    assert_eq!(usage["windows"][0]["percent"], 71.5);
    assert_eq!(usage["windows"][0]["resetsAt"].as_i64().unwrap() % 1000, 0);
    assert_eq!(account(&got, "claude")["inSet"], true);
    assert_eq!(account(&got, "claude")["order"], 1);
    // The agent's view: pin first, what pick would try now, what it skipped and why.
    let house = &got["agents"]["house"];
    assert_eq!(house["harness"], "claude");
    assert_eq!(house["pin"], json!({"id": "claude:work", "only": false}));
    assert_eq!(house["order"], json!(["claude:work", "claude"]));
    assert_eq!(house["candidates"], json!(["claude"]));
    assert_eq!(house["allOut"], false);
    assert_eq!(house["own"], Value::Null, "it follows the global set");
    assert_eq!(house["skipped"][0]["id"], "claude:work");
    assert_eq!(house["skipped"][0]["outUntil"], json!(out_until * 1000));
    assert!(!got.to_string().contains("/secret/dir"), "no account folder leaves the host");
}

#[tokio::test]
async fn the_writes_go_through_the_kernel_and_answer_with_the_fresh_read() {
    let s = start(&two()).await;

    let got = s.host.call("accountsSet", json!({"ids": ["claude:work", "claude", "codex"]})).await;
    assert_eq!(s.kernel()["set"], json!(["claude:work", "claude", "codex"]));
    assert_eq!(account(&got, "claude:work")["order"], 1);

    let got = s.host.call("accountsSet", json!({"agent": "house", "ids": ["claude", "claude:work"]})).await;
    assert_eq!(got["agents"]["house"]["own"], json!(["claude", "claude:work"]));
    let got = s.host.call("accountsSet", json!({"agent": "house", "reset": true})).await;
    assert_eq!(got["agents"]["house"]["own"], Value::Null);

    let got = s.host.call("accountPolicy", json!({"policy": "spread"})).await;
    assert_eq!(got["policy"], "spread");
    let got = s.host.call("accountPin", json!({"agent": "house", "id": "claude:work", "only": true})).await;
    assert_eq!(got["agents"]["house"]["pin"], json!({"id": "claude:work", "only": true}));
    assert_eq!(got["agents"]["house"]["order"], json!(["claude:work"]));
    let got = s.host.call("accountUnpin", json!({"agent": "house"})).await;
    assert_eq!(got["agents"]["house"]["pin"], Value::Null);
    let got = s.host.call("accountCrossHarness", json!({"value": "on"})).await;
    assert_eq!(got["crossHarness"], true);
    let got = s.host.call("accountCrossHarness", json!({"agent": "house", "value": "off"})).await;
    assert_eq!(got["agents"]["house"]["crossHarness"], false);

    // Odd arguments never reach the kernel.
    for (method, body) in [
        ("accountPolicy", json!({"policy": "random"})),
        ("accountPin", json!({"agent": "house"})),
        ("accountPin", json!({"agent": "../x", "id": "claude"})),
        ("accountsSet", json!({"ids": ["claude", "bad id!"]})),
        ("accountsSet", json!({"reset": true})),
        ("accountCrossHarness", json!({"value": "reset"})),
        ("accountClear", json!({"id": "-x"})),
    ] {
        s.host.refused(method, body.clone()).await;
    }
}

#[tokio::test]
async fn it_is_back_clears_the_account_for_the_host_and_the_kernel() {
    let s = start(&two()).await;
    s.host.reply(&s.bot, "first").await;
    s.limit("primary", 3600);
    s.host.reply(&s.bot, "second").await;
    assert_eq!(s.answers().await[1].1, json!("claude:work"));

    s.host.call("accountClear", json!({"id": "claude"})).await;
    std::fs::remove_file(s.home.join("limits.json")).unwrap();
    s.host.reply(&s.bot, "third").await;

    assert!(s.kernel()["out"]["claude"].is_null(), "{}", s.kernel());
    assert_eq!(s.answers().await[2].1, Value::Null, "the usual account is asked again");
}

#[tokio::test]
async fn a_new_claude_account_needs_yes_then_the_cli_runs_its_login_in_a_terminal() {
    let s = start(&two()).await;

    let asked = s.host.call("accountAdd", json!({"harness": "claude", "name": "extra"})).await;
    assert_eq!(asked["ok"], false);
    assert!(asked["warning"].as_str().unwrap().contains("own risk"), "{asked}");
    assert!(asked.get("term").is_none() && s.kernel()["set"] == json!(["claude", "claude:work"]));

    let got = s.host.call("accountAdd", json!({"harness": "claude", "name": "extra", "yes": true})).await;
    assert_eq!(got["account"]["id"], "claude:extra");
    assert_eq!(got["account"]["created"], true);
    assert!(got["term"].as_str().is_some());
    let deadline = Instant::now() + Duration::from_secs(10);
    while !s.home.join("login_ran").exists() {
        assert!(Instant::now() < deadline, "the CLI's login never ran");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(s.kernel()["set"], json!(["claude", "claude:work", "claude:extra"]));

    for bad in [json!({"harness": "gemini", "name": "x"}), json!({"harness": "claude", "name": "../x"}), json!({})] {
        s.host.refused("accountAdd", bad).await;
    }

    let got = s.host.call("accountRemove", json!({"id": "claude:extra"})).await;
    assert_eq!(s.kernel()["set"], json!(["claude", "claude:work"]));
    assert_eq!(account(&got, "claude:work")["order"], 2);
    let calls = std::fs::read_to_string(s.home.join("calls.jsonl")).unwrap();
    assert!(calls.contains(r#""account", "remove", "claude:extra", "--yes""#), "{calls}");
}
