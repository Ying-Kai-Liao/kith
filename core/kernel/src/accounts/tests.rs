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

    /// Count these folders as logged in.
    fn login(&mut self, dirs: &[PathBuf]) {
        self.logged
            .extend(dirs.iter().map(|d| d.to_string_lossy().into_owned()));
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

    fn machine(&self) -> PathBuf {
        self.dir.join("machine")
    }

    fn ctx<'a>(&self, out: &'a dyn Outside) -> Ctx<'a> {
        Ctx {
            machine: self.machine(),
            agents: self.dir.join("agents"),
            kernel: self.dir.join("kernel"),
            home: self.dir.join("home"),
            claude_config_dir: None,
            out,
        }
    }

    fn settings(&self, json: &str) {
        std::fs::write(self.machine().join("settings.json"), json).unwrap();
    }

    /// An account's folder, `<machine>/accounts/<harness>/<name>`.
    fn account_dir(&self, harness: &str, name: &str) -> PathBuf {
        self.machine().join("accounts").join(harness).join(name)
    }

    fn account(&self, harness: &str, name: &str) -> PathBuf {
        let d = self.account_dir(harness, name);
        std::fs::create_dir_all(&d).unwrap();
        d
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

    fn state(&self) -> Value {
        serde_json::from_slice(&std::fs::read(state_file(&self.machine())).unwrap()).unwrap()
    }

    fn put_state(&self, json: &str) {
        let f = state_file(&self.machine());
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, json).unwrap();
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A path as the code quotes it into the shell words it prints.
fn q(p: &Path) -> String {
    sh_quote(&p.to_string_lossy())
}

/// The machine's settings as the account commands read them.
fn read(h: &Home) -> Map {
    settings::read(&h.ctx(&Fake::new()))
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
    assert!(
        parse("Claude")
            .unwrap_err()
            .contains("is not an account id")
    );
    assert!(
        parse("claude:")
            .unwrap_err()
            .contains("is not an account id")
    );
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
fn a_pinned_account_takes_its_own_family_and_no_model() {
    assert!(check_main("claude:work", "claude").is_ok());
    assert!(check_main("claude-api:k", "claude").is_ok());
    assert_eq!(
        check_main("codex", "claude").unwrap_err(),
        "codex can't be the account of a claude agent: use claude, claude-api:<profile> or <harness>:<name> of those"
    );
    assert!(check_main("codex:b", "codex").is_ok());
    assert!(
        check_main("claude@opus", "claude")
            .unwrap_err()
            .contains("a pinned account takes no @model")
    );
}

#[test]
fn json_is_written_the_way_python_writes_it() {
    let v: Value = serde_json::from_str(
        r#"{"a": [1, 2.5, 70.0, 1e-7, 1e16], "b": {}, "c": [], "d": "é\u0001", "e": null, "f": true}"#,
    )
    .unwrap();
    assert_eq!(
        pyjson::dumps(&v),
        r#"{"a": [1, 2.5, 70.0, 1e-07, 1e+16], "b": {}, "c": [], "d": "\u00e9\u0001", "e": null, "f": true}"#
    );
    assert_eq!(
        pyjson::dumps_pretty(&v),
        "{\n  \"a\": [\n    1,\n    2.5,\n    70.0,\n    1e-07,\n    1e+16\n  ],\n  \"b\": {},\n  \"c\": [],\n  \"d\": \"\\u00e9\\u0001\",\n  \"e\": null,\n  \"f\": true\n}"
    );
}

#[test]
fn settings_that_cant_be_used_set_nothing() {
    let h = Home::new("settings");
    let m = h.machine();
    assert!(read_settings(&m).is_empty());
    h.settings("not json");
    assert!(read_settings(&m).is_empty());
    for bad in [
        r#"{"accounts": ["codex"], "agents": {"a": {"limit": 0}}}"#,
        r#"{"accounts": "claude"}"#,
        r#"{"accounts": [1]}"#,
        r#"{"account_cross_harness": "yes"}"#,
        r#"{"account_policy": "random"}"#,
        r#"{"pin": {"id": "claude:a"}}"#,
        r#"{"agents": {"a": {"pin": {"id": ""}}}}"#,
        r#"{"agents": {"a": {"pin": {"id": "claude:a", "only": 1}}}}"#,
        r#"{"agents": {"a": {"pin": "claude:a"}}}"#,
        r#"{"agents": {"a": {"at_limit": "never"}}}"#,
        r#"{"agents": ["a"]}"#,
        r#"{"plugins": [1]}"#,
        r#"{"delegation": {"chain_cost_cap_usd": 0}}"#,
    ] {
        h.settings(bad);
        assert!(read_settings(&m).is_empty(), "{bad}");
    }
    h.settings(r#"{"accounts": ["codex"], "permission_mode": "dontAsk", "agents": {"a": {"pin": {"id": "codex"}}}}"#);
    let s = read_settings(&m);
    assert_eq!(
        s["permission_mode"], "auto",
        "a retired mode reads as its successor"
    );
    assert_eq!(
        settings::accounts(&s, "a"),
        (Some(vec!["codex".to_owned()]), "global")
    );
}

#[test]
fn an_agents_own_set_replaces_the_global_one_even_when_empty() {
    let h = Home::new("own-set");
    h.settings(r#"{"accounts": ["codex"], "agents": {"a": {"accounts": []}, "b": {}}}"#);
    let s = read(&h);
    assert_eq!(settings::accounts(&s, "a"), (Some(vec![]), "agent"));
    assert_eq!(settings::accounts(&s, "b").1, "global");
    assert_eq!(
        settings::accounts(&s, "nobody").0,
        Some(vec!["codex".to_owned()])
    );
    assert_eq!(settings::accounts(&s, "").0, Some(vec!["codex".to_owned()]));
}

#[test]
fn the_older_settings_shape_reads_as_the_account_set() {
    let h = Home::new("migrate");
    h.agent("cdx", "codex");
    h.settings(
        r#"{"account": "codex:b", "backups": ["claude", "claude@sonnet"],
            "agents": {"cdx": {"account": "codex:z", "backups": ["codex"]}, "echo": {"backups": []}}}"#,
    );
    let s = read(&h);
    // the old chain named a harness other than the main one's: those fallbacks stay
    assert_eq!(s["account_cross_harness"], true);
    assert_eq!(
        settings::accounts(&s, "").0.unwrap(),
        ["codex:b", "claude", "claude@sonnet"]
    );
    assert_eq!(
        settings::accounts(&s, "cdx").0.unwrap(),
        ["codex:z", "codex"]
    );
    assert_eq!(
        settings::pin(&s, "cdx"),
        Some(settings::Pin {
            id: "codex:z".into(),
            only: false
        })
    );
    // echo is a claude agent (no agent.md): the global main account is not of its family, so its harness leads
    assert_eq!(settings::accounts(&s, "echo").0.unwrap(), ["claude"]);
    assert!(!s.contains_key("account") && !s.contains_key("backups"));
    assert!(
        !s["agents"]["cdx"]
            .as_object()
            .unwrap()
            .contains_key("backups")
    );
}

#[test]
fn a_file_in_the_older_shape_is_rewritten_by_list_only() {
    let h = Home::new("migrate-file");
    h.settings(r#"{"account": "claude:work", "permission_mode": "dontAsk"}"#);
    let fake = Fake::new();
    let ctx = h.ctx(&fake);
    let before = std::fs::read_to_string(h.machine().join("settings.json")).unwrap();
    assert_eq!(main_id(&ctx, "echo", None, None), "claude:work");
    assert_eq!(
        std::fs::read_to_string(h.machine().join("settings.json")).unwrap(),
        before
    );
    assert!(settings::migrate_file(&ctx).unwrap());
    assert_eq!(
        std::fs::read_to_string(h.machine().join("settings.json")).unwrap(),
        "{\n  \"permission_mode\": \"auto\",\n  \"accounts\": [\n    \"claude:work\"\n  ]\n}\n"
    );
    assert!(
        !settings::migrate_file(&ctx).unwrap(),
        "the second time there is nothing to write"
    );
    assert!(h.machine().join("settings.json.lock").exists());
}

#[test]
fn the_pin_leads_and_only_closes_the_rest() {
    let h = Home::new("pin");
    let md = h.agent("echo", "claude");
    h.settings(
        r#"{"accounts": ["claude", "claude:a", "claude:b", "codex"],
            "agents": {"echo": {"pin": {"id": "claude:b"}}, "solo": {"pin": {"id": "claude:a", "only": true}},
                       "odd": {"pin": {"id": "codex"}}}}"#,
    );
    let s = read(&h);
    let (order, pin) = static_order(&s, "echo", "claude");
    assert_eq!(order, ["claude:b", "claude", "claude:a"]);
    assert_eq!(pin.unwrap().id, "claude:b");
    assert_eq!(static_order(&s, "solo", "claude").0, ["claude:a"]);
    // a pin the agent's harness can't use is no pin
    assert_eq!(
        static_order(&s, "odd", "claude").0,
        ["claude", "claude:a", "claude:b"]
    );
    assert_eq!(
        main_id(&h.ctx(&Fake::new()), "echo", Some(&md), None),
        "claude:b"
    );
}

#[test]
fn another_harness_serves_only_when_the_agent_or_the_switch_says_so() {
    let h = Home::new("cross");
    h.settings(
        r#"{"accounts": ["claude", "codex", "codex:b"], "agents": {"own": {"accounts": ["codex"]},
            "empty": {"accounts": [], "pin": {"id": "claude:p"}}, "none": {"accounts": []},
            "on": {"account_cross_harness": true}}}"#,
    );
    let s = read(&h);
    assert_eq!(static_order(&s, "echo", "claude").0, ["claude"]);
    assert_eq!(
        static_order(&s, "on", "claude").0,
        ["claude", "codex", "codex:b"]
    );
    // an agent's own list may name any harness
    assert_eq!(static_order(&s, "own", "claude").0, ["claude", "codex"]);
    // an explicitly empty list: only the pin, if there is one, else the harness's login
    assert_eq!(static_order(&s, "empty", "claude").0, ["claude:p"]);
    assert_eq!(static_order(&s, "none", "claude").0, ["claude"]);
}

#[test]
fn the_default_login_has_no_environment() {
    let h = Home::new("default");
    let md = h.agent("echo", "claude-code");
    let fake = Fake::new();
    let ctx = h.ctx(&fake);
    assert_eq!(main_id(&ctx, "echo", Some(&md), None), "claude");
    assert_eq!(
        main_env(&ctx, "echo", &md, MainOpts::default()).unwrap(),
        "main_id=claude backup_harness='' backup_account='' backup_cfg_var='' backup_cfg_dir='' backup_api_helper=''"
    );
    assert!(
        fake.asked.borrow().is_empty(),
        "the main account asks the harness nothing"
    );
    assert!(
        !state_file(&h.machine()).exists(),
        "nothing recorded without --record"
    );
}

#[test]
fn a_named_main_account_needs_its_folder_not_a_login() {
    let h = Home::new("named");
    let md = h.agent("echo", "claude");
    h.settings(r#"{"accounts": ["claude:work"]}"#);
    let fake = Fake::new();
    let ctx = h.ctx(&fake);
    let e = main_env(&ctx, "echo", &md, MainOpts::default()).unwrap_err();
    assert_eq!(
        e,
        "echo's main account claude:work: no account work (make it ready: kith account add claude work); or run on the \
         default login: kith account unpin echo"
    );
    let dir = h.account("claude", "work");
    let out = main_env(&ctx, "echo", &md, MainOpts::default()).unwrap();
    assert_eq!(
        out,
        format!(
            "main_id=claude:work backup_harness=claude backup_account=work backup_cfg_var=CLAUDE_CONFIG_DIR backup_cfg_dir={} \
             backup_api_helper='' backup_model=''",
            q(&dir)
        )
    );
}

#[test]
fn a_global_account_of_another_family_does_not_apply() {
    let h = Home::new("family");
    let md = h.agent("echo", "codex");
    h.settings(r#"{"accounts": ["claude:work"], "agents": {"echo": {"accounts": ["codex:b"]}}}"#);
    let fake = Fake::new();
    let ctx = h.ctx(&fake);
    assert_eq!(main_id(&ctx, "echo", Some(&md), None), "codex:b");
    h.settings(r#"{"accounts": ["claude:work"]}"#);
    assert_eq!(main_id(&ctx, "echo", Some(&md), None), "codex");
}

#[test]
fn launch_env_asks_the_harness_and_names_the_login() {
    let h = Home::new("launch");
    let dir = h.account("codex", "b");
    let mut fake = Fake::new();
    fake.login(std::slice::from_ref(&dir));
    let ctx = h.ctx(&fake);
    assert_eq!(
        launch_env(&ctx, "echo", "codex:b", false, false).unwrap(),
        format!(
            "backup_harness=codex backup_account=b backup_cfg_var=CODEX_HOME backup_cfg_dir={} backup_api_helper='' backup_model=''",
            q(&dir)
        )
    );
    // not logged in: the harness's word, and the one command that fixes it
    h.account("codex", "c");
    assert_eq!(
        launch_env(&ctx, "echo", "codex:c", false, false).unwrap_err(),
        "codex:c: not logged in (make it ready: kith account add codex c)"
    );
    // the default login is checked the light way: a logged-out one is the harness's to say on the run, not here
    assert!(launch_env(&ctx, "echo", "codex", false, false).is_ok());
    assert_eq!(
        launch_env(&ctx, "echo", "claude:none", false, false).unwrap_err(),
        "claude:none: no account none (make it ready: kith account add claude none)"
    );
    assert_eq!(
        launch_env(&ctx, "echo", "gemini", false, false).unwrap_err(),
        "gemini: gemini is not supported in launch-json yet"
    );
}

#[test]
fn a_harness_that_is_not_installed_says_so() {
    let h = Home::new("missing");
    let mut fake = Fake::new();
    fake.installed = vec![];
    let ctx = h.ctx(&fake);
    h.account("codex", "c");
    assert_eq!(
        launch_env(&ctx, "echo", "codex:c", false, false).unwrap_err(),
        "codex:c: codex is not installed (make it ready: kith account add codex c)"
    );
}

#[test]
fn the_plain_id_finds_a_set_entry_with_a_model() {
    let h = Home::new("model");
    h.settings(r#"{"accounts": ["claude", "claude@sonnet", "codex@gpt-5", "codex"]}"#);
    let mut fake = Fake::new();
    fake.logged = vec![String::new()];
    let ctx = h.ctx(&fake);
    assert!(
        launch_env(&ctx, "echo", "claude", false, false)
            .unwrap()
            .ends_with("backup_model=sonnet")
    );
    assert!(
        launch_env(&ctx, "echo", "claude@opus", false, false)
            .unwrap()
            .ends_with("backup_model=opus")
    );
    // the set lists the plain id too: the exact entry wins, with no model
    assert!(
        launch_env(&ctx, "echo", "codex", false, false)
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
    let out = launch_env(&ctx, "echo", "claude-api:main", false, false).unwrap();
    assert!(out.starts_with("backup_harness=claude backup_account=main backup_cfg_var='' backup_cfg_dir='' backup_api_helper="));
    let auth = h.dir.join("kernel").join("bin/kith-auth"); // as the code joins it
    // the helper command is quoted word by word, then once more as the value of the assignment
    let helper = format!(
        "{} get --for echo --profile anthropic/main anthropic ANTHROPIC_API_KEY",
        q(&auth)
    );
    assert!(
        out.contains(&format!("backup_api_helper={}", sh_quote(&helper))),
        "{out}"
    );
    let e = launch_env(&ctx, "echo", "claude-api:other", false, false).unwrap_err();
    assert!(e.starts_with("claude-api:other: no anthropic/other key granted to echo (make it ready: kith auth set anthropic/other"));
}

#[test]
fn a_launch_notes_when_the_account_started() {
    let h = Home::new("record");
    let md = h.agent("echo", "claude");
    let dir = h.account("codex", "b");
    let mut fake = Fake::new();
    fake.login(std::slice::from_ref(&dir));
    let ctx = h.ctx(&fake);
    launch_env(&ctx, "echo", "codex:b", false, false).unwrap();
    assert!(!state_file(&h.machine()).exists());
    launch_env(&ctx, "echo", "codex:b", false, true).unwrap();
    main_env(
        &ctx,
        "echo",
        &md,
        MainOpts {
            record: true,
            ..MainOpts::default()
        },
    )
    .unwrap();
    // a launch that cannot run notes nothing
    launch_env(&ctx, "echo", "codex:none", false, true).unwrap_err();
    let s = h.state();
    assert_eq!(s["version"], 1);
    let accts = s["accounts"].as_object().unwrap();
    assert_eq!(accts.keys().collect::<Vec<_>>(), ["codex:b", "claude"]);
    assert!(accts["claude"]["last_started"].as_i64().unwrap() > 1_700_000_000);
    let text = std::fs::read_to_string(state_file(&h.machine())).unwrap();
    assert!(text.starts_with(
        "{\n  \"version\": 1,\n  \"accounts\": {\n    \"codex:b\": {\n      \"last_started\": "
    ));
    assert!(text.ends_with("}\n"));
    assert!(
        h.machine()
            .join("accounts")
            .join("state.json.lock")
            .exists()
    );
    // a state file from another writer keeps what it holds
    h.put_state(r#"{"version": 1, "accounts": {"claude:w": {"out_until": 1900000000, "reason": "rate_limit"}}}"#);
    main_env(
        &ctx,
        "echo",
        &md,
        MainOpts {
            record: true,
            ..MainOpts::default()
        },
    )
    .unwrap();
    let s = h.state();
    assert_eq!(s["accounts"]["claude:w"]["out_until"], 1_900_000_000);
    assert!(s["accounts"]["claude"]["last_started"].is_number());
}

#[test]
fn a_state_change_that_cannot_be_locked_or_written_is_noted_not_dropped_silently() {
    let seen = crate::log::tests::collect();
    let h = Home::new("state-note");
    // "accounts" is a file: the lock file can't be made, so the mark is lost, and the log says so.
    let _ = std::fs::remove_dir_all(h.machine().join("accounts"));
    std::fs::write(h.machine().join("accounts"), "in the way").unwrap();
    record_started(&h.machine(), "claude", 1_900_000_000);
    let marker = h
        .machine()
        .join("accounts")
        .join("state.json")
        .display()
        .to_string();
    assert!(
        seen.lock()
            .unwrap()
            .iter()
            .any(|m| m.contains(&marker) && m.contains("lost")),
        "{:?}",
        seen.lock().unwrap()
    );
}

#[test]
fn a_tombstoned_state_file_is_never_written_over_and_replace_brings_it_back() {
    let h = Home::new("tombstone");
    let note =
        r#"{"kith_tombstone": 1, "module": "accounts", "rollback": "kith rollback accounts"}"#;
    h.put_state(note);
    assert!(tombstoned(&h.machine()));
    record_started(&h.machine(), "claude", 1_900_000_000);
    let dir = h.machine().join("accounts");
    assert_eq!(
        std::fs::read_to_string(dir.join("state.json")).unwrap(),
        note
    );
    assert!(!dir.join("state.json.corrupt").exists(), "note not moved");
    // The old kernel's marks are not merged in either (that would write the state file).
    let old = h.machine().join("serve").join("kernel-backups-out.json");
    std::fs::create_dir_all(old.parent().unwrap()).unwrap();
    std::fs::write(&old, r#"{"claude": {"out_until": 1900000000}}"#).unwrap();
    assert!(state::read(&h.machine()).is_empty());
    assert!(old.exists(), "not renamed");
    // The rollback's export-back writes it again.
    let mut accts = serde_json::Map::new();
    accts.insert("claude".into(), serde_json::json!({"last_started": 5}));
    replace(&h.machine(), &accts).unwrap();
    assert!(!tombstoned(&h.machine()));
    assert_eq!(h.state()["accounts"]["claude"]["last_started"], 5);
}

#[test]
fn a_state_file_that_is_not_ours_is_kept_and_replaced() {
    let h = Home::new("corrupt");
    h.put_state("{not json");
    record_started(&h.machine(), "claude", 1_900_000_000);
    let dir = h.machine().join("accounts");
    assert_eq!(
        std::fs::read_to_string(dir.join("state.json.corrupt")).unwrap(),
        "{not json"
    );
    assert_eq!(
        h.state()["accounts"]["claude"]["last_started"],
        1_900_000_000
    );
    // empty, a list, no accounts: all damaged
    for bad in ["", "[]", r#"{"version": 1}"#, r#"{"accounts": []}"#] {
        h.put_state(bad);
        assert!(state::read(&h.machine()).is_empty(), "{bad:?}");
    }
    // entries that are no objects are left out
    h.put_state(r#"{"accounts": {"a": 1, "b": {"last_started": 5}}}"#);
    assert_eq!(state::read(&h.machine()).keys().collect::<Vec<_>>(), ["b"]);
}

#[test]
fn marks_from_the_older_files_count_until_cleared() {
    let h = Home::new("legacy");
    let m = h.machine();
    std::fs::create_dir_all(m.join("serve")).unwrap();
    // the host's own file: read only
    std::fs::write(
        m.join("serve").join("backups.json"),
        r#"{"claude:a": {"out_until": 1900000500, "since": 1900000000, "reason": "usage"},
            "claude:b": {"out_until": 1900000500, "since": 1900000000}, "codex": {"out_until": 100}}"#,
    )
    .unwrap();
    // the first kernel chain's file: imported once
    std::fs::write(
        m.join("serve").join("kernel-backups-out.json"),
        r#"{"claude:c": {"out_until": 1900000900, "since": 1900000100, "reason": "rate_limit"}}"#,
    )
    .unwrap();
    h.put_state(r#"{"accounts": {"claude:b": {"cleared_at": 1900000400, "cleared_host_until": 1900000500}}}"#);
    let st = state::read(&m);
    assert!(!m.join("serve").join("kernel-backups-out.json").exists());
    assert!(
        m.join("serve")
            .join("kernel-backups-out.json.imported")
            .exists()
    );
    assert_eq!(st["claude:c"]["out_until"], 1_900_000_900);
    assert_eq!(st["claude:c"]["since"], 1_900_000_100);
    let marks = state::out_marks(&m, &st, 1_900_000_200.0);
    // claude:a is out per the host; claude:b was cleared after the host's mark; claude:c is out per the imported mark
    assert_eq!(
        marks["claude:a"],
        state::Mark {
            until: 1_900_000_500,
            reason: "usage".into()
        }
    );
    assert!(!marks.contains_key("claude:b"));
    assert_eq!(marks["claude:c"].until, 1_900_000_900);
    assert!(!marks.contains_key("codex"));
    // later, nothing is out
    assert!(state::out_marks(&m, &st, 1_900_001_000.0).is_empty());
}

#[test]
fn a_fresh_reading_is_the_fullest_window() {
    let st: Value = serde_json::from_str(
        r#"{"usage": {"read_at": 1000, "windows": [{"name": "five_hour", "percent": 71.5}, {"name": "week", "percent": 20}]}}"#,
    )
    .unwrap();
    assert_eq!(state::usage_level(Some(&st), 1000.0 + 60.0), Some(71.5));
    assert_eq!(
        state::usage_level(Some(&st), 1000.0 + 1801.0),
        None,
        "stale"
    );
    assert_eq!(state::usage_level(None, 0.0), None);
    let bad: Value =
        serde_json::from_str(r#"{"usage": {"read_at": 1000, "windows": []}}"#).unwrap();
    assert_eq!(state::usage_level(Some(&bad), 1000.0), None);
}

fn picked(h: &Home, fake: &Fake, agent: &str, deep: bool, now: f64) -> Pick {
    let ctx = h.ctx(fake);
    let s = settings::read(&ctx);
    let st = state::read(&h.machine());
    pick::pick(&ctx, &s, agent, None, deep, now, &st)
}

#[test]
fn pick_skips_what_is_out_or_signed_out() {
    let h = Home::new("pick");
    h.agent("echo", "claude");
    let (a, b) = (h.account("claude", "a"), h.account("claude", "b"));
    h.settings(r#"{"accounts": ["claude", "claude:a", "claude:b", "claude:gone"]}"#);
    h.put_state(r#"{"accounts": {"claude": {"out_until": 1900000500, "reason": "rate_limit", "since": 1900000000}}}"#);
    let mut fake = Fake::new();
    fake.login(&[a, b, PathBuf::new()]);
    let p = picked(&h, &fake, "echo", true, 1_900_000_100.0);
    assert_eq!(p.candidates, ["claude:a", "claude:b"]);
    assert!(!p.all_out);
    assert_eq!(p.policy, "fill");
    let why: Vec<(&str, &str)> = p
        .skipped
        .iter()
        .map(|s| (s.id.as_str(), s.status.status))
        .collect();
    assert_eq!(why, [("claude", "out"), ("claude:gone", "signed_out")]);
    assert_eq!(p.skipped[0].status.out_until, Some(1_900_000_500));
    assert_eq!(p.skipped[1].status.why.as_deref(), Some("no account gone"));
    // after the mark lapses the first account leads again
    let p = picked(&h, &fake, "echo", true, 1_900_000_600.0);
    assert_eq!(p.candidates, ["claude", "claude:a", "claude:b"]);
}

#[test]
fn pick_with_everything_out_runs_the_first() {
    let h = Home::new("pick-all-out");
    h.agent("echo", "claude");
    h.settings(r#"{"accounts": ["claude:x", "claude:y"]}"#);
    let p = picked(&h, &Fake::new(), "echo", true, 1_900_000_100.0);
    assert!(p.all_out);
    assert_eq!(p.candidates, ["claude:x"]);
    assert_eq!(p.skipped.len(), 2);
}

#[test]
fn the_spread_policy_puts_the_most_room_first() {
    let h = Home::new("spread");
    h.agent("echo", "claude");
    for n in ["a", "b", "c"] {
        h.account("claude", n);
    }
    h.settings(r#"{"account_policy": "spread", "accounts": ["claude:a", "claude:b", "claude:c"]}"#);
    h.put_state(
        r#"{"accounts": {
          "claude:a": {"usage": {"read_at": 1900000000, "windows": [{"name": "five_hour", "percent": 80}]}, "last_started": 5},
          "claude:b": {"usage": {"read_at": 1900000000, "windows": [{"name": "five_hour", "percent": 10}]}},
          "claude:c": {"last_started": 9}}}"#,
    );
    let ctx_fake = Fake::new();
    // no login is asked for in the light check: the folders are enough
    let p = picked(&h, &ctx_fake, "echo", false, 1_900_000_100.0);
    // b has 10%, c has no reading (counts as empty) but started later than ... nothing: both are below a
    assert_eq!(p.candidates, ["claude:c", "claude:b", "claude:a"]);
    assert_eq!(p.policy, "spread");
    // a stale reading says nothing: a counts as empty too, and c (started last) goes after the others
    let p = picked(&h, &ctx_fake, "echo", false, 1_900_009_000.0);
    assert_eq!(p.candidates, ["claude:b", "claude:a", "claude:c"]);
}

#[test]
fn a_pin_is_tried_first_and_only_alone() {
    let h = Home::new("pick-pin");
    h.agent("echo", "claude");
    h.agent("solo", "claude");
    for n in ["a", "b"] {
        h.account("claude", n);
    }
    h.settings(
        r#"{"accounts": ["claude:a", "claude:b"], "agents": {"echo": {"pin": {"id": "claude:b"}},
            "solo": {"pin": {"id": "claude:b", "only": true}}}}"#,
    );
    let fake = Fake::new();
    assert_eq!(
        picked(&h, &fake, "echo", false, 1.0).candidates,
        ["claude:b", "claude:a"]
    );
    assert_eq!(
        picked(&h, &fake, "solo", false, 1.0).candidates,
        ["claude:b"]
    );
    // the pin out: a preferred pin falls back to the set, an "only" pin runs anyway and says what is wrong
    h.put_state(r#"{"accounts": {"claude:b": {"out_until": 1900000500, "reason": "rate_limit"}}}"#);
    assert_eq!(
        picked(&h, &fake, "echo", false, 1_900_000_000.0).candidates,
        ["claude:a"]
    );
    let solo = picked(&h, &fake, "solo", false, 1_900_000_000.0);
    assert!(solo.all_out);
    assert_eq!(solo.candidates, ["claude:b"]);
}

#[test]
fn main_env_by_state_starts_on_the_first_candidate() {
    let h = Home::new("main-state");
    let md = h.agent("echo", "claude");
    let (a, b) = (h.account("claude", "a"), h.account("claude", "b"));
    h.settings(r#"{"accounts": ["claude:a", "claude:b"]}"#);
    h.put_state(r#"{"accounts": {"claude:a": {"out_until": 4000000000, "reason": "rate_limit"}}}"#);
    let fake = Fake::new();
    let ctx = h.ctx(&fake);
    let by_state = MainOpts {
        state: true,
        ..MainOpts::default()
    };
    let out = main_env(&ctx, "echo", &md, by_state).unwrap();
    assert!(out.starts_with("main_id=claude:b "), "{out}");
    assert!(out.contains(&q(&b)));
    // the static main ignores the state
    assert!(
        main_env(&ctx, "echo", &md, MainOpts::default())
            .unwrap()
            .contains(&q(&a))
    );
    // a session made under an account that is a candidate goes first
    h.put_state(r#"{"accounts": {}}"#);
    let prefer = MainOpts {
        state: true,
        prefer: Some("claude:b"),
        ..MainOpts::default()
    };
    assert!(
        main_env(&ctx, "echo", &md, prefer)
            .unwrap()
            .starts_with("main_id=claude:b ")
    );
}

#[cfg(unix)]
#[test]
fn an_extra_claude_account_mirrors_the_main_login() {
    let h = Home::new("mirror");
    h.agent("echo", "claude");
    let dir = h.account("claude", "work");
    let main = h.dir.join("home").join(".claude");
    std::fs::create_dir_all(main.join("plugins").join("cache")).unwrap();
    std::fs::write(main.join("settings.json"), r#"{"model": "opus"}"#).unwrap();
    std::fs::write(
        main.join("plugins").join("installed_plugins.json"),
        r#"{"version": 2}"#,
    )
    .unwrap();
    std::fs::write(main.join("plugins").join("blocklist.json"), "not json").unwrap();
    std::fs::write(
        h.dir.join("home").join(".claude.json"),
        r#"{"hasCompletedOnboarding": true, "autoModeEnvSetup": 3, "oauthAccount": {"email": "x"}}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join(".claude.json"),
        r#"{"userID": "u", "hasCompletedOnboarding": false}"#,
    )
    .unwrap();
    let mut fake = Fake::new();
    fake.login(std::slice::from_ref(&dir));
    let ctx = h.ctx(&fake);
    launch_env(&ctx, "echo", "claude:work", false, false).unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.join("settings.json")).unwrap(),
        r#"{"model": "opus"}"#
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("plugins").join("installed_plugins.json")).unwrap(),
        r#"{"version": 2}"#
    );
    assert!(
        !dir.join("plugins").join("blocklist.json").exists(),
        "a broken file is not copied"
    );
    assert_eq!(
        std::fs::read_link(dir.join("plugins").join("cache")).unwrap(),
        main.join("plugins").join("cache")
    );
    // only the acknowledgements, into what the account already has
    let cfg: Value =
        serde_json::from_slice(&std::fs::read(dir.join(".claude.json")).unwrap()).unwrap();
    assert_eq!(
        cfg,
        serde_json::json!({"userID": "u", "hasCompletedOnboarding": true, "autoModeEnvSetup": 3})
    );
    // the write was made under its lock file
    assert!(dir.join(".claude.json.lock").exists());
    // a second launch writes nothing
    let settings_before = std::fs::metadata(dir.join("settings.json"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(mirror::prepare_claude_dir(&ctx, &dir), Vec::<String>::new());
    assert_eq!(
        std::fs::metadata(dir.join("settings.json"))
            .unwrap()
            .modified()
            .unwrap(),
        settings_before
    );
    // a run on an account is not mistaken for the main login
    let own = Ctx {
        claude_config_dir: Some(dir.to_string_lossy().into_owned()),
        ..h.ctx(&fake)
    };
    assert_eq!(main_claude_paths(&own).0, main);
    let other = Ctx {
        claude_config_dir: Some("/elsewhere".to_owned()),
        ..h.ctx(&fake)
    };
    assert_eq!(main_claude_paths(&other).0, Path::new("/elsewhere"));
}

#[test]
fn the_chain_lists_the_main_account_first_then_the_rest_of_the_set() {
    let h = Home::new("chainjson");
    let md = h.agent("echo", "claude");
    let work = h.account("claude", "work");
    h.settings(
        r#"{"accounts": ["claude", "claude:work", "codex", "bad id", "gemini"], "account_cross_harness": true}"#,
    );
    let mut fake = Fake::new();
    fake.login(&[work]);
    let ctx = h.ctx(&fake);
    let j = chain(&ctx, "echo", &md).dumps();
    assert_eq!(
        j,
        r#"{"version": 1, "agent": "echo", "chain": [{"id": "claude", "harness": "claude", "account": null, "model": null, "kind": "primary", "ready": true}, {"id": "claude:work", "harness": "claude", "account": "work", "model": null, "kind": "subscription", "ready": true}, {"id": "codex", "harness": "codex", "account": null, "model": null, "kind": "subscription", "ready": false, "why": "not logged in"}, {"id": "bad id", "harness": null, "account": null, "model": null, "kind": "unknown", "ready": false, "why": "'bad id' is not an account id: use <harness> or <harness>:<name>, with @<model> for a model of its own (claude, claude:work, claude@sonnet, claude-api:main, codex)"}, {"id": "gemini", "harness": "gemini", "account": null, "model": null, "kind": "subscription", "ready": false, "why": "gemini is not supported yet"}]}"#
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
    h.settings(r#"{"accounts": ["claude:g"], "agents": {"echo": {"pin": {"id": "claude:own"}}}}"#);
    assert!(main_json(&ctx, "echo").dumps().starts_with(r#"{"id": "claude:own", "source": "agent", "harness": "claude", "ready": false, "why": "no account own", "fix": "kith account add claude own"#));
    assert!(
        main_json(&ctx, "")
            .dumps()
            .contains(r#""id": "claude:g", "source": "global""#)
    );
    assert!(
        main_json(&ctx, "nobody")
            .dumps()
            .starts_with(r#"{"id": "claude:g", "source": "global", "harness": "claude""#)
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

#[test]
fn a_hold_keeps_the_writers_out_and_the_holders_own_snapshot_gets_through() {
    let h = Home::new("state-hold");
    h.put_state(r#"{"version": 1, "accounts": {"claude": {"last_started": 1}}}"#);
    let held = hold(&h.machine(), Duration::from_secs(5)).unwrap();
    // The holder reads without waiting on its own lock.
    assert_eq!(snapshot(&h.machine()).accounts.len(), 1);
    // A second taker waits and then gives up with a clear error, leaving the first hold alone.
    let Err(err) = hold(&h.machine(), Duration::from_millis(200)) else {
        panic!("a second hold must wait")
    };
    assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
    assert!(err.to_string().contains("state.json.lock"));
    // A writer on another thread does not get in until the hold is dropped.
    let machine = h.machine();
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = done.clone();
    let writer = std::thread::spawn(move || {
        record_started(&machine, "codex", 2);
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
    });
    std::thread::sleep(Duration::from_millis(300));
    assert!(!done.load(std::sync::atomic::Ordering::SeqCst));
    drop(held);
    writer.join().unwrap();
    assert!(done.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(h.state()["accounts"]["codex"]["last_started"], 2);
}
