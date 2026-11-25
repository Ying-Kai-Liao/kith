//! The schedule door end to end (design 2026-11-24-sched-turn, S2; the architect's MUST before the host uses the token): a real
//! core (the daemon the host tests start, its authority stepped from a real `auth.json`), the real host code (`Live::cron`, the
//! claim, the report), and the built `kith` as the program that runs the launch. Only the engine is a stand-in: a `claude` script
//! that records what it was started with. The core fires the job (`schedule.runNow`), the host pulls the turn it queued
//! (`hostclaim.queued`) and runs it.
//!
//! - an owner's job on a Mac with two accounts, the first out of usage: the launch walks to the second, with the schedule's env
//! - a guest's job, run directly: `KITH_PERSON` is the guest, its outbox is its own, and the run needs no `kith-sched`
//! - both: the claim's pid is the launch's (an ancestor of the engine), and the host's report ends the turn `done`
//!
//! The program is `target/<profile>/kith` beside the test binary: a `cargo test` of the package builds it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::routines::core_cron::{Live, TEST_KERNEL};
use crate::routines::core_runs::Ops;
use crate::routines::run_door::Started;
use crate::routines::sched_launch::{TEST_ENV, TEST_PROGRAM};
use crate::routines::switch_tests::hub;
use crate::terminal::core_writer::CoreWriter;
use kith_kernel::paths::Paths;
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The stand-ins are the process's: one world at a time.
static SERIAL: Mutex<()> = Mutex::new(());

fn repo_kernel() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../kernel").canonicalize().unwrap()
}

/// The built `kith`, beside the folder the test binary runs from (`target/debug/deps/..`).
fn built_kith() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let kith = exe.parent().unwrap().parent().unwrap().join("kith");
    assert!(kith.is_file(), "the built kith is not at {}: build the package's binaries first", kith.display());
    kith
}

fn until(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("gave up waiting for {what}");
}

struct World {
    dir: PathBuf,
    folders: Paths,
    /// What the engine stand-in recorded: one file per run.
    rec: PathBuf,
    env: Vec<(String, String)>,
    daemon: Option<crate::core_front::TestDaemon>,
    _held: std::sync::MutexGuard<'static, ()>,
}

impl World {
    /// A home with agent `house`, the owner `kai` and a guest `sam` who may use `house`, two accounts for claude (`a`, `b`) after
    /// the main one, and a core serving it.
    fn start(jobs: &Value) -> Self {
        let held = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        // Short, for the core's socket.
        let dir = PathBuf::from("/tmp").join(format!("kst-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]));
        let (home, machine) = (dir.join("home"), dir.join("machine"));
        let kernel = repo_kernel();
        let bin = dir.join("bin");
        let rec = dir.join("rec");
        for d in [&home, &machine, &bin, &rec, &home.join("agents/house/role")] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(
            home.join("agents/house/role/agent.md"),
            "---\nname: house\ndescription: test agent\n---\nbody\n",
        )
        .unwrap();
        std::fs::write(home.join("agents/house/jobs.json"), jobs.to_string()).unwrap();
        // The engine: logged in when its config dir holds `.fake-login`; an account named in FAKE_LIMIT_FOR is out of usage;
        // else it records its argv, env and the pids above it.
        let claude = r#"#!/bin/sh
case "$1 $2" in
  "--version "*) echo "2.1.309 (Claude Code)"; exit 0 ;;
  "plugin list") echo '[]'; exit 0 ;;
  "auth status") [ -f "${CLAUDE_CONFIG_DIR:-$HOME/.claude}/.fake-login" ] && { echo '{"loggedIn":true}'; exit 0; }; echo '{"loggedIn":false}'; exit 1 ;;
  "auth login") mkdir -p "$CLAUDE_CONFIG_DIR" && touch "$CLAUDE_CONFIG_DIR/.fake-login"; echo "fake login"; exit 0 ;;
