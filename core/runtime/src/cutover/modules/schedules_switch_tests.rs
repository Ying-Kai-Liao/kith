#![allow(clippy::unwrap_used)]
//! The schedules switch's locks and anchors (M4-5 handover: MUST 2, H1, H2) on the same world as `schedules_tests`.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use rusqlite::Connection;
use serde_json::{Value, json};

use super::schedules::Schedules;
use super::schedules_tests::{
    Lifted, World, cron, find, host_runs, opts, routine, world, write_state,
};
use crate::cutover::{Env, Module, Opts, SourceLock, rollback, state, step, tombstone};
use crate::db::Core;
use crate::secrets::keystore::{KeyStore, MemoryKeyStore};

/// The real module with two test seams: the early refusal can be skipped (a host that starts after it), and a hook runs
/// just before the tombstone check (a writer that did not take the lock).
struct Seamed {
    inner: Lifted,
    skip_precheck: bool,
    before_check: Box<dyn Fn(&Env)>,
}

impl Module for Seamed {
    fn name(&self) -> &'static str {
        self.inner.name()
    }
    fn tombstones(&self, env: &Env) -> Vec<PathBuf> {
        self.inner.tombstones(env)
    }
    fn import(
        &self,
        env: &Env,
        core: Option<&mut Core>,
        o: Opts,
    ) -> crate::error::Result<Vec<String>> {
        self.inner.import(env, core, o)
    }
    fn same_answer(&self, env: &Env, db: &Connection) -> crate::error::Result<Vec<String>> {
        self.inner.same_answer(env, db)
    }
    fn export_back(&self, env: &Env, core: &mut Core) -> crate::error::Result<Vec<String>> {
        self.inner.export_back(env, core)
    }
    fn restore_secrets(&self, env: &Env, core: &mut Core) -> crate::error::Result<Vec<String>> {
        self.inner.restore_secrets(env, core)
    }
    fn live(&self, env: &Env, core: &Core) -> crate::error::Result<Vec<String>> {
        self.inner.live(env, core)
    }
    fn guards_claims(&self) -> bool {
        self.inner.guards_claims()
    }
    fn depends_on(&self) -> &'static [&'static str] {
        self.inner.depends_on()
    }
    fn step_refusal(&self, env: &Env) -> Option<String> {
        if self.skip_precheck {
            None
        } else {
            self.inner.step_refusal(env)
        }
    }
    fn rollback_refusal(&self, env: &Env) -> Option<String> {
        if self.skip_precheck {
            None
        } else {
            self.inner.rollback_refusal(env)
        }
    }
    fn source_lock(&self, env: &Env) -> crate::error::Result<Option<SourceLock>> {
        self.inner.source_lock(env)
    }
    fn rollback_lock(&self, env: &Env) -> crate::error::Result<Option<SourceLock>> {
        self.inner.rollback_lock(env)
    }
    fn before_tombstone(&self, env: &Env) -> crate::error::Result<()> {
        (self.before_check)(env);
        self.inner.before_tombstone(env)
    }
}

fn seamed(
    w: &World,
    skip_precheck: bool,
    tick_wait: Duration,
    before_check: impl Fn(&Env) + 'static,
) -> Vec<Box<dyn Module>> {
    let store: MemoryKeyStore = w.store.clone();
    let keys: std::sync::Arc<dyn Fn() -> Box<dyn KeyStore>> =
        std::sync::Arc::new(move || -> Box<dyn KeyStore> { Box::new(store.clone()) });
    vec![Box::new(Seamed {
        inner: Lifted(Schedules::with_keys(keys).tick_wait(tick_wait)),
        skip_precheck,
        before_check: Box::new(before_check),
    })]
}

fn try_lock(path: &std::path::Path) -> bool {
    use fs2::FileExt as _;
    let f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    f.try_lock_exclusive().is_ok()
}

#[test]
fn a_host_lock_held_when_the_step_takes_its_lock_refuses_and_writes_nothing() {
    let w = world(false);
    let before = std::fs::read(w.mel_jobs()).unwrap();
    let _host = host_runs(&w);
    // The early refusal is skipped: this is the host that started after it was checked.
    let reg = seamed(&w, true, Duration::from_millis(100), |_| {});
    let e = step(&reg, &w.env, "schedules", opts())
        .unwrap_err()
        .to_string();
    assert!(e.contains("the host is running"), "{e}");
    assert!(state::read(&w.env.layout, "schedules").unwrap().is_none());
    assert_eq!(std::fs::read(w.mel_jobs()).unwrap(), before);
    assert!(!tombstone::is_tombstone(&w.state_file()));
}

