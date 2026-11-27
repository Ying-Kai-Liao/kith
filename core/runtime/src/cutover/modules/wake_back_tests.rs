#![allow(clippy::unwrap_used)]
#![allow(
    clippy::unused_self,
    clippy::needless_pass_by_value,
    clippy::unnecessary_wraps,
    reason = "a test rig"
)]
//! The `wake` rollback (WK6 section 2, rows U6 and U10) on temp homes only: real core.db rows made through the core's own calls
//! (`wake.enqueue`, `wake.take`, `claim_run`, `attach`), real files, a real child process for a run in flight, and the real
//! `kernel/bin/kith-wake` reading what the rollback wrote.

use std::path::PathBuf;
use std::process::Command;

use serde_json::{Value, json};

use super::wake_tests::{Proc, Rig, err, opts, rig};
use crate::conversations::claims;
use crate::conversations::sched_run::{attach, claim_run};
use crate::cutover::state::Phase;
use crate::cutover::tombstone;
use crate::db::Core;
use crate::tests::open_all;
use crate::wake::{call, file_key};

const OLD: &str = "kith-serve-host/1-a";

/// A switched rig with a core in hand (and its folders, so the core knows `wake` is switched).
fn switched(files: impl FnOnce(&Rig)) -> Rig {
    let r = rig();
    files(&r);
    r.step(r.wake(), opts()).unwrap();
    r
}

fn core(r: &Rig) -> Core {
    let mut c = open_all(&r.env.layout).with_folders(r.env.paths.as_ref().unwrap());
    claims::reconcile(&mut c, &[], &[], i64::MAX, 1).unwrap();
    c
}

/// A request for `session`, taken as a run: its turn is queued. `door` is the item's door afterwards (a `take` serves host-door
/// items; the item is turned into a run-door one in the rows, where the tick's `take_due` would have made it).
fn taken(c: &mut Core, session: &str, id: &str, door: &str) -> (String, i64) {
    call(
        c,
        "wake.enqueue",
        &json!({"session": session, "id": id, "agent": "mel", "door": "host", "text": ["hi"]}),
    )
    .unwrap();
    let said = call(
        c,
        "wake.take",
        &json!({"holder": i64::from(std::process::id()), "session": session}),
    )
    .unwrap();
    let run = said["turn"]["run"].as_str().unwrap().to_owned();
    c.conn()
        .execute(
            "UPDATE wake_sessions SET door = ?2 WHERE session = ?1",
            [session, door],
        )
        .unwrap();
    let turn = c
        .conn()
        .query_row(
            "SELECT turn_id FROM wake_turns WHERE run = ?1",
            [&run],
            |r| r.get(0),
        )
        .unwrap();
    (run, turn)
}

fn now() -> i64 {
    crate::holder_link::now_ms()
}

/// The turn claimed for a host that is gone, then attached by the run's own process: running.
fn running(c: &mut Core, session: &str, turn: i64, kid: &Proc) {
    let seen = claims::revoke_epoch(c, "owner").unwrap();
    let claim = claim_run(c, session, "owner", turn, Some(OLD), seen, now())
        .unwrap()
        .claim_id;
    let pid = u32::try_from(kid.pid()).unwrap();
    let said = attach(c, claim, Some(session), pid, &kid.stamp(), now()).unwrap();
    assert_eq!(said, json!({"attached": true}));
}

fn read(path: PathBuf) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn item_file(r: &Rig, session: &str) -> Value {
    read(r.queue().join(format!("{}.json", file_key(session))))
}

fn turn_state(c: &Core, turn: i64) -> String {
    c.conn()
        .query_row("SELECT state FROM turns WHERE id = ?1", [turn], |r| {
            r.get(0)
        })
        .unwrap()
}

// --- U6: what refuses a rollback, and writes nothing ---------------------------------------------------------------------------

