#![allow(clippy::items_after_statements)]
//! The framework on a real core.db with one test-only module. The fake keeps its "store" in `fake-src.json`, its
//! "OS secret" in `os-secret-src` (written back to `os-secret-back`), and the imported copies in core.db's `meta` table.

use std::cell::Cell;
use std::path::PathBuf;

use tempfile::TempDir;

use super::*;
use crate::tests::machine;

#[derive(Default)]
struct Fake {
    name: &'static str,
    deps: &'static [&'static str],
    /// Tombstone path in a folder that does not exist: the tombstone step fails.
    bad_tomb: Cell<bool>,
    fail_export: Cell<bool>,
}

fn fake(name: &'static str) -> Fake {
    Fake {
        name,
        ..Fake::default()
    }
}

fn meta_get(core: &Core, key: &str) -> Option<String> {
    core.conn()
        .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
        .ok()
}

fn meta_put(core: &Core, key: &str, value: &str) {
    core.conn()
        .execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
            [key, value],
        )
        .expect("meta");
}

impl Module for Fake {
    fn name(&self) -> &'static str {
        self.name
    }
    fn depends_on(&self) -> &'static [&'static str] {
        self.deps
    }
    fn tombstones(&self, env: &Env) -> Vec<PathBuf> {
        if self.bad_tomb.get() {
            return vec![env.machine.join("no-such-folder").join("src.json")];
        }
        vec![env.machine.join(format!("{}-src.json", self.name))]
    }
    fn import(&self, env: &Env, core: Option<&mut Core>, _: Opts) -> Result<Vec<String>> {
        let src = env.machine.join(format!("{}-src.json", self.name));
        let text = std::fs::read_to_string(&src).map_err(|e| Error::Io(e.to_string()))?;
        let Some(core) = core else {
            return Ok(vec![format!("would import {} bytes", text.len())]);
        };
        meta_put(core, &format!("{}-data", self.name), &text);
        if let Ok(s) = std::fs::read_to_string(env.machine.join("os-secret-src")) {
            meta_put(core, &format!("{}-secret", self.name), &s);
        }
        Ok(vec!["imported".into()])
    }
    fn same_answer(&self, env: &Env, core: &Core) -> Result<Vec<String>> {
        let src = env.machine.join(format!("{}-src.json", self.name));
        let have = meta_get(core, &format!("{}-data", self.name));
        // After the tombstone the source is gone: the core's copy is the answer.
        if tombstone::is_tombstone(&src) {
            return Ok(if have.is_some() {
                vec![]
            } else {
                vec!["core holds nothing".into()]
            });
        }
        let want = std::fs::read_to_string(&src).ok();
        Ok(if want == have {
            vec![]
        } else {
            vec![format!("data differs: {want:?} vs {have:?}")]
        })
    }
    fn export_back(&self, env: &Env, core: &mut Core) -> Result<Vec<String>> {
        if self.fail_export.get() {
            return Err(Error::Io("export failed".into()));
        }
        let data = meta_get(core, &format!("{}-data", self.name)).unwrap_or_default();
        std::fs::write(env.machine.join(format!("{}-src.json", self.name)), data)
            .map_err(|e| Error::Io(e.to_string()))?;
        Ok(vec!["exported".into()])
    }
    fn restore_secrets(&self, env: &Env, core: &mut Core) -> Result<Vec<String>> {
        if let Some(s) = meta_get(core, &format!("{}-secret", self.name)) {
            std::fs::write(env.machine.join("os-secret-back"), s)
                .map_err(|e| Error::Io(e.to_string()))?;
        }
        Ok(vec!["secret restored".into()])
    }
}

