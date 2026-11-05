#![allow(clippy::unwrap_used)]
//! The `schedules` module (M4-5a) on a real core.db, a serve.db shaped like the host's (a `kv` row `routines.v1` and the
//! `bots` table), an agents folder, and an in-memory key store standing in for this computer's.

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::{Value, json};

use crate::cutover::modules::schedules::Schedules;
use crate::cutover::{Env, Module, Opts, check, registry, rollback, state, step, tombstone};
use crate::db::{Core, MIGRATIONS};
use crate::secrets::Envelope;
use crate::secrets::keystore::{KeyStore, MemoryKeyStore};
use crate::tests::machine;

/// A key that is not a real one; no test output may ever contain it.
const SECRET: &str = "whk-0123456789-not-a-real-key";

fn opts() -> Opts {
    Opts::default()
}

/// The module with a key store of the test's own (the real one is the OS keychain); every hook is the module's.
struct Lifted(Schedules);

impl Module for Lifted {
    fn name(&self) -> &'static str {
        self.0.name()
    }
    fn tombstones(&self, env: &Env) -> Vec<PathBuf> {
        self.0.tombstones(env)
    }
    fn import(
        &self,
        env: &Env,
        core: Option<&mut Core>,
        o: Opts,
    ) -> crate::error::Result<Vec<String>> {
        self.0.import(env, core, o)
    }
    fn same_answer(&self, env: &Env, db: &Connection) -> crate::error::Result<Vec<String>> {
        self.0.same_answer(env, db)
    }
    fn export_back(&self, env: &Env, core: &mut Core) -> crate::error::Result<Vec<String>> {
        self.0.export_back(env, core)
    }
    fn restore_secrets(&self, env: &Env, core: &mut Core) -> crate::error::Result<Vec<String>> {
        self.0.restore_secrets(env, core)
    }
    fn live(&self, env: &Env, core: &Core) -> crate::error::Result<Vec<String>> {
        self.0.live(env, core)
    }
    fn digest(&self, env: &Env, db: &Connection) -> crate::error::Result<String> {
        self.0.digest(env, db)
    }
    fn guards_claims(&self) -> bool {
        self.0.guards_claims()
    }
    fn step_refusal(&self, env: &Env) -> Option<String> {
        self.0.step_refusal(env)
    }
    fn dry_run_despite_refusal(&self) -> bool {
        self.0.dry_run_despite_refusal()
    }
    fn check_notes(&self, env: &Env) -> Vec<String> {
        self.0.check_notes(env)
    }
}

struct World {
    _t: tempfile::TempDir,
    env: Env,
    store: MemoryKeyStore,
    serve: PathBuf,
}

impl World {
    fn reg(&self) -> Vec<Box<dyn Module>> {
        let store = self.store.clone();
        vec![Box::new(Lifted(Schedules::with_keys(std::sync::Arc::new(
            move || -> Box<dyn KeyStore> { Box::new(store.clone()) },
        ))))]
    }

    fn core(&self) -> Core {
        crate::start_with(&self.env.layout, MIGRATIONS).unwrap()
    }

    fn mel_jobs(&self) -> PathBuf {
        self.env.machine.join("agents/mel/jobs.json")
    }

    fn jobs(&self) -> Vec<Value> {
        serde_json::from_slice(&std::fs::read(self.mel_jobs()).unwrap()).unwrap()
    }

    fn state_file(&self) -> PathBuf {
        self.env.machine.join("sched/state.json")
    }

    fn mac(&self) -> String {
        std::fs::read_to_string(self.env.machine.join("mac-id"))
            .unwrap()
            .trim()
            .to_owned()
    }

    /// The secret under `name`, read through a second envelope over the same key store.
    fn secret(&self, name: &str) -> Option<String> {
        let c = Connection::open(&self.env.layout.db).unwrap();
        let mut e = Envelope::with_store(Box::new(self.store.clone()));
        e.get(&c, name)
            .unwrap()
            .map(|b| String::from_utf8(b.to_vec()).unwrap())
    }

    fn serve_routines(&self) -> Value {
        let c = Connection::open(&self.serve).unwrap();
        let raw: String = c
            .query_row("SELECT v FROM kv WHERE k = 'routines.v1'", [], |r| r.get(0))
            .unwrap();
        serde_json::from_str(&raw).unwrap()
    }

    fn set_serve_routines(&self, v: &Value) {
        Connection::open(&self.serve)
            .unwrap()
            .execute(
                "UPDATE kv SET v = ?1 WHERE k = 'routines.v1'",
                [v.to_string()],
            )
            .unwrap();
    }
}

