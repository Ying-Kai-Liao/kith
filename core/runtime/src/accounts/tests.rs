//! Account state in core.db (M3-9a): the import, the read parity with the file, and the `account.*` rows.
//!
//! Read parity is the point: `pick`, the statuses and the listing over `CoreState` must answer what they answer over the
//! file for the same state. It is checked on every case of the two golden corpora (`kernel/tests/difftest/golden/
//! accounts.json`, `accounts-core.json`) and on random state files.
//!
//! The golden cases are replayed for their `mkdir` and `write` setup steps only (the account folders, `settings.json`,
//! `state.json`, the agents' `agent.md`); a fake `claude`/`codex` script or a `sqlite` ledger is not replayed, a test
//! `Outside` stands in for the harnesses (everything is signed in) and the ledger is not read by `pick`. What cannot map
//! onto a read of core.db, and why:
//! - the cases that write the file (`launch-env --record`, `main-env --record`, `account add|remove`): the core writes no
//!   account state yet, so there is no core.db side to compare; the state they START from is compared;
//! - `session-account` / `session-run` (the cost ledger `runs` table) and `paths`-only cases: they read no account state;
//! - a `state.json` that is not ours (`{not json`, empty, `[1, 2]`): the import writes nothing from it and says so; the file
//!   side reads it as empty too, so both sides still answer the same (checked).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::many_single_char_names
)]

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use kith_kernel::accounts::{
    self, Ctx, FileState, Outside, StateSource, out_marks_from, pick_from, rows_from,
};
use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};
use rusqlite::Connection;
use serde_json::{Value, json};

use super::{CoreState, Map, read_all, replace_all};
use crate::db::MIGRATIONS;
use crate::import::accounts::{AccountsOptions, migrate_accounts};
use crate::layout::Layout;
use crate::tests::{machine, open_all, paths};

/// Every harness is installed and signed in; nothing else is asked.
struct SignedIn {
    asked: RefCell<usize>,
}

impl Outside for SignedIn {
    fn status(&self, _argv: &[&str], _env: &[(&str, &str)]) -> (Option<i32>, String) {
        *self.asked.borrow_mut() += 1;
        (Some(0), r#"{"loggedIn":true}"#.to_owned())
    }
    fn granted(&self, _agent: &str, _service: &str) -> Vec<String> {
        vec![]
    }
    fn on_path(&self, _program: &str) -> bool {
        true
    }
}

fn signed_in() -> SignedIn {
    SignedIn {
        asked: RefCell::new(0),
    }
}

fn memory() -> Connection {
    let mut c = Connection::open_in_memory().unwrap();
    crate::db::apply(&mut c, MIGRATIONS, Some("accounts-test")).unwrap();
    c
}

fn put(machine: &Path, rel: &str, text: &str) {
    let f = machine.join(rel);
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    std::fs::write(f, text).unwrap();
}

const NOW: f64 = 1_900_000_000.0;

// ---- the table keeps an entry exactly as the file had it -------------------------------------------------------------

#[test]
fn an_entry_reads_back_exactly_as_the_file_had_it() {
    let c = memory();
    let accounts = json!({
        "claude:work": {"out_until": 4_102_444_800_i64, "reason": "rate_limit", "since": 1, "cleared_at": 2.5,
                        "cleared_host_until": 3, "last_started": 1_800_000_000.0,
                        "usage": {"read_at": 5, "windows": [{"name": "five_hour", "percent": 80.0, "resets_at": 9}, {"percent": 10}]}},
        "claude": {},
        // Not the plain types: kept whole, so the readers trip on them the way they do on the file.
        "odd": {"out_until": "soon", "since": null, "reason": 7, "last_started": true, "usage": [1], "color": "blue",
                "big": 18_446_744_073_709_551_615_u64, "cleared_at": 18_446_744_073_709_551_615_u64},
    });
    let accounts = accounts.as_object().unwrap().clone();
    replace_all(&c, &accounts).unwrap();
    assert_eq!(read_all(&c).unwrap(), accounts);
    // A float stays a float and an integer an integer (the columns carry no type affinity).
    let v = read_all(&c).unwrap();
    assert!(v["claude:work"]["last_started"].is_f64());
    assert!(v["claude:work"]["since"].is_i64());
    // The columns hold what they are named for, and the odd values sit in extra_json.
    let (until, extra): (i64, String) = c
        .query_row(
            "SELECT out_until, extra_json FROM account_state WHERE id = 'claude:work'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(until, 4_102_444_800);
    assert_eq!(extra, "{}");
    let extra: String = c
        .query_row(
            "SELECT extra_json FROM account_state WHERE id = 'odd'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(extra.contains("\"out_until\":\"soon\"") && extra.contains("blue"));
}

#[test]
fn replacing_leaves_only_the_new_rows() {
    let c = memory();
    replace_all(&c, json!({"a": {"since": 1}, "b": {}}).as_object().unwrap()).unwrap();
    replace_all(&c, json!({"b": {"since": 2}}).as_object().unwrap()).unwrap();
    assert_eq!(
        read_all(&c).unwrap(),
        json!({"b": {"since": 2}}).as_object().unwrap().clone()
    );
}

// ---- the import ------------------------------------------------------------------------------------------------------

const STATE: &str = r#"{"version": 1, "accounts": {
    "claude:work": {"out_until": 4102444800, "reason": "rate_limit", "since": 1},
    "claude": {"last_started": 7, "usage": {"read_at": 4102444800, "windows": [{"name": "five_hour", "percent": 10, "resets_at": 4102444800}]}}
}}"#;