fn setup(names: &[&'static str]) -> (TempDir, Env) {
    let (t, m) = machine();
    std::fs::create_dir_all(&m).expect("machine");
    for n in names {
        std::fs::write(m.join(format!("{n}-src.json")), format!("{{\"{n}\":1}}")).expect("src");
    }
    std::fs::write(m.join("os-secret-src"), "s3cret").expect("secret");
    (t, Env::new(&m))
}

fn reg(f: Vec<Fake>) -> Vec<Box<dyn Module>> {
    f.into_iter().map(|m| Box::new(m) as Box<dyn Module>).collect()
}

fn phase(env: &Env, n: &str) -> Option<Phase> {
    state::read(&env.layout, n).expect("read").map(|s| s.phase)
}

fn opts() -> Opts {
    Opts::default()
}

#[test]
fn a_step_imports_checks_records_and_tombstones() {
    let (_t, env) = setup(&["fa"]);
    let r = reg(vec![fake("fa")]);
    let out = step(&r, &env, "fa", opts()).expect("step");
    assert!(out.iter().any(|l| l.contains("same answer")), "{out:?}");
    assert_eq!(phase(&env, "fa"), Some(Phase::Switched));
    let src = env.machine.join("fa-src.json");
    assert!(tombstone::is_tombstone(&src));
    let kept = std::fs::read_dir(&env.machine)
        .unwrap()
        .filter_map(std::result::Result::ok)
        .any(|e| e.file_name().to_string_lossy().starts_with("fa-src.json.pre-cutover-"));
    assert!(kept, "the original is kept");
    let snap = state::read(&env.layout, "fa").unwrap().unwrap().snapshot.unwrap();
    assert!(std::path::Path::new(&snap).is_file(), "snapshot first");
    // The switch file is the line the router greps for.
    let text = std::fs::read_to_string(state::path(&env.layout, "fa").unwrap()).unwrap();
    assert!(text.lines().any(|l| l == "state=switched"), "{text}");
}

#[test]
fn stepping_twice_changes_nothing_and_a_dry_run_writes_nothing() {
    let (_t, env) = setup(&["fa"]);
    let r = reg(vec![fake("fa")]);
    let dry = step(&r, &env, "fa", Opts { dry_run: true, force: false }).expect("dry");
    assert!(dry.iter().any(|l| l.contains("would import")), "{dry:?}");
    assert_eq!(phase(&env, "fa"), None);
    assert!(!tombstone::is_tombstone(&env.machine.join("fa-src.json")));
    step(&r, &env, "fa", opts()).expect("first");
    let before = state::read(&env.layout, "fa").unwrap();
    let again = step(&r, &env, "fa", opts()).expect("second");
    assert!(again.iter().any(|l| l.contains("already switched")), "{again:?}");
    assert_eq!(state::read(&env.layout, "fa").unwrap(), before);
}

#[test]
fn a_different_answer_leaves_the_module_stepping_and_a_rerun_resumes_with_the_same_snapshot() {
    let (_t, env) = setup(&["fa"]);
    let r = reg(vec![fake("fa")]);
    // A stale value in the core that the import does not touch: make the check fail by racing the source after import.
    // Simplest honest failure: the source is not readable text for the check (a directory), so import errors out.
    let src = env.machine.join("fa-src.json");
    std::fs::remove_file(&src).unwrap();
    assert!(step(&r, &env, "fa", opts()).is_err());
    assert_eq!(phase(&env, "fa"), Some(Phase::Stepping));
    assert!(!tombstone::is_tombstone(&src));
    let snap = state::read(&env.layout, "fa").unwrap().unwrap().snapshot;
    assert!(snap.is_some());
    std::fs::write(&src, "{}").unwrap();
    step(&r, &env, "fa", opts()).expect("resume");
    assert_eq!(phase(&env, "fa"), Some(Phase::Switched));
    assert_eq!(state::read(&env.layout, "fa").unwrap().unwrap().snapshot, snap);
}

#[test]
fn a_check_that_fails_blocks_the_switch() {
    let (_t, env) = setup(&["fa"]);
    struct Liar(Fake);
    impl Module for Liar {
        fn name(&self) -> &'static str {
            self.0.name()
        }
        fn tombstones(&self, e: &Env) -> Vec<PathBuf> {
            self.0.tombstones(e)
        }
        fn import(&self, e: &Env, c: Option<&mut Core>, o: Opts) -> Result<Vec<String>> {
            self.0.import(e, c, o)
        }
        fn same_answer(&self, _: &Env, _: &Core) -> Result<Vec<String>> {
            Ok(vec!["person a: core says no, file says yes".into()])
        }
        fn export_back(&self, e: &Env, c: &mut Core) -> Result<Vec<String>> {
            self.0.export_back(e, c)
        }
    }
    let r: Vec<Box<dyn Module>> = vec![Box::new(Liar(fake("fa")))];
    let e = step(&r, &env, "fa", opts()).expect_err("must refuse").to_string();
    assert!(e.contains("NOT switched") && e.contains("person a"), "{e}");
    assert_eq!(phase(&env, "fa"), Some(Phase::Stepping));
    assert!(!tombstone::is_tombstone(&env.machine.join("fa-src.json")));
    // The only way out is a rollback, which just cancels.
    let out = rollback(&r, &env, "fa", opts()).expect("cancel");
    assert!(out[0].contains("cancelled"), "{out:?}");
    assert_eq!(phase(&env, "fa"), None);
}

