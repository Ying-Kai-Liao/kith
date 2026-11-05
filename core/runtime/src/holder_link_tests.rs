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

/// The built holder and stub, resolved once: tests that start together would otherwise each run the cargo build that
/// makes a missing binary, and one that looks while another is writing it finds nothing.
fn bins() -> &'static (PathBuf, PathBuf) {
    static BINS: std::sync::OnceLock<(PathBuf, PathBuf)> = std::sync::OnceLock::new();
    BINS.get_or_init(|| (holder_bin(), stub_bin()))
}

/// Starts (or finds) the holder in `holder_dir` and connects, retrying until it answers: on a loaded machine the
/// holder's socket can be a moment behind `ensure`. Each test has its own folder, so nothing is shared.
fn connect_when_ready(holder_dir: &std::path::Path) -> Client {
    let end = std::time::Instant::now() + WAIT;
    loop {
        let tried = launch::ensure(&bins().0, holder_dir, SECRET, Duration::from_secs(120))
            .map_err(|e| e.to_string())
            .and_then(|()| Client::connect(holder_dir, SECRET, 1).map_err(|e| format!("{e:?}")));
        match tried {
            Ok(c) => return c,
            Err(e) if std::time::Instant::now() >= end => panic!("the holder never came up: {e}"),
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

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
        let c = connect_when_ready(&holder_dir);
        let (machine_dir, m) = machine();
        let mut core = start_with(&Layout::new(&m), MIGRATIONS).unwrap();
        claims::reconcile(&mut core, &[], &[], i64::MAX, 1).unwrap();
        Rig {
            locks: dir.path().join("locks"),
            stub: bins().1.clone(),
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
        forget_proc(&mut core, &serde_json::json!({"pid": 11}), false, &[]).unwrap(),
        serde_json::json!({"forgot": true})
    );
    let after = core.holder_record().unwrap();
    assert_eq!(after.instance.as_deref(), Some("i1"));
    assert_eq!(after.procs.iter().map(|p| p.pid).collect::<Vec<_>>(), [12]);
    assert_eq!(
        forget_proc(&mut core, &serde_json::json!({"pid": 11}), false, &[]).unwrap(),
        serde_json::json!({"forgot": false})
    );
    assert!(forget_proc(&mut core, &serde_json::json!({}), false, &[]).is_err());
}

// --- the kill that is sent again, and the groups a blocked pass ended ---------------------------------------------------

/// `end_held` is done only when the holder says the tree is gone, and only then is the process acked. One that has not
/// finished (no tries left, or the link broke under it) is `false`, the process is still the holder's, and the end goes
/// back in the queue to be sent again (Claims4 finding 4, row `c4-nokillresend`).
#[test]
fn an_end_that_is_not_finished_is_sent_again_and_acked_only_when_gone() {
    let mut rig = Rig::new();
    let (claim, _) = rig.claimed("conv-e");
    let (held, _) = rig.warm("conv-e", claim);
    let relay = crate::attach::Relay::new();
    assert!(
        !super::end_held_within(&mut rig.c, &relay, &held, 0),
        "no kill sent, not done"
    );
    let listed = |c: &mut Client| c.list().unwrap().iter().any(|h| h.id == held.id);
    assert!(
        listed(&mut rig.c),
        "not acked, so the holder still lists it"
    );
    // The request waits a growing time and then comes up again; the wait is not skipped.
    let terminals = crate::terminal::Terminals::new();
    assert_eq!(terminals.requeue_end(held.clone(), 0), 1);
    assert_eq!(terminals.ending(), 1);
    assert!(terminals.due_ends().is_empty(), "not before its wait");
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(terminals.due_ends(), vec![(held.clone(), 1)]);
    assert_eq!(terminals.ending(), 0);
    assert_eq!(terminals.requeue_end(held.clone(), 1), 2, "rounds count up");
    // The same end asked twice is one entry, with the larger count.
    assert_eq!(terminals.requeue_end(held.clone(), 0), 2);
    assert_eq!(terminals.ending(), 1, "de-duplicated by held id");
    // Asked again with tries, it ends and is acked.
    let mut done = false;
    for _ in 0..20 {
        if super::end_held(&mut rig.c, &relay, &held) {
            done = true;
            break;
        }
    }
    assert!(done);
    assert!(!listed(&mut rig.c), "acked once gone");
    // A link that breaks under the end leaves it unfinished.
    let _ = rig.c.send(proto::SHUTDOWN, 0, &[]);
    std::thread::sleep(Duration::from_millis(300));
    assert!(!super::end_held_within(&mut rig.c, &relay, &held, 3));
}

/// A pass that ends some groups and cannot tell another process from a stranger returns the blocked reason, and the
/// groups it did end are in the notes (`reconcile` logs them before it returns `Lost::Blocked`).
#[test]
fn a_blocked_pass_still_notes_the_groups_it_ended() {
    let mut rig = Rig::new();
    let (c1, _) = rig.claimed("conv-g1");
    let (h1, _) = rig.warm("conv-g1", c1);
    let (c2, _) = rig.claimed("conv-g2");
    let (h2, _) = rig.warm("conv-g2", c2);
    let held = rig.c.list().unwrap();
    let mut recorded = super::record_procs(&held);
    assert_eq!(recorded.len(), 2, "{recorded:?}");
    let (a, b) = (recorded[0].pid, recorded[1].pid);
    // The second one's start is in the old form: it cannot be told from a stranger, so it blocks.
    recorded[1].started = "Tue Nov  3 10:00:00 2026".into();
    let mut notes = Vec::new();
    let out = super::end_groups(&mut notes, &recorded, Duration::from_secs(10));
    let stuck = out.unwrap_err();
    assert!(stuck.contains(&format!("pid {b} is running")), "{stuck}");
    assert!(
        notes.contains(&format!("holder: ended the process group of pid {a}")),
        "{notes:?}"
    );
    rig.finish(&[&h1, &h2]);
}

#[test]
fn forget_refuses_a_process_the_live_holder_lists_unless_the_holder_is_blocked() {
    let (_t, m) = machine();
    let mut core = start_with(&Layout::new(&m), MIGRATIONS).unwrap();
    core.save_holder_record(&crate::db::HolderRecord {
        instance: Some("i1".into()),
        procs: vec![RecordedProc {
            pid: 21,
            started: "e1".into(),
            conv: "c".into(),
        }],
    })
    .unwrap();
    let pid = serde_json::json!({"pid": 21});
    assert!(forget_proc(&mut core, &pid, false, &[21]).is_err());
    assert_eq!(
        core.holder_record().unwrap().procs.len(),
        1,
        "nothing dropped"
    );
    assert_eq!(
        forget_proc(&mut core, &pid, true, &[21]).unwrap(),
        serde_json::json!({"forgot": true})
    );
}

#[test]
fn only_the_owner_may_forget_a_holder_process() {
    let p = serde_json::json!({"pid": 21});
    assert!(crate::call::check(kith_api::Principal::Owner, "holder.forget", &p).is_ok());
    for who in [
        kith_api::Principal::Member,
        kith_api::Principal::Bot,
        kith_api::Principal::RunToken,
    ] {
        assert!(
            matches!(
                crate::call::check(who, "holder.forget", &p),
                Err(crate::error::Error::NotAllowed(_))
            ),
            "refused for another principal"
        );
    }
}

#[test]
fn a_turn_step_runs_only_against_the_recorded_conversation_and_not_an_older_claim() {
    use super::turn_target;
    let seen = Some((10, 77, "conv-a".to_string()));
    assert_eq!(
        turn_target(seen.clone(), "conv-a", 10).unwrap(),
        (77, "conv-a".into())
    );
    assert!(
        turn_target(seen.clone(), "conv-a", 12).is_ok(),
        "a later turn of the same process"
    );
    assert!(matches!(
        turn_target(seen.clone(), "conv-b", 12),
        Err(TurnStep::Refused(_))
    ));
    assert!(matches!(
        turn_target(seen, "conv-a", 9),
        Err(TurnStep::Refused(_))
    ));
    assert!(matches!(
        turn_target(None, "conv-a", 12),
        Err(TurnStep::Refused(_))
    ));
}

/// A core.db error after the take gives the lock back too: nothing guarantees a retry, and a lock held for a claim that
/// is not running locks the conversation until the stub dies.
#[test]
fn a_redeem_error_after_the_take_gives_the_lock_back() {
    let mut rig = Rig::new();
    let (claim, _) = rig.claimed("conv-k");
    let (held, pid) = rig.warm("conv-k", claim);
    // A newer core took over: every claim transaction of this one is refused (`StaleGeneration`).
    rig.core
        .lock()
        .unwrap()
        .conn()
        .execute(
            "UPDATE counters SET value = value + 1 WHERE name = 'generation'",
            [],
        )
        .unwrap();
    let step = begin_turn(&mut rig.c, &rig.core, &held, "conv-k", claim, pid);
    assert!(matches!(step, TurnStep::Lost(_)), "{step:?}");
    let next = take_turn_lock(
        &mut rig.c,
        &held,
        "conv-k",
        u64::try_from(claim).unwrap() + 1,
    )
    .unwrap();
    assert_eq!(next.result, TurnLock::Locked, "given back: {next:?}");
    rig.finish(&[&held]);
}

/// A `LockStuck` turn's claim is ended through the claims API: it never stays `claimed` (which would keep the
/// conversation's one writer slot for a process nobody drives), its turn is queued again, and the process's end is the
/// kill the claim answers. A second `LockStuck` for the same claim changes nothing.
#[test]
fn a_lock_stuck_turn_ends_its_claim_once() {
    let rig = Rig::new();
    let (claim, turn) = rig.claimed("conv-s");
    // The holder reported the spawn: the claim names a process, so ending it is a kill to send.
    let mut core = rig.core.lock().unwrap();
    claims::report(
        &mut core,
        claim,
        "spawned",
        r#"{"pid":4242,"started":"e1"}"#,
        20,
    )
    .unwrap();
    drop(core);
    let kill = end_stuck_claim(&rig.core, claim).unwrap().unwrap();
    assert_eq!(kill.pid, Some(4242));
    assert_eq!(
        rig.row("SELECT state FROM claims WHERE claim_id = ?1", claim),
        "ending"
    );
    assert_eq!(
        rig.row("SELECT end_reason FROM claims WHERE claim_id = ?1", claim),
        "lock_stuck"
    );
    assert_eq!(
        rig.row("SELECT state FROM turns WHERE id = ?1", turn),
        "queued"
    );
    // Again: the kill is answered again, nothing is written twice.
    assert!(end_stuck_claim(&rig.core, claim).unwrap().is_some());
    assert_eq!(
        rig.row("SELECT state FROM turns WHERE id = ?1", turn),
        "queued"
    );

    // A claim with no process recorded ends at once (nothing can be writing), and a repeat is a no-op.
    let (bare, bare_turn) = rig.claimed("conv-s2");
    assert!(end_stuck_claim(&rig.core, bare).unwrap().is_none());
    assert!(end_stuck_claim(&rig.core, bare).unwrap().is_none());
    assert_eq!(
        rig.row("SELECT state FROM claims WHERE claim_id = ?1", bare),
        "ended"
    );
    assert_eq!(
        rig.row("SELECT state FROM turns WHERE id = ?1", bare_turn),
        "queued"
    );
    rig.finish(&[]);
}