fn core_rows(l: &Layout) -> Map {
    let core = open_all(l);
    read_all(core.conn()).unwrap()
}

fn log_rows(l: &Layout) -> i64 {
    let core = open_all(l);
    core.conn()
        .query_row(
            "SELECT COUNT(*) FROM import_log WHERE step = 'accounts'",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

#[test]
fn the_import_copies_the_file_once_and_a_second_run_writes_nothing() {
    let (_t, m) = machine();
    put(&m, "accounts/state.json", STATE);
    let l = Layout::new(&m);
    let first = migrate_accounts(&paths(&m), AccountsOptions::default()).unwrap();
    assert!(first.changed && first.backup.is_some(), "{first:?}");
    assert_eq!(core_rows(&l), FileState.accounts(&m));
    assert_eq!(log_rows(&l), 1);
    let again = migrate_accounts(&paths(&m), AccountsOptions::default()).unwrap();
    assert!(!again.changed && again.backup.is_none(), "{again:?}");
    assert_eq!(log_rows(&l), 1, "no second log row");
}

#[test]
fn a_dry_run_changes_nothing_and_does_not_make_core_db() {
    let (_t, m) = machine();
    put(&m, "accounts/state.json", STATE);
    let before = std::fs::read(m.join("accounts/state.json")).unwrap();
    let r = migrate_accounts(&paths(&m), AccountsOptions { dry_run: true }).unwrap();
    assert!(r.changed && r.dry_run && r.backup.is_none(), "{r:?}");
    assert!(
        !Layout::new(&m).db.exists(),
        "a dry run does not create core.db"
    );
    assert_eq!(
        std::fs::read(m.join("accounts/state.json")).unwrap(),
        before
    );
    // After a real run the dry run says there is nothing to do.
    migrate_accounts(&paths(&m), AccountsOptions::default()).unwrap();
    let r = migrate_accounts(&paths(&m), AccountsOptions { dry_run: true }).unwrap();
    assert!(!r.changed, "{r:?}");
}

#[test]
fn the_import_leaves_the_sources_alone_and_the_file_side_still_imports_the_legacy_marks() {
    let (_t, m) = machine();
    put(&m, "accounts/state.json", STATE);
    put(
        &m,
        "serve/kernel-backups-out.json",
        r#"{"claude": {"out_until": 4102444900, "reason": "five_hour", "since": 5}, "claude:work": {"out_until": 3}}"#,
    );
    let state_before = std::fs::read(m.join("accounts/state.json")).unwrap();
    let r = migrate_accounts(&paths(&m), AccountsOptions::default()).unwrap();
    assert!(
        r.lines.iter().any(|x| x.contains("kernel-backups-out")),
        "{r:?}"
    );
    // Neither source was written or renamed.
    assert_eq!(
        std::fs::read(m.join("accounts/state.json")).unwrap(),
        state_before
    );
    assert!(m.join("serve/kernel-backups-out.json").exists());
    assert!(!m.join("accounts/state.json.lock").exists());
    // core.db holds the file's accounts with the legacy mark laid over (the later out_until wins; 3 < 4102444800 loses).
    let rows = core_rows(&Layout::new(&m));
    assert_eq!(rows["claude"]["out_until"], json!(4_102_444_900_i64));
    assert_eq!(rows["claude"]["reason"], json!("five_hour"));
    assert_eq!(rows["claude"]["last_started"], json!(7));
    assert_eq!(rows["claude:work"]["out_until"], json!(4_102_444_800_i64));
    // The file side reads the same afterwards (this is the one that renames the legacy file, as it always did).
    assert_eq!(FileState.accounts(&m), rows);
    assert!(!m.join("serve/kernel-backups-out.json").exists());
    // And with the legacy file gone and state.json now holding the merge, a re-import changes nothing.
    let again = migrate_accounts(&paths(&m), AccountsOptions::default()).unwrap();
    assert!(!again.changed, "{again:?}");
}

#[test]
fn a_file_that_is_not_ours_imports_nothing_and_says_so() {
    for text in ["{not json", "", "[1, 2]", r#"{"version": 1}"#] {
        let (_t, m) = machine();
        put(&m, "accounts/state.json", text);
        let r = migrate_accounts(&paths(&m), AccountsOptions::default()).unwrap();
        assert!(
            r.lines.iter().any(|x| x.contains("not ours")),
            "{text:?}: {r:?}"
        );
        assert!(core_rows(&Layout::new(&m)).is_empty());
        assert_eq!(
            FileState.accounts(&m),
            Map::new(),
            "the file side reads it as empty too"
        );
    }
}

#[test]
fn no_file_means_nothing_to_import_and_rows_already_there_stay() {
    let (_t, m) = machine();
    let r = migrate_accounts(&paths(&m), AccountsOptions::default()).unwrap();
    assert!(
        !r.changed && r.lines.iter().any(|x| x.contains("nothing to import")),
        "{r:?}"
    );
    assert!(!Layout::new(&m).db.exists());
    put(&m, "accounts/state.json", STATE);
    migrate_accounts(&paths(&m), AccountsOptions::default()).unwrap();
    std::fs::remove_file(m.join("accounts/state.json")).unwrap();
    migrate_accounts(&paths(&m), AccountsOptions::default()).unwrap();
    assert_eq!(core_rows(&Layout::new(&m)).len(), 2);
}

#[test]
fn a_changed_file_replaces_the_rows_and_unknown_top_level_keys_are_reported() {
    let (_t, m) = machine();
    put(&m, "accounts/state.json", STATE);
    migrate_accounts(&paths(&m), AccountsOptions::default()).unwrap();
    put(
        &m,
        "accounts/state.json",
        r#"{"accounts": {"claude": {"last_started": 9, "note": "kept in extra"}}, "future": 1}"#,
    );
    let r = migrate_accounts(&paths(&m), AccountsOptions::default()).unwrap();
    assert!(
        r.changed && r.lines.iter().any(|x| x.contains("future")),
        "{r:?}"
    );
    assert_eq!(core_rows(&Layout::new(&m)), FileState.accounts(&m));
    assert_eq!(log_rows(&Layout::new(&m)), 2);
}

#[test]
fn an_import_under_a_launchs_lock_reads_a_whole_file() {
    use fs2::FileExt as _;
    let (_t, m) = machine();
    put(&m, "accounts/state.json", STATE);
    put(&m, "accounts/state.json.lock", "");
    // A writer holding the exclusive lock blocks the snapshot until it lets go, then the snapshot sees the new file.
    let lock = std::fs::OpenOptions::new()
        .write(true)
        .open(m.join("accounts/state.json.lock"))
        .unwrap();
    lock.lock_exclusive().unwrap();
    let mm = m.clone();
    let reader = std::thread::spawn(move || accounts::snapshot(&mm));
    std::thread::sleep(std::time::Duration::from_millis(150));
    assert!(!reader.is_finished(), "the read waits for the writer");
    put(
        &m,
        "accounts/state.json",
        r#"{"version": 1, "accounts": {"claude": {"last_started": 99}}}"#,
    );
    lock.unlock().unwrap();
    let snap = reader.join().unwrap();
    assert_eq!(snap.accounts["claude"]["last_started"], json!(99));
}

// ---- read parity on the golden corpora -------------------------------------------------------------------------------

struct Replayed {
    root: tempfile::TempDir,
}

impl Replayed {
    fn machine(&self) -> PathBuf {
        self.root.path().join("home/.kith")
    }
    fn ctx<'a>(&self, out: &'a dyn Outside) -> Ctx<'a> {
        Ctx {
            machine: self.machine(),
            agents: self.root.path().join("kith/agents"),
            kernel: self.root.path().join("kith/kernel"),
            home: self.root.path().join("home"),
            claude_config_dir: None,
            out,
        }
    }
    fn agents(&self) -> Vec<String> {
        let d = self.root.path().join("kith/agents");
        let mut v: Vec<String> = std::fs::read_dir(d)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }
}