#[test]
fn a_crash_between_the_record_and_the_tombstone_is_repaired_by_stepping_again() {
    let (_t, env) = setup(&["fa"]);
    let f = fake("fa");
    f.bad_tomb.set(true);
    let r = reg(vec![f]);
    assert!(step(&r, &env, "fa", opts()).is_err());
    assert_eq!(phase(&env, "fa"), Some(Phase::Switched), "recorded before the tombstone");
    let f = fake("fa");
    let r = reg(vec![f]);
    let out = step(&r, &env, "fa", opts()).expect("repair");
    assert!(out.iter().any(|l| l.contains("already switched")), "{out:?}");
    assert!(tombstone::is_tombstone(&env.machine.join("fa-src.json")));
}

#[test]
fn a_rollback_exports_the_core_back_keeps_later_writes_and_writes_the_secret_back() {
    let (_t, env) = setup(&["fa"]);
    let r = reg(vec![fake("fa")]);
    step(&r, &env, "fa", opts()).expect("step");
    // A write made after the switch (a revoke): the file must carry it back.
    {
        let core = crate::start_with(&env.layout, MIGRATIONS).unwrap();
        meta_put(&core, "fa-data", "{\"fa\":1,\"revoked\":[\"p\"]}");
    }
    let out = rollback(&r, &env, "fa", opts()).expect("rollback");
    assert!(out.iter().any(|l| l.contains("secret restored")), "{out:?}");
    let src = env.machine.join("fa-src.json");
    assert!(!tombstone::is_tombstone(&src));
    assert_eq!(std::fs::read_to_string(&src).unwrap(), "{\"fa\":1,\"revoked\":[\"p\"]}");
    assert_eq!(std::fs::read_to_string(env.machine.join("os-secret-back")).unwrap(), "s3cret");
    assert_eq!(phase(&env, "fa"), None);
    // Twice is a no-op; and a step after a rollback imports the exported file again.
    let again = rollback(&r, &env, "fa", opts()).expect("again");
    assert!(again[0].contains("not switched"), "{again:?}");
    step(&r, &env, "fa", opts()).expect("re-step");
    assert_eq!(phase(&env, "fa"), Some(Phase::Switched));
}

#[test]
fn a_rollback_waits_for_a_live_claim_unless_forced() {
    let (_t, env) = setup(&["fa"]);
    let r = reg(vec![fake("fa")]);
    step(&r, &env, "fa", opts()).expect("step");
    {
        let core = crate::start_with(&env.layout, MIGRATIONS).unwrap();
        core.conn()
            .execute_batch(
                "INSERT INTO conversations (id, agent, engine, created_at, updated_at) VALUES ('c1', 'a', 'claude', 1, 1);
                 INSERT INTO claims (conversation, generation, kind, state, created_at) VALUES ('c1', 1, 'turn', 'running', 1);",
            )
            .unwrap();
    }
    let e = rollback(&r, &env, "fa", opts()).expect_err("live").to_string();
    assert!(e.contains("live") && e.contains("--force"), "{e}");
    assert_eq!(phase(&env, "fa"), Some(Phase::Switched), "nothing changed");
    assert!(tombstone::is_tombstone(&env.machine.join("fa-src.json")));
    let out = rollback(&r, &env, "fa", Opts { dry_run: false, force: true }).expect("forced");
    assert!(out.iter().any(|l| l.contains("--force")), "{out:?}");
    assert_eq!(phase(&env, "fa"), None);
}

