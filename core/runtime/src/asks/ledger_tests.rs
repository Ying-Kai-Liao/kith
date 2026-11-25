//! The ask ledger (P2-3, design 2026-11-24-ask-ledger, A1): one test per rule, through the daemon's own dispatch
//! (`Shared::dispatch`: the principal check, the table's strict param check and the routing, which a stub `core-call`
//! cannot see: lesson #1072), with a world the test owns (the process table) in place of the system's.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kith_api::Principal;
use kith_kernel::paths::Paths;
use serde_json::{Value, json};

use crate::conversations::{claims, sched_run};
use crate::daemon::{Config, Daemon};
use crate::error::Error;
use crate::gate::Liveness;
use crate::tests::{machine, paths};
use crate::wake::{Check, Civil, Outside, PrView, Transcripts, World};

const HOST: &str = "kith-serve-host/1-a";

/// The world: a process table the test fills, and the clock. Every probe notes whether its thread held the core lock.
pub(super) struct Fake {
    procs: Mutex<BTreeMap<i64, String>>,
    under_lock: AtomicUsize,
    seq: AtomicU64,
}

struct Out(Arc<Fake>);

impl Fake {
    fn probe(&self) {
        if crate::daemon::lock_held() {
            self.under_lock.fetch_add(1, Ordering::SeqCst);
        }
    }
}

impl Outside for Out {
    fn now(&self) -> f64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64()
    }
    fn new_id(&self) -> String {
        format!(
            "{:012x}",
            0xa5c0_0000_0000_u64 + self.0.seq.fetch_add(1, Ordering::SeqCst)
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
        false
    }
    fn pr_view(&self, _: &str, _: i64) -> Option<PrView> {
        None
    }
    fn pr_checks(&self, _: &str, _: i64) -> Option<Vec<Check>> {
        None
    }
    fn local_epoch(&self, _: Civil) -> Option<f64> {
        None
    }
    fn read_text(&self, path: &std::path::Path) -> Option<String> {
        self.0.probe();
        std::fs::read_to_string(path).ok()
    }
}

struct NoTranscripts;