fn replay(case: &Value) -> Replayed {
    let root = tempfile::tempdir().unwrap();
    for step in case["setup"].as_array().unwrap() {
        if let Some(d) = step["mkdir"].as_str() {
            std::fs::create_dir_all(root.path().join(d)).unwrap();
        } else if let Some(f) = step["write"].as_str() {
            put(root.path(), f, step["text"].as_str().unwrap());
        }
    }
    std::fs::create_dir_all(root.path().join("home/.kith")).unwrap();
    Replayed { root }
}

fn golden(name: &str) -> Vec<Value> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../kernel/tests/difftest/golden")
        .join(name);
    serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
}

/// `pick` (cheap and deep), the out marks and the listing, over the file and over core.db.
fn same_answers(r: &Replayed, core: &Connection, what: &str) {
    let out = signed_in();
    let ctx = r.ctx(&out);
    let m = r.machine();
    let src = CoreState(core);
    assert_eq!(
        out_marks_from(&FileState, &m, NOW),
        out_marks_from(&src, &m, NOW),
        "{what}: out marks"
    );
    let s = accounts::settings::read(&ctx);
    let mut agents = r.agents();
    agents.push(String::new());
    for agent in &agents {
        for deep in [false, true] {
            if !agent.is_empty() {
                let a = pick_from(&ctx, &s, agent, None, deep, NOW, &FileState);
                let b = pick_from(&ctx, &s, agent, None, deep, NOW, &src);
                assert_eq!(
                    format!("{a:?}"),
                    format!("{b:?}"),
                    "{what}: pick {agent} deep={deep}"
                );
            }
            let a = rows_from(&ctx, &s, agent, deep, NOW, &FileState);
            let b = rows_from(&ctx, &s, agent, deep, NOW, &src);
            assert_eq!(a, b, "{what}: list --agent {agent:?} deep={deep}");
        }
    }
}

