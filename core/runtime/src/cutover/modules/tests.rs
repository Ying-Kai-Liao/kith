#![allow(clippy::unwrap_used)]
//! The S1 modules on a real core.db and real-shaped sources: `accounts/state.json` as `kith_accounts.py` writes it (with an
//! entry from an older kernel that lacks fields, and a usage reading) and a session registry as `kith-sessions record` does.

use rusqlite::Connection;
use serde_json::{Value, json};

use crate::cutover::same_answer::may_serve;
use crate::cutover::{Env, Module, marker, rollback, state, step, tombstone};
use crate::cutover::{Opts, check, registry};
use crate::tests::machine;

fn opts() -> Opts {
    Opts::default()
}

fn paths_of(m: &std::path::Path) -> kith_kernel::paths::Paths {
    let mut p = crate::tests::paths(m);
    p.state = m.join("state");
    p.home = m.join("home");
    p
}

fn setup() -> (tempfile::TempDir, Env) {
    let (t, m) = machine();
    std::fs::create_dir_all(m.join("accounts")).unwrap();
    let state = json!({
        "version": 1,
        "accounts": {
            "claude": {"last_started": 1_900_000_100},
            "claude:work": {
                "out_until": 1_900_000_000, "reason": "rate_limit", "since": 1_899_990_000, "cleared_at": 1_899_999_000,
                "usage": {"read_at": 1_899_990_000, "windows": [{"name": "five_hour", "percent": 71.5, "resets_at": 1_899_999_999}]},
                "last_started": 1_899_990_000
            },
            // written by an older kernel: nothing but an unknown key
            "codex:old": {"note": "kept whole"},
        }
    });
    std::fs::write(
        m.join("accounts/state.json"),
        serde_json::to_string_pretty(&state).unwrap(),
    )
    .unwrap();
    (t, Env::with_paths(&paths_of(&m)))
}

fn read_state(env: &Env) -> Value {
    serde_json::from_slice(&std::fs::read(kith_kernel::accounts::state_file(&env.machine)).unwrap())
        .unwrap()
}

fn db(env: &Env) -> Connection {
    Connection::open(&env.layout.db).unwrap()
}

fn serves(env: &Env, m: &dyn Module) -> bool {
    may_serve(m, env, &db(env), false).ok
}

fn writes(env: &Env) -> bool {
    crate::accounts::core_writes(&db(env)).unwrap()
}

// --- accounts ------------------------------------------------------------------------------------------------------------

#[test]
fn accounts_nothing_is_switched_by_default() {
    let (_t, env) = setup();
    let reg = registry();
    assert!(reg.iter().any(|m| m.name() == "accounts"));
    assert_eq!(
        crate::cutover::status(&reg, &env).unwrap()[0],
        "accounts: not switched"
    );
    assert!(!kith_kernel::accounts::tombstoned(&env.machine));
    // A core that never stepped: no write, no serve.
    crate::tests::open_all(&env.layout);
    assert!(!writes(&env));
    assert!(!serves(&env, &super::accounts::Accounts));
}

#[test]
fn accounts_a_step_makes_the_core_the_writer_and_the_file_a_tombstone() {
    let (_t, env) = setup();
    let reg = registry();
    let before = read_state(&env);
    let out = step(&reg, &env, "accounts", opts()).unwrap();
    assert!(out.iter().any(|l| l.contains("same answer")), "{out:?}");
    assert!(serves(&env, &super::accounts::Accounts));
    assert!(writes(&env), "may_serve yes: the failover writes the row");
    assert!(tombstone::is_tombstone(&kith_kernel::accounts::state_file(
        &env.machine
    )));
    // The rows are the file's, an older entry's unknown key included.
    let rows = crate::accounts::read_all(&db(&env)).unwrap();
    assert_eq!(Value::Object(rows), before["accounts"]);
    // The original is kept beside the note.
    let kept = std::fs::read_dir(env.machine.join("accounts"))
        .unwrap()
        .flatten()
        .any(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("state.json.pre-cutover-")
        });
    assert!(kept);
}

