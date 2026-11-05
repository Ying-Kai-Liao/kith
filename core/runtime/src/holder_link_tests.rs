#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use kith_holder::client::Client;
use kith_holder::{launch, proto, wire};

use super::{Answered, TurnLock, TurnRelease, release_turn_lock, take_turn_lock};
use crate::terminal::tests::daemon::{holder_bin, sandbox};

const SECRET: &str = "0123456789abcdef0123456789abcdef";
const WAIT: Duration = Duration::from_secs(20);

/// The built `kith-stub`, next to the holder binary.
fn stub_bin() -> PathBuf {
    let beside = holder_bin().with_file_name("kith-stub");
    if !beside.is_file() {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let built = std::process::Command::new(cargo)
            .args(["build", "--manifest-path"])
            .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../stub/Cargo.toml"))
            .status()
            .unwrap();
        assert!(
            built.success() && beside.is_file(),
            "kith-stub is not built"
        );
    }
    beside
}

fn warm_spawn(
    conv: &str,
    claim: u64,
    stub: Option<&PathBuf>,
    locks: &std::path::Path,
) -> proto::Spawn {
    proto::Spawn {
        conv: conv.into(),
        claim,
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
        stub: stub.map(|s| s.display().to_string()),
        lock_dir: Some(locks.display().to_string()),
    }
}

fn start(c: &mut Client, spec: &proto::Spawn) -> Result<proto::HeldId, String> {
    c.spawn(spec).unwrap();
    let (kind, _, payload) = c
        .take(WAIT, |f| {
            matches!(f.0, proto::SPAWNED | proto::SPAWN_FAILED)
        })
        .expect("an answer to the spawn");
    if kind == proto::SPAWN_FAILED {
        return Err(wire::parse::<proto::SpawnFailed>(&payload).unwrap().error);
    }
    Ok(wire::parse::<proto::Spawned>(&payload).unwrap().held_id())
}

fn ok<T>(claim: u64, result: T) -> Answered<T> {
    Answered { claim, result }
}