fn routine(id: &str, bot: &str, enabled: bool, triggers: &Value, key: &str) -> Value {
    json!({
        "id": id, "botId": bot, "name": format!("name of {id}"), "instruction": format!("do {id}"),
        "triggers": triggers, "enabled": enabled, "createdAt": 1, "updatedAt": 2, "nextRuns": [],
        "webhookKey": key, "deleted": false, "timeoutSeconds": 600, "lastError": null,
    })
}

fn cron() -> Value {
    json!({"type": "cron", "expression": "0 8 * * *", "timeZone": "Europe/Paris"})
}

/// A Mac with the agent `mel`, a serve.db whose bot `b1` is mel's, and the routines `r1` (one cron trigger, enabled) and
/// `r2` (a webhook and an interval, disabled). With `extra`: `b2` (a host-native bot) with the routine `r3`.
fn world(extra: bool) -> World {
    let (t, m) = machine();
    let mut p = crate::tests::paths(&m);
    p.state = m.join("state");
    p.home = m.join("home");
    p.agents = m.join("agents");
    std::fs::create_dir_all(p.agents.join("mel/role")).unwrap();
    std::fs::write(p.agents.join("mel/role/agent.md"), "---\nname: mel\n---\n").unwrap();
    std::fs::write(
        p.agents.join("mel/jobs.json"),
        serde_json::to_string_pretty(&json!([
            {"id": "digest", "cron": "0 8 * * *", "task": "send the digest"},
        ]))
        .unwrap(),
    )
    .unwrap();
    let serve = m.join("serve/serve.db");
    std::fs::create_dir_all(serve.parent().unwrap()).unwrap();
    let c = Connection::open(&serve).unwrap();
    c.execute_batch(
        "CREATE TABLE kv(k TEXT PRIMARY KEY, v TEXT NOT NULL);
         CREATE TABLE bots(id TEXT PRIMARY KEY, rev INTEGER NOT NULL, deleted INTEGER NOT NULL DEFAULT 0,
                           config TEXT NOT NULL, session_id TEXT, read_rev INTEGER NOT NULL DEFAULT 0);",
    )
    .unwrap();
    let bot = |id: &str, config: &Value| {
        c.execute(
            "INSERT INTO bots (id, rev, config) VALUES (?1, 1, ?2)",
            rusqlite::params![id, config.to_string()],
        )
        .unwrap();
    };
    bot("b1", &json!({"id": "b1", "hqAgent": "mel"}));
    bot("b2", &json!({"id": "b2"}));
    let mut routines = vec![
        routine("r1", "b1", true, &json!([cron()]), ""),
        routine(
            "r2",
            "b1",
            false,
            &json!([{"type": "webhook"}, {"type": "interval", "seconds": 3600}]),
            SECRET,
        ),
    ];
    if extra {
        routines.push(routine("r3", "b2", true, &json!([cron()]), ""));
    }
    c.execute(
        "INSERT INTO kv (k, v) VALUES ('routines.v1', ?1)",
        [json!({"routines": routines, "runs": []}).to_string()],
    )
    .unwrap();
    drop(c);
    let mut env = Env::with_paths(&p);
    env.serve_db = Some(serve.clone());
    World {
        _t: t,
        env,
        store: MemoryKeyStore::default(),
        serve,
    }
}

fn find<'a>(jobs: &'a [Value], id: &str) -> &'a Value {
    jobs.iter()
        .find(|j| j["id"] == id)
        .unwrap_or_else(|| panic!("no job {id}: {jobs:?}"))
}

fn kernel_json(t: &kith_kernel::schedule::Trigger) -> Value {
    serde_json::to_value(t).unwrap()
}

fn write_state(w: &World, v: &Value) {
    std::fs::create_dir_all(w.state_file().parent().unwrap()).unwrap();
    std::fs::write(w.state_file(), v.to_string()).unwrap();
}

// --- the refusal, the dry run and the check ------------------------------------------------------------------------------

/// A running host: `host.lock` in serve.db's folder, held with the same advisory lock the host takes.
fn host_runs(w: &World) -> std::fs::File {
    use fs2::FileExt as _;
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(w.serve.parent().unwrap().join("host.lock"))
        .unwrap();
    f.try_lock_exclusive().unwrap();
    f
}