#[test]
fn each_refusal_of_the_rollback_is_found_on_its_own() {
    let r = switched(|_| {});
    let kid = Proc::new();
    let mut c = core(&r);
    // (a) a wake turn claimed and no process attached.
    let (_, turn) = taken(&mut c, "sa", "a:1", "host");
    let seen = claims::revoke_epoch(&c, "owner").unwrap();
    claim_run(&mut c, "sa", "owner", turn, Some(OLD), seen, now()).unwrap();
    let said = crate::wake::rollback::refusals(c.conn()).unwrap();
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].starts_with("(a)"), "{said:?}");
    // (b) a host-door run whose turn is running.
    let (_, turn) = taken(&mut c, "sb", "b:1", "host");
    running(&mut c, "sb", turn, &kid);
    let said = crate::wake::rollback::refusals(c.conn()).unwrap();
    assert!(
        said.iter()
            .any(|l| l.starts_with("(b)") && l.contains("sb")),
        "{said:?}"
    );
    // (d) a run-door run that is running with no start stamp on its claim.
    let kid2 = Proc::new();
    let (_, turn) = taken(&mut c, "sd", "d:1", "run");
    running(&mut c, "sd", turn, &kid2);
    assert!(
        !crate::wake::rollback::refusals(c.conn())
            .unwrap()
            .iter()
            .any(|l| l.starts_with("(d)")),
        "a run with its pid and stamp is handed over"
    );
    c.conn()
        .execute(
            "UPDATE claims SET pid_started = NULL WHERE conversation = 'sd'",
            [],
        )
        .unwrap();
    let said = crate::wake::rollback::refusals(c.conn()).unwrap();
    assert!(
        said.iter()
            .any(|l| l.starts_with("(d)") && l.contains("sd")),
        "{said:?}"
    );
    // (c) two wake runs of one conversation that are not waiting.
    let seen = claims::revoke_epoch(&c, "owner").unwrap();
    let t2 = claims::enqueue_turn(
        &mut c,
        &claims::NewTurn {
            conversation: "sc",
            agent: "mel",
            engine: "claude",
            source: "wake",
            person: "owner",
            prompt: "x",
            deadline: None,
            revoke_seen: seen,
        },
        now(),
    )
    .unwrap();
    let seen = claims::revoke_epoch(&c, "owner").unwrap();
    let t3 = claims::enqueue_turn(
        &mut c,
        &claims::NewTurn {
            conversation: "sc",
            agent: "mel",
            engine: "claude",
            source: "wake",
            person: "owner",
            prompt: "y",
            deadline: None,
            revoke_seen: seen,
        },
        now(),
    )
    .unwrap();
    for t in [t2, t3] {
        c.conn()
            .execute("UPDATE turns SET state = 'running' WHERE id = ?1", [t])
            .unwrap();
    }
    let said = crate::wake::rollback::refusals(c.conn()).unwrap();
    assert!(
        said.iter()
            .any(|l| l.starts_with("(c)") && l.contains("sc")),
        "{said:?}"
    );
}

#[test]
fn a_refused_rollback_writes_nothing_and_leaves_the_queue_the_cores() {
    let r = switched(|_| {});
    let kid = Proc::new();
    {
        let mut c = core(&r);
        let (_, turn) = taken(&mut c, "sd", "d:1", "run");
        running(&mut c, "sd", turn, &kid);
        c.conn()
            .execute(
                "UPDATE claims SET pid_started = NULL WHERE conversation = 'sd'",
                [],
            )
            .unwrap();
    }
    let e = err(r.rollback(r.wake(), opts()));
    assert!(
        e.contains("(d)") && e.contains("--force does not override"),
        "{e}"
    );
    // Not even the rolling-back record: the core still serves, the tombstone stays, no file was written.
    assert_eq!(r.phase(), Some(Phase::Switched));
    assert!(tombstone::is_tombstone(&r.tomb()));
    assert!(!r.queue().join(format!("{}.json", file_key("sd"))).exists());
    assert!(r.serves());
    // --force does not get past it either.
    let forced = r.rollback(
        r.wake(),
        crate::cutover::Opts {
            force: true,
            ..opts()
        },
    );
    assert!(err(forced).contains("(d)"));
}

#[test]
fn a_running_host_refuses_a_rollback_whatever_force_says() {
    let r = switched(|_| {});
    let _host = r.running_host();
    for force in [false, true] {
        let e = err(r.rollback(r.wake(), crate::cutover::Opts { force, ..opts() }));
        assert!(
            e.contains("the host is running") && e.contains("--force does not override"),
            "{e}"
        );
    }
    assert_eq!(r.phase(), Some(Phase::Switched));
}

// --- U6: what the rollback writes ----------------------------------------------------------------------------------------------

#[test]
fn a_waiting_run_comes_back_owed_with_the_same_tries_and_its_turn_is_cancelled() {
    let r = switched(|_| {});
    let turn = {
        let mut c = core(&r);
        let (run, turn) = taken(&mut c, "s-wait", "w:1", "run");
        c.conn()
            .execute(
                "UPDATE wake_sessions SET tries = 2 WHERE session = 's-wait'",
                [],
            )
            .unwrap();
        assert_ne!(run, "");
        turn
    };
    let said = r.rollback(r.wake(), opts()).unwrap();
    assert!(
        said.iter()
            .any(|l| l.contains("1 run(s) that were waiting are owed again")),
        "{said:?}"
    );
    assert_eq!(r.phase(), None);
    assert!(!r.tomb().exists(), "Python reads its files again");
    let it = item_file(&r, "s-wait");
    assert_eq!(it["v"], 1);
    assert_eq!(it["tries"], 2, "a hand-back is not a failed try");
    assert_eq!(it["turn"], Value::Null);
    assert_eq!(it["requests"][0]["id"], "w:1");
    assert_eq!(it["requests"][0]["run"], Value::Null);
    let c = open_all(&r.env.layout);
    assert_eq!(turn_state(&c, turn), "cancelled");
    let links: i64 = c
        .conn()
        .query_row("SELECT count(*) FROM wake_turns", [], |r| r.get(0))
        .unwrap();
    assert_eq!(links, 0);
}

