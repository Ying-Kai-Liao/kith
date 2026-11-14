#![allow(clippy::items_after_statements, clippy::unwrap_used)]
//! The framework on a real core.db with one test-only module. The fake keeps its "store" in `fake-src.json`, its
//! "OS secret" in `os-secret-src` (written back to `os-secret-back`), and the imported copies in core.db's `meta` table.

use std::cell::Cell;
use std::path::PathBuf;

use rusqlite::Connection;
use tempfile::TempDir;

use super::same_answer::{Serve, may_serve};
use super::*;
use crate::tests::machine;

#[derive(Default)]
struct Fake {
    name: &'static str,
    deps: &'static [&'static str],
    /// Tombstone path in a folder that does not exist: the tombstone step fails.
    bad_tomb: Cell<bool>,
    fail_export: Cell<bool>,
    /// The source lock: a flock on `fake.lock` held by `step`; `seen` records whether it was held at each hook.
    lock: bool,
    refuse: Option<&'static str>,
    seen: std::cell::RefCell<Vec<(&'static str, bool)>>,
}

impl Fake {
    fn probe(&self, env: &Env, at: &'static str) {
        use fs2::FileExt as _;
        let f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(env.machine.join("fake.lock"))
            .expect("lock file");
        let held = f.try_lock_exclusive().is_err();
        self.seen.borrow_mut().push((at, held));
    }
}

fn fake(name: &'static str) -> Fake {
    Fake {
        name,
        ..Fake::default()
    }
}

