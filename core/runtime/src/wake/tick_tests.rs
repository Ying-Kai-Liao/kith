//! `wake.tick` and `wake.released` through the daemon's own dispatch (`Shared::dispatch`: the principal check, the table's
//! strict param check and the daemon's routing, which a stub `core-call` cannot see: lesson #1072), with a world the test
//! owns (clock, processes, `gh`, transcripts) in place of the system's.
//!
//! What only the daemon's own loops do (a launcher's exit being seen, `end_gone`) is set up through the same functions
//! they call. Every probe of the fake world notes whether the thread held the core lock (`lock_held`): the tick must read
//! no folder, transcript or `gh` under it.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kith_api::Principal;
use kith_kernel::paths::Paths;
use serde_json::{Value, json};

use super::outside::{Check, Civil, Outside, PrView, Transcripts};
use super::{CoreLock, World};
use crate::conversations::{claims, sched_run};
use crate::daemon::{Config, Daemon};
use crate::error::Error;
use crate::gate::Liveness;
use crate::tests::{machine, paths};

const HOST: &str = "kith-serve-host/1-a";

/// A world the test drives. Its probes are called by the tick with no lock held, from the thread that runs the tick.
struct Fake {
    now: Mutex<f64>,
    marker_reads: Mutex<Vec<String>>,
    markers: Mutex<Vec<(String, String)>>,
    view: Mutex<Option<PrView>>,
    checks: Mutex<Option<Vec<Check>>>,
    gh_delay: Mutex<Duration>,
    in_gh: AtomicUsize,
    /// Probes that ran while their thread held the core lock.
    under_lock: AtomicUsize,
    seq: AtomicU64,
    /// Runs once, at the tick's first look at the clock: the moment between its two phases.
    between: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    procs: Mutex<BTreeMap<i64, String>>,
}

impl Fake {
    fn new() -> Arc<Self> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        Arc::new(Self {
            now: Mutex::new(now),
            marker_reads: Mutex::new(Vec::new()),
            markers: Mutex::new(Vec::new()),
            view: Mutex::new(None),
            checks: Mutex::new(None),
            gh_delay: Mutex::new(Duration::ZERO),
            in_gh: AtomicUsize::new(0),
            under_lock: AtomicUsize::new(0),
            seq: AtomicU64::new(0),
            between: Mutex::new(None),
            procs: Mutex::new(BTreeMap::new()),
        })
    }

    fn world(self: &Arc<Self>) -> World {
        let f = self.clone();
        Arc::new(move |_: &Paths| {
            (
                Box::new(Out(f.clone())) as Box<dyn Outside>,
                Box::new(Tr(f.clone())) as Box<dyn Transcripts>,
            )
        })
    }

    fn probe(&self) {
        if crate::daemon::lock_held() {
            self.under_lock.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn advance(&self, secs: f64) {
        *self.now.lock().unwrap() += secs;
    }

    fn see_marker(&self, sid: &str, run: &str) {
        self.markers
            .lock()
            .unwrap()
            .push((sid.to_owned(), format!("[kith wake {run}]")));
    }
}

struct Out(Arc<Fake>);

impl Outside for Out {
    fn now(&self) -> f64 {
        let hook = self.0.between.lock().unwrap().take();
        if let Some(h) = hook {
            h();
        }
        *self.0.now.lock().unwrap()
    }
    fn new_id(&self) -> String {
        format!(
            "{:012x}",
            0xabc0_0000_0000_u64 + self.0.seq.fetch_add(1, Ordering::SeqCst)
        )
    }
    fn alive(&self, pid: i64, started: Option<&str>) -> bool {
        self.0.probe();
        self.0
            .procs
            .lock()
            .unwrap()
            .get(&pid)
            .is_some_and(|s| started.is_none_or(|w| w == s))
    }
    fn proc_start(&self, pid: i64) -> Option<String> {
        self.0.probe();
        self.0.procs.lock().unwrap().get(&pid).cloned()
    }
    fn writer_live(&self, _: &str) -> bool {
        self.0.probe();
        false
    }
    fn pr_view(&self, _: &str, _: i64) -> Option<PrView> {
        self.0.probe();
        self.0.in_gh.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(*self.0.gh_delay.lock().unwrap());
        self.0.view.lock().unwrap().clone()
    }
    fn pr_checks(&self, _: &str, _: i64) -> Option<Vec<Check>> {
        self.0.probe();
        self.0.checks.lock().unwrap().clone()
    }
    fn local_epoch(&self, _: Civil) -> Option<f64> {
        self.0.probe();
        None
    }
    fn ci_poll(&self) -> f64 {
        0.0
    }
}

struct Tr(Arc<Fake>);

impl Transcripts for Tr {
    fn has(&self, sid: &str, marker: &str) -> bool {
        self.0.probe();
        self.0
            .marker_reads
            .lock()
            .unwrap()
            .push(format!("{sid} {marker}"));
        self.0
            .markers
            .lock()
            .unwrap()
            .iter()
            .any(|(s, m)| s == sid && m == marker)
    }
}

/// Nothing runs: every launcher and group is gone (what `end_gone` asks).
struct Gone;

impl Liveness for Gone {
    fn start(&self, _: u32) -> Option<String> {
        None
    }
    fn gone(&self, _: u32) -> bool {
        true
    }
}

struct Rig {
    _t: tempfile::TempDir,
    d: Daemon,
    f: Arc<Fake>,
}

fn rig_with(lock_wait: Duration) -> Rig {
    let (t, m) = machine();
    let d = Daemon::spawn_with(
        &paths(&m),
        Config {
            holder_on: false,
            key_load: None,
            host: false,
            wait_for_embedded: Duration::from_secs(10),
            lock_wait,
            ..Config::default()
        },
    );
    d.ready(Duration::from_secs(10)).expect("the daemon serves");
    let f = Fake::new();
    d.shared().set_wake_world(f.world());
    Rig { _t: t, d, f }
}

fn rig() -> Rig {
    rig_with(Duration::from_secs(10))
}

fn ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .try_into()
        .unwrap()
}

