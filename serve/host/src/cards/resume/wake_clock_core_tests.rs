//! `WK4c` on a real core (the daemon the host tests start, as `wake/tests.rs`): the rows the host calls, not a script's answers. A
//! channel wake's turn is counted in `turns`, and a host restart is a second host instance's `adopt`.
//!
//! The launch's own steps (`claim.external` on the session, then `claim.attach` naming the work key) are rows of the claims
//! module: the test home has it switched (`TestDaemon::switched`), and a child process stands for the launch. The core's own
//! rules for them are also tested on its tables in `core/runtime/src/conversations/detached_tests.rs`, and the kernel's call
//! order in `core/kernel` `writer_turn_tests.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::super::channel_wake::{ChannelWake, Seen, attempt_over, unattached};
use super::begin;
use crate::api::rig::Rig;
use crate::core_front::{Answer, TestDaemon, call_owner, serve_for_test};
use crate::terminal::core_writer::{CoreWriter, RunClaim};
use crate::wake::{self, Request};
use kith_kernel::paths::Paths;
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const WK: &str = "tg:7#house";
/// The host instances of two starts of the host.
const FIRST: &str = "kith-serve-host/1-a";
const SECOND: &str = "kith-serve-host/2-b";

struct Real {
    home: PathBuf,
    folders: Paths,
    daemon: Option<TestDaemon>,
}