fn meta_get(db: &Connection, key: &str) -> Option<String> {
    db.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
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
    fn step_refusal(&self, _env: &Env) -> Option<String> {
        self.refuse.map(str::to_owned)
    }
    fn source_lock(&self, env: &Env) -> Result<Option<SourceLock>> {
        use fs2::FileExt as _;
        if !self.lock {
            return Ok(None);
        }
        let f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(env.machine.join("fake.lock"))
            .map_err(|e| Error::Io(e.to_string()))?;
        f.lock_exclusive().map_err(|e| Error::Io(e.to_string()))?;
        Ok(Some(Box::new(f)))
    }
    fn tombstones(&self, env: &Env) -> Vec<PathBuf> {
        if self.lock {
            self.probe(env, "tombstones");
        }
        if self.bad_tomb.get() {
            return vec![env.machine.join("no-such-folder").join("src.json")];
        }
        vec![env.machine.join(format!("{}-src.json", self.name))]
    }
    fn import(&self, env: &Env, core: Option<&mut Core>, _: Opts) -> Result<Vec<String>> {
        let src = env.machine.join(format!("{}-src.json", self.name));
        if self.lock {
            self.probe(env, "import");
        }
        if tombstone::is_tombstone(&src) {
            return Err(Error::Refused("the source is a tombstone".into()));
        }
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
    fn same_answer(&self, env: &Env, db: &Connection) -> Result<Vec<String>> {
        if self.lock {
            self.probe(env, "same_answer");
        }
        let src = env.machine.join(format!("{}-src.json", self.name));
        let have = meta_get(db, &format!("{}-data", self.name));
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
        let data = meta_get(core.conn(), &format!("{}-data", self.name)).unwrap_or_default();
        std::fs::write(env.machine.join(format!("{}-src.json", self.name)), data)
            .map_err(|e| Error::Io(e.to_string()))?;
        Ok(vec!["exported".into()])
    }
    fn restore_secrets(&self, env: &Env, core: &mut Core) -> Result<Vec<String>> {
        let mut out = vec!["secret restored".to_owned()];
        if let Some(s) = meta_get(core.conn(), &format!("{}-secret", self.name)) {
            let mut store = FileStore(env.machine.join("os-store"));
            // A tiny limit, so the split path runs through a real rollback.
            let items = std::iter::once(("fake-secret".to_owned(), s)).collect();
            out.extend(secrets_back::restore_all(&mut store, &items, Some(8))?);
        }
        Ok(out)
    }
}

/// An OS store made of files, one per item name.
struct FileStore(PathBuf);

impl secrets_back::OsStore for FileStore {
    fn get(&self, name: &str) -> Result<Option<zeroize::Zeroizing<String>>> {
        Ok(std::fs::read_to_string(self.0.join(name.replace('#', "_")))
            .ok()
            .map(zeroize::Zeroizing::new))
    }
    fn put(&mut self, name: &str, value: &str) -> Result<()> {
        std::fs::create_dir_all(&self.0).map_err(|e| Error::Io(e.to_string()))?;
        std::fs::write(self.0.join(name.replace('#', "_")), value)
            .map_err(|e| Error::Io(e.to_string()))
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
    f.into_iter()
        .map(|m| Box::new(m) as Box<dyn Module>)
        .collect()
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
        .any(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("fa-src.json.pre-cutover-")
        });
    assert!(kept, "the original is kept");
    let snap = state::read(&env.layout, "fa")
        .unwrap()
        .unwrap()
        .snapshot
        .unwrap();
    assert!(std::path::Path::new(&snap).is_file(), "snapshot first");
    // The switch file is the line the router greps for.
    let text = std::fs::read_to_string(state::path(&env.layout, "fa").unwrap()).unwrap();
    assert!(text.lines().any(|l| l == "state=switched"), "{text}");
}

#[test]
fn stepping_twice_changes_nothing_and_a_dry_run_writes_nothing() {
    let (_t, env) = setup(&["fa"]);
    let r = reg(vec![fake("fa")]);
    let dry = step(
        &r,
        &env,
        "fa",
        Opts {
            dry_run: true,
            force: false,
        },
    )
    .expect("dry");
    assert!(dry.iter().any(|l| l.contains("would import")), "{dry:?}");
    assert_eq!(phase(&env, "fa"), None);
    assert!(!tombstone::is_tombstone(&env.machine.join("fa-src.json")));
    step(&r, &env, "fa", opts()).expect("first");
    let before = state::read(&env.layout, "fa").unwrap();
    let again = step(&r, &env, "fa", opts()).expect("second");
    assert!(
        again.iter().any(|l| l.contains("already switched")),
        "{again:?}"
    );
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
    assert_eq!(
        state::read(&env.layout, "fa").unwrap().unwrap().snapshot,
        snap
    );
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
        fn same_answer(&self, _: &Env, _: &Connection) -> Result<Vec<String>> {
            Ok(vec!["person a: core says no, file says yes".into()])
        }
        fn export_back(&self, e: &Env, c: &mut Core) -> Result<Vec<String>> {
            self.0.export_back(e, c)
        }
    }
    let r: Vec<Box<dyn Module>> = vec![Box::new(Liar(fake("fa")))];
    let e = step(&r, &env, "fa", opts())
        .expect_err("must refuse")
        .to_string();
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
    assert_eq!(
        phase(&env, "fa"),
        Some(Phase::Switched),
        "recorded before the tombstone"
    );
    let f = fake("fa");
    let r = reg(vec![f]);
    // A switched record over a source that is not a tombstone is stepped again from the import, not tombstoned as it is.
    let out = step(&r, &env, "fa", opts()).expect("repair");
    assert!(
        !out.iter().any(|l| l.contains("already switched")),
        "{out:?}"
    );
    assert!(out.iter().any(|l| l.contains("same answer")), "{out:?}");
    assert!(tombstone::is_tombstone(&env.machine.join("fa-src.json")));
    assert_eq!(phase(&env, "fa"), Some(Phase::Switched));
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
    assert_eq!(
        std::fs::read_to_string(&src).unwrap(),
        "{\"fa\":1,\"revoked\":[\"p\"]}"
    );
    let store = FileStore(env.machine.join("os-store"));
    assert_eq!(
        secrets_back::read_back(&store, "fake-secret")
            .unwrap()
            .as_ref()
            .map(|v| v.as_str()),
        Some("s3cret"),
        "the secret came back through the split path (limit 8, value 6: plain)"
    );
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
    let e = rollback(&r, &env, "fa", opts())
        .expect_err("live")
        .to_string();
    assert!(e.contains("live") && e.contains("--force"), "{e}");
    assert_eq!(phase(&env, "fa"), Some(Phase::Switched), "nothing changed");
    assert!(tombstone::is_tombstone(&env.machine.join("fa-src.json")));
    let out = rollback(
        &r,
        &env,
        "fa",
        Opts {
            dry_run: false,
            force: true,
        },
    )
    .expect("forced");
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
    let e = step(&r, &env, "fa", opts())
        .expect_err("no step mid-rollback")
        .to_string();
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
    let e = step(&r, &env, "fb", opts())
        .expect_err("needs fa")
        .to_string();
    assert!(e.contains("needs fa switched"), "{e}");
    step(&r, &env, "fa", opts()).unwrap();
    step(&r, &env, "fb", opts()).unwrap();
    let e = rollback(&r, &env, "fa", opts())
        .expect_err("fb depends")
        .to_string();
    assert!(e.contains("fb depends on fa"), "{e}");
    rollback(&r, &env, "fb", opts()).unwrap();
    rollback(&r, &env, "fa", opts()).unwrap();
}