impl Rig {
    fn call(&self, who: Principal, method: &str, params: &Value) -> Result<Value, Error> {
        self.d.shared().dispatch(who, method, params)
    }

    fn owner(&self, method: &str, params: &Value) -> Value {
        self.call(Principal::Owner, method, params)
            .unwrap_or_else(|e| panic!("{method}: {e}"))
    }

    fn host(&self, method: &str, params: &Value) -> Value {
        self.call(Principal::Host, method, params)
            .unwrap_or_else(|e| panic!("{method}: {e}"))
    }

    /// One tick, and the check that none of its probes ran under the core lock.
    fn tick(&self) -> Value {
        let v = self.host("wake.tick", &json!({}));
        assert_eq!(
            self.f.under_lock.load(Ordering::SeqCst),
            0,
            "a probe ran under the core lock"
        );
        v
    }

    fn enqueue(&self, session: &str, id: &str, door: &str) {
        self.enqueue_for(session, id, door, None);
    }

    fn enqueue_for(&self, session: &str, id: &str, door: &str, person: Option<&str>) {
        let mut p =
            json!({"session": session, "id": id, "agent": "echo", "door": door, "text": ["hi"]});
        if let Some(who) = person {
            p["person"] = json!(who);
        }
        self.owner("wake.enqueue", &p);
    }