#[test]
fn pick_and_the_listing_over_core_db_answer_what_the_file_answers_on_every_golden_case() {
    let mut with_state = 0;
    let mut total = 0;
    for file in ["accounts.json", "accounts-core.json"] {
        for case in golden(file) {
            total += 1;
            let name = case["name"].as_str().unwrap().to_owned();
            let r = replay(&case);
            let state_file = r.machine().join("accounts/state.json");
            // Import the case's state (a launch's own write comes after the read being compared).
            let rep = migrate_accounts(&paths(&r.machine()), AccountsOptions::default()).unwrap();
            if state_file.exists() {
                with_state += 1;
            } else {
                assert!(!rep.changed, "{name}: {rep:?}");
            }
            let core = open_all(&Layout::new(&r.machine()));
            assert_eq!(
                read_all(core.conn()).unwrap(),
                FileState.accounts(&r.machine()),
                "{name}: the rows"
            );
            same_answers(&r, core.conn(), &name);
        }
    }
    assert!(total >= 47, "the corpora are read: {total}");
    assert!(
        with_state >= 16,
        "the cases that carry a state.json are read: {with_state}"
    );
}

#[test]
fn the_golden_states_that_are_not_ours_import_as_no_rows() {
    for case in golden("accounts-core.json") {
        let name = case["name"].as_str().unwrap();
        if ![
            "accounts-main-env-state-corrupt",
            "accounts-main-env-state-empty",
            "accounts-main-env-state-pre-599",
            "accounts-launch-env-record-corrupt",
        ]
        .contains(&name)
        {
            continue;
        }
        let r = replay(&case);
        let rep = migrate_accounts(&paths(&r.machine()), AccountsOptions::default()).unwrap();
        assert!(
            rep.lines.iter().any(|x| x.contains("not ours")),
            "{name}: {rep:?}"
        );
        assert!(core_rows(&Layout::new(&r.machine())).is_empty(), "{name}");
    }
}