#[test]
fn may_serve_follows_the_switch_file_the_marker_and_the_tombstones_and_a_file_can_only_deny() {
    let (_t, env) = setup(&["fa"]);
    let f = fake("fa");
    let r = reg(vec![fake("fa")]);
    {
        let _c = crate::start_with(&env.layout, MIGRATIONS).unwrap();
    }
    let db = || Connection::open(&env.layout.db).unwrap();
    let write = |phase| {
        state::write(
            &env.layout,
            "fa",
            &Switch {
                phase,
                snapshot: None,
                at: 7,
            },
        )
        .unwrap();
    };
    // Nothing switched, no opt-in: no, quietly. The legacy side answers.
    assert_eq!(
        may_serve(&f, &env, &db(), false),
        Serve {
            ok: false,
            warning: None
        }
    );
    // Opt-in with no file: honoured only while the import invariant holds (nothing imported yet: it does not).
    let s = may_serve(&f, &env, &db(), true);
    assert!(
        !s.ok && s.warning.as_ref().is_some_and(|w| w.contains("difference")),
        "{s:?}"
    );
    // A forged "switched" has no marker in core.db: nothing is served, opt-in or not.
    write(Phase::Switched);
    for opt in [false, true] {
        let s = may_serve(&f, &env, &db(), opt);
        assert!(
            !s.ok
                && s.warning
                    .as_ref()
                    .is_some_and(|w| w.contains("no matching marker")),
            "{s:?}"
        );
    }
    // Corrupt, stepping and rolling-back files deny too.
    std::fs::write(state::path(&env.layout, "fa").unwrap(), "garbage").unwrap();
    assert!(!may_serve(&f, &env, &db(), true).ok);
    write(Phase::Stepping);
    assert!(!may_serve(&f, &env, &db(), true).ok);
    write(Phase::RollingBack);
    assert!(!may_serve(&f, &env, &db(), true).ok);
    state::clear(&env.layout, "fa").unwrap();
    // A real step: switched, marker, tombstone: served. A later write through the core does not unserve it.
    step(&r, &env, "fa", opts()).unwrap();
    assert_eq!(
        may_serve(&f, &env, &db(), false),
        Serve {
            ok: true,
            warning: None
        }
    );
    {
        let c = crate::start_with(&env.layout, MIGRATIONS).unwrap();
        c.conn()
            .execute("DELETE FROM meta WHERE key = 'fa-data'", [])
            .unwrap();
    }
    assert!(
        may_serve(&f, &env, &db(), true).ok,
        "the data is not compared once switched; the opt-in is ignored"
    );
    // A forged file with a different `at` than the marker holds denies; a restored older core.db (no marker) too.
    let real = state::read(&env.layout, "fa").unwrap().unwrap();
    state::write(
        &env.layout,
        "fa",
        &Switch {
            at: real.at + 1,
            ..real.clone()
        },
    )
    .unwrap();
    assert!(!may_serve(&f, &env, &db(), false).ok);
    state::write(&env.layout, "fa", &real).unwrap();
    // A source written again after the switch: still served, loudly.
    std::fs::write(env.machine.join("fa-src.json"), "{}").unwrap();
    let s = may_serve(&f, &env, &db(), false);
    assert!(
        s.ok && s.warning.as_ref().is_some_and(|w| w.contains("WARNING")),
        "{s:?}"
    );
}