#[test]
fn accounts_the_rust_writers_leave_the_note_alone_and_the_core_row_writes() {
    let (_t, env) = setup();
    step(&registry(), &env, "accounts", opts()).unwrap();
    let file = kith_kernel::accounts::state_file(&env.machine);
    let note = std::fs::read(&file).unwrap();
    kith_kernel::accounts::record_started(&env.machine, "claude", 5);
    assert_eq!(std::fs::read(&file).unwrap(), note, "refused, not written");
    // The core writes through the gate the switch opened.
    let mut core = crate::start_with(&env.layout, crate::db::MIGRATIONS).unwrap();
    assert!(crate::accounts::core_writes(core.conn()).unwrap());
    let tx = core.claim_tx().unwrap();
    crate::accounts::mark_out(
        &tx,
        "claude",
        Some(2_000_000_000),
        "rate_limit",
        1_900_000_200,
        true,
    )
    .unwrap();
    tx.commit().unwrap();
    assert_eq!(
        crate::accounts::read_all(core.conn()).unwrap()["claude"]["out_until"],
        2_000_000_000
    );
}

#[test]
fn accounts_a_tampered_marker_or_switch_file_denies_everything() {
    let (_t, env) = setup();
    step(&registry(), &env, "accounts", opts()).unwrap();
    // The marker is gone (core.db from before the step, or another machine's).
    db(&env)
        .execute("DELETE FROM meta WHERE key = 'cutover:accounts'", [])
        .unwrap();
    assert!(!serves(&env, &super::accounts::Accounts));
    assert!(!writes(&env));
    // A marker for another switch (`at` differs): a forged or stale file.
    let mut core = crate::start_with(&env.layout, crate::db::MIGRATIONS).unwrap();
    marker::write(&mut core, "accounts", 1, "").unwrap();
    drop(core);
    assert!(!serves(&env, &super::accounts::Accounts));
    assert!(!writes(&env));
}

#[test]
fn accounts_a_missing_tombstone_still_serves_the_core_but_warns() {
    // The framework's rule (same_answer.rs): the core is the truth, but a plain file back means something wrote the old
    // store after the switch, which is said loudly. (A missing marker is the deny, above.)
    let (_t, env) = setup();
    step(&registry(), &env, "accounts", opts()).unwrap();
    std::fs::write(
        kith_kernel::accounts::state_file(&env.machine),
        r#"{"version": 1, "accounts": {}}"#,
    )
    .unwrap();
    let s = may_serve(&super::accounts::Accounts, &env, &db(&env), false);
    assert!(s.ok);
    assert!(s.warning.unwrap().contains("no longer a tombstone"));
}

#[test]
fn accounts_a_rollback_gives_python_the_file_back_and_the_core_denies() {
    let (_t, env) = setup();
    let reg = registry();
    let before = read_state(&env);
    step(&reg, &env, "accounts", opts()).unwrap();
    // The core changed an entry while it was the writer.
    let mut core = crate::start_with(&env.layout, crate::db::MIGRATIONS).unwrap();
    let tx = core.claim_tx().unwrap();
    crate::accounts::mark_out(
        &tx,
        "claude",
        Some(2_000_000_000),
        "rate_limit",
        1_900_000_200,
        true,
    )
    .unwrap();
    tx.commit().unwrap();
    drop(core);
    rollback(&reg, &env, "accounts", opts()).unwrap();
    assert!(!kith_kernel::accounts::tombstoned(&env.machine));
    let after = read_state(&env);
    assert_eq!(
        after["accounts"]["claude:work"],
        before["accounts"]["claude:work"]
    );
    assert_eq!(
        after["accounts"]["codex:old"],
        before["accounts"]["codex:old"]
    );
    assert_eq!(after["accounts"]["claude"]["out_until"], 2_000_000_000);
    assert!(!serves(&env, &super::accounts::Accounts));
    assert!(!writes(&env));
    // Python writes again: the Rust kernel's writer stands for it.
    kith_kernel::accounts::record_started(&env.machine, "claude:work", 7);
    assert_eq!(
        read_state(&env)["accounts"]["claude:work"]["last_started"],
        7
    );
    // And the module can be stepped again.
    step(&reg, &env, "accounts", opts()).unwrap();
    assert!(serves(&env, &super::accounts::Accounts));
}