#[test]
fn a_running_run_goes_back_with_its_pid_and_start_and_the_real_kith_wake_leaves_it_alone() {
    let r = switched(|_| {});
    let mut kid = Proc::new();
    let run = {
        let mut c = core(&r);
        let (run, turn) = taken(&mut c, "s-run", "r:1", "run");
        running(&mut c, "s-run", turn, &kid);
        run
    };
    r.rollback(r.wake(), opts()).unwrap();
    let it = item_file(&r, "s-run");
    assert_eq!(it["turn"]["run"], run.as_str());
    assert_eq!(it["turn"]["pid"], kid.pid());
    assert_eq!(it["turn"]["procStart"], kid.stamp().as_str());
    assert_eq!(it["turn"]["sid"], "s-run", "the turn's conversation");
    assert_eq!(
        it["requests"][0]["run"],
        run.as_str(),
        "the request stays with its run"
    );
    // The real kith-wake: a deliverer that is running keeps the turn.
    // The claims are the Python's again for this check (a Mac that switched claims would ask a core that is not up).
    for dep in ["claims", "transcripts"] {
        let _ = std::fs::remove_file(
            r.env
                .layout
                .dir
                .join("cutover")
                .join(format!("{dep}.switch")),
        );
    }
    let drain = || {
        let kernel = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../kernel");
        let out = Command::new("python3")
            .arg(kernel.join("bin/kith-wake"))
            .arg("drain")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", r.dir.path().join("home"))
            .env("KITH_STATE", &r.state)
            .env("KITH_MACHINE", &r.machine)
            .env("KITH_HOME", r.dir.path().join("kith"))
            .env("KITH_WAKE_NO_KICK", "1")
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    drain();
    assert_eq!(
        item_file(&r, "s-run")["turn"]["run"],
        run.as_str(),
        "the pid lives: recover leaves the turn"
    );
    kid.kill();
    drain();
    let it = item_file(&r, "s-run");
    assert_eq!(
        it["turn"],
        Value::Null,
        "the holder is gone and its marker was not seen: owed again"
    );
    assert_eq!(it["requests"][0]["run"], Value::Null);
}

#[test]
fn stale_files_are_moved_aside_and_answered_ids_and_waiters_come_back_as_they_were() {
    let mut waiter = Value::Null;
    let r = switched(|r| {
        r.item("s-old", "run", &[("old:1", None)], None);
        r.item("s-keep", "run", &[("keep:1", None)], None);
        r.answered("done:1", "answered");
        waiter = r.waiter("w1", "s-keep");
    });
    // The core answers a request after the switch: the file from before the switch must not bring it back.
    {
        let mut c = core(&r);
        let said = call(&mut c, "wake.withdraw", &json!({"session": "s-old"})).unwrap();
        assert_eq!(said["count"], 1);
    }
    let stale = r.queue().join(format!("{}.json", file_key("s-old")));
    assert!(stale.exists(), "the pre-switch file is still there");
    r.rollback(r.wake(), opts()).unwrap();
    assert!(!stale.exists(), "the stale file is moved aside");
    let aside: Vec<_> = std::fs::read_dir(r.queue())
        .unwrap()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(".before-rollback-")
        })
        .collect();
    assert_eq!(aside.len(), 1);
    assert!(
        aside[0]
            .path()
            .join(format!("{}.json", file_key("s-old")))
            .exists()
    );
    let answered = read(r.queue().join("answered.json"));
    assert_eq!(
        answered["old:1"]["how"], "withdrawn",
        "the core's answer is in the file"
    );
    assert_eq!(answered["done:1"]["how"], "answered");
    assert_eq!(
        read(r.waits().join("w1.json")),
        waiter,
        "a waiter round-trips"
    );
    assert_eq!(item_file(&r, "s-keep")["requests"][0]["id"], "keep:1");
    // Python's own view of the files: the same queue the core held.
    let files = r.files();
    assert!(files.items.contains_key("s-keep") && !files.items.contains_key("s-old"));
    assert!(files.answered.contains_key("old:1"));
}