#[test]
fn the_opt_in_guard_checks_again_only_when_the_old_store_changed() {
    let (_t, env) = setup(&["fa"]);
    let f = fake("fa");
    {
        let _c = crate::start_with(&env.layout, MIGRATIONS).unwrap();
    }
    let db = || Connection::open(&env.layout.db).unwrap();
    let g = Guard::default();
    assert!(!g.may_serve(&f, &env, &db(), true).ok, "nothing imported");
    // The same answer is served from the cache while nothing changed, even though core.db changed under it.
    {
        let c = crate::start_with(&env.layout, MIGRATIONS).unwrap();
        meta_put(
            &c,
            "fa-data",
            &std::fs::read_to_string(env.machine.join("fa-src.json")).unwrap(),
        );
    }
    assert!(
        !g.may_serve(&f, &env, &db(), true).ok,
        "cached until the sources change"
    );
    // The old store is written (kith-auth is still the writer in this mode): the guard looks again, and the data differ.
    std::fs::write(env.machine.join("fa-src.json"), "{\"fa\":2,\"revoked\":1}").unwrap();
    assert!(!g.may_serve(&f, &env, &db(), true).ok);
    {
        let c = crate::start_with(&env.layout, MIGRATIONS).unwrap();
        meta_put(&c, "fa-data", "{\"fa\":2,\"revoked\":1}");
    }
    // Same size, so only the mtime or inode can differ: rewrite through a rename to change the inode.
    std::fs::write(env.machine.join("fa-src.tmp"), "{\"fa\":2,\"revoked\":1}").unwrap();
    std::fs::rename(
        env.machine.join("fa-src.tmp"),
        env.machine.join("fa-src.json"),
    )
    .unwrap();
    assert!(
        g.may_serve(&f, &env, &db(), true).ok,
        "the sources changed, the check ran again and passes"
    );
}

#[test]
fn a_finished_switch_whose_record_was_lost_is_recorded_again_without_importing() {
    let (_t, env) = setup(&["fa"]);
    let r = reg(vec![fake("fa")]);
    step(&r, &env, "fa", opts()).unwrap();
    let at = state::read(&env.layout, "fa").unwrap().unwrap().at;
    state::clear(&env.layout, "fa").unwrap();
    // The sources are tombstones: an import would fail. Stepping resumes the switch instead.
    let out = step(&r, &env, "fa", opts()).expect("resume");
    assert!(out[0].contains("recorded as switched again"), "{out:?}");
    assert_eq!(
        state::read(&env.layout, "fa").unwrap().unwrap().at,
        at,
        "the marker's at, so it matches"
    );
    let db = Connection::open(&env.layout.db).unwrap();
    assert!(may_serve(&fake("fa"), &env, &db, false).ok);
    // A tombstone without the marker (a restored core.db) is not resumed.
    state::clear(&env.layout, "fa").unwrap();
    {
        let mut c = crate::start_with(&env.layout, MIGRATIONS).unwrap();
        marker::clear(&mut c, "fa").unwrap();
    }
    assert!(step(&r, &env, "fa", opts()).is_err());
}

