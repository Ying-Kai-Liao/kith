//! A turn on a conversation that `kith <agent> --direct` holds (M3 cutover P3, design 1.3 and 1.6). `--direct` runs
//! the engine under a one-shot `kith-stub` that holds the conversation's writer lock for its whole life. A turn the
//! core claims meanwhile must find the lock busy, end the claim `busy`, park `lock_busy`, and run only after the
//! one-shot is gone. Real processes throughout: a real holder, a real resident stub for the turn, a real one-shot
//! stub for `--direct`, and a real core.db. No product code is changed by this file.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use kith_holder::client::Client;
use kith_holder::{launch, proto, wire};
use kith_kernel::accounts::{Ctx, Outside};
use kith_kernel::threads;
use serde_json::json;

use crate::accounts::CORE_WRITES;
use crate::accounts::failover::{NewSession, Started, claim_next};
use crate::conversations::claims::{self, Kind, NewClaim};
use crate::db::{Core, MIGRATIONS};
use crate::holder_link::{TurnLock, TurnStep, begin_turn, finish_turn, take_turn_lock};
use crate::layout::Layout;
use crate::start_with;
use crate::terminal::tests::daemon::{holder_bin, sandbox};
use crate::tests::machine;

const SECRET: &str = "0123456789abcdef0123456789abcdef";
const WAIT: Duration = Duration::from_secs(20);
const MS: i64 = 1_900_000_000_000;
/// The conversation, and so the writer lock's key K.
const CONV: &str = "c1";

struct SignedIn;

impl Outside for SignedIn {
    fn status(&self, _argv: &[&str], _env: &[(&str, &str)]) -> (Option<i32>, String) {
        (Some(0), r#"{"loggedIn":true}"#.to_owned())
    }
    fn granted(&self, _agent: &str, _service: &str) -> Vec<String> {
        vec![]
    }
    fn on_path(&self, _program: &str) -> bool {
        true
    }
}

fn no_env(_: &str) -> Option<String> {
    None
}

/// The built holder and stub, resolved once so tests that start together run one cargo build, not several.
fn bins() -> &'static (PathBuf, PathBuf) {
    static BINS: std::sync::OnceLock<(PathBuf, PathBuf)> = std::sync::OnceLock::new();
    BINS.get_or_init(|| {
        let holder = holder_bin();
        let stub = holder.with_file_name("kith-stub");
        if !stub.is_file() {
            let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
            let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../stub/Cargo.toml");
            let built = Command::new(cargo)
                .args(["build", "--manifest-path"])
                .arg(manifest)
                .status()
                .unwrap();
            assert!(built.success() && stub.is_file(), "kith-stub is not built");
        }
        (holder, stub)
    })
}