#[test]
fn a_rerun_after_a_crash_before_the_record_was_cleared_writes_the_same_files() {
    let r = switched(|r| {
        r.item("s1", "run", &[("a:1", None)], None);
        r.answered("done:1", "answered");
        r.waiter("w1", "s1");
    });
    let kid = Proc::new();
    {
        let mut c = core(&r);
        let (_, turn) = taken(&mut c, "s2", "b:1", "run");
        running(&mut c, "s2", turn, &kid);
    }
    r.rollback(r.wake(), opts()).unwrap();
    let snapshot = |r: &Rig| -> Vec<(String, String)> {
        let mut all = Vec::new();
        for d in [r.queue(), r.waits()] {
            for e in std::fs::read_dir(d).unwrap().flatten() {
                if e.path().is_file() && e.file_name().to_string_lossy().ends_with(".json") {
                    all.push((
                        e.file_name().to_string_lossy().into_owned(),
                        std::fs::read_to_string(e.path()).unwrap(),
                    ));
                }
            }
        }
        all.sort();
        all
    };
    let first = snapshot(&r);
    assert!(first.len() >= 4, "{first:?}");
    // The crash: the files are out, the tombstone is gone, and the record still says rolling-back (the marker is core.db's).
    {
        let mut c = open_all(&r.env.layout);
        super::wake::write_switched(&r.env.layout, &mut c, 7).unwrap();
        crate::cutover::state::write(
            &r.env.layout,
            "wake",
            &crate::cutover::state::Switch {
                phase: Phase::RollingBack,
                snapshot: None,
                at: 7,
            },
        )
        .unwrap();
    }
    r.rollback(r.wake(), opts()).unwrap();
    assert_eq!(r.phase(), None);
    assert_eq!(snapshot(&r), first, "the same rows give the same files");
}

// --- U10: step, rollback, step -------------------------------------------------------------------------------------------------

#[test]
fn a_second_step_after_a_rollback_leaves_no_waiting_wake_turn_without_a_run() {
    let r = switched(|r| r.item("s1", "run", &[("a:1", None)], None));
    {
        let mut c = core(&r);
        taken(&mut c, "s-w", "w:1", "host");
        taken(&mut c, "s-x", "x:1", "run");
    }
    r.rollback(r.wake(), opts()).unwrap();
    r.step(r.wake(), opts()).unwrap();
    assert_eq!(r.phase(), Some(Phase::Switched));
    assert_eq!(r.rows(), r.files());
    let c = open_all(&r.env.layout);
    let orphans: i64 = c
        .conn()
        .query_row(
            "SELECT count(*) FROM turns t WHERE t.source = 'wake' AND t.state IN ('queued', 'parked')
               AND NOT EXISTS (SELECT 1 FROM wake_turns w WHERE w.turn_id = t.id)",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(orphans, 0);
    let waiting: i64 = c
        .conn()
        .query_row(
            "SELECT count(*) FROM turns WHERE source = 'wake' AND state IN ('queued', 'parked')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        waiting, 0,
        "the waiting turns were cancelled by the rollback"
    );
    // Both requests are owed again after it.
    assert_eq!(r.rows().items.len(), 3);
    assert!(r.rows().items.values().all(|it| it.turn.is_none()));
}

/// WK6 model result: a turn that came in with {pid, procStart} and whose holder still runs is not settled by the rollback's
/// settle pass either (the core's probe is what holds it; preflight 1a.3/1a.4 are an earlier named refusal of the same thing).
#[test]
fn the_rollbacks_settle_pass_leaves_a_turn_whose_holder_runs() {
    let r = switched(|_| {});
    let kid = Proc::new();
    let mut c = core(&r);
    call(
        &mut c,
        "wake.enqueue",
        &json!({"session": "s-live", "id": "l:1", "agent": "mel", "door": "host", "text": ["hi"]}),
    )
    .unwrap();
    let said = call(
        &mut c,
        "wake.take",
        &json!({"holder": kid.pid(), "session": "s-live"}),
    )
    .unwrap();
    let run = said["turn"]["run"].as_str().unwrap().to_owned();
    let settled = crate::wake::settle_only(&mut c, r.env.paths.as_ref().unwrap()).unwrap();
    assert!(settled.is_empty(), "{settled:?}");
    let st = crate::wake::load(c.conn()).unwrap();
    let turn = st.items["s-live"].turn.clone().unwrap();
    assert_eq!(turn.run, run);
    assert_eq!(turn.pid, Some(kid.pid()));
    assert_eq!(turn.proc_start.as_deref(), Some(kid.stamp().as_str()));
}
