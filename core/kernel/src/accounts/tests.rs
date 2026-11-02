#![allow(clippy::unwrap_used)]

use super::*;
use std::cell::RefCell;

/// Fakes for the outside world: which harnesses are installed and logged in (by the config dir they were asked in), the
/// grants, and a log of what was asked.
struct Fake {
    installed: Vec<&'static str>,
    /// Config dirs that count as logged in; "" is the ambient login.
    logged: Vec<String>,
    grants: Vec<String>,
    asked: RefCell<Vec<String>>,
}

impl Fake {
    fn new() -> Fake {
        Fake {
            installed: vec!["claude", "codex"],
            logged: vec![],
            grants: vec![],
            asked: RefCell::new(vec![]),
        }
    }
}

impl Outside for Fake {
    fn status(&self, argv: &[&str], env: &[(&str, &str)]) -> (Option<i32>, String) {
        self.asked
            .borrow_mut()
            .push(format!("{} {:?}", argv.join(" "), env));
        if !self.installed.contains(&argv[0]) {
            return (None, String::new());
        }
        let dir = env.first().map_or("", |e| e.1);
        if self.logged.iter().any(|d| d == dir) {
            (Some(0), r#"{"loggedIn":true}"#.to_owned())
        } else {
            (Some(1), String::new())
        }
    }

    fn granted(&self, _agent: &str, _service: &str) -> Vec<String> {
        self.grants.clone()
    }

    fn on_path(&self, program: &str) -> bool {
        self.installed.contains(&program)
    }
}

struct Home {
    dir: PathBuf,
}

impl Home {
    fn new(tag: &str) -> Home {
        let dir = std::env::temp_dir().join(format!("kith-accounts-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("machine")).unwrap();
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        Home { dir }
    }

    fn ctx<'a>(&self, out: &'a dyn Outside) -> Ctx<'a> {
        Ctx {
            machine: self.dir.join("machine"),
            agents: self.dir.join("agents"),
            kernel: self.dir.join("kernel"),
            out,
        }
    }

    fn settings(&self, json: &str) {
        std::fs::write(self.dir.join("machine/settings.json"), json).unwrap();
    }

    fn account(&self, harness: &str, name: &str) {
        std::fs::create_dir_all(self.dir.join("machine/accounts").join(harness).join(name))
            .unwrap();
    }