impl Transcripts for NoTranscripts {
    fn has(&self, _: &str, _: &str) -> bool {
        false
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

/// The daemon goes first and the machine folder last: a daemon that outlives its folder cannot stop.
pub(super) struct Rig {
    pub(super) d: Daemon,
    pub(super) f: Arc<Fake>,
    pub(super) state: PathBuf,
    pub(super) tmp: tempfile::TempDir,
}

impl Rig {
    pub(super) fn new() -> Self {
        let (t, m) = machine();
        let d = Daemon::spawn_with(
            &paths(&m),
            Config {
                holder_on: false,
                key_load: None,
                host: false,
                wait_for_embedded: Duration::from_secs(10),
                lock_wait: Duration::from_secs(10),
                ..Config::default()
            },
        );
        d.ready(Duration::from_secs(10)).expect("the daemon serves");
        d.shared()
            .with_core(|c| claims::reconcile(c, &[], &[], i64::MAX, 1).map(|_| ()))
            .unwrap();
        let f = Arc::new(Fake {
            procs: Mutex::new(BTreeMap::new()),
            under_lock: AtomicUsize::new(0),
            seq: AtomicU64::new(0),
        });
        let world: World = {
            let f = f.clone();
            Arc::new(move |_: &Paths| {
                (
                    Box::new(Out(f.clone())) as Box<dyn Outside>,
                    Box::new(NoTranscripts) as Box<dyn Transcripts>,
                )
            })
        };
        d.shared().set_wake_world(world);
        let state = t.path().join("state");
        Self {
            d,
            f,
            state,
            tmp: t,
        }
    }

    pub(super) fn call(
        &self,
        who: Principal,
        method: &str,
        params: &Value,
    ) -> Result<Value, Error> {
        self.d.shared().dispatch(who, method, params)
    }

    pub(super) fn owner(&self, method: &str, params: &Value) -> Value {
        self.call(Principal::Owner, method, params)
            .unwrap_or_else(|e| panic!("{method}: {e}"))
    }

    pub(super) fn host(&self, method: &str, params: &Value) -> Value {
        self.call(Principal::Host, method, params)
            .unwrap_or_else(|e| panic!("{method}: {e}"))
    }

    /// One tick, and the check that none of its probes ran under the core lock.
    pub(super) fn tick(&self) -> Value {
        let v = self.host("wake.tick", &json!({}));
        assert_eq!(
            self.f.under_lock.load(Ordering::SeqCst),
            0,
            "a probe ran under the core lock"
        );
        v
    }

    /// The caller's view: agent `boss`, in session `sess-1`, from a terminal.
    pub(super) fn env(&self, extra: &[(&str, &str)]) -> Value {
        let mut e = json!({
            "KITH_STATE": self.state,
            "KITH_AGENT": "boss",
            "CLAUDE_CODE_SESSION_ID": "sess-1",
            "KITH_VIA": "terminal",
        });
        for (k, v) in extra {
            e[*k] = json!(v);
        }
        json!({"env": e})
    }

    pub(super) fn open(&self, id: &str, mode: &str, extra: &Value, env: &[(&str, &str)]) -> Value {
        let mut p = self.env(env);
        p["id"] = json!(id);
        p["asked"] = json!("echo");
        p["mode"] = json!(mode);
        for (k, v) in extra.as_object().into_iter().flatten() {
            p[k] = v.clone();
        }
        self.owner("ask.open", &p)
    }

    pub(super) fn open_bg(&self, id: &str) -> Value {
        self.open(id, "bg", &json!({"task": "do the thing"}), &[])
    }

    pub(super) fn sql<T: rusqlite::types::FromSql + Send + 'static>(
        &self,
        sql: &str,
        arg: &str,
    ) -> T {
        self.d
            .shared()
            .with_core(|c| Ok(c.conn().query_row(sql, [arg], |r| r.get(0))?))
            .unwrap()
    }

    pub(super) fn ask_state(&self, id: &str) -> String {
        self.sql("SELECT state FROM asks WHERE id = ?1", id)
    }

    pub(super) fn turn_state(&self, turn: i64) -> String {
        self.sql("SELECT state FROM turns WHERE id = ?1", &turn.to_string())
    }

    pub(super) fn rows(&self, id: &str) -> Vec<(String, bool)> {
        self.d
            .shared()
            .with_core(|c| {
                let mut st = c
                    .conn()
                    .prepare("SELECT kind, read FROM ask_rows WHERE ask = ?1 ORDER BY n")?;
                Ok(st
                    .query_map([id], |r| {
                        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? == 1))
                    })?
                    .collect::<Result<Vec<_>, _>>()?)
            })
            .unwrap()
    }

    pub(super) fn wake_state(&self, id: &str, n: i64) -> String {
        self.owner("wake.queueStatus", &json!({"id": format!("ask:{id}/{n}")}))["state"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    pub(super) fn finish(&self, id: &str, out: Option<&Value>) -> Value {
        // The host's report names its turn: the ask's current one.
        let turn: Option<i64> = self
            .d
            .shared()
            .with_core(|c| {
                Ok(c.conn()
                    .query_row("SELECT turn FROM asks WHERE id = ?1", [id], |r| r.get(0))?)
            })
            .unwrap();
        let mut p = json!({"id": id});
        if let Some(turn) = turn {
            p["turn"] = json!(turn);
        }
        if let Some(o) = out {
            p["out"] = json!(o.to_string());
        }
        self.owner("ask.finish", &p)
    }

    /// The host claims the ask's turn and the callee attaches itself: the turn is `running`. Answers the claim.
    pub(super) fn start(&self, id: &str, turn: i64) -> i64 {
        let claim = self.claim(id, turn);
        let pid = std::process::id();
        let started = kith_kernel::procstart::start_stamp(pid).expect("a start stamp");
        let said = self
            .d
            .shared()
            .with_core(|c| {
                sched_run::attach(c, claim, Some(&format!("ask:{id}")), pid, &started, ms())
            })
            .unwrap();
        assert_eq!(said, json!({"attached": true}));
        claim
    }

    /// The host claims the ask's turn: `claimed`, with no process yet.
    pub(super) fn claim(&self, id: &str, turn: i64) -> i64 {
        let claimed = self.host(
            "hostclaim.claim",
            &json!({"conversation": format!("ask:{id}"), "kind": "askRun", "turn": turn, "person": "owner",
                    "revokeSeen": 0, "holderInstance": HOST}),
        );
        claimed["claimId"]
            .as_i64()
            .unwrap_or_else(|| panic!("{claimed}"))
    }

    /// The launcher's report and `end_gone`: the claim ends and the turn is `failed` (`exit`) or `done` (`done`).
    pub(super) fn report(&self, claim: i64, kind: &str) {
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
}

pub(super) fn ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .try_into()
        .unwrap()
}

pub(super) fn answer(text: &str) -> Value {
    json!({"type": "result", "result": text, "total_cost_usd": 0.25, "session_id": "sid-9", "is_error": false})
}

pub(super) fn turn_of(opened: &Value) -> i64 {
    opened["turn"]
        .as_i64()
        .unwrap_or_else(|| panic!("{opened}"))
}

// ---- R1 ----

#[test]
fn a_bg_open_makes_one_turn_and_a_replay_makes_no_second() {
    let r = Rig::new();
    let o = r.open_bg("a1");
    let turn = turn_of(&o);
    assert_eq!(r.turn_state(turn), "queued");
    let (source, conversation, prompt, person): (String, String, String, String) =
        r.d.shared()
            .with_core(|c| {
                Ok(c.conn().query_row(
                    "SELECT source, conversation, prompt, person FROM turns WHERE id = ?1",
                    [turn],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )?)
            })
            .unwrap();
    assert_eq!(
        (
            source.as_str(),
            conversation.as_str(),
            prompt.as_str(),
            person.as_str()
        ),
        ("ask", "ask:a1", "do the thing", "owner")
    );
    assert!(o["out"].as_str().unwrap().ends_with("core/asks/a1.out"));
    // The same open again finds the one turn.
    let again = r.open_bg("a1");
    assert_eq!(again["replayed"], json!(true));
    assert_eq!(turn_of(&again), turn);
    let turns: i64 = r.sql("SELECT count(*) FROM turns WHERE source = ?1", "ask");
    assert_eq!(turns, 1);
    // The run door lists it for a host that asks for asks, and for no other.
    let listed = r.host("hostclaim.queued", &json!({"sources": ["ask"]}));
    let item = &listed["turns"][0];
    assert_eq!((&item["ask"], &item["turn"]), (&json!("a1"), &json!(turn)));
    assert_eq!(item["out"], o["out"]);
    assert_eq!(r.host("hostclaim.queued", &json!({}))["turns"], json!([]));
    // A fg ask makes no turn.
    r.open("f1", "fg", &json!({"cliPid": 1, "cliStart": "x"}), &[]);
    let turns: i64 = r.sql("SELECT count(*) FROM turns WHERE source = ?1", "ask");
    assert_eq!(turns, 1);
}

#[test]
fn a_bg_open_keeps_what_the_caller_asked_for_and_the_run_door_lists_it() {
    let r = Rig::new();
    let o = r.open(
        "o1",
        "bg",
        &json!({"task": "do the thing", "model": "opus", "effort": "high", "folder": "/work/x", "project": "kith"}),
        &[],
    );
    assert_eq!(r.turn_state(turn_of(&o)), "queued");
    let first = r.host("hostclaim.queued", &json!({"sources": ["ask"]}));
    let item = &first["turns"][0];
    assert_eq!(
        item["opts"],
        json!({"model": "opus", "effort": "high", "folder": "/work/x", "project": "kith"})
    );
    assert_eq!(item["askSession"], Value::Null);
    // An ask opened without them (a row from before the fields) lists none, and an empty word is not one.
    r.open("o2", "bg", &json!({"task": "t", "model": ""}), &[]);
    let listed = r.host("hostclaim.queued", &json!({"sources": ["ask"]}));
    let plain = listed["turns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["ask"] == "o2")
        .unwrap();
    assert_eq!(plain["opts"], Value::Null);
}

#[test]
fn the_run_door_covers_the_ask_turn() {
    let r = Rig::new();
    let turn = turn_of(&r.open_bg("a1"));
    let claim = r.claim("a1", turn);
    // The callee names its conversation, as a wake's does: without it the attach is refused.
    let pid = std::process::id();
    let started = kith_kernel::procstart::start_stamp(pid).unwrap();
    let without =
        r.d.shared()
            .with_core(|c| sched_run::attach(c, claim, None, pid, &started, ms()))
            .unwrap();
    assert_eq!(without["refused"], json!("gone"));
    let said =
        r.d.shared()
            .with_core(|c| sched_run::attach(c, claim, Some("ask:a1"), pid, &started, ms()))
            .unwrap();
    assert_eq!(said, json!({"attached": true}));
    assert_eq!(r.turn_state(turn), "running");
    // The record learns its run and its session at the attach.
    let (rec_claim, rec_pid, session): (i64, i64, String) =
        r.d.shared()
            .with_core(|c| {
                Ok(c.conn().query_row(
                    "SELECT claim_id, pid, session FROM asks WHERE id = 'a1'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?)
            })
            .unwrap();
    assert_eq!(
        (rec_claim, rec_pid, session.as_str()),
        (claim, i64::from(pid), "ask:a1")
    );
    // R3a: the launcher's exit report only sets the claim `ending`; `end_gone` ends it and the turn is `failed`.
    r.host(
        "hostclaim.report",
        &json!({"claimId": claim, "kind": "exit",
                "body": json!({"pid": pid, "out_of_usage": false}).to_string()}),
    );
    assert_eq!(r.turn_state(turn), "running");
    r.d.shared()
        .with_core(|c| claims::end_gone(c, &Gone, ms()))
        .unwrap();
    assert_eq!(r.turn_state(turn), "failed");
}

// ---- R3 ----

#[test]
fn a_finish_with_a_result_is_answered_with_a_row_and_a_wake() {
    let r = Rig::new();
    r.open_bg("a1");
    let done = r.finish("a1", Some(&answer("forty-two")));
    assert_eq!(
        (&done["settled"], &done["state"], &done["row"]),
        (&json!(true), &json!("answered"), &json!(1))
    );
    assert_eq!(r.ask_state("a1"), "answered");
    assert_eq!(r.rows("a1"), [("answer".to_owned(), false)]);
    assert_eq!(r.wake_state("a1", 1), "queued");
    let cost: f64 = r.sql("SELECT cost FROM asks WHERE id = ?1", "a1");
    assert!((cost - 0.25).abs() < 1e-9);
    let session: String = r.sql("SELECT session FROM asks WHERE id = ?1", "a1");
    assert_eq!(session, "sid-9");
    // The wake carries the answer to the asker's session.
    let item = &r.owner("wake.queueList", &json!({}))["items"][0];
    assert_eq!(item["session"], json!("sess-1"));
    assert_eq!(item["requests"][0]["text"], json!("forty-two"));
}

#[test]
fn a_finish_without_a_result_is_lost_with_a_row_and_a_wake() {
    let r = Rig::new();
    r.open_bg("a1");
    let done = r.finish("a1", None);
    assert_eq!(done["state"], json!("lost"));
    assert_eq!(r.ask_state("a1"), "lost");
    assert_eq!(r.rows("a1"), [("lost".to_owned(), false)]);
    assert_eq!(r.wake_state("a1", 1), "queued");
    let text: String = r.sql("SELECT text FROM ask_rows WHERE ask = ?1 AND n = 1", "a1");
    assert_eq!(text, "echo stopped before answering; its session is ask:a1");
    // The host's report and the tick settle once: the second finish does nothing, and makes no second row.
    let again = r.finish("a1", Some(&answer("late")));
    assert_eq!(again["settled"], json!(false));
    assert_eq!(r.rows("a1").len(), 1);
}

#[test]
fn a_finish_after_an_end_does_nothing() {
    let r = Rig::new();
    r.open_bg("a1");
    r.owner("ask.end", &json!({"roots": ["a1"]}));
    let done = r.finish("a1", Some(&answer("too late")));
    assert_eq!(done["settled"], json!(false));
    assert_eq!(r.ask_state("a1"), "stopped");
    assert_eq!(r.rows("a1"), []);
    assert_eq!(r.wake_state("a1", 1), "unknown");
    // A post to it keeps nothing either.
    let posted = r.owner("ask.post", &json!({"id": "a1", "text": "hi"}));
    assert_eq!(posted["stopped"], json!(true));
    assert_eq!(r.rows("a1"), []);
}

#[test]
fn a_fg_finish_prints_its_own_answer_so_it_gets_no_row_and_no_wake() {
    let r = Rig::new();
    r.open("f1", "fg", &json!({"cliPid": 4242, "cliStart": "s1"}), &[]);
    let done = r.owner(
        "ask.finish",
        &json!({"id": "f1", "result": "hello", "cost": 0.5, "session": "sid-1"}),
    );
    assert_eq!(done["state"], json!("answered"));
    assert_eq!(r.rows("f1"), []);
    assert_eq!(r.wake_state("f1", 1), "unknown");
    let delivered: i64 = r.sql("SELECT delivered FROM asks WHERE id = ?1", "f1");
    assert_eq!(delivered, 1);
    // A callee that failed settles `stopped`, as kith-ask did.
    r.open("f2", "fg", &json!({"cliPid": 4242, "cliStart": "s1"}), &[]);
    let bad = r.owner(
        "ask.finish",
        &json!({"id": "f2", "result": "boom", "isError": true}),
    );
    assert_eq!(bad["state"], json!("stopped"));
}

// ---- R5 ----

#[test]
fn a_read_withdraws_the_owed_wake() {
    let r = Rig::new();
    r.open_bg("a1");
    r.finish("a1", Some(&answer("forty-two")));
    let read = r.owner("ask.read", &json!({"id": "a1"}));
    assert_eq!(read["rows"].as_array().unwrap().len(), 1);
    assert_eq!(read["rows"][0]["text"], json!("forty-two"));
    assert_eq!(r.rows("a1"), [("answer".to_owned(), true)]);
    assert_eq!(r.wake_state("a1", 1), "withdrawn");
    // Nothing is owed any more: the tick takes no run, and a second read finds nothing.
    let ticked = r.tick();
    assert_eq!(ticked["taken"], json!([]));
    assert_eq!(r.owner("ask.read", &json!({"id": "a1"}))["rows"], json!([]));
    let woken: i64 = r.sql("SELECT count(*) FROM turns WHERE source = ?1", "wake");
    assert_eq!(woken, 0);
}

#[test]
fn a_wake_take_marks_the_row_read() {
    let r = Rig::new();
    r.open_bg("a1");
    r.finish("a1", Some(&answer("forty-two")));
    let ticked = r.tick();
    assert_eq!(ticked["taken"].as_array().unwrap().len(), 1, "{ticked}");
    assert_eq!(r.rows("a1"), [("answer".to_owned(), true)]);
    // The row came by the wake: `--wait` has nothing left.
    assert_eq!(r.owner("ask.read", &json!({"id": "a1"}))["rows"], json!([]));
    let delivered: i64 = r.sql("SELECT delivered FROM asks WHERE id = ?1", "a1");
    assert_eq!(delivered, 1);
}

#[test]
fn a_progress_post_waits_for_wait_and_an_answer_post_owes_a_wake() {
    let r = Rig::new();
    r.open_bg("a1");
    let p = r.owner(
        "ask.post",
        &json!({"id": "a1", "text": "half way", "kind": "progress", "as": "echo"}),
    );
    assert_eq!(p["row"], json!(1));
    assert_eq!(r.wake_state("a1", 1), "unknown");
    // The same row again is a duplicate.
    let dup = r.owner(
        "ask.post",
        &json!({"id": "a1", "text": "half way", "kind": "progress", "as": "echo"}),
    );
    assert_eq!(dup["row"], Value::Null);
    let a = r.owner("ask.post", &json!({"id": "a1", "text": "done"}));
    assert_eq!(a["row"], json!(2));
    assert_eq!(r.wake_state("a1", 2), "queued");
    let read = r.owner("ask.read", &json!({"id": "a1"}));
    assert_eq!(read["rows"].as_array().unwrap().len(), 2);
}

// ---- R6 ----

#[test]
fn an_end_cancels_a_queued_and_a_claimed_turn_and_ends_the_running_ones_claim() {
    let r = Rig::new();
    let queued = turn_of(&r.open_bg("a1"));
    let claimed = turn_of(&r.open_bg("a2"));
    let running = turn_of(&r.open_bg("a3"));
    let held = r.claim("a2", claimed);
    let live = r.start("a3", running);
    let ended = r.owner("ask.end", &json!({"roots": ["a1", "a2", "a3"]}));
    assert_eq!(ended["ended"], json!(["a1", "a2", "a3"]));
    assert_eq!(r.turn_state(queued), "cancelled");
    assert_eq!(r.turn_state(claimed), "cancelled");
    let claim_state = |id: i64| -> String {
        r.sql(
            "SELECT state FROM claims WHERE claim_id = ?1",
            &id.to_string(),
        )
    };
    assert_eq!(claim_state(held), "ended");
    // The running callee is not touched here: its claim is `ending`, and the kill goes to the host's waiter that holds it.
    assert_eq!(claim_state(live), "ending");
    let kills = ended["kill"].as_array().unwrap();
    assert_eq!(kills.len(), 1, "{ended}");
    assert_eq!(kills[0]["claimId"], json!(live));
    assert_eq!(r.ask_state("a3"), "stopped");
}

// ---- R4 ----

#[test]
fn the_tick_settles_an_ended_turn_that_had_no_report() {
    let r = Rig::new();
    let t1 = turn_of(&r.open_bg("a1"));
    let t2 = turn_of(&r.open_bg("a2"));
    let t3 = turn_of(&r.open_bg("a3"));
    // a1: the callee ran and died with nothing on its out file. a2: it wrote its answer, and the host never reported.
    let c1 = r.start("a1", t1);
    r.report(c1, "exit");
    let out2 = PathBuf::from(r.sql::<String>("SELECT out_path FROM asks WHERE id = ?1", "a2"));
    std::fs::create_dir_all(out2.parent().unwrap()).unwrap();
    std::fs::write(&out2, answer("from the file").to_string()).unwrap();
    let c2 = r.start("a2", t2);
    r.report(c2, "done");
    // a3 is still running: not settled.
    let c3 = r.claim("a3", t3);
    let _ = c3;
    let ticked = r.tick();
    let mut settled: Vec<&str> = ticked["asks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    settled.sort_unstable();
    assert_eq!(settled, ["a1", "a2"], "{ticked}");
    assert_eq!(r.ask_state("a1"), "lost");
    assert_eq!(r.ask_state("a2"), "answered");
    assert_eq!(r.ask_state("a3"), "open");
    // Each settled ask owes its asker; the tick that settled them took them on the next one.
    assert_eq!(r.rows("a1"), [("lost".to_owned(), false)]);
    assert_eq!(r.rows("a2"), [("answer".to_owned(), false)]);
    assert_eq!(r.wake_state("a2", 1), "queued");
    // A second tick settles nothing again.
    assert_eq!(r.tick()["asks"], json!([]));
}

#[test]
fn the_tick_settles_a_fg_ask_whose_pid_is_gone_and_leaves_one_whose_pid_lives() {
    let r = Rig::new();
    r.f.procs
        .lock()
        .unwrap()
        .insert(4243, "live-start".to_owned());
    r.open(
        "dead",
        "fg",
        &json!({"cliPid": 4242, "cliStart": "dead-start"}),
        &[],
    );
    r.open(
        "alive",
        "fg",
        &json!({"cliPid": 4243, "cliStart": "live-start"}),
        &[],
    );
    // Same pid, another process started since: that is gone too.
    r.open(
        "reused",
        "fg",
        &json!({"cliPid": 4243, "cliStart": "older-start"}),
        &[],
    );
    let ticked = r.tick();
    let mut settled: Vec<&str> = ticked["asks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    settled.sort_unstable();
    assert_eq!(settled, ["dead", "reused"], "{ticked}");
    assert_eq!(r.ask_state("dead"), "lost");
    assert_eq!(r.ask_state("alive"), "open");
    assert_eq!(r.rows("dead"), [("lost".to_owned(), false)]);
    assert_eq!(r.wake_state("dead", 1), "queued");
}

#[test]
fn the_tick_forgets_a_settled_ask_after_31_days() {
    let r = Rig::new();
    r.open_bg("old");
    r.finish("old", Some(&answer("x")));
    r.open_bg("fresh");
    r.finish("fresh", Some(&answer("y")));
    r.d.shared()
        .with_core(|c| {
            c.conn().execute(
                "UPDATE asks SET at_ms = 1, answered_ms = 1 WHERE id = 'old'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    r.tick();
    let n: i64 = r.sql("SELECT count(*) FROM asks WHERE id = ?1", "old");
    let rows: i64 = r.sql("SELECT count(*) FROM ask_rows WHERE ask = ?1", "old");
    let kept: i64 = r.sql("SELECT count(*) FROM asks WHERE id = ?1", "fresh");
    assert_eq!((n, rows, kept), (0, 0, 1));
}

// ---- R7 ----

#[test]
fn waiting_for_a_parked_grandchild_shows_on_the_root() {
    let r = Rig::new();
    r.open("root", "fg", &json!({"cliPid": 1, "cliStart": "x"}), &[]);
    r.open(
        "child",
        "fg",
        &json!({"cliPid": 1, "cliStart": "x"}),
        &[("KITH_ASK_ID", "root")],
    );
    r.open(
        "grand",
        "fg",
        &json!({"cliPid": 1, "cliStart": "x"}),
        &[("KITH_ASK_ID", "child")],
    );
    let waiting = |r: &Rig, id: &str| -> Value {
        let all = r.owner("ask.delegations", &json!({}));
        all["delegations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["id"] == json!(id))
            .unwrap()["waiting"]
            .clone()
    };
    // Nothing parked: each waits on the agent below it, and the leaf on nothing.
    assert_eq!(waiting(&r, "root")["on"], json!("agent"));
    assert_eq!(waiting(&r, "grand"), Value::Null);
    r.owner("ask.park", &json!({"id": "grand", "card": "card-1"}));
    for id in ["root", "child", "grand"] {
        let w = waiting(&r, id);
        assert_eq!(
            (&w["on"], &w["card"]),
            (&json!("card"), &json!("card-1")),
            "{id}"
        );
    }
    // The card's answer clears it up the chain.
    let resumed = r.owner("ask.unpark", &json!({"id": "grand", "note": "ok"}));
    let resume_turn = resumed["turn"].as_i64().unwrap();
    assert_eq!(waiting(&r, "root")["on"], json!("agent"));
    assert_eq!(waiting(&r, "grand"), Value::Null);
    // A parked ask whose run ended with no answer stays open (the card's answer resumes it).
    r.owner("ask.park", &json!({"id": "grand", "card": "card-2"}));
    let held = r.owner("ask.finish", &json!({"id": "grand", "turn": resume_turn}));
    assert_eq!(
        (&held["settled"], &held["parked"]),
        (&json!(false), &json!(true))
    );
    assert_eq!(r.ask_state("grand"), "open");
}

// ---- the table ----

#[test]
fn every_ledger_row_checks_its_params() {
    let r = Rig::new();
    for (method, who) in [
        ("ask.finish", Principal::Owner),
        ("ask.finish", Principal::Host),
        ("ask.post", Principal::Owner),
        ("ask.read", Principal::Owner),
        ("ask.park", Principal::Owner),
        ("ask.unpark", Principal::Owner),
        ("ask.open", Principal::Owner),
    ] {
        let unknown = r.call(who, method, &json!({"id": "x", "zzz": 1}));
        assert!(
            matches!(unknown, Err(Error::BadParams(_))),
            "{method}: {unknown:?}"
        );
        let none = r.call(who, method, &json!({}));
        assert!(
            matches!(none, Err(Error::BadParams(_))),
            "{method}: {none:?}"
        );
    }
    // The rows nothing but the owner may call.
    for method in ["ask.post", "ask.read"] {
        let denied = r.call(Principal::Host, method, &json!({"id": "x", "text": "t"}));
        assert!(
            matches!(denied, Err(Error::NotAllowed(_))),
            "{method}: {denied:?}"
        );
    }
    // Every param the table names reaches the code: a documented call is never refused as a param error.
    let ok = r.call(
        Principal::Owner,
        "ask.open",
        &json!({"id": "p1", "asked": "echo", "mode": "bg", "task": "t", "engine": "claude", "cliPid": 1, "cliStart": "s",
               "model": "m", "effort": "low", "folder": "/f", "project": "p"}),
    );
    assert!(ok.is_ok(), "{ok:?}");
    let bad_mode = r.call(
        Principal::Owner,
        "ask.open",
        &json!({"id": "p2", "asked": "echo", "mode": "sideways"}),
    );
    assert!(matches!(bad_mode, Err(Error::BadParams(_))), "{bad_mode:?}");
    let no_task = r.call(
        Principal::Owner,
        "ask.open",
        &json!({"id": "p3", "asked": "echo", "mode": "bg"}),
    );
    assert!(matches!(no_task, Err(Error::BadParams(_))), "{no_task:?}");
    for (method, params) in [
        (
            "ask.finish",
            json!({"id": "p1", "out": "{}", "result": "r", "cost": 1.0, "isError": true, "session": "s"}),
        ),
        (
            "ask.post",
            json!({"id": "p1", "text": "t", "as": "echo", "kind": "progress"}),
        ),
        ("ask.read", json!({"id": "p1"})),
        ("ask.park", json!({"id": "p1", "card": "c"})),
        ("ask.unpark", json!({"id": "p1"})),
    ] {
        let said = r.call(Principal::Owner, method, &params);
        assert!(
            !matches!(said, Err(Error::BadParams(_))),
            "{method}: {said:?}"
        );
    }
}

// ---- the card's resume is the ask's turn (R7 amended) ----

fn cost_of(r: &Rig, id: &str) -> f64 {
    r.sql("SELECT cost FROM asks WHERE id = ?1", id)
}

#[test]
fn a_parked_ask_is_not_settled_by_its_runs_end_even_with_words() {
    let r = Rig::new();
    let turn = turn_of(&r.open_bg("a1"));
    let parked = r.owner("ask.park", &json!({"id": "a1", "card": "c1"}));
    assert_eq!(parked["parked"], json!(true));
    // The parked run ends its turn with words ("waiting on a card"): that is not the answer.
    let said = r.owner(
        "ask.finish",
        &json!({"id": "a1", "turn": turn, "out": answer("I am waiting on a card").to_string()}),
    );
    assert_eq!(
        (&said["settled"], &said["parked"]),
        (&json!(false), &json!(true))
    );
    assert_eq!(r.ask_state("a1"), "open");
    assert_eq!(r.rows("a1"), []);
    assert_eq!(r.wake_state("a1", 1), "unknown");
    // Its cost joined the chain all the same.
    assert!((cost_of(&r, "a1") - 0.25).abs() < 1e-9);
    // The tick leaves a parked ask alone too, even with its turn ended.
    let claim = r.start("a1", turn);
    r.report(claim, "exit");
    assert_eq!(r.tick()["asks"], json!([]));
    assert_eq!(r.ask_state("a1"), "open");
}

#[test]
fn the_cards_answer_makes_the_resume_the_asks_turn_and_every_settle_goes_by_it() {
    let r = Rig::new();
    let first = turn_of(&r.open_bg("a1"));
    r.owner("ask.park", &json!({"id": "a1", "card": "c1"}));
    let un = r.owner(
        "ask.unpark",
        &json!({"id": "a1", "note": "yes, go on", "session": "sid-7"}),
    );
    let second = un["turn"].as_i64().unwrap_or_else(|| panic!("{un}"));
    assert_ne!(first, second);
    assert_ne!(un["out"], json!(null));
    let (source, conv, prompt): (String, String, String) =
        r.d.shared()
            .with_core(|c| {
                Ok(c.conn().query_row(
                    "SELECT source, conversation, prompt FROM turns WHERE id = ?1",
                    [second],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?)
            })
            .unwrap();
    assert_eq!(
        (source.as_str(), conv.as_str(), prompt.as_str()),
        ("ask", "ask:a1", "yes, go on")
    );
    let cur: i64 = r.sql("SELECT turn FROM asks WHERE id = ?1", "a1");
    assert_eq!(cur, second);
    // The first run's late report settles nothing, but its cost counts; the resume's report settles.
    let stale = r.owner(
        "ask.finish",
        &json!({"id": "a1", "turn": first, "out": answer("old").to_string()}),
    );
    assert_eq!(stale["settled"], json!(false));
    assert_eq!(r.ask_state("a1"), "open");
    let done = r.owner(
        "ask.finish",
        &json!({"id": "a1", "turn": second, "out": answer("the real answer").to_string()}),
    );
    assert_eq!(done["state"], json!("answered"));
    assert_eq!(r.rows("a1"), [("answer".to_owned(), false)]);
    assert_eq!(r.wake_state("a1", 1), "queued");
    assert!((cost_of(&r, "a1") - 0.5).abs() < 1e-9, "one cost per turn");
    // A stopped ask gets no turn from a card's answer.
    r.open_bg("a2");
    r.owner("ask.park", &json!({"id": "a2", "card": "c2"}));
    r.owner("ask.end", &json!({"roots": ["a2"]}));
    let none = r.owner("ask.unpark", &json!({"id": "a2", "note": "late"}));
    assert_eq!(none["turn"], json!(null));
}

#[test]
fn a_fg_ask_resumed_after_its_card_is_settled_by_its_turn_not_its_dead_cli() {
    let r = Rig::new();
    // The CLI exited with 3 (parked): its process is gone.
    r.open(
        "f1",
        "fg",
        &json!({"cliPid": 4242, "cliStart": "gone"}),
        &[],
    );
    r.owner("ask.park", &json!({"id": "f1", "card": "c1"}));
    let held = r.owner("ask.finish", &json!({"id": "f1", "result": "waiting"}));
    assert_eq!(held["parked"], json!(true));
    let un = r.owner("ask.unpark", &json!({"id": "f1", "note": "approved"}));
    let turn = un["turn"].as_i64().unwrap();
    // The tick does not settle it lost by the CLI pid: it has a turn.
    assert_eq!(r.tick()["asks"], json!([]));
    assert_eq!(r.ask_state("f1"), "open");
    // Its run's end settles it, and (no caller left to print) the asker gets the row and the wake.
    let done = r.owner(
        "ask.finish",
        &json!({"id": "f1", "turn": turn, "out": answer("done").to_string()}),
    );
    assert_eq!(done["state"], json!("answered"));
    assert_eq!(r.rows("f1"), [("answer".to_owned(), false)]);
    assert_eq!(r.wake_state("f1", 1), "queued");
}

#[test]
fn a_runs_cost_joins_the_chain_once_per_turn_also_after_a_stop() {
    let r = Rig::new();
    r.open_bg("a1");
    r.finish("a1", Some(&answer("x")));
    r.finish("a1", Some(&answer("x")));
    assert!((cost_of(&r, "a1") - 0.25).abs() < 1e-9);
    r.open_bg("a2");
    r.owner("ask.end", &json!({"roots": ["a2"]}));
    let said = r.finish("a2", Some(&answer("late")));
    assert_eq!(said["settled"], json!(false));
    assert!((cost_of(&r, "a2") - 0.25).abs() < 1e-9);
    r.finish("a2", Some(&answer("late")));
    assert!((cost_of(&r, "a2") - 0.25).abs() < 1e-9);
    assert_eq!(r.rows("a2"), []);
}

#[test]
fn only_an_open_ask_parks() {
    let r = Rig::new();
    r.open_bg("a1");
    r.owner("ask.end", &json!({"roots": ["a1"]}));
    let said = r.owner("ask.park", &json!({"id": "a1", "card": "c"}));
    assert_eq!(said["parked"], json!(false));
    let parked: Option<String> = r.sql("SELECT parked_card FROM asks WHERE id = ?1", "a1");
    assert_eq!(parked, None);
}

#[test]
fn a_report_that_names_no_turn_is_not_the_current_resumed_one() {
    let r = Rig::new();
    // The fg CLI never names a turn: the callee parks while running, the card is answered, then the CLI reports the parked run.
    r.open("f1", "fg", &json!({"cliPid": 1, "cliStart": "x"}), &[]);
    r.owner("ask.park", &json!({"id": "f1", "card": "c1"}));
    let un = r.owner("ask.unpark", &json!({"id": "f1", "note": "approved"}));
    let t2 = un["turn"].as_i64().unwrap();
    let said = r.owner(
        "ask.finish",
        &json!({"id": "f1", "result": "waiting words", "cost": 0.5}),
    );
    assert_eq!(said["settled"], json!(false));
    assert_eq!(r.ask_state("f1"), "open");
    assert_eq!(r.rows("f1"), []);
    // T2's own cost is still to be counted, once.
    let done = r.owner(
        "ask.finish",
        &json!({"id": "f1", "turn": t2, "out": answer("real").to_string()}),
    );
    assert_eq!(done["state"], json!("answered"));
    assert!((cost_of(&r, "f1") - 0.75).abs() < 1e-9);
}

#[test]
fn an_unpark_with_no_note_of_an_open_ask_is_refused() {
    let r = Rig::new();
    r.open_bg("a1");
    r.owner("ask.park", &json!({"id": "a1", "card": "c1"}));
    let refused = r.call(Principal::Owner, "ask.unpark", &json!({"id": "a1"}));
    assert!(matches!(refused, Err(Error::Refused(_))), "{refused:?}");
    let parked: Option<String> = r.sql("SELECT parked_card FROM asks WHERE id = ?1", "a1");
    assert_eq!(parked.as_deref(), Some("c1"));
    // An ended ask only clears the card.
    r.owner("ask.end", &json!({"roots": ["a1"]}));
    r.owner("ask.unpark", &json!({"id": "a1"}));
}

// ---- a lost report's cost is booked by the tick (cost only) ----

#[test]
fn the_tick_books_the_cost_of_a_parked_fg_run_whose_report_was_lost_once() {
    let r = Rig::new();
    // The CLI's own run: it parked, its process died with no report, and its answer file holds its words and cost.
    r.open(
        "f1",
        "fg",
        &json!({"cliPid": 4242, "cliStart": "gone"}),
        &[],
    );
    r.owner("ask.park", &json!({"id": "f1", "card": "c1"}));
    let dir = r.tmp.path().join("machine/core/asks");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("f1.out"), answer("waiting on a card").to_string()).unwrap();
    r.tick();
    assert!((cost_of(&r, "f1") - 0.25).abs() < 1e-9);
    // Cost only: still open, still parked, no row, no wake.
    assert_eq!(r.ask_state("f1"), "open");
    let parked: Option<String> = r.sql("SELECT parked_card FROM asks WHERE id = ?1", "f1");
    assert_eq!(parked.as_deref(), Some("c1"));
    assert_eq!(r.rows("f1"), []);
    assert_eq!(r.wake_state("f1", 1), "unknown");
    // A second tick books nothing more, and a late report of that run adds nothing.
    r.tick();
    assert!((cost_of(&r, "f1") - 0.25).abs() < 1e-9);
    let late = r.owner(
        "ask.finish",
        &json!({"id": "f1", "result": "waiting on a card", "cost": 0.25}),
    );
    assert_eq!(late["settled"], json!(false));
    assert!((cost_of(&r, "f1") - 0.25).abs() < 1e-9);
}

#[test]
fn the_tick_books_the_cost_of_a_parked_bg_turn_whose_report_was_lost_once() {
    let r = Rig::new();
    let o = r.open_bg("a1");
    let turn = turn_of(&o);
    r.owner("ask.park", &json!({"id": "a1", "card": "c1"}));
    // Its run is still going: nothing to book.
    let claim = r.start("a1", turn);
    r.tick();
    assert!(cost_of(&r, "a1").abs() < 1e-9);
    let out = PathBuf::from(o["out"].as_str().unwrap());
    std::fs::create_dir_all(out.parent().unwrap()).unwrap();
    std::fs::write(&out, answer("waiting on a card").to_string()).unwrap();
    r.report(claim, "done");
    r.tick();
    assert!((cost_of(&r, "a1") - 0.25).abs() < 1e-9);
    assert_eq!(r.ask_state("a1"), "open");
    assert_eq!(r.rows("a1"), []);
    r.tick();
    assert!((cost_of(&r, "a1") - 0.25).abs() < 1e-9);
    // The late report of that turn adds nothing.
    let late = r.owner(
        "ask.finish",
        &json!({"id": "a1", "turn": turn, "out": answer("waiting on a card").to_string()}),
    );
    assert_eq!(late["settled"], json!(false));
    assert!((cost_of(&r, "a1") - 0.25).abs() < 1e-9);
}

#[test]
fn a_host_report_that_names_no_turn_does_not_book_a_cost_the_tick_books_later() {
    let r = Rig::new();
    let o = r.open_bg("a1");
    let turn = turn_of(&o);
    r.owner("ask.park", &json!({"id": "a1", "card": "c1"}));
    let claim = r.start("a1", turn);
    let said = r.owner(
        "ask.finish",
        &json!({"id": "a1", "result": "waiting", "cost": 0.25}),
    );
    assert_eq!(said["settled"], json!(false));
    assert!(cost_of(&r, "a1").abs() < 1e-9);
    let out = PathBuf::from(o["out"].as_str().unwrap());
    std::fs::create_dir_all(out.parent().unwrap()).unwrap();
    std::fs::write(&out, answer("waiting").to_string()).unwrap();
    r.report(claim, "done");
    r.tick();
    r.tick();
    assert!((cost_of(&r, "a1") - 0.25).abs() < 1e-9);
}

#[test]
fn a_resumed_fg_asks_ended_turn_with_no_result_does_not_book_the_cli_runs_cost_again() {
    let r = Rig::new();
    r.open(
        "f1",
        "fg",
        &json!({"cliPid": 4242, "cliStart": "gone"}),
        &[],
    );
    r.owner("ask.park", &json!({"id": "f1", "card": "c1"}));
    let dir = r.tmp.path().join("machine/core/asks");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("f1.out"), answer("waiting").to_string()).unwrap();
    let un = r.owner("ask.unpark", &json!({"id": "f1", "note": "approved"}));
    let t2 = un["turn"].as_i64().unwrap();
    // The resume's engine was killed: its turn ended and its own out file never came.
    let claim = r.start("f1", t2);
    r.report(claim, "exit");
    r.tick();
    r.tick();
    // Only the CLI run's cost (key 0) was booked, once.
    assert!((cost_of(&r, "f1") - 0.25).abs() < 1e-9);
}

#[test]
fn a_bg_ask_on_a_named_conversation_has_its_lost_reports_cost_booked_once() {
    let r = Rig::new();
    let o = r.open(
        "n1",
        "bg",
        &json!({"task": "go", "conversation": "named-conv"}),
        &[],
    );
    let turn = turn_of(&o);
    let claimed = r.host(
        "hostclaim.claim",
        &json!({"conversation": "named-conv", "kind": "askRun", "turn": turn, "person": "owner",
                "revokeSeen": 0, "holderInstance": HOST}),
    );
    let claim = claimed["claimId"].as_i64().unwrap();
    let pid = std::process::id();
    let started = kith_kernel::procstart::start_stamp(pid).unwrap();
    r.d.shared()
        .with_core(|c| sched_run::attach(c, claim, Some("named-conv"), pid, &started, ms()))
        .unwrap();
    let out = PathBuf::from(o["out"].as_str().unwrap());
    std::fs::create_dir_all(out.parent().unwrap()).unwrap();
    std::fs::write(&out, answer("here").to_string()).unwrap();
    r.report(claim, "done");
    r.owner("ask.park", &json!({"id": "n1", "card": "c1"}));
    r.tick();
    r.tick();
    assert!((cost_of(&r, "n1") - 0.25).abs() < 1e-9);
}