#[test]
fn accounts_a_repeated_step_and_a_crash_part_way_resume() {
    let (_t, env) = setup();
    let reg = registry();
    step(&reg, &env, "accounts", opts()).unwrap();
    let again = step(&reg, &env, "accounts", opts()).unwrap();
    assert!(
        again.iter().any(|l| l.contains("already switched")),
        "{again:?}"
    );
    // A crash between the record and the tombstone: the file is back (as a copy of the original), the record says switched.
    let file = kith_kernel::accounts::state_file(&env.machine);
    std::fs::remove_file(&file).unwrap();
    step(&reg, &env, "accounts", opts()).unwrap();
    assert!(tombstone::is_tombstone(&file), "step repairs the tombstone");
    // A crash during the import: the record says stepping and nothing is tombstoned.
    rollback(&reg, &env, "accounts", opts()).unwrap();
    let at = 1;
    state::write(
        &env.layout,
        "accounts",
        &state::Switch {
            phase: state::Phase::Stepping,
            snapshot: None,
            at,
        },
    )
    .unwrap();
    assert!(!serves(&env, &super::accounts::Accounts));
    step(&reg, &env, "accounts", opts()).unwrap();
    assert!(serves(&env, &super::accounts::Accounts));
}

#[test]
fn accounts_same_answer_catches_a_seeded_difference() {
    let (_t, env) = setup();
    let reg = registry();
    // Nothing is in the core yet: every entry differs.
    crate::tests::open_all(&env.layout);
    let first = check(&reg, &env, "accounts").unwrap();
    assert!(first[0].contains("3 difference(s)"), "{first:?}");
    // Import, then seed a difference in each direction.
    let mut core = crate::start_with(&env.layout, crate::db::MIGRATIONS).unwrap();
    let m = super::accounts::Accounts;
    m.import(&env, Some(&mut core), opts()).unwrap();
    assert!(m.same_answer(&env, core.conn()).unwrap().is_empty());
    core.conn()
        .execute(
            "UPDATE account_state SET out_until = 5 WHERE id = 'claude:work'",
            [],
        )
        .unwrap();
    core.conn()
        .execute(
            "INSERT INTO account_state (id, extra_json) VALUES ('codex:ghost', '{}')",
            [],
        )
        .unwrap();
    let diffs = m.same_answer(&env, core.conn()).unwrap();
    assert_eq!(diffs.len(), 2, "{diffs:?}");
    assert!(
        diffs
            .iter()
            .any(|d| d.starts_with("claude:work: state.json says"))
    );
    assert!(
        diffs
            .iter()
            .any(|d| d == "codex:ghost: in the core, not in state.json")
    );
    drop(core);
    // A step imports again (rows that differ are replaced by the file's) and then agrees.
    let out = step(&reg, &env, "accounts", opts()).unwrap();
    assert!(out.iter().any(|l| l.contains("same answer")));
}

#[test]
fn accounts_a_file_that_says_nothing_has_nothing_to_compare() {
    let (_t, env) = setup();
    let file = kith_kernel::accounts::state_file(&env.machine);
    let m = super::accounts::Accounts;
    let core = crate::tests::open_all(&env.layout);
    std::fs::write(&file, "{not json").unwrap();
    assert!(m.same_answer(&env, core.conn()).unwrap().is_empty());
    std::fs::remove_file(&file).unwrap();
    assert!(m.same_answer(&env, core.conn()).unwrap().is_empty());
}

#[test]
fn accounts_two_steps_at_once_one_refuses() {
    let (_t, env) = setup();
    // The first holds the core's lifetime lock (a daemon or another shell would): the step refuses instead of racing.
    let held = crate::start_with(&env.layout, crate::db::MIGRATIONS).unwrap();
    let err = step(&registry(), &env, "accounts", opts()).unwrap_err();
    drop(held);
    assert!(!err.to_string().is_empty());
    assert!(!kith_kernel::accounts::tombstoned(&env.machine));
    step(&registry(), &env, "accounts", opts()).unwrap();
}

// --- transcripts ---------------------------------------------------------------------------------------------------------

fn registry_dir(env: &Env) -> std::path::PathBuf {
    kith_kernel::threads::registry_dir(env.state.as_deref().unwrap())
}

fn register(env: &Env, sid: &str, cwd: &str) {
    let dir = registry_dir(env);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{sid}.json")),
        json!({"v": 1, "sessionId": sid, "agent": "coder", "cwd": cwd}).to_string(),
    )
    .unwrap();
}