    fn agent(&self, name: &str, harness: &str) -> PathBuf {
        let d = self.dir.join("agents").join(name).join("role");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("agent.md"),
            format!("---\nname: {name}\nharness: {harness}\n---\nbody\n"),
        )
        .unwrap();
        d.join("agent.md")
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn ids_parse_like_the_python() {
    assert_eq!(parse("claude").unwrap(), ("claude".into(), None));
    assert_eq!(
        parse("claude:work@opus").unwrap(),
        ("claude".into(), Some("work".into()))
    );
    assert_eq!(
        parse("claude-api:main").unwrap(),
        ("claude-api".into(), Some("main".into()))
    );
    assert!(parse("claude-api").unwrap_err().contains("needs a profile"));
    assert!(parse("Claude").unwrap_err().contains("is not a backup id"));
    assert!(parse("claude:").unwrap_err().contains("is not a backup id"));
    assert!(
        parse("claude@")
            .unwrap_err()
            .contains("@ needs a model name")
    );
    assert_eq!(split("codex@gpt-5"), ("codex", Some("gpt-5")));
}

#[test]
fn a_claude_model_never_reaches_another_harness() {
    assert!(check_entry_model("claude@sonnet").is_ok());
    assert!(check_entry_model("claude-api:k@opus").is_ok());
    assert!(check_entry_model("codex@gpt-5").is_ok());
    let e = check_entry_model("codex@sonnet").unwrap_err();
    assert_eq!(
        e,
        "sonnet is a Claude model and codex is not Claude: use codex@<a codex model>, or claude@sonnet"
    );
}

#[test]
fn a_main_account_takes_its_own_family_and_no_model() {
    assert!(check_main("claude:work", "claude").is_ok());
    assert!(check_main("claude-api:k", "claude").is_ok());
    assert!(
        check_main("codex", "claude")
            .unwrap_err()
            .contains("can't be the main account of a claude agent")
    );
    assert!(check_main("codex:b", "codex").is_ok());
    assert!(
        check_main("claude@opus", "claude")
            .unwrap_err()
            .contains("takes no @model")
    );
}

#[test]
fn settings_that_cant_be_used_set_nothing() {
    let h = Home::new("settings");
    assert!(read_settings(&h.dir.join("machine")).is_empty());
    h.settings("not json");
    assert!(read_settings(&h.dir.join("machine")).is_empty());
    h.settings(r#"{"backups": ["codex"], "agents": {"a": {"limit": 0}}}"#);
    assert!(read_settings(&h.dir.join("machine")).is_empty());
    assert_eq!(backups(&h.dir.join("machine"), "a"), Vec::<String>::new());
    h.settings(r#"{"backups": ["codex"], "permission_mode": "dontAsk"}"#);
    assert_eq!(backups(&h.dir.join("machine"), "a"), ["codex"]);
}

#[test]
fn an_agents_own_chain_replaces_the_global_one_even_when_empty() {
    let h = Home::new("chain");
    h.settings(r#"{"backups": ["codex"], "agents": {"a": {"backups": []}, "b": {}}}"#);
    let m = h.dir.join("machine");
    assert_eq!(backups(&m, "a"), Vec::<String>::new());
    assert_eq!(backups(&m, "b"), ["codex"]);
    assert_eq!(backups(&m, "nobody"), ["codex"]);
}

#[test]
fn the_default_login_has_no_environment() {
    let h = Home::new("default");
    let md = h.agent("echo", "claude-code");
    let fake = Fake::new();
    let ctx = h.ctx(&fake);
    assert_eq!(main_id(&ctx, "echo", Some(&md), None), "claude");
    assert_eq!(
        main_env(&ctx, "echo", &md).unwrap(),
        "main_id=claude backup_harness='' backup_account='' backup_cfg_var='' backup_cfg_dir='' backup_api_helper=''"
    );
    assert!(
        fake.asked.borrow().is_empty(),
        "the main account asks the harness nothing"
    );
}

#[test]
fn a_named_main_account_needs_its_folder_not_a_login() {
    let h = Home::new("named");
    let md = h.agent("echo", "claude");
    h.settings(r#"{"account": "claude:work"}"#);
    let fake = Fake::new();
    let ctx = h.ctx(&fake);
    let e = main_env(&ctx, "echo", &md).unwrap_err();
    assert_eq!(
        e,
        "echo's main account claude:work: no account work (make it ready: kith account add claude work); or run on the \
         default login: kith account use --reset --agent echo"
    );
    h.account("claude", "work");
    let out = main_env(&ctx, "echo", &md).unwrap();
    let dir = h.dir.join("machine/accounts/claude/work");
    assert_eq!(
        out,
        format!(
            "main_id=claude:work backup_harness=claude backup_account=work backup_cfg_var=CLAUDE_CONFIG_DIR backup_cfg_dir={} \
             backup_api_helper='' backup_model=''",
            dir.display()
        )
    );
}

#[test]
fn a_global_account_of_another_family_does_not_apply() {
    let h = Home::new("family");
    let md = h.agent("echo", "codex");
    h.settings(r#"{"account": "claude:work", "agents": {"echo": {"account": "codex:b"}}}"#);
    let fake = Fake::new();
    let ctx = h.ctx(&fake);
    assert_eq!(main_id(&ctx, "echo", Some(&md), None), "codex:b");
    h.settings(r#"{"account": "claude:work"}"#);
    assert_eq!(main_id(&ctx, "echo", Some(&md), None), "codex");
}

#[test]
fn launch_env_asks_the_harness_and_names_the_login() {
    let h = Home::new("launch");
    h.account("codex", "b");
    let dir = h.dir.join("machine/accounts/codex/b");
    let mut fake = Fake::new();
    fake.logged = vec![dir.to_string_lossy().into_owned()];
    let ctx = h.ctx(&fake);
    assert_eq!(
        launch_env(&ctx, "echo", "codex:b", false).unwrap(),
        format!(
            "backup_harness=codex backup_account=b backup_cfg_var=CODEX_HOME backup_cfg_dir={} backup_api_helper='' backup_model=''",
            dir.display()
        )
    );
    // not logged in: the harness's word, and the one command that fixes it
    assert_eq!(
        launch_env(&ctx, "echo", "codex", false).unwrap_err(),
        "codex: not logged in (make it ready: codex login)"
    );
    assert_eq!(
        launch_env(&ctx, "echo", "claude:none", false).unwrap_err(),
        "claude:none: no account none (make it ready: kith account add claude none)"
    );
    assert_eq!(
        launch_env(&ctx, "echo", "gemini", false).unwrap_err(),
        "gemini: gemini is not supported in launch-json yet"
    );
}

#[test]
fn a_harness_that_is_not_installed_says_so() {
    let h = Home::new("missing");
    let mut fake = Fake::new();
    fake.installed = vec![];
    let ctx = h.ctx(&fake);
    assert_eq!(
        launch_env(&ctx, "echo", "codex", false).unwrap_err(),
        "codex: codex is not installed (make it ready: codex login)"
    );
}

#[test]
fn the_plain_id_finds_a_chain_entry_with_a_model() {
    let h = Home::new("model");
    h.settings(r#"{"backups": ["claude@sonnet", "codex@gpt-5", "codex"]}"#);
    let mut fake = Fake::new();
    fake.logged = vec![String::new()];
    let ctx = h.ctx(&fake);
    assert!(
        launch_env(&ctx, "echo", "claude", false)
            .unwrap()
            .ends_with("backup_model=sonnet")
    );
    assert!(
        launch_env(&ctx, "echo", "claude@opus", false)
            .unwrap()
            .ends_with("backup_model=opus")
    );
    // the chain lists the plain id too: the exact entry wins, with no model
    assert!(
        launch_env(&ctx, "echo", "codex", false)
            .unwrap()
            .ends_with("backup_model=''")
    );
}

#[test]
fn an_api_key_entry_runs_claude_through_a_helper() {
    let h = Home::new("api");
    let mut fake = Fake::new();
    fake.grants = vec!["anthropic/main".into()];
    let ctx = h.ctx(&fake);
    let out = launch_env(&ctx, "echo", "claude-api:main", false).unwrap();
    assert!(out.starts_with("backup_harness=claude backup_account=main backup_cfg_var='' backup_cfg_dir='' backup_api_helper="));
    assert!(
        out.contains(
            "kith-auth get --for echo --profile anthropic/main anthropic ANTHROPIC_API_KEY"
        )
    );
    let e = launch_env(&ctx, "echo", "claude-api:other", false).unwrap_err();
    assert!(e.starts_with("claude-api:other: no anthropic/other key granted to echo (make it ready: kith auth set anthropic/other"));
}

#[test]
fn the_chain_lists_the_main_account_first_and_each_account_once() {
    let h = Home::new("chainjson");
    let md = h.agent("echo", "claude");
    h.account("claude", "work");
    h.settings(
        r#"{"backups": ["claude:work", "codex", "claude:work", "claude", "bad id", "gemini"]}"#,
    );
    let mut fake = Fake::new();
    fake.logged = vec![
        h.dir
            .join("machine/accounts/claude/work")
            .to_string_lossy()
            .into_owned(),
    ];
    let ctx = h.ctx(&fake);
    let j = chain(&ctx, "echo", &md).dumps();
    assert_eq!(
        j,
        r#"{"version": 1, "agent": "echo", "chain": [{"id": "claude", "harness": "claude", "account": null, "model": null, "kind": "primary", "ready": true}, {"id": "claude:work", "harness": "claude", "account": "work", "model": null, "kind": "subscription", "ready": true}, {"id": "codex", "harness": "codex", "account": null, "model": null, "kind": "subscription", "ready": false, "why": "not logged in"}, {"id": "bad id", "harness": null, "account": null, "model": null, "kind": "unknown", "ready": false, "why": "'bad id' is not a backup id: use <harness> or <harness>:<name>, with @<model> for a model of its own (claude, claude:work, claude@sonnet, claude-api:main, codex)"}, {"id": "gemini", "harness": "gemini", "account": null, "model": null, "kind": "subscription", "ready": false, "why": "gemini is not supported yet"}]}"#
    );
}

#[test]
fn main_json_says_where_the_choice_comes_from() {
    let h = Home::new("mainjson");
    h.agent("echo", "claude");
    let fake = Fake::new();
    let ctx = h.ctx(&fake);
    assert_eq!(
        main_json(&ctx, "").dumps(),
        r#"{"id": null, "source": "global", "harness": null}"#
    );
    assert_eq!(
        main_json(&ctx, "echo").dumps(),
        r#"{"id": "claude", "source": "default", "harness": "claude", "ready": true}"#
    );
    h.settings(r#"{"account": "claude:g", "agents": {"echo": {"account": "claude:own"}}}"#);
    assert!(main_json(&ctx, "echo").dumps().starts_with(r#"{"id": "claude:own", "source": "agent", "harness": "claude", "ready": false, "why": "no account own", "fix": "kith account add claude own"#));
    assert!(
        main_json(&ctx, "")
            .dumps()
            .contains(r#""id": "claude:g", "source": "global""#)
    );
}

fn ledger_with(h: &Home, sql: &str) {
    let c = Connection::open(h.dir.join("machine/cost.db")).unwrap();
    c.execute_batch(sql).unwrap();
}

#[test]
fn a_session_belongs_to_the_account_the_ledger_says() {
    let h = Home::new("ledger");
    let m = h.dir.join("machine");
    assert_eq!(session_account(&m, "s1"), None, "no ledger");
    ledger_with(
        &h,
        "create table runs (session text, agent text, provider text, account text, account_id text, model text, effort text);
         insert into runs values ('s1', 'echo', 'claude', 'work', 'claude:work', 'opus', 'high');
         insert into runs values ('s2', 'echo', 'codex', null, null, null, null);
         insert into runs values ('s3', 'echo', 'claude', 'w', '', '', '');
         insert into runs values ('s4', 'echo', null, null, null, null, null);",
    );
    assert_eq!(session_account(&m, "s1").as_deref(), Some("claude:work"));
    assert_eq!(session_account(&m, "s2").as_deref(), Some("codex"));
    assert_eq!(session_account(&m, "s3").as_deref(), Some("claude:w"));
    assert_eq!(session_account(&m, "s4"), None);
    assert_eq!(session_account(&m, "nope"), None);
    assert_eq!(session_account(&m, ""), None);
}

#[test]
fn a_ledger_from_before_account_id_still_answers() {
    let h = Home::new("oldledger");
    let m = h.dir.join("machine");
    ledger_with(
        &h,
        "create table runs (session text, agent text, provider text, account text, model text, effort text); insert into runs values ('s1', 'a', 'claude', 'work', null, null);",
    );
    assert_eq!(session_account(&m, "s1").as_deref(), Some("claude:work"));
    ledger_with(
        &h,
        "drop table runs; create table runs (session text, agent text, model text, effort text); insert into runs values ('s1', 'a', 'opus', null);",
    );
    assert_eq!(
        session_account(&m, "s1"),
        None,
        "before provider the ledger doesn't say"
    );
    let r = session_run(&m, "s1").unwrap();
    assert_eq!(r.harness, "claude");
    assert_eq!(r.model.as_deref(), Some("opus"));
}

#[test]
fn a_session_run_prints_json_and_shell() {
    let h = Home::new("run");
    let m = h.dir.join("machine");
    ledger_with(
        &h,
        "create table runs (session text, agent text, provider text, account text, account_id text, model text, effort text);
         insert into runs values ('s1', 'echo', 'codex', null, null, 'gpt-5', null);",
    );
    let r = session_run(&m, "s1").unwrap();
    assert_eq!(
        r.json().dumps(),
        r#"{"agent": "echo", "harness": "codex", "model": "gpt-5", "effort": null}"#
    );
    assert_eq!(r.sh(), "s_harness=codex s_model=gpt-5 s_effort=''");
    assert!(session_run(&m, "other").is_none());
    assert_eq!(
        sh_line("", None, None),
        "s_harness='' s_model='' s_effort=''"
    );
}

#[test]
fn the_account_list_is_sorted_and_says_why_not() {
    let h = Home::new("list");
    h.account("claude", "b");
    h.account("claude", "a");
    h.account("claude", ".hidden");
    h.account("codex", "c");
    let mut fake = Fake::new();
    fake.logged = vec![
        h.dir
            .join("machine/accounts/claude/a")
            .to_string_lossy()
            .into_owned(),
    ];
    let ctx = h.ctx(&fake);
    let text = list(&ctx, false);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines,
        [
            "claude:a             claude  logged in",
            "claude:b             claude  not logged in",
            "codex:c              codex   not logged in"
        ]
    );
    let json = list(&ctx, true);
    assert!(json.starts_with(r#"[{"id": "claude:a", "harness": "claude", "name": "a", "dir": ""#));
    assert!(json.contains(r#""logged_in": false, "why": "not logged in"}"#));
    let empty = Home::new("list-empty");
    assert_eq!(list(&empty.ctx(&fake), true), "[]\n");
    assert_eq!(
        list(&empty.ctx(&fake), false),
        "no extra accounts (kith account add claude|codex <name>)\n"
    );
}

#[test]
fn json_escapes_the_way_python_does() {
    // written without a raw string: a tool that tidies escapes would turn the expectation into the characters
    let want = ["\"", "\\u00e9", "\\ud83d\\ude00", "\\\"", "\\n", "\""].concat();
    assert_eq!(J::s("\u{e9}\u{1F600}\"\n").dumps(), want);
}

#[test]
fn shell_words_are_quoted_like_shlex() {
    assert_eq!(sh_quote(""), "''");
    assert_eq!(sh_quote("a b'c"), "'a b'\"'\"'c'");
    assert_eq!(sh_quote("claude:work@x"), "claude:work@x");
}