esac
who=$(basename "${CLAUDE_CONFIG_DIR:-default}")
case " ${FAKE_LIMIT_FOR:-} " in *" $who "*) echo "You've hit your weekly limit · resets 9pm"; exit 1 ;; esac
f="$REC/$who.$$"
{
  echo "who=$who"
  p=$PPID
  while [ "$p" -gt 1 ]; do echo "anc=$p"; p=$(ps -o ppid= -p "$p" | tr -d ' '); [ -n "$p" ] || break; done
  echo "argv=$*"
  env | sort
} > "$f.tmp"
mv "$f.tmp" "$f"
exit 0
"#;
        let engine = bin.join("claude");
        std::fs::write(&engine, claude).unwrap();
        std::fs::set_permissions(&engine, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::write(home.join(".claude/.fake-login"), "").unwrap();

        let vars = [
            ("KITH_STATE", machine.to_string_lossy().into_owned()),
            ("KITH_HOME", home.to_string_lossy().into_owned()),
            ("KITH_AGENTS", home.join("agents").to_string_lossy().into_owned()),
        ];
        let lookup = |k: &str| vars.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone());
        let folders = kith_kernel::paths::paths(&home, &kernel, &lookup);
        std::fs::create_dir_all(&folders.secrets).unwrap();
        let auth = folders.secrets.join("auth.json");
        let mut env: Vec<(String, String)> = vars.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect();
        let p = |x: &Path| x.to_string_lossy().into_owned();
        env.extend([
            ("HOME".to_owned(), p(&home)),
            ("PATH".to_owned(), format!("{}:/usr/bin:/bin:/usr/local/bin:/opt/homebrew/bin", bin.display())),
            ("REC".to_owned(), p(&rec)),
            ("KITH_AUTH".to_owned(), p(&auth)),
            ("KITH_BIN".to_owned(), p(&built_kith())),
            // No host here: nothing is asked of one.
            ("KITH_SERVE_DIR".to_owned(), p(&dir.join("serve"))),
            ("KITH_SERVE_PORT".to_owned(), "1".to_owned()),
            ("TZ".to_owned(), "UTC".to_owned()),
        ]);
        let world = Self { dir, folders, rec, env, daemon: None, _held: held };
        world.python(&kernel, &["person", "kai", "owner"]);
        world.python(&kernel, &["person", "sam", "name=Sam"]);
        world.python(&kernel, &["member", "house", "sam", "mode=default"]);
        let said = crate::core_front::step_authority_for_test(&world.folders, &kernel).expect("the authority step");
        assert!(said.iter().any(|l| l.contains("switched")), "{said:?}");
        let mut world = world;
        // Two accounts after the main one: a run that finds the main one out of usage moves to `a`.
        for a in ["a", "b"] {
            world.kith(&["account", "add", "claude", a, "--yes"]);
        }
        world.kith(&["account", "set", "claude", "claude:a", "claude:b"]);
        let daemon = crate::core_front::serve_for_test(&world.folders);
        daemon.reconciled();
        world.daemon = Some(daemon);
        *TEST_KERNEL.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(kernel);
        *TEST_PROGRAM.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(built_kith());
        world
    }

    fn command(&self, program: &Path) -> Command {
        let mut c = Command::new(program);
        c.env_clear().envs(self.env.iter().map(|(k, v)| (k, v)));
        c
    }

    fn python(&self, kernel: &Path, args: &[&str]) {
        let out = self.command(Path::new("python3")).arg(kernel.join("bin/kith-auth")).args(args).output().unwrap();
        assert!(out.status.success(), "kith-auth {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn kith(&self, args: &[&str]) {
        let out = self.command(&built_kith()).args(args).stdin(std::process::Stdio::null()).output().unwrap();
        assert!(
            out.status.success(),
            "kith {args:?}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// The engine's environment: what the launch and the walk gave it.
    fn set_child_env(&self, extra: &[(&str, &str)]) {
        let mut env = self.env.clone();
        env.extend(extra.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())));
        *TEST_ENV.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(env);
    }

    fn core(&self, method: &str, params: &Value) -> Value {
        match kith_core::reach(&self.folders, method, params, false) {
            kith_core::Reached::Answered(v) => v,
            other => panic!("{method}: {other:?}"),
        }
    }

    fn db(&self) -> rusqlite::Connection {
        let db = rusqlite::Connection::open_with_flags(
            self.folders.machine.join("core/core.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        db.busy_timeout(Duration::from_secs(5)).unwrap();
        db
    }

    /// Fires `job` now and runs the turn it queued through the host's door; returns the turn's id, its claim and the runs the
    /// engine recorded once the turn is over.
    fn fire_and_run(&self, job: &str) -> (i64, i64, Vec<String>) {
        let fired = self.core("schedule.runNow", &json!({"agent": "house", "id": job, "key": format!("e2e-{job}")}));
        assert_eq!(fired["outcome"], "fired", "{fired}");
        let writer = Arc::new(CoreWriter::new(self.folders.clone()));
        let turns = writer.queued().expect("the core's queue");
        let t = turns.iter().find(|t| t["job"] == job).unwrap_or_else(|| panic!("no turn for {job}: {turns:?}"));
        let turn = t["turn"].as_i64().unwrap();
        let hub = hub();
        let started = Live(writer).cron(&hub, t, job);
        assert_eq!(started, Started::Ran);
        until("the turn to end", 120, || {
            let state: String =
                self.db().query_row("SELECT state FROM turns WHERE id = ?1", [turn], |r| r.get(0)).unwrap();
            state == "done" || state == "failed"
        });
        let (state, claim): (String, i64) = self
            .db()
            .query_row("SELECT state, claim_id FROM turns WHERE id = ?1", [turn], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!(state, "done", "the run ended clean");
        let runs = std::fs::read_dir(&self.rec)
            .unwrap()
            .map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap())
            .collect();
        (turn, claim, runs)
    }

    /// The pid the core recorded on the claim.
    fn claim_pid(&self, claim: i64) -> i64 {
        self.db().query_row("SELECT pid FROM claims WHERE claim_id = ?1", [claim], |r| r.get(0)).unwrap()
    }

    fn log_text(&self) -> String {
        let dir = self.folders.logs.join("sched/runs/house");
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Drop for World {
    fn drop(&mut self) {
        if let Some(d) = self.daemon.take() {
            d.stop();
        }
        *TEST_KERNEL.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        *TEST_PROGRAM.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        *TEST_ENV.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn value_of(run: &str, name: &str) -> Option<String> {
    run.lines().find_map(|l| l.strip_prefix(&format!("{name}=")).map(str::to_owned))
}

fn ancestors(run: &str) -> Vec<i64> {
    run.lines().filter_map(|l| l.strip_prefix("anc=")?.parse().ok()).collect()
}

#[test]
fn an_owners_job_walks_to_the_second_account_when_the_first_is_out_of_usage() {
    let w = World::start(&json!([{"id": "walk", "cron": "0 8 * * *", "task": "x"}]));
    w.set_child_env(&[("FAKE_LIMIT_FOR", "default")]);
    let (turn, claim, runs) = w.fire_and_run("walk");
    assert_eq!(runs.len(), 1, "only the second account's attempt ran the engine: {runs:?}");
    let run = &runs[0];
    assert_eq!(value_of(run, "who").as_deref(), Some("a"), "the first account was out of usage");
    assert_eq!(value_of(run, "KITH_ACCOUNT_ID").as_deref(), Some("claude:a"));
    assert_eq!(value_of(run, "KITH_VIA").as_deref(), Some("schedule"));
    assert_eq!(value_of(run, "KITH_SCHED_JOB").as_deref(), Some("walk"));
    assert_eq!(value_of(run, "KITH_BACKUP_CHAIN").as_deref(), Some("on"));
    assert_eq!(value_of(run, "KITH_PERSON"), None, "the owner is no person");
    assert_eq!(value_of(run, "KITH_RUN_OUTBOX"), None, "the owner has no app chat with house here: no outbox");
    let argv = value_of(run, "argv").unwrap();
    assert!(argv.contains("Scheduled job \"walk\" of house, cron `0 8 * * *`, core turn"), "{argv}");
    assert!(
        ancestors(run).contains(&w.claim_pid(claim)),
        "the claim's pid is the launch's, which the engine descends from: claim pid {} in {:?} (turn {turn})",
        w.claim_pid(claim),
        ancestors(run)
    );
    let log = w.log_text();
    assert!(log.starts_with("# ") && log.contains(" house walk (core turn "), "the log is headed once: {log}");
    assert!(log.contains("out of usage; moving to claude:a"), "{log}");
}

#[test]
fn a_guests_job_runs_directly_as_the_guest_with_its_own_outbox() {
    let w = World::start(&json!([{"id": "g", "cron": "0 9 * * *", "task": "x", "person": "sam"}]));
    w.set_child_env(&[]);
    let (_, claim, runs) = w.fire_and_run("g");
    assert_eq!(runs.len(), 1, "{runs:?}");
    let run = &runs[0];
    assert_eq!(value_of(run, "KITH_PERSON").as_deref(), Some("sam"));
    assert_eq!(value_of(run, "KITH_VIA").as_deref(), Some("schedule"));
    let outbox = value_of(run, "KITH_RUN_OUTBOX").expect("a guest's run has an outbox (dropped when it has no chat)");
    let meta: Value = serde_json::from_str(&std::fs::read_to_string(format!("{outbox}.json")).unwrap()).unwrap();
    assert_eq!(
        (meta["person"].as_str(), meta["via"].as_str(), meta["job"].as_str()),
        (Some("sam"), Some("schedule"), Some("g"))
    );
    assert!(ancestors(run).contains(&w.claim_pid(claim)), "claim pid {} in {:?}", w.claim_pid(claim), ancestors(run));
    assert!(!w.folders.machine.join("sched/state.json").exists(), "no kith-sched ran: its state file is never written");
    let _ = std::fs::remove_dir_all(&outbox);
    let _ = std::fs::remove_file(format!("{outbox}.json"));
}