fn connect_when_ready(holder_dir: &Path) -> Client {
    let end = Instant::now() + WAIT;
    loop {
        let tried = launch::ensure(&bins().0, holder_dir, SECRET, Duration::from_secs(120))
            .map_err(|e| e.to_string())
            .and_then(|()| Client::connect(holder_dir, SECRET, 1).map_err(|e| format!("{e:?}")));
        match tried {
            Ok(c) => return c,
            Err(e) if Instant::now() >= end => panic!("the holder never came up: {e}"),
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + WAIT;
    while !f() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

struct Rig {
    _dir: tempfile::TempDir,
    machine: PathBuf,
    _machine: tempfile::TempDir,
    c: Client,
    core: Mutex<Core>,
    locks: PathBuf,
}

impl Rig {
    fn new() -> Rig {
        let dir = sandbox();
        let c = connect_when_ready(&dir.path().join("h"));
        let (machine_dir, m) = machine();
        let mut core = start_with(&Layout::new(&m), MIGRATIONS).unwrap();
        claims::reconcile(&mut core, &[], &[], i64::MAX, 1).unwrap();
        std::fs::write(
            m.join("settings.json"),
            json!({"accounts": ["claude"]}).to_string(),
        )
        .unwrap();
        let role = m.join("agents/echo/role");
        std::fs::create_dir_all(&role).unwrap();
        std::fs::write(
            role.join("agent.md"),
            "---\nname: echo\nharness: claude-code\n---\nbody\n",
        )
        .unwrap();
        core.conn()
            .execute(
                "INSERT INTO conversations (id, agent, engine, created_at, updated_at) VALUES (?1, 'echo', 'claude', 1, 1)",
                [CONV],
            )
            .unwrap();
        core.conn()
            .execute(
                "INSERT INTO meta (key, value) VALUES (?1, '1')",
                [CORE_WRITES],
            )
            .unwrap();
        let locks = dir.path().join("locks");
        std::fs::create_dir_all(&locks).unwrap();
        Rig {
            _dir: dir,
            machine: m,
            _machine: machine_dir,
            c,
            core: Mutex::new(core),
            locks,
        }
    }

    /// `kith <agent> --direct`'s one-shot: a stub that holds key K for its engine's whole life. The engine reads its
    /// stdin to the end, so dropping the child's stdin ends it. Returns once the lock is really held.
    fn one_shot(&self) -> Child {
        // The engine starts only once the stub holds the lock, so its marker says the lock is held. Probing the lock
        // instead would race: a probe holds it while it runs, and the one-shot would be told busy.
        let ready = self.locks.join("ready");
        let mut child = Command::new(&bins().1)
            .args(["--lock-dir"])
            .arg(&self.locks)
            .args(["--key", CONV, "--", "/bin/sh", "-c"])
            .arg(format!("touch '{}'; exec cat", ready.display()))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        wait_for("the one-shot to hold the lock", || {
            if let Some(status) = child.try_wait().unwrap() {
                panic!("the one-shot ended early: {status:?}");
            }
            ready.exists()
        });
        child
    }

    /// Whether another writer holds K now, asked as `--direct` asks: exit 75 is busy, a free lock is taken and given
    /// back as the probe ends.
    fn lock_busy(&self) -> bool {
        Command::new(&bins().1)
            .args(["--lock-dir"])
            .arg(&self.locks)
            .args(["--key", CONV, "--", "/bin/sh", "-c", "exit 0"])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .code()
            == Some(75)
    }

    /// A queued turn for sam on the conversation.
    fn queue_turn(&self) -> i64 {
        let core = self.core.lock().unwrap();
        core.conn()
            .execute(
                "INSERT INTO turns (conversation, person, source, state, queued_at) VALUES (?1, 'sam', 'app', 'queued', 1)",
                [CONV],
            )
            .unwrap();
        core.conn().last_insert_rowid()
    }

    /// The core's only way to claim a turn, as the host's start does.
    fn claim_next(&self, turn: i64) -> i64 {
        let out = SignedIn;
        let ctx = Ctx {
            machine: self.machine.clone(),
            agents: self.machine.join("agents"),
            kernel: self.machine.join("kernel"),
            home: self.machine.join("home"),
            claude_config_dir: None,
            out: &out,
        };
        let tctx = threads::Ctx {
            state: None,
            machine: Some(self.machine.clone()),
            home: self.machine.join("home"),
            env: &no_env,
        };
        let req = NewClaim {
            conversation: CONV,
            kind: Kind::Turn,
            person: "sam",
            turn: Some(turn),
            holder_instance: Some("h1"),
            account: None,
            process: None,
            revoke_seen: 0,
        };
        let mut core = self.core.lock().unwrap();
        let started = claim_next(
            &mut core,
            &ctx,
            &req,
            &NewSession {
                ctx: &tctx,
                cwd: "/work/p",
                session_id: "s-1",
            },
            MS,
        )
        .expect("claim_next");
        let Started::Claimed { claimed, .. } = started else {
            panic!("exhausted")
        };
        claimed.claim_id
    }

    /// The turn's warm process under a resident stub, and its pid. Started once; every claim of the turn uses it.
    fn warm(&mut self) -> (proto::HeldId, u32) {
        let spec = proto::Spawn {
            conv: CONV.into(),
            claim: 1,
            generation: 1,
            argv: vec!["/bin/sh".into(), "-c".into(), "exec sleep 300".into()],
            env: std::env::vars()
                .filter(|(k, _)| k == "PATH")
                .collect::<BTreeMap<_, _>>(),
            cwd: "/".into(),
            mode: proto::Mode::Pipes,
            cols: 80,
            rows: 24,
            kill_on_lease_loss: false,
            stub: Some(bins().1.display().to_string()),
            lock_dir: Some(self.locks.display().to_string()),
        };
        self.c.spawn(&spec).unwrap();
        let (kind, _, payload) = self
            .c
            .take(WAIT, |f| {
                matches!(f.0, proto::SPAWNED | proto::SPAWN_FAILED)
            })
            .expect("an answer to the spawn");
        assert_eq!(kind, proto::SPAWNED, "the warm process did not start");
        let held = wire::parse::<proto::Spawned>(&payload).unwrap().held_id();
        let pid = self
            .c
            .list()
            .unwrap()
            .into_iter()
            .find(|h| h.id == held.id)
            .unwrap()
            .pid;
        (held, pid)
    }

    fn text(&self, sql: &str, id: i64) -> String {
        self.core
            .lock()
            .unwrap()
            .conn()
            .query_row(sql, [id], |r| r.get::<_, rusqlite::types::Value>(0))
            .map(|v| match v {
                rusqlite::types::Value::Text(s) => s,
                rusqlite::types::Value::Null => "NULL".into(),
                other => format!("{other:?}"),
            })
            .unwrap()
    }

    fn turn_state(&self, turn: i64) -> String {
        self.text("SELECT state FROM turns WHERE id = ?1", turn)
    }

    /// The caller's level-triggered wake with its own probe: lock free means no one-shot holds K.
    fn unpark(&self) -> Vec<i64> {
        let free = !self.lock_busy();
        claims::unpark_ready(&mut self.core.lock().unwrap(), CONV, free, false, MS).unwrap()
    }

    /// Parks the turn the way a busy `--direct` makes it: a claim, a take that finds K held, a busy report.
    fn park_behind(&mut self, turn: i64) -> (proto::HeldId, u32) {
        let (held, pid) = self.warm();
        let claim = self.claim_next(turn);
        let step = begin_turn(&mut self.c, &self.core, &held, CONV, claim, pid);
        assert_eq!(step, TurnStep::Busy);
        assert_eq!(
            self.text("SELECT end_reason FROM claims WHERE claim_id = ?1", claim),
            "busy"
        );
        assert_eq!(
            self.text("SELECT state FROM claims WHERE claim_id = ?1", claim),
            "ended"
        );
        assert_eq!(self.turn_state(turn), "parked");
        assert_eq!(
            self.text("SELECT parked_reason FROM turns WHERE id = ?1", turn),
            "lock_busy"
        );
        (held, pid)
    }

    fn teardown(mut self, held: &proto::HeldId) {
        for _ in 0..40 {
            if self.c.kill(held).unwrap() == proto::KillState::Gone {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.c.ack(held);
        let _ = self.c.send(proto::SHUTDOWN, 0, &[]);
    }
}

#[test]
fn a_turn_behind_a_direct_run_parks_busy_and_runs_after_the_run_exits() {
    let mut rig = Rig::new();
    let mut direct = rig.one_shot();
    let turn = rig.queue_turn();
    let (held, pid) = rig.park_behind(turn);

    // The one-shot is alive: nothing wakes the turn, however often the tick asks.
    assert_eq!(rig.unpark(), Vec::<i64>::new());
    assert_eq!(rig.unpark(), Vec::<i64>::new());
    assert_eq!(rig.turn_state(turn), "parked");
    assert!(rig.lock_busy(), "the probe must not have taken the lock");

    // The one-shot exits on its own: its engine reads to the end of stdin.
    drop(direct.stdin.take());
    assert!(direct.wait().unwrap().success());
    wait_for("the lock to be free", || !rig.lock_busy());

    assert_eq!(rig.unpark(), vec![turn]);
    assert_eq!(
        rig.unpark(),
        Vec::<i64>::new(),
        "asked twice changes nothing"
    );
    assert_eq!(rig.turn_state(turn), "queued");

    let claim = rig.claim_next(turn);
    let step = begin_turn(&mut rig.c, &rig.core, &held, CONV, claim, pid);
    assert_eq!(step, TurnStep::Running { again: false });
    assert_eq!(rig.turn_state(turn), "running");
    // The turn holds the lock now: a second `--direct` would be told busy.
    assert!(rig.lock_busy());
    // A begin whose answer was lost is asked again and stands.
    assert_eq!(
        begin_turn(&mut rig.c, &rig.core, &held, CONV, claim, pid),
        TurnStep::Running { again: true }
    );

    assert_eq!(
        finish_turn(&mut rig.c, &held, CONV, claim),
        TurnStep::Finished
    );
    claims::report(
        &mut rig.core.lock().unwrap(),
        claim,
        "done",
        &format!(r#"{{"pid":{pid}}}"#),
        MS + 1,
    )
    .unwrap();
    assert_eq!(rig.turn_state(turn), "done");
    assert!(!rig.lock_busy(), "the lock is free after the turn");
    rig.teardown(&held);
}

#[test]
fn a_direct_run_killed_with_sigkill_frees_the_lock_and_the_parked_turn_runs() {
    let mut rig = Rig::new();
    let mut direct = rig.one_shot();
    let turn = rig.queue_turn();
    let (held, pid) = rig.park_behind(turn);
    assert_eq!(rig.unpark(), Vec::<i64>::new());

    // SIGKILL: no release step runs. The lock is an fd lock, so the OS drops it with the stub. The engine outlives the
    // stub, so ending its stdin ends it too.
    direct.kill().unwrap();
    direct.wait().unwrap();
    drop(direct.stdin.take());
    wait_for("the lock to be free", || !rig.lock_busy());

    assert_eq!(rig.unpark(), vec![turn]);
    let claim = rig.claim_next(turn);
    let took = take_turn_lock(&mut rig.c, &held, CONV, u64::try_from(claim).unwrap()).unwrap();
    assert_eq!(took.result, TurnLock::Locked, "{took:?}");
    assert_eq!(
        begin_turn(&mut rig.c, &rig.core, &held, CONV, claim, pid),
        TurnStep::Running { again: false }
    );
    assert_eq!(
        finish_turn(&mut rig.c, &held, CONV, claim),
        TurnStep::Finished
    );
    assert!(!rig.lock_busy());
    rig.teardown(&held);
}