// ---- read parity on random state files -------------------------------------------------------------------------------

fn num() -> impl Strategy<Value = Value> {
    prop_oneof![
        (0_i64..4_200_000_000).prop_map(Value::from),
        (0_u32..40_000_000).prop_map(|n| Value::from(f64::from(n) * 0.125 + 1_000_000_000.0)),
        Just(Value::from(0)),
    ]
}

fn odd() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        Just(Value::Bool(true)),
        Just(json!("soon")),
        Just(json!("1900000500")),
        Just(json!([1])),
    ]
}

fn field() -> impl Strategy<Value = Value> {
    prop_oneof![8 => num(), 1 => odd()]
}

fn usage() -> impl Strategy<Value = Value> {
    let window = (0_u32..1600, any::<bool>()).prop_map(|(p, f)| {
        let percent = if f {
            Value::from(f64::from(p) / 16.0)
        } else {
            Value::from(p / 16)
        };
        json!({"name": "five_hour", "percent": percent, "resets_at": 4_102_444_800_i64})
    });
    prop_oneof![
        8 => (num(), prop::collection::vec(window, 0..3)).prop_map(|(r, w)| json!({"read_at": r, "windows": w})),
        1 => Just(json!({})),
        1 => odd(),
    ]
}

fn entry() -> impl Strategy<Value = Value> {
    (
        prop::option::of(field()),
        prop::option::of(prop_oneof![
            Just(json!("rate_limit")),
            Just(json!("five_hour")),
            Just(json!("")),
            Just(json!(3))
        ]),
        prop::option::of(field()),
        prop::option::of(field()),
        prop::option::of(field()),
        prop::option::of(field()),
        prop::option::of(usage()),
        prop::option::of(Just(json!("kept"))),
    )
        .prop_map(|(out, reason, since, cleared, host, last, usage, extra)| {
            let mut m = Map::new();
            for (k, v) in [
                ("out_until", out),
                ("reason", reason),
                ("since", since),
                ("cleared_at", cleared),
                ("cleared_host_until", host),
                ("last_started", last),
                ("usage", usage),
                ("note", extra),
            ] {
                if let Some(v) = v {
                    m.insert(k.to_owned(), v);
                }
            }
            Value::Object(m)
        })
}

fn state_file() -> impl Strategy<Value = Value> {
    let id = prop_oneof![
        Just("claude"),
        Just("claude:a"),
        Just("claude:b"),
        Just("claude:c")
    ];
    (
        prop::collection::btree_map(id, entry(), 0..5),
        prop::option::of(Just(json!(["not", "an", "object"]))),
    )
        .prop_map(|(accts, bad)| {
            let mut accounts: Map = accts.into_iter().map(|(k, v)| (k.to_owned(), v)).collect();
            if let Some(b) = bad {
                accounts.insert("claude:junk".to_owned(), b);
            }
            json!({"version": 1, "accounts": accounts})
        })
}