#[test]
fn a_rollback_that_stops_half_way_stays_closed_and_a_rerun_finishes_it() {
    let (_t, env) = setup(&["fa"]);
    let f = fake("fa");
    let r = reg(vec![f]);
    step(&r, &env, "fa", opts()).expect("step");
    let f = fake("fa");
    f.fail_export.set(true);
    let broken = reg(vec![f]);
    assert!(rollback(&broken, &env, "fa", opts()).is_err());
    assert_eq!(phase(&env, "fa"), Some(Phase::RollingBack));
    let e = step(&r, &env, "fa", opts()).expect_err("no step mid-rollback").to_string();
    assert!(e.contains("rollback"), "{e}");
    rollback(&r, &env, "fa", opts()).expect("finish");
    assert_eq!(phase(&env, "fa"), None);
    assert!(!tombstone::is_tombstone(&env.machine.join("fa-src.json")));
}

#[test]
fn modules_are_ordered_by_what_they_depend_on() {
    let (_t, env) = setup(&["fa", "fb"]);
    let r = reg(vec![
        fake("fa"),
        Fake {
            name: "fb",
            deps: &["fa"],
            ..Fake::default()
        },
    ]);
    let e = step(&r, &env, "fb", opts()).expect_err("needs fa").to_string();
    assert!(e.contains("needs fa switched"), "{e}");
    step(&r, &env, "fa", opts()).unwrap();
    step(&r, &env, "fb", opts()).unwrap();
    let e = rollback(&r, &env, "fa", opts()).expect_err("fb depends").to_string();
    assert!(e.contains("fb depends on fa"), "{e}");
    rollback(&r, &env, "fb", opts()).unwrap();
    rollback(&r, &env, "fa", opts()).unwrap();
}

#[test]
fn may_serve_needs_the_switch_and_a_passing_check() {
    let (_t, env) = setup(&["fa"]);
    let f = fake("fa");
    let r = reg(vec![fake("fa")]);
    let core = crate::start_with(&env.layout, MIGRATIONS).unwrap();
    assert_eq!(may_serve(&f, &env, &core), Serve { ok: false, warning: None });
    drop(core);
    step(&r, &env, "fa", opts()).unwrap();
    let core = crate::start_with(&env.layout, MIGRATIONS).unwrap();
    assert!(may_serve(&f, &env, &core).ok);
    core.conn().execute("DELETE FROM meta WHERE key = 'fa-data'", []).unwrap();
    let s = may_serve(&f, &env, &core);
    assert!(!s.ok && s.warning.is_some_and(|w| w.contains("difference")), "{s:?}");
}

#[test]
fn a_damaged_switch_file_counts_as_switched_and_names_are_checked() {
    let (_t, env) = setup(&["fa"]);
    let r = reg(vec![fake("fa")]);
    std::fs::create_dir_all(state::dir(&env.layout)).unwrap();
    std::fs::write(state::path(&env.layout, "fa").unwrap(), "garbage").unwrap();
    assert!(step(&r, &env, "fa", opts()).is_err());
    assert!(state::path(&env.layout, "../x").is_err());
    assert!(step(&r, &env, "nope", opts()).unwrap_err().to_string().contains("no module"));
}

#[test]
fn the_cli_words_parse() {
    let (_t, env) = setup(&["fa"]);
    let r = reg(vec![fake("fa")]);
    let a = |s: &[&str]| s.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
    assert_eq!(run_with(&r, &env, &a(&["modules"])).unwrap(), vec!["fa"]);
    assert!(run_with(&r, &env, &a(&["step", "fa", "--dry-run"])).is_ok());
    assert!(run_with(&r, &env, &a(&["rollback", "fa", "--dry-run"])).is_err());
    assert!(run_with(&r, &env, &a(&["step"])).is_err());
    run_with(&r, &env, &a(&["step", "fa"])).unwrap();
    assert!(run_with(&r, &env, &a(&["status"])).unwrap()[0].contains("switched"));
    assert_eq!(run_with(&r, &env, &a(&["check", "fa"])).unwrap(), vec!["fa: same answer"]);
    assert_eq!(run(&env.machine, &a(&["status"])).unwrap(), vec!["no module has a cutover yet"]);
}