impl Real {
    fn start() -> Self {
        let home = PathBuf::from("/tmp").join(format!("kwk-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]));
        std::fs::create_dir_all(home.join(".kith")).expect("home");
        let folders = crate::kernel::folders(&home);
        // The launch's own rows (`claim.external`, `claim.attach`) answer only a switched `claims` module: the real step, as
        // `routines::launch_turn_e2e_tests` runs it. It wants a start stub to exist (nothing here runs it).
        let stub = PathBuf::from("/tmp").join(format!("kwk-stub-{}", std::process::id()));
        std::fs::write(&stub, "#!/bin/sh\n").expect("stub");
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("stub mode");
        // SAFETY: every test of this file sets the same name before it starts a thread of its own (one process each under nextest).
        unsafe { std::env::set_var("KITH_STUB_BIN", &stub) };
        std::fs::create_dir_all(folders.machine.join("live/terminal")).expect("tombstone dir");
        let kernel = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../kernel");
        let said = crate::core_front::step_claims_for_test(&folders, &kernel).expect("the claims step");
        assert!(said.iter().any(|l| l.contains("switched")), "{said:?}");
        let daemon = serve_for_test(&folders);
        crate::terminal::core_claims::enable(&folders.state, folders.clone());
        let dir = folders.machine.join("core/cutover");
        std::fs::create_dir_all(&dir).expect("cutover dir");
        std::fs::write(dir.join("wake.switch"), "state=switched\nat=1\n").expect("switch record");
        daemon.reconciled();
        Self { home, folders, daemon: Some(daemon) }
    }

    fn state(&self) -> &Path {
        &self.folders.state
    }

    fn writer(&self, instance: &str) -> Arc<CoreWriter> {
        Arc::new(CoreWriter::with_instance(self.folders.clone(), instance.to_owned()))
    }

    /// Rows of `turns` on `conversation`: (id, state).
    fn turns(&self, conversation: &str) -> Vec<(i64, String)> {
        self.daemon.as_ref().expect("the daemon").turns(conversation)
    }
}

impl Drop for Real {
    fn drop(&mut self) {
        if let Some(d) = self.daemon.take() {
            d.stop();
        }
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

async fn hub_on(core: &Real) -> Rig {
    let r = Rig::new().await;
    r.owner_bot("house");
    r.hub.terminal.set_env(crate::terminal::Env { state: core.state().to_path_buf(), sessions: None });
    r
}

/// A wake request on `session`, ticked into a turn, as the core lists it.
fn owed(core: &Real, session: &str) -> Value {
    let req = Request {
        session,
        id: "card:c1",
        agent: "house",
        person: None,
        room: None,
        host_door: true,
        why: "a card was answered",
        text: "go on",
    };
    wake::enqueue(core.state(), &req).unwrap();
    wake::tick(core.state()).unwrap();
    listed(core).into_iter().find(|t| t["conversation"] == session).expect("the wake's turn is queued")
}

fn listed(core: &Real) -> Vec<Value> {
    core.writer(FIRST).queued_from(&["wake"]).expect("queued")
}

async fn until(what: &str, f: impl Fn() -> bool) {
    for _ in 0..400 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("never happened: {what}");
}

fn claim_state(core: &Real, claim: i64) -> String {
    let row = core.writer(FIRST).claim_row(claim).expect("the core answers");
    row.and_then(|r| r["state"].as_str().map(str::to_owned)).unwrap_or_else(|| "none".to_owned())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_channel_wake_is_one_turn_claimed_on_the_work_key_and_a_launch_that_never_attached_frees_it() {
    let core = Real::start();
    let r = hub_on(&core).await;
    let turn = owed(&core, WK)["turn"].as_i64().unwrap();
    let RunClaim::Claimed(claim) = core.writer(FIRST).claim_run_as("wakeRun", WK, "", turn) else {
        panic!("the wake turn is claimed")
    };
    assert_eq!(core.turns(WK), [(turn, "claimed".to_owned())], "one turn row for the wake, claimed on the work key");
    assert!(listed(&core).is_empty(), "a claimed turn is not listed again, so nothing can begin it twice");
    // A second wake of the thread cannot be claimed while the first holds the key: one wake at a time.
    let again = core.writer(FIRST).claim_run_as("wakeRun", WK, "", turn);
    assert!(!matches!(again, RunClaim::Claimed(_)), "a claimed turn is not claimed twice");

    // The launch stopped before it attached: the claim is still `claimed` with no process, so the next attempt may carry it.
    let w = ChannelWake::for_test("run-1", turn, claim, WK);
    assert!(!attempt_over(&r.hub, &w, &Seen { pid: Some(std::process::id()), code: Some(1) }).await);
    // No attempt attached: no writer holds the session, so the turn is freed (cancelled, never redeemed).
    unattached(&r.hub, &w, false).await;
    until("the turn is cancelled", || core.turns(WK)[0].1 == "cancelled").await;
    assert_eq!(core.turns(WK).len(), 1, "still one turn row");
    assert!(wake::status(core.state(), "card:c1").as_deref() != Some("answered"), "never delivered, never answered");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_before_the_attach_requeues_the_turn_and_it_is_claimed_and_run_once() {
    let core = Real::start();
    let turn = owed(&core, WK)["turn"].as_i64().unwrap();
    let RunClaim::Claimed(first) = core.writer(FIRST).claim_run_as("wakeRun", WK, "", turn) else { panic!("claimed") };
    // The host restarts: its adopt gives back a claim nobody attached, and the in-memory batch is gone with it.
    core.writer(SECOND).adopt().expect("adopt");
    assert_eq!(claim_state(&core, first), "ended");
    let again = listed(&core);
    assert_eq!(again.len(), 1, "the turn is queued again");
    assert_eq!(again[0]["redeemedBefore"], false, "not redeemed: it may run");
    // The clock claims it again, once.
    let RunClaim::Claimed(second) = core.writer(SECOND).claim_run_as("wakeRun", WK, "", turn) else {
        panic!("claimed")
    };
    assert_ne!(first, second);
    assert_eq!(listed(&core), Vec::<Value>::new());
    assert_eq!(core.turns(WK).len(), 1, "one turn row for the whole story: {:?}", core.turns(WK));
}

/// A process that stands for a launch (or a warm stub): alive until the test kills it or drops it.
struct Child(std::process::Child);

impl Child {
    fn new() -> Self {
        Self(std::process::Command::new("sleep").arg("120").spawn().expect("sleep"))
    }

    fn pid(&self) -> u32 {
        self.0.id()
    }

    fn stamp(&self) -> String {
        match kith_kernel::procstart::probe(self.0.id()) {
            kith_kernel::procstart::Probe::Alive(Some(s)) => s,
            other => panic!("{other:?}"),
        }
    }

    fn kill(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        self.kill();
    }
}

/// One row as the launch asks it: the owner's, on the daemon's socket.
fn owner_row(core: &Real, method: &str, params: &Value) -> Value {
    match call_owner(&core.folders, method, params, false) {
        Answer::Done(v) => v,
        Answer::Refused(said) => panic!("{method} refused: {said}"),
        Answer::Down { why, .. } => panic!("{method}: {why}"),
    }
}

/// The launch's two steps in the order the kernel takes them: the writer claim on the session, then the attach of the wake's
/// claim naming the work key. Answers (the session's claim, the attach's answer).
fn launch(core: &Real, child: &Child, sid: &str, claim: i64) -> (i64, Value) {
    let (pid, started) = (child.pid(), child.stamp());
    let writer = owner_row(
        core,
        "claim.external",
        &json!({"conversation": sid, "pid": pid, "started": started, "person": "", "agent": "house"}),
    );
    let session = writer["claimId"].as_i64().unwrap_or_else(|| panic!("the session is taken: {writer}"));
    let attached =
        owner_row(core, "claim.attach", &json!({"claimId": claim, "conversation": WK, "pid": pid, "started": started}));
    (session, attached)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_channel_launch_attaches_the_wake_claim_on_the_work_key_after_its_session_and_the_turn_runs_once() {
    let core = Real::start();
    let turn = owed(&core, WK)["turn"].as_i64().unwrap();
    let RunClaim::Claimed(claim) = core.writer(FIRST).claim_run_as("wakeRun", WK, "", turn) else { panic!("claimed") };
    let child = Child::new();
    let (session, attached) = launch(&core, &child, "sid-1", claim);
    assert_eq!(attached, json!({"attached": true}));
    assert_ne!(session, claim, "the session's claim is its own, the wake's is the delivery record");
    assert_eq!(core.turns(WK), [(turn, "running".to_owned())], "one turn row for the whole story");
    assert_eq!(core.turns("sid-1"), Vec::<(i64, String)>::new(), "the session's claim made no turn");
    assert_eq!(claim_state(&core, claim), "running");
    // The pull does not list it again, and a second wake of the thread cannot be claimed while this one holds the key.
    assert_eq!(listed(&core), Vec::<Value>::new());
    let row = core.writer(FIRST).claim_row(claim).unwrap().unwrap();
    assert_eq!(row["pid"].as_u64(), Some(u64::from(child.pid())));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_before_the_attach_runs_the_turn_once_and_the_late_attach_is_refused() {
    let core = Real::start();
    let turn = owed(&core, WK)["turn"].as_i64().unwrap();
    let RunClaim::Claimed(first) = core.writer(FIRST).claim_run_as("wakeRun", WK, "", turn) else { panic!("claimed") };
    let child = Child::new();
    // The host restarted after the spawn: the unattached claim is given back and the turn queued again, not redeemed.
    core.writer(SECOND).adopt().expect("adopt");
    assert_eq!(claim_state(&core, first), "ended");
    assert_eq!(listed(&core).len(), 1, "queued again");
    assert_eq!(listed(&core)[0]["redeemedBefore"], false);
    // The old launch then asks to attach: refused (exit 10, no engine), and it took the session's claim only to give it back.
    let (pid, started) = (child.pid(), child.stamp());
    let late = owner_row(
        &core,
        "claim.attach",
        &json!({"claimId": first, "conversation": WK, "pid": pid, "started": started}),
    );
    assert_eq!(late["refused"], "gone", "{late}");
    // The clock claims it again and the next launch attaches: one run, one turn row.
    let RunClaim::Claimed(second) = core.writer(SECOND).claim_run_as("wakeRun", WK, "", turn) else {
        panic!("claimed again")
    };
    let (_, attached) = launch(&core, &child, "sid-1", second);
    assert_eq!(attached, json!({"attached": true}));
    assert_eq!(core.turns(WK), [(turn, "running".to_owned())]);
    assert_eq!(listed(&core), Vec::<Value>::new(), "never begun a second time");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_after_the_attach_leaves_the_claim_and_ends_it_when_the_group_is_gone_and_the_wake_is_answered() {
    let core = Real::start();
    let turn = owed(&core, WK)["turn"].as_i64().unwrap();
    let RunClaim::Claimed(claim) = core.writer(FIRST).claim_run_as("wakeRun", WK, "", turn) else { panic!("claimed") };
    let mut child = Child::new();
    let pid = child.pid();
    launch(&core, &child, "sid-1", claim);
    // The host restarts: an attached claim has a process, so adopt leaves it alone and the turn is not listed again.
    core.writer(SECOND).adopt().expect("adopt");
    assert_eq!(claim_state(&core, claim), "running");
    assert_eq!(listed(&core), Vec::<Value>::new(), "never begun again");
    // The launch's process goes (its pipes died with the old host): the second host's report ends the claim once the group is
    // gone, the turn is over, and the tick answers the wake (redeemed, no transcript to read).
    child.kill();
    assert!(matches!(
        core.writer(SECOND).run_over(claim, pid, false, "gone"),
        crate::terminal::core_writer::Told::Done
    ));
    until("the claim ends", || claim_state(&core, claim) == "ended").await;
    until("the wake is answered", || {
        let _ = wake::tick(core.state());
        wake::status(core.state(), "card:c1").as_deref() == Some("answered")
    })
    .await;
    assert_eq!(core.turns(WK).len(), 1, "one turn row, never begun again");
    assert_eq!(listed(&core), Vec::<Value>::new());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bot_turn_redeemed_and_cut_by_a_restart_is_cancelled_and_never_begun_again() {
    let core = Real::start();
    let r = hub_on(&core).await;
    let session = "c-bot1";
    let turn = owed(&core, session)["turn"].as_i64().unwrap();
    let stub = Child::new();
    let first = core.writer(FIRST);
    first.register_proc(stub.pid(), &stub.stamp(), "", "house", Some(session), None).expect("the warm process");
    let claimed = first
        .data(
            "hostclaim.turn",
            &json!({"conversation": session, "turn": turn, "person": "owner", "revokeSeen": 0, "cwd": "/tmp",
                    "sessionId": session, "holderInstance": FIRST, "account": "main"}),
        )
        .expect("claimed");
    let claim = claimed["claimId"].as_i64().unwrap_or_else(|| panic!("{claimed}"));
    let redeemed = first
        .data(
            "hostclaim.redeem",
            &json!({"claimId": claim, "pid": stub.pid(), "started": stub.stamp(), "holderInstance": FIRST}),
        )
        .expect("redeemed");
    assert_eq!(redeemed["running"], true, "{redeemed}");
    // The host restarts before the prompt went: the turn is queued again, and it says its claim ran.
    let second = core.writer(SECOND);
    second.adopt().expect("adopt");
    let again = listed(&core).into_iter().find(|t| t["conversation"] == session).expect("queued again");
    assert_eq!(again["redeemedBefore"], true);
    // The clock cancels it instead of beginning it: the prompt may have gone, it is never sent twice.
    begin(&r.hub, &second, core.state(), &again).await;
    until("the turn is cancelled", || core.turns(session)[0].1 == "cancelled").await;
    assert_eq!(core.turns(session).len(), 1, "no second turn for the same wake");
    assert!(listed(&core).iter().all(|t| t["conversation"] != session), "not listed, so not begun again");
}