#[test]
fn no_host_can_start_between_the_refusal_and_the_tombstone_and_it_can_after() {
    let w = world(false);
    let lock = w.serve.parent().unwrap().join("host.lock");
    assert!(
        !lock.exists(),
        "the step makes host.lock when it is missing"
    );
    let during = Rc::new(Cell::new(None));
    let seen = Rc::clone(&during);
    let lock2 = lock.clone();
    let reg = seamed(&w, false, Duration::from_secs(1), move |_| {
        seen.set(Some(try_lock(&lock2)));
    });
    step(&reg, &w.env, "schedules", opts()).unwrap();
    assert_eq!(
        during.get(),
        Some(false),
        "the host's try_lock_exclusive failed during the step"
    );
    assert!(try_lock(&lock), "the lock is let go when the step ends");
}

#[test]
fn a_rollback_holds_host_lock_too() {
    let w = world(false);
    let reg = seamed(&w, false, Duration::from_secs(1), |_| {});
    step(&reg, &w.env, "schedules", opts()).unwrap();
    let _host = host_runs(&w);
    let reg = seamed(&w, true, Duration::from_millis(100), |_| {});
    let e = rollback(&reg, &w.env, "schedules", opts())
        .unwrap_err()
        .to_string();
    assert!(e.contains("host is running"), "{e}");
    assert!(
        tombstone::is_tombstone(&w.state_file()),
        "nothing was written back"
    );
}

#[test]
fn a_tick_lock_held_by_another_writer_makes_the_step_wait_then_refuse() {
    use fs2::FileExt as _;
    let w = world(false);
    let before = std::fs::read(w.mel_jobs()).unwrap();
    std::fs::create_dir_all(w.env.machine.join("sched")).unwrap();
    let tick = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(w.env.machine.join("sched/tick.lock"))
        .unwrap();
    tick.try_lock_exclusive().unwrap();
    let reg = seamed(&w, false, Duration::from_millis(150), |_| {});
    let t = std::time::Instant::now();
    let e = step(&reg, &w.env, "schedules", opts())
        .unwrap_err()
        .to_string();
    assert!(e.contains("tick.lock") && e.contains("held"), "{e}");
    assert!(t.elapsed() >= Duration::from_millis(150), "it waited");
    assert!(state::read(&w.env.layout, "schedules").unwrap().is_none());
    assert_eq!(std::fs::read(w.mel_jobs()).unwrap(), before);
    // The host.lock taken first was let go with the refusal.
    assert!(try_lock(&w.serve.parent().unwrap().join("host.lock")));
    drop(tick);
    step(
        &seamed(&w, false, Duration::from_secs(1), |_| {}),
        &w.env,
        "schedules",
        opts(),
    )
    .unwrap();
}

#[test]
fn a_state_file_that_changed_after_the_import_is_not_tombstoned() {
    let w = world(false);
    write_state(&w, &json!({"tick": "2026-11-04T08:00", "last": {}}));
    let file = w.state_file();
    let reg = seamed(&w, false, Duration::from_secs(1), move |_| {
        std::fs::write(
            &file,
            json!({"tick": "2026-11-04T08:01", "last": {}}).to_string(),
        )
        .unwrap();
    });
    let e = step(&reg, &w.env, "schedules", opts())
        .unwrap_err()
        .to_string();
    assert!(e.contains("changed after the import"), "{e}");
    assert!(
        !tombstone::is_tombstone(&w.state_file()),
        "the file was not tombstoned"
    );
    // A rerun imports the new file and switches.
    step(
        &seamed(&w, false, Duration::from_secs(1), |_| {}),
        &w.env,
        "schedules",
        opts(),
    )
    .unwrap();
    assert!(tombstone::is_tombstone(&w.state_file()));
}

/// The world with routines whose next runs the host stored: a daily cron, an interval, a fired once, an unfired
/// webhook-only routine and a disabled one.
fn with_next_runs(w: &World, once_at: i64, now: i64) {
    let mut v = w.serve_routines();
    let rs = v["routines"].as_array_mut().unwrap();
    rs.clear();
    let mut daily = routine("daily", "b1", true, &json!([cron()]), "");
    daily["nextRuns"] = json!([now + 3_600_000]);
    let mut every = routine(
        "every",
        "b1",
        true,
        &json!([{"type": "interval", "seconds": 3600}]),
        "",
    );
    every["nextRuns"] = json!([now + 1_800_000]);
    let mut once = routine(
        "once",
        "b1",
        true,
        &json!([{"type": "once", "at": once_at}]),
        "",
    );
    once["nextRuns"] = json!([null]);
    let mut paused = routine("paused", "b1", false, &json!([cron()]), "");
    paused["nextRuns"] = json!([now + 1000]);
    let multi = {
        let mut m = routine(
            "multi",
            "b1",
            true,
            &json!([cron(), {"type": "interval", "seconds": 600}]),
            "",
        );
        m["nextRuns"] = json!([now + 7_200_000, null]);
        m
    };
    rs.extend([daily, every, once, paused, multi]);
    w.set_serve_routines(&v);
}