#[test]
fn the_step_refuses_while_the_host_runs_but_the_dry_run_and_check_work() {
    let w = world(false);
    let before = std::fs::read(w.mel_jobs()).unwrap();
    let host = host_runs(&w);
    // A real step: refused, one line, and nothing was made.
    let e = step(&registry(), &w.env, "schedules", opts())
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("stop the host") && e.contains("--dry-run"),
        "{e}"
    );
    assert_eq!(e.lines().count(), 1, "{e}");
    assert!(state::read(&w.env.layout, "schedules").unwrap().is_none());
    assert!(!w.env.layout.db.exists(), "no core.db was opened or made");
    // The dry run goes through, says what it would do, and writes nothing at all.
    let out = step(
        &registry(),
        &w.env,
        "schedules",
        Opts {
            dry_run: true,
            ..opts()
        },
    )
    .unwrap();
    let text = out.join("\n");
    assert!(
        text.contains("3 job(s): 3 to add") && text.contains("dry run"),
        "{text}"
    );
    assert!(!text.contains(SECRET));
    assert_eq!(std::fs::read(w.mel_jobs()).unwrap(), before);
    assert!(!w.env.machine.join("mac-id").exists());
    assert!(!w.env.machine.join("sched").exists());
    assert!(!w.env.layout.db.exists());
    // `check` says the core does not answer the same yet, and (with a core) still writes nothing.
    crate::tests::open_all(&w.env.layout);
    let out = check(&registry(), &w.env, "schedules").unwrap();
    assert!(out[0].contains("difference"), "{out:?}");
    assert!(
        out.iter().any(|l| l.contains("a routine, not a job yet")),
        "{out:?}"
    );
    assert_eq!(std::fs::read(w.mel_jobs()).unwrap(), before);
    assert!(!w.env.machine.join("mac-id").exists());
    // The host stops (its lock goes with the process; the file stays): the step is allowed, and needs no `--force`.
    drop(host);
    step(&w.reg(), &w.env, "schedules", opts()).unwrap();
    assert!(state::read(&w.env.layout, "schedules").unwrap().is_some());
}

#[test]
fn a_rollback_refuses_while_the_host_runs_and_goes_through_once_it_stops() {
    let w = world(false);
    let reg = w.reg();
    step(&reg, &w.env, "schedules", opts()).unwrap();
    let host = host_runs(&w);
    let before = w.serve_routines();
    let err = rollback(&reg, &w.env, "schedules", opts())
        .unwrap_err()
        .to_string();
    assert!(err.contains("stop the host first"), "{err}");
    assert_eq!(w.serve_routines(), before, "serve.db was not touched");
    assert!(state::read(&w.env.layout, "schedules").unwrap().is_some());
    assert!(tombstone::is_tombstone(&w.state_file()));
    // `--force` does not get past a running host: it would overwrite what the rollback puts back.
    let err = rollback(
        &reg,
        &w.env,
        "schedules",
        Opts {
            force: true,
            ..opts()
        },
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("stop the host first"), "{err}");
    assert_eq!(w.serve_routines(), before, "serve.db was not touched");
    assert!(state::read(&w.env.layout, "schedules").unwrap().is_some());
    drop(host);
    let out = rollback(&reg, &w.env, "schedules", opts())
        .unwrap()
        .join("\n");
    assert!(
        out.contains("start the host") && !out.contains("restart"),
        "{out}"
    );
    assert!(state::read(&w.env.layout, "schedules").unwrap().is_none());
}

#[test]
fn a_stale_lock_file_without_a_host_does_not_count_as_running() {
    let w = world(false);
    std::fs::write(w.serve.parent().unwrap().join("host.lock"), "").unwrap();
    step(&w.reg(), &w.env, "schedules", opts()).unwrap();
}

#[cfg(unix)]
#[test]
fn a_lock_file_that_cannot_be_opened_refuses_without_saying_stop_the_host() {
    use std::os::unix::fs::PermissionsExt as _;
    let w = world(false);
    let lock = w.serve.parent().unwrap().join("host.lock");
    std::fs::write(&lock, "").unwrap();
    std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o000)).unwrap();
    // A superuser can still open it: nothing to test then.
    if std::fs::File::open(&lock).is_ok() {
        return;
    }
    let e = step(&registry(), &w.env, "schedules", opts())
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("host.lock cannot be checked") && !e.contains("stop the host"),
        "{e}"
    );
    std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o644)).unwrap();
}

#[test]
fn check_names_routine_jobs_another_mac_made_and_never_counts_them_as_a_difference() {
    let w = world(false);
    let reg = w.reg();
    step(&reg, &w.env, "schedules", opts()).unwrap();
    rollback(&reg, &w.env, "schedules", opts()).unwrap();
    // The Mac keeps its id; one job from a Mac with another id, and a hand-made job without `mac`.
    let mut jobs = w.jobs();
    jobs.push(json!({"id": "old-1", "task": "t", "mac": "ffffffffffffffffffffffffffffffff", "routine": {"id": "old-1"},
                      "trigger": {"type": "interval", "seconds": 3600}}));
    jobs.push(json!({"id": "hand", "task": "t", "routine": {"id": "hand"}, "cron": "0 8 * * *"}));
    std::fs::write(w.mel_jobs(), serde_json::to_string_pretty(&jobs).unwrap()).unwrap();
    crate::tests::open_all(&w.env.layout);
    let out = check(&reg, &w.env, "schedules").unwrap();
    let text = out.join("\n");
    assert!(
        text.contains("mel/old-1: made on another Mac (ffffffffffffffffffffffffffffffff)"),
        "{text}"
    );
    assert!(
        !text.contains("mel/hand") && !text.contains("mel/digest"),
        "{text}"
    );
    let db = Connection::open(&w.env.layout.db).unwrap();
    let diffs = reg[0].same_answer(&w.env, &db).unwrap().join("\n");
    assert!(!diffs.contains("another Mac"), "{diffs}");
}