#[test]
fn random_state_files_read_the_same_from_core_db() {
    let mut runner = TestRunner::new(Config {
        cases: 96,
        failure_persistence: None,
        ..Config::default()
    });
    runner
        .run(&(state_file(), prop_oneof![Just("fill"), Just("spread")]), |(state, policy)| {
            let root = tempfile::tempdir().unwrap();
            let r = Replayed { root };
            let m = r.machine();
            put(&m, "accounts/state.json", &state.to_string());
            put(
                &m,
                "settings.json",
                &format!(r#"{{"account_policy": "{policy}", "accounts": ["claude", "claude:a", "claude:b", "claude:c"]}}"#),
            );
            put(&r.root.path().join("kith/agents"), "echo/role/agent.md", "---\nname: echo\nharness: claude-code\n---\nbody\n");
            for n in ["a", "b"] {
                std::fs::create_dir_all(m.join("accounts/claude").join(n)).unwrap();
            }
            let c = memory();
            replace_all(&c, &accounts::snapshot(&m).accounts).unwrap();
            prop_assert_eq!(read_all(&c).unwrap(), FileState.accounts(&m));
            same_answers(&r, &c, &state.to_string());
            Ok(())
        })
        .unwrap();
}

// ---- the rows --------------------------------------------------------------------------------------------------------

#[test]
fn account_state_and_pick_answer_from_core_db() {
    let (_t, m) = machine();
    put(&m, "accounts/state.json", STATE);
    put(
        &m,
        "settings.json",
        r#"{"accounts": ["claude:work", "claude"]}"#,
    );
    std::fs::create_dir_all(m.join("accounts/claude/work")).unwrap();
    migrate_accounts(&paths(&m), AccountsOptions::default()).unwrap();
    // The file moves on after the import: the rows answer the copy, not the file.
    put(
        &m,
        "accounts/state.json",
        r#"{"version": 1, "accounts": {}}"#,
    );
    let core = open_all(&Layout::new(&m));
    let all = core
        .call(kith_api::Principal::Owner, "account.state", &json!({}))
        .unwrap();
    assert_eq!(
        all["accounts"]["claude:work"]["out_until"],
        json!(4_102_444_800_i64)
    );
    let one = core
        .call(
            kith_api::Principal::Owner,
            "account.state",
            &json!({"entry": "claude"}),
        )
        .unwrap();
    assert_eq!(one["state"]["last_started"], json!(7));
    let none = core
        .call(
            kith_api::Principal::Owner,
            "account.state",
            &json!({"entry": "codex:x"}),
        )
        .unwrap();
    assert!(none["state"].is_null());

    let out = signed_in();
    let ctx = Ctx {
        machine: m.clone(),
        agents: m.join("agents"),
        kernel: m.join("kernel"),
        home: m.join("home"),
        claude_config_dir: None,
        out: &out,
    };
    put(
        &m,
        "agents/echo/role/agent.md",
        "---\nname: echo\nharness: claude-code\n---\nbody\n",
    );
    let p = super::call::pick_json(&core, &ctx, &json!({"agent": "echo"}), NOW).unwrap();
    assert_eq!(
        p["candidates"],
        json!(["claude"]),
        "claude:work is out in the copy: {p}"
    );
    assert_eq!(p["skipped"][0]["id"], json!("claude:work"));
    assert_eq!(p["skipped"][0]["status"], json!("out"));
    assert_eq!(p["all_out"], json!(false));
    assert!(super::call::pick_json(&core, &ctx, &json!({}), NOW).is_err());
}

#[test]
fn the_rows_are_owner_only_and_read_only() {
    for name in ["account.state", "account.pick"] {
        let row = kith_api::method(name).unwrap();
        assert!(row.allows(kith_api::Principal::Owner));
        assert!(!row.allows(kith_api::Principal::RunToken), "{name}");
    }
}
