//! Two computers, each with a real core (the daemon the host tests start), and the host's half of the link between them: what the
//! pass sends and follows, what the callee answers, what a stop and a cut do. The only thing not real is the channel between the
//! two (the relay), which is [`Direct`]: a call of the callee's handler with the caller's pairing, or "off", or "not paired".

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use kith_kernel::paths::Paths;
use serde_json::{Value, json};

use super::callee::{self, Caller, row_id};
use super::fence::{self, Step};
use super::{Pass, Reach, Reached, Wire, unpaired};
use crate::core_front::{Answer, TestDaemon, call_host, call_owner, serve_for_test};
use crate::terminal::core_writer::CoreWriter;

/// One computer: a home, a core, a daemon.
struct Side {
    home: PathBuf,
    folders: Paths,
    daemon: Option<TestDaemon>,
}

impl Side {
    fn start() -> Self {
        // Short, for the core's socket; and with a `.kith` in it, or every home's state is the kernel checkout's.
        let home = PathBuf::from("/tmp").join(format!("kwp-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]));
        std::fs::create_dir_all(home.join(".kith")).expect("home");
        let folders = crate::kernel::folders(&home);
        std::fs::create_dir_all(folders.state.join("live/terminal")).expect("terminal claims folder");
        let stub = home.join("kith-stub");
        std::fs::write(&stub, "#!/bin/sh\n").expect("stub");
        std::fs::set_permissions(&stub, std::os::unix::fs::PermissionsExt::from_mode(0o755)).expect("stub mode");
        // SAFETY: set before this test starts a thread of its own; every test sets the same name.
        unsafe { std::env::set_var("KITH_STUB_BIN", &stub) };
        crate::core_front::step_claims_for_test(&folders, &home.join("kith/kernel")).expect("the claims step");
        let daemon = serve_for_test(&folders);
        daemon.reconciled();
        Self { home, folders, daemon: Some(daemon) }
    }

    fn owner(&self, method: &str, params: &Value) -> Value {
        match call_owner(&self.folders, method, params, false) {
            Answer::Done(v) => v,
            Answer::Refused(why) | Answer::Down { why, .. } => panic!("{method}: {why}"),
        }
    }

    fn host(&self, method: &str, params: &Value) -> Value {
        match call_host(&self.folders, method, params) {
            Answer::Done(v) => v,
            Answer::Refused(why) | Answer::Down { why, .. } => panic!("{method}: {why}"),
        }
    }

    /// A peer ask of `echo`, opened by `boss` in `sess-1` as `kith ask echo@mini` opens it.
    fn ask(&self, id: &str, extra: &Value) -> Value {
        let mut p = json!({"id": id, "asked": "echo", "mode": "peer", "peer": "mini", "task": "do it there", "crossing": true,
                           "home": self.home, "env": {"KITH_AGENT": "boss", "CLAUDE_CODE_SESSION_ID": "sess-1",
                                                      "KITH_VIA": "terminal", "KITH_STATE": self.folders.state}});
        for (k, v) in extra.as_object().into_iter().flatten() {
            p[k] = v.clone();
        }
        self.owner("ask.open", &p)
    }

    fn state(&self, id: &str) -> String {
        self.owner("ask.record", &json!({"id": id}))["state"].as_str().unwrap_or_default().to_owned()
    }

    fn rows(&self, id: &str) -> Vec<Value> {
        self.owner("ask.read", &json!({"id": id}))["rows"].as_array().cloned().unwrap_or_default()
    }

    fn queued(&self) -> Vec<Value> {
        CoreWriter::with_instance(self.folders.clone(), "kith-serve-host/1-a".to_owned())
            .queued_from(&["ask"])
            .expect("the core lists the ask turns")
    }

    /// The host starts the callee's turn and the callee attaches itself (the kernel's launch does, in a run): the turn is `running`
    /// with a session. Answers the claim.
    fn run(&self, turn: &Value, session: &str) -> i64 {
        let conversation = turn["conversation"].as_str().unwrap();
        let claimed = self.host(
            "hostclaim.claim",
            &json!({"conversation": conversation, "kind": "askRun", "turn": turn["turn"], "person": "owner",
                    "revokeSeen": 0, "holderInstance": "kith-serve-host/1-a"}),
        );
        let claim = claimed["claimId"].as_i64().unwrap_or_else(|| panic!("{claimed}"));
        let pid = std::process::id();
        let started = kith_kernel::procstart::start_stamp(pid).expect("a start stamp");
        // The launch takes the writer claim on the engine's session first; the attach that names the session needs it.
        self.owner(
            "claim.external",
            &json!({"conversation": session, "pid": pid, "started": started, "person": "", "agent": "echo"}),
        );
        let said = self.host(
            "claim.attach",
            &json!({"claimId": claim, "pid": pid, "started": started, "conversation": conversation, "session": session}),
        );
        assert_eq!(said["attached"], json!(true), "{said}");
        claim
    }

    /// The callee ended with `answer` (the out file's JSON), as the door tells the ask.
    fn finish(&self, id: &str, turn: &Value, answer: &str, session: &str) {
        let out = json!({"result": answer, "total_cost_usd": 0.25, "session_id": session}).to_string();
        // A refusal is an answer too (the ask was stopped: the core leaves it alone), as the door reads it.
        let _ = call_host(&self.folders, "ask.finish", &json!({"id": id, "turn": turn["turn"], "out": out}));
    }
}

impl Drop for Side {
    fn drop(&mut self) {
        if let Some(d) = self.daemon.take() {
            d.stop();
        }
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

/// The link between the two: B's handlers called as the relay would, with A's pairing; or off; or B no longer holding the pairing.
struct Direct {
    b: Paths,
    /// The epoch B's host still holds for A (`peers_in`); `None` once B unlinked.
    b_epoch: Mutex<Option<String>>,
    off: AtomicBool,
    /// A key B's host refuses the calls of (an item it would not take), while it answers every other.
    bad_key: Mutex<Option<String>>,
    calls: Mutex<Vec<String>>,
}

impl Direct {
    fn to(b: &Side) -> Self {
        Self {
            b: b.folders.clone(),
            b_epoch: Mutex::new(Some("e1".to_owned())),
            off: AtomicBool::new(false),
            bad_key: Mutex::default(),
            calls: Mutex::default(),
        }
    }

    fn calls(&self, method: &str) -> usize {
        self.calls.lock().unwrap().iter().filter(|m| *m == method).count()
    }
}

impl Wire for Direct {
    fn call<'a>(&'a self, _computer: &'a str, epoch: &'a str, method: &'static str, params: Value) -> Reached<'a> {
        Box::pin(async move {
            if self.off.load(Ordering::SeqCst) {
                return Err(Reach::Away("off".to_owned()));
            }
            self.calls.lock().unwrap().push(method.to_owned());
            if self.bad_key.lock().unwrap().as_deref().is_some_and(|k| params["key"] == json!(k)) {
                return Err(Reach::Refused("a parameter is not one B takes".to_owned()));
            }
            if self.b_epoch.lock().unwrap().as_deref() != Some(epoch) {
                return Ok(unpaired(method));
            }
            let caller = Caller { computer: "macA", name: "A", epoch };
            callee::handle(&self.b, &caller, method, &params, &|a| a == "echo", 1_000).await.map_err(Reach::Refused)
        })
    }
}

/// A and B, paired both ways under `e1`, A knowing B as `mini`.
fn pair() -> (Side, Side, Direct) {
    let (a, b) = (Side::start(), Side::start());
    a.host("ask.peerLink", &json!({"computer": "macB", "dir": "out", "epoch": "e1", "name": "mini"}));
    b.host("ask.peerLink", &json!({"computer": "macA", "dir": "in", "epoch": "e1", "name": "A"}));
    let wire = Direct::to(&b);
    (a, b, wire)
}

fn turn_of(b: &Side, ask: &str) -> Value {
    b.queued().into_iter().find(|t| t["ask"] == json!(ask)).unwrap_or_else(|| panic!("no queued turn of {ask}"))
}

fn on_b(key: &str) -> String {
    row_id(&Caller { computer: "macA", name: "A", epoch: "e1" }, key)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_foreground_ask_goes_over_the_link_and_the_answer_comes_back_once() {
    let (a, b, wire) = pair();
    let pass = Pass::default();
    a.ask("p1", &json!({}));
    pass.run(&a.folders, &wire).await;
    // B holds an ordinary background ask carrying where it came from; its callee runs as B's owner.
    let id = on_b("p1");
    assert_eq!(b.state(&id), "open");
    let turn = turn_of(&b, &id);
    assert!(turn["prompt"].as_str().unwrap().contains("Asked from A through kith ask"), "{turn}");
    assert_eq!(a.state("p1"), "open");
    b.run(&turn, "sess-b");
    b.finish(&id, &turn, "forty-two", "sess-b");
    pass.run(&a.folders, &wire).await;
    assert_eq!(a.state("p1"), "answered");
    // `ask.read` is the asker's one way to the rows: the answer once, then nothing.
    let rows = a.rows("p1");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!((rows[0]["kind"].as_str(), rows[0]["text"].as_str()), (Some("answer"), Some("forty-two")));
    pass.run(&a.folders, &wire).await;
    pass.run(&a.folders, &wire).await;
    assert!(a.rows("p1").is_empty(), "read already");
    assert_eq!(a.owner("ask.peerOwed", &json!({})), json!({"send": [], "follow": [], "stop": [], "to": []}));
    // What it cost there joined the chain here, once.
    assert!((a.owner("ask.record", &json!({"id": "p1"}))["cost"].as_f64().unwrap() - 0.25).abs() < 1e-9);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_send_that_is_made_again_is_the_same_ask() {
    let (a, b, wire) = pair();
    a.ask("p1", &json!({}));
    // A's host died after B took the send and before A noted it: the next pass sends it again.
    Pass::default().run(&a.folders, &wire).await;
    let before = b.queued().len();
    let params = json!({"key": "p1", "epoch": "e1", "agent": "echo", "task": "do it there"});
    let caller = Caller { computer: "macA", name: "A", epoch: "e1" };
    let said = callee::handle(&b.folders, &caller, "peerAsk", &params, &|_| true, 1_000).await.unwrap();
    assert_eq!(said["found"], json!(true), "{said}");
    assert_eq!(b.queued().len(), before, "no second turn");
    assert_eq!(b.queued().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_callee_that_is_off_is_waited_for_and_asked_again_when_it_is_back() {
    let (a, b, wire) = pair();
    let pass = Pass::default();
    a.ask("p1", &json!({}));
    wire.off.store(true, Ordering::SeqCst);
    pass.run(&a.folders, &wire).await;
    assert_eq!(a.state("p1"), "open");
    assert_eq!(b.queued(), Vec::<Value>::new());
    // One failed call, then the computer is left alone for a while.
    pass.run(&a.folders, &wire).await;
    wire.off.store(false, Ordering::SeqCst);
    let fresh = Pass::default();
    fresh.run(&a.folders, &wire).await;
    assert_eq!(b.queued().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn background_ask_then_wait_reads_the_rows_a_follow_copied() {
    let (a, b, wire) = pair();
    let pass = Pass::default();
    a.ask("p1", &json!({}));
    pass.run(&a.folders, &wire).await;
    let id = on_b("p1");
    let turn = turn_of(&b, &id);
    b.run(&turn, "sess-b");
    b.owner("ask.post", &json!({"id": id, "text": "halfway", "kind": "progress"}));
    pass.run(&a.folders, &wire).await;
    let first = a.rows("p1");
    assert_eq!(first.iter().map(|r| r["kind"].as_str().unwrap()).collect::<Vec<_>>(), ["progress"], "{first:?}");
    b.finish(&id, &turn, "done", "sess-b");
    pass.run(&a.folders, &wire).await;
    let last = a.rows("p1");
    assert_eq!(last.iter().map(|r| r["text"].as_str().unwrap()).collect::<Vec<_>>(), ["done"], "{last:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restart_of_either_host_mid_way_still_gives_one_answer() {
    let (a, b, wire) = pair();
    a.ask("p1", &json!({}));
    Pass::default().run(&a.folders, &wire).await;
    let id = on_b("p1");
    let turn = turn_of(&b, &id);
    b.run(&turn, "sess-b");
    // B's host restarted mid-run: its adopt keeps the claim, the run goes on and finishes; A has a new pass (A restarted too).
    Pass::default().run(&a.folders, &wire).await;
    b.finish(&id, &turn, "once", "sess-b");
    for _ in 0..3 {
        Pass::default().run(&a.folders, &wire).await;
    }
    let rows = a.rows("p1");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(a.state("p1"), "answered");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_to_reaches_the_session_of_the_ask_there_once() {
    let (a, b, wire) = pair();
    let pass = Pass::default();
    a.ask("p1", &json!({}));
    pass.run(&a.folders, &wire).await;
    let id = on_b("p1");
    let turn = turn_of(&b, &id);
    b.run(&turn, "sess-b");
    pass.run(&a.folders, &wire).await;
    let said = a.owner("ask.to", &json!({"id": "p1", "text": "and one more thing"}));
    assert_eq!(said["peer"], json!("macB"), "{said}");
    pass.run(&a.folders, &wire).await;
    assert_eq!(wire.calls("peerTo"), 1);
    assert_eq!(a.owner("ask.peerOwed", &json!({}))["to"], json!([]), "the computer took it");
    let queued = b.owner(
        "wake.queueStatus",
        &json!({"id": format!("ask-to:{id}/{}", super::short_hash(&["and one more thing"]))}),
    );
    assert_eq!(queued["state"], json!("queued"), "{queued}");
    // Again: nothing more is sent.
    pass.run(&a.folders, &wire).await;
    assert_eq!(wire.calls("peerTo"), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_end_mid_run_stops_the_run_there_and_nothing_reaches_the_asker() {
    let (a, b, wire) = pair();
    let pass = Pass::default();
    a.ask("p1", &json!({}));
    pass.run(&a.folders, &wire).await;
    let id = on_b("p1");
    let turn = turn_of(&b, &id);
    let claim = b.run(&turn, "sess-b");
    a.owner("ask.end", &json!({"roots": ["p1"]}));
    assert_eq!(a.state("p1"), "stopped");
    pass.run(&a.folders, &wire).await;
    // B's run is stopped: its claim is `ending`, which the door's waiter reads as a kill.
    assert_eq!(b.state(&id), "stopped");
    let shown = b.host("hostclaim.show", &json!({"claimId": claim}));
    assert_eq!(shown["claim"]["state"], json!("ending"), "{shown}");
    // The run's own end settles nothing and the asker hears nothing, whatever B says afterwards.
    b.finish(&id, &turn, "too late", "sess-b");
    pass.run(&a.folders, &wire).await;
    assert_eq!(a.rows("p1"), Vec::<Value>::new());
    assert_eq!(a.state("p1"), "stopped");
    assert_eq!(a.owner("ask.peerOwed", &json!({}))["stop"], json!([]), "B confirmed: nothing is owed");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stop_is_sent_again_until_the_computer_confirms() {
    let (a, b, wire) = pair();
    a.ask("p1", &json!({}));
    Pass::default().run(&a.folders, &wire).await;
    a.owner("ask.end", &json!({"roots": ["p1"]}));
    wire.off.store(true, Ordering::SeqCst);
    Pass::default().run(&a.folders, &wire).await;
    assert_eq!(a.owner("ask.peerOwed", &json!({}))["stop"].as_array().unwrap().len(), 1);
    wire.off.store(false, Ordering::SeqCst);
    Pass::default().run(&a.folders, &wire).await;
    assert_eq!(a.owner("ask.peerOwed", &json!({}))["stop"], json!([]));
    assert_eq!(b.state(&on_b("p1")), "stopped");
    assert_eq!(b.queued(), Vec::<Value>::new(), "the queued turn never starts");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_continued_ask_goes_on_the_session_of_the_one_before() {
    let (a, b, wire) = pair();
    let pass = Pass::default();
    a.ask("p1", &json!({}));
    pass.run(&a.folders, &wire).await;
    let first = turn_of(&b, &on_b("p1"));
    b.run(&first, "sess-b");
    b.finish(&on_b("p1"), &first, "one", "sess-b");
    pass.run(&a.folders, &wire).await;
    assert_eq!(a.state("p1"), "answered");
    a.ask("p2", &json!({"cont": "p1", "task": "and then"}));
    pass.run(&a.folders, &wire).await;
    let second = turn_of(&b, &on_b("p2"));
    assert_eq!(second["conversation"], json!("sess-b"), "the same session resumes there: {second}");
    assert_eq!(second["prompt"], json!("and then"), "a continued ask carries only its words");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unlink_on_b_mid_queue_starts_nothing_and_cuts_a_off() {
    let (a, b, wire) = pair();
    let pass = Pass::default();
    a.ask("p1", &json!({}));
    pass.run(&a.folders, &wire).await;
    let id = on_b("p1");
    let turn = turn_of(&b, &id);
    // B unlinks: the core first (`ask.endPeer`), then the host's row.
    b.host("ask.endPeer", &json!({"computer": "macA", "dir": "in"}));
    *wire.b_epoch.lock().unwrap() = None;
    let claimed = call_host(
        &b.folders,
        "hostclaim.claim",
        &json!({"conversation": turn["conversation"], "kind": "askRun", "turn": turn["turn"],
                "person": "owner", "revokeSeen": 0, "holderInstance": "kith-serve-host/1-a"}),
    );
    assert!(!matches!(&claimed, Answer::Done(v) if v["claimId"].is_i64()), "nothing starts");
    assert_eq!(b.state(&id), "refused");
    // A's next pass finds B no longer paired: the ask is cut off, once, with its notice.
    pass.run(&a.folders, &wire).await;
    assert_eq!(a.state("p1"), "cut_off");
    let rows = a.rows("p1");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(rows[0]["text"].as_str().unwrap().contains("cut off"), "{rows:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pairing_a_core_ended_and_a_host_row_that_remains_is_finished_by_the_reconcile() {
    // The crash between `ask.endPeer` and the `peers_in` delete: the core ended the epoch, the host still holds its row.
    let (_a, b, _wire) = pair();
    b.host("ask.endPeer", &json!({"computer": "macA", "dir": "in"}));
    let listed = b.host("ask.peerList", &json!({}));
    let held = vec![(("macA".to_owned(), "e1".to_owned()), "A".to_owned())];
    assert_eq!(fence::plan(&listed, &held, &[]), [Step::Remove { computer: "macA".to_owned(), dir: "in" }]);
    // The other way: the core holds a pairing the host dropped.
    let listed = {
        let (_a2, b2, _w) = pair();
        b2.host("ask.peerList", &json!({}))
    };
    assert_eq!(
        fence::plan(&listed, &[], &[]),
        [Step::End { computer: "macA".to_owned(), dir: "in", epoch: "e1".to_owned() },]
    );
    // A host row the core never heard of (a pairing from before the link carried asks) is made known to it.
    let none = json!({"in": [], "out": [], "ended": []});
    assert_eq!(
        fence::plan(&none, &held, &[]),
        [Step::Tell { computer: "macA".to_owned(), dir: "in", epoch: "e1".to_owned(), name: "A".to_owned() }]
    );
    // Both agree: nothing to do. A newer epoch of the core is never ended by a row of an older one.
    assert_eq!(fence::plan(&b.host("ask.peerList", &json!({})), &[], &[]), []);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_that_is_not_here_is_a_refusal_the_asker_reads() {
    let (a, _b, wire) = pair();
    let pass = Pass::default();
    a.ask("p1", &json!({"asked": "nobody"}));
    pass.run(&a.folders, &wire).await;
    assert_eq!(a.state("p1"), "refused");
    let rows = a.rows("p1");
    assert!(rows[0]["text"].as_str().unwrap().contains("no agent called nobody"), "{rows:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stop_that_comes_before_the_ask_leaves_nothing_to_start() {
    let (_a, b, _wire) = pair();
    let caller = Caller { computer: "macA", name: "A", epoch: "e1" };
    let stopped = callee::handle(&b.folders, &caller, "peerStop", &json!({"key": "p9"}), &|_| true, 1).await.unwrap();
    assert_eq!(stopped, json!({"paired": true, "state": "stopped"}));
    let params = json!({"key": "p9", "epoch": "e1", "agent": "echo", "task": "late"});
    let said = callee::handle(&b.folders, &caller, "peerAsk", &params, &|_| true, 1).await.unwrap();
    assert_eq!(said["state"], json!("stopped"), "{said}");
    assert_eq!(b.queued(), Vec::<Value>::new());
}

fn waiting_of(a: &Side, id: &str) -> Value {
    a.owner("ask.delegations", &json!({}))["delegations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["id"] == json!(id))
        .unwrap()["waiting"]
        .clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn one_failed_pass_shows_waiting_on_the_computer_and_a_good_one_clears_it_with_nothing_settled() {
    let (a, b, wire) = pair();
    let pass = Pass::default();
    a.ask("p1", &json!({}));
    a.ask("p2", &json!({}));
    assert_eq!(waiting_of(&a, "p1"), Value::Null);
    wire.off.store(true, Ordering::SeqCst);
    pass.run(&a.folders, &wire).await;
    // Both asks say so (the second was skipped behind the first failed call, and is shown all the same); no row, nothing settled.
    for id in ["p1", "p2"] {
        let w = waiting_of(&a, id);
        assert_eq!((&w["on"], &w["computer"]), (&json!("computer"), &json!("macB")), "{id}: {w}");
        assert_eq!(a.state(id), "open");
        assert!(a.rows(id).is_empty());
    }
    assert_eq!(b.queued(), Vec::<Value>::new());
    // The computer is back: the next pass that reaches it clears the mark.
    wire.off.store(false, Ordering::SeqCst);
    let fresh = Pass::default();
    fresh.run(&a.folders, &wire).await;
    for id in ["p1", "p2"] {
        assert_eq!(waiting_of(&a, id), Value::Null, "{id}");
        assert_eq!(a.state(id), "open");
    }
    assert_eq!(b.queued().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn one_item_the_computer_would_not_take_does_not_hold_up_the_rest() {
    let (a, b, wire) = pair();
    let pass = Pass::default();
    a.ask("p1", &json!({}));
    a.ask("p2", &json!({}));
    *wire.bad_key.lock().unwrap() = Some("p1".to_owned());
    pass.run(&a.folders, &wire).await;
    // p2 went over in the same pass; p1 stays owed, and is not shown as waiting on a computer that answered.
    assert_eq!(b.queued().len(), 1);
    assert_eq!(b.queued()[0]["ask"], json!(on_b("p2")));
    let owed = a.owner("ask.peerOwed", &json!({}));
    assert_eq!(owed["send"].as_array().unwrap().len(), 1, "{owed}");
    assert_eq!(owed["send"][0]["id"], json!("p1"));
    assert_eq!(waiting_of(&a, "p1"), Value::Null);
    // The computer was not marked away: the next pass tries p1 again at once, and goes through once B takes it.
    *wire.bad_key.lock().unwrap() = None;
    pass.run(&a.folders, &wire).await;
    assert_eq!(b.queued().len(), 2);
    assert_eq!(a.owner("ask.peerOwed", &json!({}))["send"], json!([]));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stale_not_paired_answer_never_ends_a_newer_pairing() {
    let (a, _b, _wire) = pair();
    a.ask("p1", &json!({}));
    // The pass read its items at e1; the owner linked again (e2) before the answer came back as "not paired".
    a.host("ask.peerLink", &json!({"computer": "macB", "dir": "out", "epoch": "e2", "name": "mini"}));
    assert_eq!(a.state("p1"), "cut_off", "the old epoch's ask went with it");
    a.ask("p2", &json!({}));
    Pass::default().cut_off(&a.folders, "macB", "e1").await;
    assert_eq!(a.state("p2"), "open", "the e2 pairing and its ask are left alone");
    let listed = a.host("ask.peerList", &json!({}));
    assert_eq!(listed["out"][0]["epoch"], json!("e2"), "{listed}");
    // The epoch that is live ends it.
    Pass::default().cut_off(&a.folders, "macB", "e2").await;
    assert_eq!(a.state("p2"), "cut_off");
}