// --- the import ----------------------------------------------------------------------------------------------------------

#[test]
fn a_step_turns_routines_into_jobs_and_keys_and_the_sched_state_into_a_tombstone() {
    let w = world(false);
    write_state(
        &w,
        &json!({
            "tick": "2026-11-04T08:00",
            "last": {"mel/digest": {"at": "2026-11-04T08:00", "pid": 42, "log": "/x.log", "why": "due 08:00"}},
            "pruned": 5,
        }),
    );
    let digest = w.jobs()[0].clone();
    let out = step(&w.reg(), &w.env, "schedules", opts()).unwrap();
    let text = out.join("\n");
    assert!(text.contains("same answer"), "{text}");
    assert!(!text.contains(SECRET), "a key is never printed");
    assert_eq!(
        state::read(&w.env.layout, "schedules")
            .unwrap()
            .unwrap()
            .phase,
        state::Phase::Switched
    );
    // The jobs: kith-sched's own job is untouched; the routines follow it.
    let jobs = w.jobs();
    let ids: Vec<&str> = jobs.iter().map(|j| j["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["digest", "r1", "r2-1", "r2-2"]);
    assert_eq!(jobs[0], digest);
    let mac = w.mac();
    let r1 = find(&jobs, "r1");
    assert_eq!(r1["task"], "do r1");
    assert_eq!(r1["mac"], mac.as_str());
    assert!(r1.get("paused").is_none() && r1.get("cron").is_none());
    assert_eq!(
        r1["trigger"],
        kernel_json(&kith_kernel::schedule::Trigger::Cron {
            expr: "0 8 * * *".into(),
            tz: kith_kernel::schedule::Zone::parse("Europe/Paris").unwrap(),
        })
    );
    assert_eq!(r1["routine"]["id"], "r1");
    assert_eq!(r1["routine"]["bot"], "b1");
    assert_eq!(r1["routine"]["timeoutSeconds"], 600);
    // A disabled routine is paused; each trigger of a multi-trigger routine is a job with a derived id.
    let (w1, w2) = (find(&jobs, "r2-1"), find(&jobs, "r2-2"));
    assert_eq!(w1["paused"], true);
    assert_eq!(w2["paused"], true);
    assert_eq!(
        w1["trigger"],
        kernel_json(&kith_kernel::schedule::Trigger::Webhook)
    );
    assert_eq!(
        w2["trigger"],
        kernel_json(&kith_kernel::schedule::Trigger::Interval { every: 3600 })
    );
    assert_eq!(w1["routine"]["trigger"], 1);
    assert_eq!(w2["routine"]["trigger"], 2);
    // The key sits where hooks.rs reads it, for the job that takes deliveries and for no other.
    let key = |job: &str| w.secret(&crate::schedules::hook_key_name("mel", job));
    assert_eq!(key("r2-1").as_deref(), Some(SECRET));
    assert_eq!(key("r2-2"), None);
    assert_eq!(key("r1"), None);
    // kith-sched's state is in the core and its file is a note.
    assert!(tombstone::is_tombstone(&w.state_file()));
    let c = Connection::open(&w.env.layout.db).unwrap();
    let n: i64 = c
        .query_row(
            "SELECT count(*) FROM sched_runs WHERE agent = 'mel' AND job = 'digest' AND mac = ?1",
            [&mac],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
    // serve.db is only ever read.
    assert_eq!(
        w.serve_routines()["routines"][1]["webhookKey"],
        SECRET,
        "the host still has its routines"
    );
}

#[test]
fn importing_again_changes_nothing_and_mints_no_new_key() {
    let w = world(false);
    let reg = w.reg();
    let m = &reg[0];
    let mut core = w.core();
    let first = m.import(&w.env, Some(&mut core), opts()).unwrap();
    assert!(first.join("\n").contains("3 added"), "{first:?}");
    let bytes = std::fs::read(w.mel_jobs()).unwrap();
    let name = crate::schedules::hook_key_name("mel", "r2-1");
    let stamp = |c: &Core| -> (i64, Vec<u8>) {
        c.conn()
            .query_row(
                "SELECT updated_at, ciphertext FROM secret_records WHERE name = ?1",
                [&name],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
    };
    let key_row = stamp(&core);
    let again = m.import(&w.env, Some(&mut core), opts()).unwrap();
    let text = again.join("\n");
    assert!(text.contains("0 added, 0 updated, 3 unchanged"), "{text}");
    assert!(!text.contains("webhook key stored"), "{text}");
    assert_eq!(std::fs::read(w.mel_jobs()).unwrap(), bytes);
    assert_eq!(stamp(&core), key_row, "the record was not written again");
    assert_eq!(
        m.same_answer(&w.env, core.conn()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_changed_routine_updates_its_own_job_and_a_deleted_one_takes_its_job_away() {
    let w = world(false);
    let reg = w.reg();
    let mut core = w.core();
    reg[0].import(&w.env, Some(&mut core), opts()).unwrap();
    // The host enables r2 and edits r1's instruction, and deletes nothing yet.
    let mut v = w.serve_routines();
    v["routines"][1]["enabled"] = json!(true);
    v["routines"][0]["instruction"] = json!("do r1 differently");
    w.set_serve_routines(&v);
    let diffs = reg[0].same_answer(&w.env, core.conn()).unwrap();
    assert!(
        diffs
            .iter()
            .any(|d| d.contains("r1") && d.contains("differs")),
        "{diffs:?}"
    );
    let out = reg[0]
        .import(&w.env, Some(&mut core), opts())
        .unwrap()
        .join("\n");
    assert!(out.contains("0 added, 3 updated, 0 unchanged"), "{out}");
    let jobs = w.jobs();
    assert_eq!(find(&jobs, "r1")["task"], "do r1 differently");
    assert!(find(&jobs, "r2-1").get("paused").is_none());
    // The routine is deleted on the host: its jobs go, kith-sched's own stays.
    let mut v = w.serve_routines();
    v["routines"].as_array_mut().unwrap().remove(1);
    w.set_serve_routines(&v);
    let diffs = reg[0].same_answer(&w.env, core.conn()).unwrap();
    assert!(
        diffs.iter().any(|d| d.contains("gone from serve.db")),
        "{diffs:?}"
    );
    let out = reg[0]
        .import(&w.env, Some(&mut core), opts())
        .unwrap()
        .join("\n");
    assert!(out.contains("2 removed"), "{out}");
    let ids: Vec<String> = w
        .jobs()
        .iter()
        .map(|j| j["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(ids, ["digest", "r1"]);
}

// --- what is refused -----------------------------------------------------------------------------------------------------

#[test]
fn a_routine_on_a_host_native_bot_is_refused_and_the_step_cannot_switch_with_it() {
    let w = world(true);
    let before_db = std::fs::read(&w.serve).unwrap();
    let err = step(&w.reg(), &w.env, "schedules", opts())
        .unwrap_err()
        .to_string();
    assert!(err.contains("NOT switched") && err.contains("r3"), "{err}");
    assert_eq!(
        state::read(&w.env.layout, "schedules")
            .unwrap()
            .unwrap()
            .phase,
        state::Phase::Stepping
    );
    assert!(!tombstone::is_tombstone(&w.state_file()));
    // The routines that can move did; the native one wrote nothing, anywhere.
    let ids: Vec<String> = w
        .jobs()
        .iter()
        .map(|j| j["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(ids, ["digest", "r1", "r2-1", "r2-2"]);
    assert_eq!(std::fs::read(&w.serve).unwrap(), before_db);
    let out = Lifted(Schedules::new())
        .import(
            &w.env,
            None,
            Opts {
                dry_run: true,
                ..opts()
            },
        )
        .unwrap()
        .join("\n");
    assert!(
        out.contains("make it a kith agent first") && out.contains("1 refused"),
        "{out}"
    );
    assert!(
        w.secret(&crate::schedules::hook_key_name("mel", "r3"))
            .is_none()
    );
}

#[test]
fn an_id_collision_refuses_that_routine_and_never_overwrites_the_job() {
    let w = world(false);
    // A job of the same id that is not r1's.
    let mine =
        json!({"id": "r1", "cron": "5 5 * * *", "task": "somebody's own job", "person": "sam"});
    std::fs::write(
        w.mel_jobs(),
        serde_json::to_string_pretty(&json!([mine])).unwrap(),
    )
    .unwrap();
    let mut core = w.core();
    let reg = w.reg();
    let out = reg[0]
        .import(&w.env, Some(&mut core), opts())
        .unwrap()
        .join("\n");
    assert!(
        out.contains("refused mel/r1") && out.contains("not this routine's"),
        "{out}"
    );
    let jobs = w.jobs();
    assert_eq!(find(&jobs, "r1"), &mine, "left exactly as it was");
    // The other routine was not held up by it.
    assert!(jobs.iter().any(|j| j["id"] == "r2-1"));
    let diffs = reg[0].same_answer(&w.env, core.conn()).unwrap();
    assert!(diffs.iter().any(|d| d.contains("mel/r1")), "{diffs:?}");
}

#[test]
fn a_job_imported_on_another_mac_is_left_to_that_mac() {
    let w = world(false);
    let reg = w.reg();
    let mut core = w.core();
    reg[0].import(&w.env, Some(&mut core), opts()).unwrap();
    // The synced jobs.json now says r1 belongs to another Mac (this one's serve.db has the same routine id).
    let mut jobs = w.jobs();
    for j in &mut jobs {
        if j["id"] == "r1" {
            j["mac"] = json!("ffffffffffffffffffffffffffffffff");
            j["task"] = json!("their copy");
        }
    }
    std::fs::write(w.mel_jobs(), serde_json::to_string_pretty(&jobs).unwrap()).unwrap();
    let out = reg[0]
        .import(&w.env, Some(&mut core), opts())
        .unwrap()
        .join("\n");
    assert!(out.contains("another Mac"), "{out}");
    assert_eq!(find(&w.jobs(), "r1")["task"], "their copy");
    assert_eq!(
        find(&w.jobs(), "r1")["mac"],
        "ffffffffffffffffffffffffffffffff"
    );
}

#[test]
fn a_jobs_file_that_cannot_be_read_is_left_alone_and_said() {
    let w = world(false);
    std::fs::write(w.mel_jobs(), "{not json").unwrap();
    let mut core = w.core();
    let reg = w.reg();
    let out = reg[0]
        .import(&w.env, Some(&mut core), opts())
        .unwrap()
        .join("\n");
    assert!(
        out.contains("refused mel") && out.contains("untouched"),
        "{out}"
    );
    assert_eq!(std::fs::read_to_string(w.mel_jobs()).unwrap(), "{not json");
    assert_ne!(
        reg[0].same_answer(&w.env, core.conn()).unwrap(),
        Vec::<String>::new()
    );
}

// --- the switch, the tombstone and the way back ---------------------------------------------------------------------------

#[test]
fn a_rollback_puts_keys_and_flags_back_removes_the_jobs_and_writes_the_state_again() {
    let w = world(false);
    write_state(
        &w,
        &json!({
            "tick": "2026-11-04T08:00",
            "last": {"mel/digest": {"at": "2026-11-04T08:00", "pid": 42, "log": "/x.log", "why": "due 08:00"}},
            "pruned": 5,
        }),
    );
    let reg = w.reg();
    step(&reg, &w.env, "schedules", opts()).unwrap();
    // While the core owned the routines, the host's copy drifted (its key was regenerated, r2 was enabled).
    let mut v = w.serve_routines();
    v["routines"][1]["webhookKey"] = json!("a-key-the-host-minted-on-its-own");
    v["routines"][1]["enabled"] = json!(true);
    v["routines"][0]["name"] = json!("renamed by the host");
    w.set_serve_routines(&v);
    // The core paused r1's job (a pause while switched).
    let mut jobs = w.jobs();
    for j in &mut jobs {
        if j["id"] == "r1" {
            j["paused"] = json!(true);
        }
    }
    std::fs::write(w.mel_jobs(), serde_json::to_string_pretty(&jobs).unwrap()).unwrap();

    let out = rollback(&reg, &w.env, "schedules", opts()).unwrap();
    let text = out.join("\n");
    assert!(text.contains("rolled back"), "{text}");
    assert!(text.contains("start the host"), "{text}");
    assert!(!text.contains(SECRET));
    assert!(state::read(&w.env.layout, "schedules").unwrap().is_none());
    // The routines on the host: its key and its flags are the core's; what the host owns is left alone.
    let back = w.serve_routines();
    assert_eq!(back["routines"][1]["webhookKey"], SECRET);
    assert_eq!(back["routines"][1]["enabled"], false);
    assert_eq!(back["routines"][0]["enabled"], false, "the pause came back");
    assert_eq!(back["routines"][0]["name"], "renamed by the host");
    assert_eq!(back["runs"], json!([]));
    // The jobs this import made are gone, kith-sched's own is not, the key records are gone.
    let ids: Vec<String> = w
        .jobs()
        .iter()
        .map(|j| j["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(ids, ["digest"]);
    assert!(
        w.secret(&crate::schedules::hook_key_name("mel", "r2-1"))
            .is_none()
    );
    // kith-sched's file is back, with the last run from the core and the key the original had besides.
    assert!(!tombstone::is_tombstone(&w.state_file()));
    let s: Value = serde_json::from_slice(&std::fs::read(w.state_file()).unwrap()).unwrap();
    assert_eq!(s["pruned"], 5);
    assert_eq!(s["last"]["mel/digest"]["at"], "2026-11-04T08:00");
    assert_eq!(s["last"]["mel/digest"]["pid"], 42);
    assert_eq!(s["last"]["mel/digest"]["log"], "/x.log");
    assert!(s["tick"].is_string(), "{s}");
}

#[test]
fn a_routine_the_host_no_longer_has_is_not_brought_back_by_a_rollback() {
    let w = world(false);
    let reg = w.reg();
    step(&reg, &w.env, "schedules", opts()).unwrap();
    let mut v = w.serve_routines();
    v["routines"].as_array_mut().unwrap().remove(0);
    w.set_serve_routines(&v);
    let out = rollback(&reg, &w.env, "schedules", opts())
        .unwrap()
        .join("\n");
    assert!(out.contains("routine r1 is no longer in serve.db"), "{out}");
    assert_eq!(w.serve_routines()["routines"].as_array().unwrap().len(), 1);
}

#[test]
fn a_rollback_waits_for_unfinished_scheduled_turns_unless_forced() {
    let w = world(false);
    let reg = w.reg();
    step(&reg, &w.env, "schedules", opts()).unwrap();
    {
        let core = w.core();
        core.conn()
            .execute_batch(
                "INSERT INTO conversations (id, agent, engine, created_at, updated_at) VALUES ('c1', 'mel', 'claude', 0, 0);
                 INSERT INTO turns (conversation, source, state, queued_at) VALUES ('c1', 'schedule', 'queued', 0);
                 INSERT INTO sched_fires (agent, job, slot, outcome, turn_id, at)
                   VALUES ('mel', 'r1', '2026-11-04T08:00:00Z', 'fired', (SELECT max(id) FROM turns), 0);",
            )
            .unwrap_or_else(|e| panic!("the rig's rows no longer fit the schema: {e}"));
    }
    let err = rollback(&reg, &w.env, "schedules", opts())
        .unwrap_err()
        .to_string();
    assert!(err.contains("scheduled turn(s) have not finished"), "{err}");
    assert!(state::read(&w.env.layout, "schedules").unwrap().is_some());
    rollback(
        &reg,
        &w.env,
        "schedules",
        Opts {
            force: true,
            ..opts()
        },
    )
    .unwrap();
    assert!(state::read(&w.env.layout, "schedules").unwrap().is_none());
}

#[test]
fn registered_but_never_stepped_the_module_changes_nothing() {
    let w = world(false);
    let before = std::fs::read(w.mel_jobs()).unwrap();
    assert_eq!(
        crate::cutover::status(&registry(), &w.env)
            .unwrap()
            .last()
            .unwrap(),
        "schedules: not switched"
    );
    assert_eq!(std::fs::read(w.mel_jobs()).unwrap(), before);
    assert!(!Path::new(&w.state_file()).exists());
}

fn ids(jobs: &[Value]) -> Vec<String> {
    jobs.iter()
        .map(|j| j["id"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn a_routine_that_changes_its_trigger_count_or_agent_leaves_no_stale_job() {
    let w = world(false);
    let reg = w.reg();
    let mut core = w.core();
    reg[0].import(&w.env, Some(&mut core), opts()).unwrap();
    // 1 -> 2 triggers: r1 becomes r1-1 and r1-2, and the old r1 goes.
    let mut v = w.serve_routines();
    v["routines"][0]["triggers"] = json!([cron(), {"type": "interval", "seconds": 600}]);
    w.set_serve_routines(&v);
    let diffs = reg[0].same_answer(&w.env, core.conn()).unwrap();
    assert!(
        diffs.iter().any(|d| d.contains("mel/r1:")),
        "the old job is a difference: {diffs:?}"
    );
    reg[0].import(&w.env, Some(&mut core), opts()).unwrap();
    assert_eq!(ids(&w.jobs()), ["digest", "r2-1", "r2-2", "r1-1", "r1-2"]);
    assert_eq!(
        reg[0].same_answer(&w.env, core.conn()).unwrap(),
        Vec::<String>::new()
    );
    // 2 -> 1.
    let mut v = w.serve_routines();
    v["routines"][0]["triggers"] = json!([cron()]);
    w.set_serve_routines(&v);
    reg[0].import(&w.env, Some(&mut core), opts()).unwrap();
    assert_eq!(ids(&w.jobs()), ["digest", "r2-1", "r2-2", "r1"]);
    // A routine that is only refused this round keeps its jobs: r1's bot becomes a host-native one.
    let mut v = w.serve_routines();
    v["routines"][0]["botId"] = json!("b2");
    w.set_serve_routines(&v);
    reg[0].import(&w.env, Some(&mut core), opts()).unwrap();
    assert!(ids(&w.jobs()).contains(&"r1".to_owned()));
    let mut v = w.serve_routines();
    v["routines"][0]["botId"] = json!("b1");
    w.set_serve_routines(&v);
    // The bot moves to another agent: the job and its key go from mel to ola.
    let ola = w.env.machine.join("agents/ola");
    std::fs::create_dir_all(ola.join("role")).unwrap();
    std::fs::write(ola.join("role/agent.md"), "---\nname: ola\n---\n").unwrap();
    Connection::open(&w.serve)
        .unwrap()
        .execute(
            "INSERT INTO bots (id, rev, config) VALUES ('b3', 1, ?1)",
            [json!({"id": "b3", "hqAgent": "ola"}).to_string()],
        )
        .unwrap();
    let mut v = w.serve_routines();
    v["routines"][1]["botId"] = json!("b3");
    w.set_serve_routines(&v);
    reg[0].import(&w.env, Some(&mut core), opts()).unwrap();
    assert_eq!(ids(&w.jobs()), ["digest", "r1"]);
    let ola_jobs: Vec<Value> =
        serde_json::from_slice(&std::fs::read(ola.join("jobs.json")).unwrap()).unwrap();
    assert_eq!(ids(&ola_jobs), ["r2-1", "r2-2"]);
    drop(core);
    assert_eq!(
        w.secret(&crate::schedules::hook_key_name("mel", "r2-1")),
        None
    );
    assert_eq!(
        w.secret(&crate::schedules::hook_key_name("ola", "r2-1"))
            .as_deref(),
        Some(SECRET)
    );
}

#[test]
fn a_check_compares_the_key_without_showing_it() {
    let w = world(false);
    let reg = w.reg();
    let mut core = w.core();
    reg[0].import(&w.env, Some(&mut core), opts()).unwrap();
    let mut v = w.serve_routines();
    v["routines"][1]["webhookKey"] = json!("the-host-rotated-its-key");
    w.set_serve_routines(&v);
    let diffs = reg[0].same_answer(&w.env, core.conn()).unwrap().join("\n");
    assert!(diffs.contains("not the host's"), "{diffs}");
    assert!(
        !diffs.contains(SECRET) && !diffs.contains("rotated"),
        "{diffs}"
    );
}

#[test]
fn a_rollback_that_crashed_after_the_jobs_went_strands_no_key_when_run_again() {
    let w = world(false);
    let reg = w.reg();
    step(&reg, &w.env, "schedules", opts()).unwrap();
    // The crash: the jobs are already out of jobs.json, the key records are not yet deleted.
    let jobs: Vec<Value> = w
        .jobs()
        .into_iter()
        .filter(|j| j.get("routine").is_none())
        .collect();
    std::fs::write(w.mel_jobs(), serde_json::to_string_pretty(&jobs).unwrap()).unwrap();
    assert!(
        w.secret(&crate::schedules::hook_key_name("mel", "r2-1"))
            .is_some()
    );
    rollback(&reg, &w.env, "schedules", opts()).unwrap();
    assert!(
        w.secret(&crate::schedules::hook_key_name("mel", "r2-1"))
            .is_none()
    );
    assert!(state::read(&w.env.layout, "schedules").unwrap().is_none());
}

#[test]
fn pausing_one_trigger_of_a_multi_trigger_routine_keeps_it_enabled_through_a_rollback() {
    let w = world(false);
    let mut v = w.serve_routines();
    v["routines"][1]["enabled"] = json!(true);
    w.set_serve_routines(&v);
    let reg = w.reg();
    step(&reg, &w.env, "schedules", opts()).unwrap();
    let mut jobs = w.jobs();
    for j in &mut jobs {
        if j["id"] == "r2-1" {
            j["paused"] = json!(true);
        }
    }
    std::fs::write(w.mel_jobs(), serde_json::to_string_pretty(&jobs).unwrap()).unwrap();
    rollback(&reg, &w.env, "schedules", opts()).unwrap();
    assert_eq!(w.serve_routines()["routines"][1]["enabled"], true);
}

#[test]
fn the_dry_run_leaves_serve_db_and_the_sched_state_exactly_as_they_were() {
    let w = world(false);
    write_state(&w, &json!({"tick": "2026-11-04T08:00", "last": {}}));
    let (db, st) = (
        std::fs::read(&w.serve).unwrap(),
        std::fs::read(w.state_file()).unwrap(),
    );
    step(
        &registry(),
        &w.env,
        "schedules",
        Opts {
            dry_run: true,
            ..opts()
        },
    )
    .unwrap();
    assert_eq!(std::fs::read(&w.serve).unwrap(), db);
    assert_eq!(std::fs::read(w.state_file()).unwrap(), st);
}