#[test]
fn the_row_answers_ok_and_warning_and_knows_no_module_it_does_not_have() {
    let (_t, env) = setup(&["fa"]);
    let r = reg(vec![fake("fa")]);
    step(&r, &env, "fa", opts()).unwrap();
    let db = Connection::open(&env.layout.db).unwrap();
    let g = Guard::default();
    let v = serve_json(&r, &env, &db, "fa", false, &g).unwrap();
    assert_eq!(v, serde_json::json!({"ok": true, "warning": null}));
    assert!(serve_json(&r, &env, &db, "nope", false, &g).is_err());
    assert!(serve_json(&[], &env, &db, "fa", false, &g).is_err());
}

#[test]
fn check_on_a_switched_module_says_the_core_is_the_only_store() {
    let (_t, env) = setup(&["fa"]);
    let r = reg(vec![fake("fa")]);
    step(&r, &env, "fa", opts()).unwrap();
    assert_eq!(
        check(&r, &env, "fa").unwrap(),
        vec!["fa: switched: the core is the only store"]
    );
}

#[test]
fn the_check_runs_while_a_core_holds_the_lock() {
    let (_t, env) = setup(&["fa"]);
    let r = reg(vec![fake("fa")]);
    step(&r, &env, "fa", opts()).unwrap();
    std::fs::remove_file(state::path(&env.layout, "fa").unwrap()).unwrap();
    let _daemon = crate::start_with(&env.layout, MIGRATIONS).unwrap();
    assert!(
        step(&r, &env, "fa", opts()).is_err(),
        "a step needs the lock"
    );
    let out = check(&r, &env, "fa").expect("a read-only check does not");
    assert_eq!(out, vec!["fa: same answer"]);
}

#[test]
fn a_damaged_switch_file_counts_as_switched_and_names_are_checked() {
    let (_t, env) = setup(&["fa"]);
    let r = reg(vec![fake("fa")]);
    std::fs::create_dir_all(state::dir(&env.layout)).unwrap();
    std::fs::write(state::path(&env.layout, "fa").unwrap(), "garbage").unwrap();
    assert!(step(&r, &env, "fa", opts()).is_err());
    assert!(state::path(&env.layout, "../x").is_err());
    assert!(
        step(&r, &env, "nope", opts())
            .unwrap_err()
            .to_string()
            .contains("no module")
    );
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
    assert!(run_with(&r, &env, &a(&["check", "fa"])).unwrap()[0].contains("only store"));
    // The real registry: the S1 modules, none of them switched.
    assert_eq!(
        run(
            &crate::tests::paths(&env.machine),
            &env.machine,
            &a(&["status"])
        )
        .unwrap(),
        vec![
            "accounts: not switched",
            "transcripts: not switched",
            "claims: not switched",
            "schedules: not switched",
            "authority: not switched"
        ]
    );
}

#[test]
fn the_source_lock_is_held_from_the_import_through_the_tombstone_and_let_go_after() {
    let (_t, env) = setup(&["fa"]);
    let f = Fake {
        lock: true,
        ..fake("fa")
    };
    // `reg` boxes the fake; keep a second handle to read what it saw.
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    struct Spy(
        Fake,
        std::rc::Rc<std::cell::RefCell<Vec<(&'static str, bool)>>>,
    );
    impl Module for Spy {
        fn name(&self) -> &'static str {
            self.0.name()
        }
        fn tombstones(&self, env: &Env) -> Vec<PathBuf> {
            let t = self.0.tombstones(env);
            self.1
                .borrow_mut()
                .extend(self.0.seen.borrow_mut().drain(..));
            t
        }
        fn import(&self, env: &Env, core: Option<&mut Core>, o: Opts) -> Result<Vec<String>> {
            self.0.import(env, core, o)
        }
        fn same_answer(&self, env: &Env, db: &Connection) -> Result<Vec<String>> {
            self.0.same_answer(env, db)
        }
        fn export_back(&self, env: &Env, core: &mut Core) -> Result<Vec<String>> {
            self.0.export_back(env, core)
        }
        fn source_lock(&self, env: &Env) -> Result<Option<SourceLock>> {
            self.0.source_lock(env)
        }
    }
    let r: Vec<Box<dyn Module>> = vec![Box::new(Spy(f, seen.clone()))];
    step(&r, &env, "fa", opts()).expect("step");
    let seen = seen.borrow();
    for at in ["import", "same_answer", "tombstones"] {
        assert!(
            seen.iter().any(|(a, held)| *a == at && *held),
            "{at} ran without the lock: {seen:?}"
        );
    }
    assert!(seen.iter().all(|(_, held)| *held), "{seen:?}");
    // And it is let go when the step ends.
    use fs2::FileExt as _;
    let f = std::fs::File::open(env.machine.join("fake.lock")).expect("lock file");
    f.try_lock_exclusive().expect("the step let the lock go");
}