    fn state_of(&self, id: &str) -> String {
        self.owner("wake.queueStatus", &json!({"id": id}))["state"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    fn sql<T: rusqlite::types::FromSql + Send + 'static>(&self, sql: &str, arg: &str) -> T {
        self.d
            .shared()
            .with_core(|c| Ok(c.conn().query_row(sql, [arg], |r| r.get(0))?))
            .unwrap()
    }

    fn exec(&self, sql: &str, arg: &str) {
        self.d
            .shared()
            .with_core(|c| {
                c.conn().execute(sql, [arg])?;
                Ok(())
            })
            .unwrap();
    }

    /// The run in flight for `session` and the turn that carries it.
    fn run_of(&self, session: &str) -> (String, i64) {
        let run: String = self.sql(
            "SELECT turn_run FROM wake_sessions WHERE session = ?1",
            session,
        );
        let turn: i64 = self.sql("SELECT turn_id FROM wake_turns WHERE run = ?1", &run);
        (run, turn)
    }

    fn turn_state(&self, turn: i64) -> String {
        self.sql("SELECT state FROM turns WHERE id = ?1", &turn.to_string())
    }

    fn tries(&self, session: &str) -> i64 {
        self.sql(
            "SELECT tries FROM wake_sessions WHERE session = ?1",
            session,
        )
    }

    fn turns_of(&self, session: &str) -> i64 {
        self.sql(
            "SELECT count(*) FROM turns WHERE conversation = ?1 AND source = 'wake'",
            session,
        )
    }

    /// The host claims the run's turn and the launcher attaches: the turn is `running` and was redeemed. Answers the claim.
    fn start(&self, session: &str, turn: i64, person: &str) -> i64 {
        let claimed = self.host(
            "hostclaim.claim",
            &json!({"conversation": session, "kind": "wakeRun", "turn": turn, "person": person,
                    "revokeSeen": 0, "holderInstance": HOST}),
        );
        let claim = claimed["claimId"]
            .as_i64()
            .unwrap_or_else(|| panic!("{claimed}"));
        let pid = std::process::id();
        let started =
            kith_kernel::procstart::start_stamp(pid).expect("the test process has a start stamp");
        let said = self
            .d
            .shared()
            .with_core(|c| sched_run::attach(c, claim, Some(session), pid, &started, ms()))
            .unwrap();
        assert_eq!(said, json!({"attached": true}));
        claim
    }

    /// The launcher's report, and `end_gone` once its group is gone: the claim ends and the turn is `done` (`done`) or
    /// `failed` (`exit`).
    fn finish(&self, claim: i64, kind: &str) {
        self.host(
            "hostclaim.report",
            &json!({"claimId": claim, "kind": kind,
                    "body": json!({"pid": std::process::id(), "out_of_usage": false}).to_string()}),
        );
        let ended = self
            .d
            .shared()
            .with_core(|c| claims::end_gone(c, &Gone, ms()))
            .unwrap();
        assert!(ended.contains(&claim), "{ended:?}");
    }

    /// Enqueues, ticks until the run has its turn, starts it and finishes it. Answers (run, turn).
    fn ran(&self, session: &str, id: &str, door: &str, kind: &str) -> (String, i64) {
        self.enqueue(session, id, door);
        self.tick();
        let (run, turn) = self.run_of(session);
        let claim = self.start(session, turn, "owner");
        self.finish(claim, kind);
        (run, turn)
    }

    /// A terminal holds the conversation.
    fn terminal(&self, session: &str) -> i64 {
        let pid = std::process::id();
        let started = kith_kernel::procstart::start_stamp(pid).unwrap();
        self.host(
            "hostclaim.claim",
            &json!({"conversation": session, "kind": "terminal", "person": "owner", "revokeSeen": 0,
                    "holderInstance": HOST, "pid": pid, "started": started}),
        )["claimId"]
            .as_i64()
            .unwrap()
    }

    fn end_claim(&self, claim: i64) {
        self.exec(
            "UPDATE claims SET state = 'ended', ended_at = 1 WHERE claim_id = ?1",
            &claim.to_string(),
        );
    }
}

// ---- the rows ----

#[test]
fn the_tick_is_the_hosts_and_released_the_owners_and_both_check_their_params() {
    let r = rig();
    assert!(matches!(
        r.call(Principal::Owner, "wake.tick", &json!({})),
        Err(Error::NotAllowed(_))
    ));
    assert!(matches!(
        r.call(Principal::Host, "wake.tick", &json!({"x": 1})),
        Err(Error::BadParams(_))
    ));
    assert!(matches!(
        r.call(Principal::Host, "wake.released", &json!({"session": "s"})),
        Err(Error::NotAllowed(_))
    ));
    assert!(matches!(
        r.call(Principal::Owner, "wake.released", &json!({})),
        Err(Error::BadParams(_))
    ));
    assert!(matches!(
        r.call(
            Principal::Owner,
            "wake.released",
            &json!({"session": "s", "x": 1})
        ),
        Err(Error::BadParams(_))
    ));
    // The wake rows served inside one `with_core` keep working.
    r.enqueue("s1", "r1", "run");
    assert_eq!(r.state_of("r1"), "queued");
}

#[test]
fn a_tick_takes_what_is_owed_as_one_run_and_one_turn_and_a_second_tick_adds_none() {
    let r = rig();
    r.enqueue("s1", "r1", "run");
    r.enqueue("s1", "r2", "run");
    r.enqueue("s2", "r3", "host");
    let v = r.tick();
    assert_eq!(v["taken"].as_array().unwrap().len(), 2, "{v}");
    assert_eq!(r.turns_of("s1"), 1);
    assert_eq!(r.turns_of("s2"), 1);
    let (run, turn) = r.run_of("s1");
    assert_eq!(r.turn_state(turn), "queued");
    assert_eq!(r.state_of("r1"), "running");
    assert_eq!(r.state_of("r2"), "running");
    assert_eq!(
        r.owner("wake.queueStatus", &json!({"id": "r1"}))["run"],
        run
    );
    // Nothing a repeat could make: the run is its own holder, with no pid.
    let again = r.tick();
    assert_eq!(again["taken"], json!([]), "{again}");
    assert_eq!(r.turns_of("s1"), 1);
}

#[test]
fn two_ticks_at_once_make_one_turn_per_run() {
    let r = rig();
    for i in 0..4 {
        r.enqueue(&format!("s{i}"), &format!("r{i}"), "run");
    }
    std::thread::scope(|s| {
        for _ in 0..3 {
            s.spawn(|| r.host("wake.tick", &json!({})));
        }
    });
    for i in 0..4 {
        assert_eq!(r.turns_of(&format!("s{i}")), 1, "s{i}");
    }
    assert_eq!(r.f.under_lock.load(Ordering::SeqCst), 0);
}

#[test]
fn a_session_a_terminal_holds_gets_its_turn_parked_without_reading_a_claim_file() {
    let r = rig();
    let terminal = r.terminal("s1");
    r.enqueue("s1", "r1", "run");
    r.tick();
    let (_, turn) = r.run_of("s1");
    assert_eq!(r.turn_state(turn), "parked");
    assert_eq!(
        r.sql::<String>(
            "SELECT parked_reason FROM turns WHERE id = ?1",
            &turn.to_string()
        ),
        "terminal_live"
    );
    // The terminal lets go and the next tick wakes it (the backstop of `wake.released`).
    r.end_claim(terminal);
    let v = r.tick();
    assert_eq!(v["unparked"], json!([turn]), "{v}");
    assert_eq!(r.turn_state(turn), "queued");
}

// ---- R4, one test per row ----

#[test]
fn r4_a_done_turn_answers_the_run() {
    let r = rig();
    let (run, turn) = r.ran("s1", "r1", "run", "done");
    assert_eq!(r.turn_state(turn), "done");
    let v = r.tick();
    assert_eq!(
        v["settled"],
        json!([{"run": run, "how": "answered"}]),
        "{v}"
    );
    assert_eq!(r.state_of("r1"), "answered");
    // The run is gone from the queue, and its link with it.
    assert_eq!(
        r.sql::<i64>("SELECT count(*) FROM wake_turns WHERE run = ?1", &run),
        0
    );
    assert!(
        r.f.marker_reads.lock().unwrap().is_empty(),
        "a done turn needs no transcript"
    );
}

#[test]
fn r4_redeemed_with_no_transcript_to_read_is_answered_and_never_sent_twice() {
    let r = rig();
    // A bot chat's thread is not a Claude session: there is no transcript.
    let (run, turn) = r.ran("acp:chat:thread", "r1", "host", "exit");
    assert_eq!(r.turn_state(turn), "failed");
    let v = r.tick();
    assert_eq!(
        v["settled"],
        json!([{"run": run, "how": "answered"}]),
        "{v}"
    );
    assert_eq!(r.state_of("r1"), "answered");
    assert!(r.f.marker_reads.lock().unwrap().is_empty());
}

#[test]
fn r4_redeemed_with_its_marker_in_the_transcript_is_recovered() {
    let r = rig();
    let (run, _) = r.ran("s1", "r1", "run", "exit");
    r.f.see_marker("s1", &run);
    let v = r.tick();
    assert_eq!(
        v["settled"],
        json!([{"run": run, "how": "recovered"}]),
        "{v}"
    );
    assert_eq!(r.state_of("r1"), "recovered");
}

#[test]
fn r4_a_fenced_conversation_is_withdrawn_not_failed_and_not_tried() {
    let r = rig();
    r.enqueue("s1", "r1", "run");
    r.tick();
    let (run, turn) = r.run_of("s1");
    // The ask of the conversation was stopped: its claim is refused and the core cancels the turn.
    r.exec(
        "INSERT INTO asks (id, chain, conversation, state) VALUES ('a1', 'c1', ?1, 'stopped')",
        "s1",
    );
    let refused = r.host(
        "hostclaim.claim",
        &json!({"conversation": "s1", "kind": "wakeRun", "turn": turn, "person": "owner",
                "revokeSeen": 0, "holderInstance": HOST}),
    );
    assert_eq!(refused["refused"], "stopped", "{refused}");
    assert_eq!(r.turn_state(turn), "cancelled");
    let v = r.tick();
    assert_eq!(
        v["settled"],
        json!([{"run": run, "how": "withdrawn"}]),
        "{v}"
    );
    assert_eq!(r.state_of("r1"), "withdrawn");
}

#[test]
fn r4_the_marker_is_looked_at_before_the_fence_so_a_delivered_request_is_never_withdrawn() {
    let r = rig();
    let (run, _) = r.ran("s1", "r1", "run", "exit");
    r.exec(
        "INSERT INTO asks (id, chain, conversation, state) VALUES ('a1', 'c1', ?1, 'stopped')",
        "s1",
    );
    r.f.see_marker("s1", &run);
    let v = r.tick();
    assert_eq!(
        v["settled"],
        json!([{"run": run, "how": "recovered"}]),
        "{v}"
    );
}

#[test]
fn r4_redeemed_with_no_marker_is_given_back_as_a_failed_try() {
    let r = rig();
    let (run, _) = r.ran("s1", "r1", "run", "exit");
    let v = r.tick();
    assert_eq!(v["settled"], json!([{"run": run, "how": "tried"}]), "{v}");
    assert_eq!(r.state_of("r1"), "queued");
    assert_eq!(r.tries("s1"), 1);
    // The marker was looked for, in the session's own transcript.
    assert_eq!(
        *r.f.marker_reads.lock().unwrap(),
        vec![format!("s1 [kith wake {run}]")]
    );
}

#[test]
fn r4_a_revoked_claim_with_no_marker_is_withdrawn_not_given_back() {
    let r = rig();
    r.enqueue_for("s1", "r1", "host", Some("ann"));
    r.tick();
    let (run, turn) = r.run_of("s1");
    let claim = r.start("s1", turn, "ann");
    r.d.shared()
        .with_core(|c| claims::revoke_person(c, "ann", ms()))
        .unwrap();
    r.d.shared()
        .with_core(|c| claims::end_gone(c, &Gone, ms()))
        .unwrap();
    assert_eq!(r.turn_state(turn), "cancelled");
    let _ = claim;
    let v = r.tick();
    assert_eq!(
        v["settled"],
        json!([{"run": run, "how": "withdrawn"}]),
        "{v}"
    );
    assert_eq!(r.state_of("r1"), "withdrawn");
}

#[test]
fn r4_never_redeemed_and_held_is_given_back_without_a_try() {
    let r = rig();
    r.enqueue("s1", "r1", "run");
    r.tick();
    let (run, turn) = r.run_of("s1");
    // The turn was refused as held and cancelled; a terminal has the conversation.
    r.exec(
        "UPDATE turns SET state = 'cancelled', finished_at = 1 WHERE id = ?1",
        &turn.to_string(),
    );
    r.terminal("s1");
    let v = r.tick();
    assert_eq!(v["settled"], json!([{"run": run, "how": "back"}]), "{v}");
    assert_eq!(r.tries("s1"), 0);
    assert_eq!(r.state_of("r1"), "queued");
    assert!(
        r.f.marker_reads.lock().unwrap().is_empty(),
        "never redeemed: nothing to look for"
    );
}

#[test]
fn r4_never_redeemed_and_not_held_is_a_failed_try_and_five_end_failed() {
    let r = rig();
    r.enqueue("s1", "r1", "run");
    for n in 1..=5 {
        let v = r.tick();
        assert_eq!(v["taken"].as_array().unwrap().len(), 1, "try {n}: {v}");
        let (run, turn) = r.run_of("s1");
        r.exec(
            "UPDATE turns SET state = 'cancelled', finished_at = 1 WHERE id = ?1",
            &turn.to_string(),
        );
        let v = r.tick();
        if n < 5 {
            assert_eq!(
                v["settled"],
                json!([{"run": run, "how": "tried"}]),
                "try {n}: {v}"
            );
            assert_eq!(r.tries("s1"), n);
            assert_eq!(r.state_of("r1"), "queued");
            // Not offered again before its pause.
            assert_eq!(r.tick()["taken"], json!([]));
            r.f.advance(700.0);
        } else {
            assert_eq!(r.state_of("r1"), "failed", "{v}");
        }
    }
    assert_eq!(r.turns_of("s1"), 5);
}

#[test]
fn r4_a_run_with_no_turn_link_keeps_the_old_rule() {
    let r = rig();
    r.enqueue("s1", "r1", "host");
    // `wake.take` (the host's old taker) holds the run by a pid that is not running; the link is what a run imported from
    // the files lacks.
    let taken = r.owner("wake.take", &json!({"holder": 424_242, "session": "s1"}));
    let run = taken["turn"]["run"].as_str().unwrap().to_owned();
    r.exec("DELETE FROM wake_turns WHERE run = ?1", &run);
    r.exec("DELETE FROM turns WHERE conversation = ?1", "s1");
    r.f.see_marker("s1", &run);
    let v = r.tick();
    assert_eq!(v["settled"], json!([]), "no link, no R4: {v}");
    assert_eq!(r.state_of("r1"), "recovered");
}

#[test]
fn r4_a_revoke_that_cancelled_a_turn_with_no_claim_ends_withdrawn_with_one_turn() {
    for stop_first in [true, false] {
        let r = rig();
        r.enqueue_for("s1", "r1", "host", Some("ann"));
        r.tick();
        let (_, turn) = r.run_of("s1");
        // `revoke_person` cancels the waiting turn and writes no end reason; the host's withdrawPerson comes after.
        r.d.shared()
            .with_core(|c| claims::revoke_person(c, "ann", ms()))
            .unwrap();
        assert_eq!(r.turn_state(turn), "cancelled");
        if stop_first {
            let said = r.owner("wake.withdrawPerson", &json!({"persons": ["ann"]}));
            assert_eq!(said["count"], 1, "{said}");
        }
        let v = r.tick();
        assert_eq!(v["taken"], json!([]), "{v}");
        assert_eq!(
            r.state_of("r1"),
            "withdrawn",
            "stop first: {stop_first}: {v}"
        );
        assert_eq!(r.turns_of("s1"), 1);
        assert_eq!(r.tries("s1"), 0);
    }
}

#[test]
fn a_turn_cancelled_with_its_claim_still_ending_is_settled_only_once_the_claim_ended() {
    let r = rig();
    r.enqueue_for("s1", "r1", "host", Some("ann"));
    r.tick();
    let (run, turn) = r.run_of("s1");
    r.start("s1", turn, "ann");
    r.d.shared()
        .with_core(|c| claims::revoke_person(c, "ann", ms()))
        .unwrap();
    assert_eq!(r.turn_state(turn), "cancelled");
    r.f.see_marker("s1", &run);
    // The engine still runs: nothing is read and nothing settled.
    let v = r.tick();
    assert_eq!(v["settled"], json!([]), "{v}");
    assert!(r.f.marker_reads.lock().unwrap().is_empty());
    assert_eq!(r.state_of("r1"), "running");
    r.d.shared()
        .with_core(|c| claims::end_gone(c, &Gone, ms()))
        .unwrap();
    let v = r.tick();
    assert_eq!(
        v["settled"],
        json!([{"run": run, "how": "recovered"}]),
        "{v}"
    );
}

#[test]
fn a_turn_that_ran_before_its_claim_was_cleared_still_counts_as_redeemed() {
    let r = rig();
    let (run, turn) = r.ran("s1", "r1", "run", "exit");
    // A requeue or a busy park clears `turns.claim_id`; a later cancel leaves a turn with no claim that once ran.
    r.exec(
        "UPDATE turns SET state = 'cancelled', claim_id = NULL WHERE id = ?1",
        &turn.to_string(),
    );
    r.f.see_marker("s1", &run);
    let v = r.tick();
    assert_eq!(
        v["settled"],
        json!([{"run": run, "how": "recovered"}]),
        "{v}"
    );
}

#[test]
fn a_run_with_no_link_and_no_holder_pid_is_a_dead_holder_and_a_link_without_a_turn_is_an_error() {
    let r = rig();
    r.enqueue("s1", "r1", "host");
    let taken = r.owner("wake.take", &json!({"holder": 424_242, "session": "s1"}));
    let run = taken["turn"]["run"].as_str().unwrap().to_owned();
    r.exec(
        "UPDATE wake_sessions SET turn_pid = NULL WHERE session = ?1",
        "s1",
    );
    // The link points at a turn that is not there: refused, not read as an unlinked run.
    r.exec("DELETE FROM turns WHERE conversation = ?1", "s1");
    assert!(matches!(
        r.call(Principal::Host, "wake.tick", &json!({})),
        Err(Error::Refused(_))
    ));
    r.exec("DELETE FROM wake_turns WHERE run = ?1", &run);
    r.f.see_marker("s1", &run);
    r.tick();
    assert_eq!(r.state_of("r1"), "recovered");
}

// ---- (b) the marker is read only for a turn that was final in the snapshot ----

#[test]
fn a_turn_that_goes_final_between_the_phases_is_settled_on_the_next_tick() {
    let r = rig();
    r.enqueue("s1", "r1", "run");
    r.tick();
    let (run, turn) = r.run_of("s1");
    let claim = r.start("s1", turn, "owner");
    assert_eq!(r.turn_state(turn), "running");
    r.f.see_marker("s1", &run);
    // Between the phases the launcher is reported and its group is gone: final now, not in the snapshot.
    std::thread::scope(|s| {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let (go, wait) = std::sync::mpsc::channel::<()>();
        *r.f.between.lock().unwrap() = Some(Box::new(move || {
            tx.send(()).unwrap();
            wait.recv().unwrap();
        }));
        let tick = s.spawn(|| r.tick());
        rx.recv().unwrap();
        r.finish(claim, "exit");
        go.send(()).unwrap();
        let v = tick.join().unwrap();
        assert_eq!(
            v["settled"],
            json!([]),
            "settled with a marker read before the turn was seen final: {v}"
        );
    });
    assert!(
        r.f.marker_reads.lock().unwrap().is_empty(),
        "the marker was read for a turn that was not final"
    );
    assert_eq!(r.state_of("r1"), "running");
    // The next tick has seen it final, reads the transcript after that, and settles.
    let v = r.tick();
    assert_eq!(
        v["settled"],
        json!([{"run": run, "how": "recovered"}]),
        "{v}"
    );
    assert_eq!(r.f.marker_reads.lock().unwrap().len(), 1);
}

// ---- (d) R7: a waiter a Stop removed between the phases does not fire ----

#[test]
fn a_waiter_that_a_stop_removed_between_the_phases_does_not_fire() {
    let r = rig();
    r.owner(
        "wake.wait",
        &json!({"on": "at:+30s", "session": "s1", "agent": "echo", "note": ["x"]}),
    );
    r.f.advance(3600.0);
    // The Stop lands after the tick has read the waiter and before it writes.
    let rig_ref = &r;
    std::thread::scope(|s| {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let (go, wait) = std::sync::mpsc::channel::<()>();
        *rig_ref.f.between.lock().unwrap() = Some(Box::new(move || {
            tx.send(()).unwrap();
            wait.recv().unwrap();
        }));
        let tick = s.spawn(|| rig_ref.tick());
        rx.recv().unwrap();
        let stopped = rig_ref.owner("wake.withdraw", &json!({"session": "s1"}));
        assert_eq!(stopped["count"], 1, "{stopped}");
        go.send(()).unwrap();
        let v = tick.join().unwrap();
        assert_eq!(v["fired"], 0, "{v}");
        assert_eq!(v["taken"], json!([]), "{v}");
    });
    let list = r.owner("wake.queueList", &json!({}));
    assert_eq!(list["items"], json!([]), "{list}");
    assert_eq!(list["waiters"], json!([]), "{list}");
}

#[test]
fn a_waiter_whose_thing_happened_fires_and_its_request_is_taken_in_the_same_tick() {
    let r = rig();
    r.owner(
        "wake.wait",
        &json!({"on": "at:+30s", "session": "s1", "agent": "echo", "note": ["x"]}),
    );
    r.f.advance(3600.0);
    let v = r.tick();
    assert_eq!(v["fired"], 1, "{v}");
    assert_eq!(v["taken"].as_array().unwrap().len(), 1, "{v}");
    assert_eq!(r.turns_of("s1"), 1);
}

// ---- (c) R5: a Stop reaches a run that has not started ----

fn two_owed(r: &Rig, session: &str) {
    r.enqueue(session, "r1", "run");
    r.enqueue(session, "r2", "run");
}

#[test]
fn r5_a_stop_of_a_queued_run_cancels_its_turn_withdraws_the_named_id_and_gives_the_rest_back() {
    let r = rig();
    two_owed(&r, "s1");
    r.tick();
    let (_, turn) = r.run_of("s1");
    assert_eq!(r.turn_state(turn), "queued");
    let said = r.owner("wake.withdraw", &json!({"session": "s1", "id": "r1"}));
    assert_eq!(said["count"], 1, "{said}");
    assert_eq!(r.turn_state(turn), "cancelled");
    assert_eq!(r.state_of("r1"), "withdrawn");
    assert_eq!(r.state_of("r2"), "queued");
    assert_eq!(r.tries("s1"), 0, "given back, not a failed try");
    assert_eq!(
        r.sql::<i64>("SELECT count(*) FROM wake_turns WHERE session = ?1", "s1"),
        0
    );
    // It is owed again and the next tick makes a new run and turn for it.
    let v = r.tick();
    assert_eq!(v["taken"].as_array().unwrap().len(), 1, "{v}");
}

#[test]
fn r5_a_stop_of_a_parked_run_cancels_its_turn() {
    let r = rig();
    r.terminal("s1");
    two_owed(&r, "s1");
    r.tick();
    let (_, turn) = r.run_of("s1");
    assert_eq!(r.turn_state(turn), "parked");
    let said = r.owner("wake.withdraw", &json!({"session": "s1"}));
    assert_eq!(said["count"], 2, "{said}");
    assert_eq!(r.turn_state(turn), "cancelled");
    assert_eq!(r.state_of("r1"), "withdrawn");
    assert_eq!(r.state_of("r2"), "withdrawn");
}

#[test]
fn r5_a_stop_of_a_claimed_run_with_no_process_ends_the_claim_and_cancels_the_turn() {
    let r = rig();
    two_owed(&r, "s1");
    r.tick();
    let (_, turn) = r.run_of("s1");
    let claimed = r.host(
        "hostclaim.claim",
        &json!({"conversation": "s1", "kind": "wakeRun", "turn": turn, "person": "owner",
                "revokeSeen": 0, "holderInstance": HOST}),
    );
    let claim = claimed["claimId"].as_i64().unwrap();
    assert_eq!(r.turn_state(turn), "claimed");
    r.owner("wake.withdraw", &json!({"session": "s1", "id": "r2"}));
    assert_eq!(r.turn_state(turn), "cancelled");
    assert_eq!(
        r.sql::<String>(
            "SELECT state FROM claims WHERE claim_id = ?1",
            &claim.to_string()
        ),
        "ended"
    );
    assert_eq!(r.state_of("r2"), "withdrawn");
    assert_eq!(r.state_of("r1"), "queued");
    // The launcher that would have attached is refused.
    let pid = std::process::id();
    let started = kith_kernel::procstart::start_stamp(pid).unwrap();
    let late =
        r.d.shared()
            .with_core(|c| sched_run::attach(c, claim, Some("s1"), pid, &started, ms()))
            .unwrap();
    assert_eq!(late["refused"], "gone", "{late}");
}

#[test]
fn r5_a_running_turn_is_left_alone_by_a_stop() {
    let r = rig();
    two_owed(&r, "s1");
    r.tick();
    let (_, turn) = r.run_of("s1");
    let claim = r.start("s1", turn, "owner");
    for stop in [
        ("wake.withdraw", json!({"session": "s1"})),
        ("wake.withdraw", json!({"session": "s1", "id": "r1"})),
    ] {
        let said = r.owner(stop.0, &stop.1);
        assert_eq!(said["count"], 0, "{said}");
    }
    assert_eq!(r.turn_state(turn), "running");
    assert_eq!(r.state_of("r1"), "running");
    assert_eq!(r.state_of("r2"), "running");
    let _ = claim;
}

#[test]
fn r5_a_stop_that_names_another_request_leaves_the_run_alone() {
    let r = rig();
    r.enqueue("s1", "r1", "run");
    r.tick();
    r.enqueue("s1", "r2", "run");
    let (_, turn) = r.run_of("s1");
    let said = r.owner("wake.withdraw", &json!({"session": "s1", "id": "r2"}));
    assert_eq!(said["count"], 1, "{said}");
    assert_eq!(r.turn_state(turn), "queued");
    assert_eq!(r.state_of("r1"), "running");
}

#[test]
fn r5_a_revoke_withdraws_a_persons_unstarted_run_and_the_ask_end_does_the_same() {
    let r = rig();
    r.enqueue_for("s1", "r1", "host", Some("ann"));
    r.enqueue_for("s2", "r2", "run", None);
    r.tick();
    let (_, t1) = r.run_of("s1");
    let (_, t2) = r.run_of("s2");
    let said = r.owner("wake.withdrawPerson", &json!({"persons": ["ann"]}));
    assert_eq!(said["count"], 1, "{said}");
    assert_eq!(r.turn_state(t1), "cancelled");
    assert_eq!(r.state_of("r1"), "withdrawn");
    // `kith ask --end`'s step (`withdraw_owed_in`, inside the stop's own transaction).
    r.d.shared()
        .with_core(|c| {
            let tx = c.claim_tx()?;
            let n = super::withdraw_owed_in(&tx, ms(), &["s2".to_owned()], &[])?;
            assert_eq!(n, 1);
            tx.commit()?;
            Ok(())
        })
        .unwrap();
    assert_eq!(r.turn_state(t2), "cancelled");
    assert_eq!(r.state_of("r2"), "withdrawn");
}

// ---- (e) nothing slow under the core lock ----

#[test]
fn a_gh_that_hangs_leaves_every_other_call_answered() {
    // The lock would answer `busy` after 400 ms; `gh` takes 2 s.
    let r = rig_with(Duration::from_millis(400));
    *r.f.gh_delay.lock().unwrap() = Duration::from_secs(2);
    r.owner(
        "wake.wait",
        &json!({"on": "ci:o/r#5", "session": "s1", "agent": "echo"}),
    );
    r.enqueue("s2", "r2", "run");
    std::thread::scope(|s| {
        let tick = s.spawn(|| r.tick());
        let begun = Instant::now();
        while r.f.in_gh.load(Ordering::SeqCst) == 0 {
            assert!(
                begun.elapsed() < Duration::from_secs(5),
                "the tick never reached gh"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        for _ in 0..5 {
            let at = Instant::now();
            let list = r.owner("wake.queueList", &json!({}));
            assert!(
                at.elapsed() < Duration::from_millis(300),
                "a call waited for the tick's gh"
            );
            assert!(list["waiters"].as_array().unwrap().len() == 1);
            r.owner(
                "wake.enqueue",
                &json!({"session": "s3", "id": "r3", "agent": "echo", "text": ["hi"]}),
            );
        }
        let v = tick.join().unwrap();
        // Its phase 2 still ran: the owed session was taken after the slow part.
        assert_eq!(v["taken"].as_array().unwrap().len(), 2, "{v}");
    });
    assert_eq!(r.f.under_lock.load(Ordering::SeqCst), 0);
}

// ---- (f) wake.released ----

#[test]
fn released_unparks_and_a_lost_one_is_unparked_by_the_next_tick() {
    let r = rig();
    for (session, lost) in [("s1", false), ("s2", true)] {
        let terminal = r.terminal(session);
        r.enqueue(session, &format!("r-{session}"), "run");
        r.tick();
        let (_, turn) = r.run_of(session);
        assert_eq!(r.turn_state(turn), "parked", "{session}");
        // While the terminal holds it, nothing wakes the turn.
        let said = r.owner("wake.released", &json!({"session": session}));
        assert_eq!(said["unparked"], json!([]), "{said}");
        assert_eq!(r.turn_state(turn), "parked");
        r.end_claim(terminal);
        if lost {
            // The call never arrives (a `kill -9`, the core down): the tick catches it.
            let v = r.tick();
            assert_eq!(v["unparked"], json!([turn]), "{v}");
        } else {
            let said = r.owner("wake.released", &json!({"session": session}));
            assert_eq!(said["unparked"], json!([turn]), "{said}");
        }
        assert_eq!(r.turn_state(turn), "queued", "{session}");
    }
}

// ---- the world ----

#[test]
fn gh_is_found_on_path_then_where_the_installers_put_it() {
    let t = tempfile::tempdir().unwrap();
    let bin = t.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let name = if cfg!(windows) { "gh.exe" } else { "gh" };
    std::fs::write(bin.join(name), "").unwrap();
    let path = std::env::join_paths([&bin]).unwrap();
    assert_eq!(super::outside::gh_in(Some(&path), None), bin.join(name));
    // Nothing on PATH: `%ProgramFiles%\GitHub CLI` is looked in.
    let pf = t.path().join("Program Files");
    let cli = pf.join("GitHub CLI");
    std::fs::create_dir_all(&cli).unwrap();
    std::fs::write(cli.join("gh.exe"), "").unwrap();
    let empty = std::env::join_paths([t.path().join("nothing")]).unwrap();
    assert_eq!(
        super::outside::gh_in(Some(&empty), Some(pf.as_os_str())),
        cli.join("gh.exe")
    );
}

#[test]
fn the_core_lock_trait_is_what_the_daemon_gives_the_tick() {
    // `tick` runs over any `CoreLock`: the lock is the caller's, taken once per phase.
    struct Counting(Arc<crate::daemon::Shared>, AtomicUsize);
    impl CoreLock for Counting {
        fn with_core<R>(
            &self,
            f: impl FnOnce(&mut crate::db::Core) -> Result<R, Error>,
        ) -> Result<R, Error> {
            self.1.fetch_add(1, Ordering::SeqCst);
            self.0.with_core(f)
        }
    }
    let r = rig();
    let counting = Counting(r.d.shared().clone(), AtomicUsize::new(0));
    super::tick(&counting, &json!({}), &r.f.world()).unwrap();
    assert_eq!(
        counting.1.load(Ordering::SeqCst),
        2,
        "one lock for each phase"
    );
}