#[test]
fn transcripts_have_no_python_writer_to_tombstone_and_the_step_switches() {
    let (_t, env) = setup();
    let reg = registry();
    register(&env, "s-one", "/work");
    register(&env, "s-two", "/other");
    let m = super::transcripts::Transcripts;
    assert!(m.tombstones(&env).is_empty());
    let out = step(&reg, &env, "transcripts", opts()).unwrap();
    assert!(out.iter().any(|l| l.contains("same answer")), "{out:?}");
    assert!(serves(&env, &m));
    let n: i64 = db(&env)
        .query_row("SELECT COUNT(*) FROM engine_sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 2);
    // The registry was only read.
    assert!(registry_dir(&env).join("s-one.json").is_file());
    // Marker tampered: deny.
    db(&env)
        .execute("DELETE FROM meta WHERE key = 'cutover:transcripts'", [])
        .unwrap();
    assert!(!serves(&env, &m));
}

#[test]
fn transcripts_same_answer_catches_a_missing_and_a_changed_session() {
    let (_t, env) = setup();
    register(&env, "s-one", "/work");
    let m = super::transcripts::Transcripts;
    let mut core = crate::start_with(&env.layout, crate::db::MIGRATIONS).unwrap();
    assert_eq!(m.same_answer(&env, core.conn()).unwrap().len(), 1);
    m.import(&env, Some(&mut core), opts()).unwrap();
    assert!(m.same_answer(&env, core.conn()).unwrap().is_empty());
    // A session registered after the import, and one whose working folder moved.
    register(&env, "s-new", "/w2");
    register(&env, "s-one", "/elsewhere");
    let diffs = m.same_answer(&env, core.conn()).unwrap();
    assert_eq!(diffs.len(), 2, "{diffs:?}");
    assert!(
        diffs
            .iter()
            .any(|d| d.contains("s-new") && d.contains("not in the core"))
    );
    assert!(diffs.iter().any(|d| d.contains("working folder")));
    // The core holding a session the registry does not have is not a difference.
    std::fs::remove_file(registry_dir(&env).join("s-new.json")).unwrap();
    register(&env, "s-one", "/work");
    assert!(m.same_answer(&env, core.conn()).unwrap().is_empty());
    std::fs::remove_file(registry_dir(&env).join("s-one.json")).unwrap();
    assert!(m.same_answer(&env, core.conn()).unwrap().is_empty());
}

#[test]
fn transcripts_a_rollback_clears_the_switch_and_a_step_resumes() {
    let (_t, env) = setup();
    let reg = registry();
    register(&env, "s-one", "/work");
    let m = super::transcripts::Transcripts;
    step(&reg, &env, "transcripts", opts()).unwrap();
    assert!(serves(&env, &m));
    let out = rollback(&reg, &env, "transcripts", opts()).unwrap();
    assert!(out.iter().any(|l| l.contains("rolled back")), "{out:?}");
    assert!(!serves(&env, &m));
    assert!(state::read(&env.layout, "transcripts").unwrap().is_none());
    // A crash during the import resumes.
    state::write(
        &env.layout,
        "transcripts",
        &state::Switch {
            phase: state::Phase::Stepping,
            snapshot: None,
            at: 1,
        },
    )
    .unwrap();
    register(&env, "s-two", "/w");
    step(&reg, &env, "transcripts", opts()).unwrap();
    assert!(serves(&env, &m));
    // `kith migrate check` on a fixture is the same-answer; on a switched module it says the core is the store.
    assert!(check(&reg, &env, "transcripts").unwrap()[0].contains("only store"));
}

#[test]
fn both_s1_modules_are_listed_and_checkable() {
    let (_t, env) = setup();
    register(&env, "s-one", "/work");
    let reg = registry();
    let a = |s: &[&str]| s.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
    assert_eq!(
        crate::cutover::run_with(&reg, &env, &a(&["modules"])).unwrap(),
        vec!["accounts", "transcripts"]
    );
    crate::tests::open_all(&env.layout);
    for m in ["accounts", "transcripts"] {
        let out = check(&reg, &env, m).unwrap();
        assert!(out[0].contains("difference"), "{m}: {out:?}");
    }
    for m in ["accounts", "transcripts"] {
        step(&reg, &env, m, opts()).unwrap();
        assert!(check(&reg, &env, m).unwrap()[0].contains("only store"));
    }
}