#[test]
fn a_refused_step_leaves_no_snapshot_record_marker_or_tombstone_and_refuses_again() {
    let (_t, env) = setup(&["fa"]);
    let r = reg(vec![Fake {
        refuse: Some("the readers are not in the core"),
        lock: true,
        ..fake("fa")
    }]);
    for _ in 0..2 {
        for dry_run in [false, true] {
            let e = step(
                &r,
                &env,
                "fa",
                Opts {
                    dry_run,
                    ..Opts::default()
                },
            )
            .unwrap_err();
            assert!(e.to_string().contains("the readers are not in the core"));
        }
    }
    assert_eq!(phase(&env, "fa"), None);
    assert!(!tombstone::is_tombstone(&env.machine.join("fa-src.json")));
    assert!(!env.layout.db.exists(), "no core.db was opened or made");
    assert!(!env.machine.join("fake.lock").exists(), "no lock was taken");
}

#[test]
fn a_damaged_record_is_recorded_again_when_the_tombstones_and_the_marker_agree() {
    let (_t, env) = setup(&["fa"]);
    let r = reg(vec![fake("fa")]);
    step(&r, &env, "fa", opts()).unwrap();
    let at = state::read(&env.layout, "fa").unwrap().unwrap().at;
    std::fs::write(state::path(&env.layout, "fa").unwrap(), "garbage").unwrap();
    // A dry run still reports the damage.
    let dry = Opts {
        dry_run: true,
        ..opts()
    };
    assert!(step(&r, &env, "fa", dry).is_err());
    let out = step(&r, &env, "fa", opts()).expect("repaired");
    assert!(out[0].contains("recorded as switched again"), "{out:?}");
    let s = state::read(&env.layout, "fa").unwrap().unwrap();
    assert_eq!((s.phase, s.at), (Phase::Switched, at));
}

#[test]
fn a_damaged_record_with_no_marker_or_a_real_source_refuses_with_the_repair_line() {
    let (_t, env) = setup(&["fa"]);
    let r = reg(vec![fake("fa")]);
    step(&r, &env, "fa", opts()).unwrap();
    std::fs::write(state::path(&env.layout, "fa").unwrap(), "garbage").unwrap();
    // The marker is gone (a restored core.db).
    {
        let mut c = crate::start_with(&env.layout, MIGRATIONS).unwrap();
        marker::clear(&mut c, "fa").unwrap();
    }
    let e = step(&r, &env, "fa", opts()).unwrap_err().to_string();
    assert!(e.contains("damaged") && e.contains("Repair"), "{e}");
    assert!(
        e.contains("remove the record") && e.contains("restored from before the switch"),
        "{e}"
    );
    // The marker is back but a source is a real file.
    {
        let mut c = crate::start_with(&env.layout, MIGRATIONS).unwrap();
        marker::write(&mut c, "fa", 1, "", None).unwrap();
    }
    for p in fake("fa").tombstones(&env) {
        std::fs::write(&p, "real").unwrap();
    }
    let e = step(&r, &env, "fa", opts()).unwrap_err().to_string();
    assert!(e.contains("Repair"), "{e}");
    assert_eq!(
        std::fs::read_to_string(state::path(&env.layout, "fa").unwrap()).unwrap(),
        "garbage",
        "nothing was written"
    );
}