fn ended(c: &mut Client, held: &proto::HeldId) {
    for _ in 0..40 {
        if c.kill(held).unwrap() == proto::KillState::Gone {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = c.ack(held);
}

/// The core's calls against a real holder and a real stub: typed answers that carry the claim, and a missing stub is a
/// failed spawn with the hint, never a bare engine.
#[test]
fn the_cores_take_and_release_return_typed_answers_with_the_claim() {
    let dir = sandbox();
    let holder_dir = dir.path().join("h");
    launch::ensure(&holder_bin(), &holder_dir, SECRET, Duration::from_secs(120)).unwrap();
    let mut c = Client::connect(&holder_dir, SECRET, 1).unwrap();
    let locks = dir.path().join("locks");
    let stub = stub_bin();

    let missing = PathBuf::from("/no/such/kith-stub");
    let why = start(&mut c, &warm_spawn("conv-x", 1, Some(&missing), &locks)).unwrap_err();
    assert!(
        why.contains("kith-stub") && why.contains("Install"),
        "{why}"
    );
    let why = start(&mut c, &warm_spawn("conv-x", 1, None, &locks)).unwrap_err();
    assert!(why.contains("Install"), "{why}");

    let held = start(&mut c, &warm_spawn("conv-x", 1, Some(&stub), &locks)).unwrap();
    assert_eq!(
        take_turn_lock(&mut c, &held, "conv-x", 1).unwrap(),
        ok(1, TurnLock::Locked)
    );
    assert_eq!(
        take_turn_lock(&mut c, &held, "conv-x", 1).unwrap(),
        ok(1, TurnLock::Locked),
        "a retried take"
    );
    assert_eq!(
        take_turn_lock(&mut c, &held, "conv-x", 2).unwrap(),
        ok(2, TurnLock::Busy)
    );
    assert_eq!(
        release_turn_lock(&mut c, &held, "conv-x", 2).unwrap(),
        ok(2, TurnRelease::NotHeld),
        "another claim's release frees nothing"
    );
    assert_eq!(
        release_turn_lock(&mut c, &held, "conv-x", 1).unwrap(),
        ok(1, TurnRelease::Released)
    );
    assert_eq!(
        release_turn_lock(&mut c, &held, "conv-x", 1).unwrap(),
        ok(1, TurnRelease::NotHeld)
    );
    let again = take_turn_lock(&mut c, &held, "conv-x", 1).unwrap();
    assert_eq!(again.claim, 1);
    assert!(
        matches!(again.result, TurnLock::Error(ref why) if why == "claim-released"),
        "{again:?}"
    );
    assert_eq!(
        take_turn_lock(&mut c, &held, "conv-x", 2).unwrap(),
        ok(2, TurnLock::Locked)
    );
    ended(&mut c, &held);
    let _ = c.send(proto::SHUTDOWN, 0, &[]);
}

// --- the turn steps against a real holder, stub and core.db (M3-10b3) ---------------------------------------------------

use std::sync::Mutex;

use super::{TurnStep, begin_turn, exit_body, finish_turn, forget_proc};
use crate::conversations::claims::{self, Kind, NewClaim};
use crate::db::{Core, MIGRATIONS, RecordedProc};
use crate::layout::Layout;
use crate::start_with;
use crate::tests::machine;

struct Rig {
    _dir: tempfile::TempDir,
    _machine: tempfile::TempDir,
    c: Client,
    core: Mutex<Core>,
    locks: PathBuf,
    stub: PathBuf,
}

impl Rig {
    fn new() -> Rig {
        let dir = sandbox();
        let holder_dir = dir.path().join("h");
        launch::ensure(&holder_bin(), &holder_dir, SECRET, Duration::from_secs(120)).unwrap();
        let c = Client::connect(&holder_dir, SECRET, 1).unwrap();
        let (machine_dir, m) = machine();
        let mut core = start_with(&Layout::new(&m), MIGRATIONS).unwrap();
        claims::reconcile(&mut core, &[], &[], i64::MAX, 1).unwrap();
        Rig {
            locks: dir.path().join("locks"),
            stub: stub_bin(),
            _dir: dir,
            _machine: machine_dir,
            c,
            core: Mutex::new(core),
        }
    }

    /// A conversation with a queued turn, claimed: the claim id.
    fn claimed(&self, conv: &str) -> (i64, i64) {
        let mut core = self.core.lock().unwrap();
        core.conn()
            .execute(
                "INSERT OR IGNORE INTO conversations (id, agent, engine, created_at, updated_at) VALUES (?1, 'a', 'claude', 1, 1)",
                [conv],
            )
            .unwrap();
        core.conn()
            .execute(
                "INSERT INTO turns (conversation, person, source, state, queued_at) VALUES (?1, 'sam', 'app', 'queued', 1)",
                [conv],
            )
            .unwrap();
        let turn = core.conn().last_insert_rowid();
        let req = NewClaim {
            conversation: conv,
            kind: Kind::Turn,
            person: "sam",
            turn: Some(turn),
            holder_instance: None,
            account: None,
            process: None,
            revoke_seen: 0,
        };
        (claims::claim(&mut core, &req, 10).unwrap().claim_id, turn)
    }

    /// A warm process for `claim`, with the pid the holder listed for it.
    fn warm(&mut self, conv: &str, claim: i64) -> (proto::HeldId, u32) {
        let spec = warm_spawn(
            conv,
            u64::try_from(claim).unwrap(),
            Some(&self.stub),
            &self.locks,
        );
        let held = start(&mut self.c, &spec).unwrap();
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

    fn row(&self, sql: &str, id: i64) -> String {
        self.core
            .lock()
            .unwrap()
            .conn()
            .query_row(sql, [id], |r| r.get::<_, String>(0))
            .unwrap()
    }

    fn finish(mut self, held: &[&proto::HeldId]) {
        for h in held {
            ended(&mut self.c, h);
        }
        let _ = self.c.send(proto::SHUTDOWN, 0, &[]);
    }
}

#[test]
fn a_turn_takes_the_lock_then_redeems_and_gives_the_lock_back_at_its_end() {
    let mut rig = Rig::new();
    let (claim, turn) = rig.claimed("conv-t");
    let (held, pid) = rig.warm("conv-t", claim);

    let step = begin_turn(&mut rig.c, &rig.core, &held, "conv-t", claim, pid);
    assert_eq!(step, TurnStep::Running { again: false });
    assert_eq!(
        rig.row("SELECT state FROM claims WHERE claim_id = ?1", claim),
        "running"
    );
    assert_eq!(
        rig.row("SELECT state FROM turns WHERE id = ?1", turn),
        "running"
    );
    // A begin whose answer was lost is asked again: the same take, the same redeem.
    assert_eq!(
        begin_turn(&mut rig.c, &rig.core, &held, "conv-t", claim, pid),
        TurnStep::Running { again: true }
    );

    assert_eq!(
        finish_turn(&mut rig.c, &held, "conv-t", claim),
        TurnStep::Finished
    );
    assert_eq!(
        finish_turn(&mut rig.c, &held, "conv-t", claim),
        TurnStep::Finished,
        "a release asked again is fine"
    );
    // Never a take after the release for that claim.
    let again = begin_turn(&mut rig.c, &rig.core, &held, "conv-t", claim, pid);
    assert!(
        matches!(again, TurnStep::Refused(ref why) if why == "claim-released"),
        "{again:?}"
    );
    rig.finish(&[&held]);
}

#[test]
fn a_redeem_that_fails_after_the_take_gives_the_lock_back() {
    let mut rig = Rig::new();
    let (claim, _) = rig.claimed("conv-r");
    let (held, pid) = rig.warm("conv-r", claim);
    // The claim was ended (a stop, a revoke) between the claim and the begin: the CAS fails.
    rig.core
        .lock()
        .unwrap()
        .conn()
        .execute(
            "UPDATE claims SET state = 'ended', ended_at = 11 WHERE claim_id = ?1",
            [claim],
        )
        .unwrap();
    let step = begin_turn(&mut rig.c, &rig.core, &held, "conv-r", claim, pid);
    assert_eq!(step, TurnStep::Refused("ended".into()));
    // The lock is free again: a later claim's take on the same process is not busy.
    let next = take_turn_lock(
        &mut rig.c,
        &held,
        "conv-r",
        u64::try_from(claim).unwrap() + 1,
    )
    .unwrap();
    assert_eq!(next.result, TurnLock::Locked, "{next:?}");
    rig.finish(&[&held]);
}

#[test]
fn a_busy_lock_ends_the_claim_and_parks_the_turn() {
    let mut rig = Rig::new();
    let (first, _) = rig.claimed("conv-b");
    let (h1, p1) = rig.warm("conv-b", first);
    assert_eq!(
        begin_turn(&mut rig.c, &rig.core, &h1, "conv-b", first, p1),
        TurnStep::Running { again: false }
    );
    // The first claim is over in core.db while its process still holds the lock (a stopped claim not yet gone), and
    // the conversation's next claim starts a second process.
    rig.core
        .lock()
        .unwrap()
        .conn()
        .execute(
            "UPDATE claims SET state = 'ended', ended_at = 11 WHERE claim_id = ?1",
            [first],
        )
        .unwrap();
    let (second, turn) = rig.claimed("conv-b");
    let (h2, p2) = rig.warm("conv-b", second);
    let step = begin_turn(&mut rig.c, &rig.core, &h2, "conv-b", second, p2);
    assert_eq!(step, TurnStep::Busy);
    assert_eq!(
        rig.row("SELECT end_reason FROM claims WHERE claim_id = ?1", second),
        "busy"
    );
    assert_eq!(
        rig.row("SELECT state FROM turns WHERE id = ?1", turn),
        "parked"
    );
    assert_eq!(
        rig.row("SELECT parked_reason FROM turns WHERE id = ?1", turn),
        "lock_busy"
    );
    rig.finish(&[&h1, &h2]);
}

#[test]
fn a_process_that_exited_before_its_take_reports_its_exit_for_the_claim() {
    let mut rig = Rig::new();
    let (claim, turn) = rig.claimed("conv-x");
    let (held, pid) = rig.warm("conv-x", claim);
    // The engine ends: its stub (and the lock) go with it, and the holder answers a take with `exited` or `stub-gone`.
    let mut killed = false;
    for _ in 0..40 {
        if rig.c.kill(&held).unwrap() == proto::KillState::Gone {
            killed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(killed);
    let step = begin_turn(&mut rig.c, &rig.core, &held, "conv-x", claim, pid);
    assert!(
        matches!(step, TurnStep::Gone(ref why) if matches!(why.as_str(), "exited" | "stub-gone" | "gone")),
        "{step:?}"
    );
    // The exit report names the pid; the claim has no recorded process yet, so it ends and the turn is requeued.
    assert_eq!(
        rig.row("SELECT state FROM claims WHERE claim_id = ?1", claim),
        "ended"
    );
    assert_eq!(
        rig.row("SELECT state FROM turns WHERE id = ?1", turn),
        "queued"
    );
    rig.finish(&[&held]);
}

#[test]
fn an_exit_body_names_the_pid_and_carries_out_of_usage() {
    let exit = proto::Exit {
        code: Some(1),
        signal: None,
        error: None,
        lapsed: false,
    };
    let v: serde_json::Value =
        serde_json::from_str(&exit_body(Some(7), Some(&exit), true, None)).unwrap();
    assert_eq!((v["pid"].as_u64(), v["code"].as_i64()), (Some(7), Some(1)));
    assert_eq!(v["out_of_usage"], true);
    let bare: serde_json::Value =
        serde_json::from_str(&exit_body(None, None, false, Some("stub-gone"))).unwrap();
    assert!(bare.get("pid").is_none(), "{bare}");
    assert_eq!(
        (bare["out_of_usage"].as_bool(), bare["why"].as_str()),
        (Some(false), Some("stub-gone"))
    );
}

/// An exit that says the account ran out reaches `failover::on_exit` through the claim's report: the claim ends and the
/// turn goes back in the queue with the reason recorded.
#[test]
fn an_out_of_usage_exit_report_reaches_the_failover() {
    let rig = Rig::new();
    let (claim, turn) = rig.claimed("conv-u");
    let body = exit_body(None, None, true, None);
    claims::report(&mut rig.core.lock().unwrap(), claim, "exit", &body, 20).unwrap();
    assert_eq!(
        rig.row("SELECT end_reason FROM claims WHERE claim_id = ?1", claim),
        "out_of_usage"
    );
    assert_eq!(
        rig.row("SELECT state FROM turns WHERE id = ?1", turn),
        "queued"
    );
}

#[test]
fn forget_drops_a_recorded_process_and_asked_twice_changes_nothing() {
    let (_t, m) = machine();
    let mut core = start_with(&Layout::new(&m), MIGRATIONS).unwrap();
    let rec = crate::db::HolderRecord {
        instance: Some("i1".into()),
        procs: vec![
            RecordedProc {
                pid: 11,
                started: "Tue Nov  3 10:00:00 2026".into(),
                conv: "c1".into(),
            },
            RecordedProc {
                pid: 12,
                started: "e5".into(),
                conv: "c2".into(),
            },
        ],
    };
    core.save_holder_record(&rec).unwrap();
    assert_eq!(
        forget_proc(&mut core, &serde_json::json!({"pid": 11})).unwrap(),
        serde_json::json!({"forgot": true})
    );
    let after = core.holder_record().unwrap();
    assert_eq!(after.instance.as_deref(), Some("i1"));
    assert_eq!(after.procs.iter().map(|p| p.pid).collect::<Vec<_>>(), [12]);
    assert_eq!(
        forget_proc(&mut core, &serde_json::json!({"pid": 11})).unwrap(),
        serde_json::json!({"forgot": false})
    );
    assert!(forget_proc(&mut core, &serde_json::json!({})).is_err());
}