#[test]
fn the_import_writes_each_triggers_next_run_as_its_anchor_and_decides_a_fired_once() {
    let w = world(false);
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    let once_at = now - 3_600_000;
    with_next_runs(&w, once_at, now);
    let reg = seamed(&w, false, Duration::from_secs(1), |_| {});
    let out = step(&reg, &w.env, "schedules", opts()).unwrap().join("\n");
    assert!(out.contains("already fired in the host"), "{out}");
    let jobs: Vec<Value> = w.jobs();
    let anchor = |id: &str| {
        find(&jobs, id)["routine"]
            .get("anchor")
            .and_then(Value::as_i64)
    };
    // A cron slot is a whole minute: the host's next run carries the sub-minute part of the clock it counted from.
    assert_eq!(
        anchor("daily"),
        Some((now + 3_600_000).div_euclid(60_000) * 60_000)
    );
    assert_eq!(
        find(&jobs, "daily")["routine"]["anchorTrigger"],
        find(&jobs, "daily")["trigger"],
        "the anchor names the trigger it was read for"
    );
    assert_eq!(anchor("every"), Some(now + 1_800_000));
    assert_eq!(anchor("once"), None, "a fired once has no next run");
    assert_eq!(anchor("paused"), None, "a paused routine carries none");
    assert_eq!(
        anchor("multi-1"),
        Some((now + 7_200_000).div_euclid(60_000) * 60_000),
        "one anchor per trigger"
    );
    assert_eq!(
        anchor("multi-2"),
        None,
        "a trigger with no next run counts as before"
    );
    let c = w.core();
    let slot = crate::schedules::slot_of_ms(crate::schedules::anchor::slot_ms(once_at)).unwrap();
    let (n, outcome): (i64, String) = c
        .conn()
        .query_row(
            "SELECT count(*), max(outcome) FROM sched_fires WHERE agent = 'mel' AND job = 'once' AND slot = ?1",
            [slot],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(n, 1);
    assert!(outcome.starts_with("skipped:"), "{outcome}");
    assert!(
        crate::schedules::anchor::unused(c.conn(), "mel", "daily", now + 3_600_000).unwrap(),
        "nothing has reached the anchor yet"
    );
    // A re-import of an unchanged routine writes nothing.
    let again = crate::cutover::check(&reg, &w.env, "schedules").unwrap();
    assert!(!again.join("\n").contains("to update"), "{again:?}");
}

#[test]
fn a_run_kith_sched_left_going_is_imported_with_its_pid_and_start_stamp() {
    let w = world(false);
    write_state(
        &w,
        &json!({
            "tick": "2026-11-04T08:00",
            "last": {"mel/digest": {"at": "2026-11-04T08:00", "pid": 4242, "procStart": "e1700000000000", "log": "/x.log"}},
        }),
    );
    step(
        &seamed(&w, false, Duration::from_secs(1), |_| {}),
        &w.env,
        "schedules",
        opts(),
    )
    .unwrap();
    let (pid, stamp): (i64, Option<String>) = w
        .core()
        .conn()
        .query_row(
            "SELECT pid, proc_start FROM sched_runs WHERE agent = 'mel' AND job = 'digest'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((pid, stamp.as_deref()), (4242, Some("e1700000000000")));
}

#[test]
fn the_checks_compare_leaves_the_anchor_out() {
    let job = |anchor: i64| {
        json!({"id": "d", "trigger": {"type": "interval", "every": 60},
               "routine": {"id": "d", "bot": "b1", "trigger": 1, "anchor": anchor,
                           "anchorTrigger": {"type": "interval", "every": 60}}})
    };
    assert_ne!(job(1), job(2));
    assert_eq!(
        super::schedules::without_anchor(&job(1)),
        super::schedules::without_anchor(&job(2))
    );
    assert!(
        super::schedules::without_anchor(&job(1))["routine"]
            .get("anchor")
            .is_none()
    );
}

#[test]
fn the_host_lock_is_where_the_host_puts_it_not_beside_serve_db() {
    let mut w = world(false);
    let home = w.env.home.clone();
    w.env.serve_db = None;
    let var = |k: &str| match k {
        "KITH_SERVE_DB" => Some("/elsewhere/serve.db".into()),
        "HOME" => Some(home.clone().into_os_string()),
        _ => None,
    };
    let dir = super::schedules::host_dir_with(&w.env, &var);
    assert_eq!(dir, home.join(".kith").join("serve"));
    let var = |k: &str| (k == "KITH_SERVE_HOME").then(|| "/data/kith-serve".into());
    assert_eq!(
        super::schedules::host_dir_with(&w.env, &var),
        PathBuf::from("/data/kith-serve")
    );
}
