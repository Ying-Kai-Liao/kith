//! WK4c on a real core (the daemon the host tests start, as `wake/tests.rs`): the orderings the design's model found, not a
//! script's answers. A channel wake's turn is counted in `turns`, the launch is played by a process of the test (it takes the
//! session's claim, then attaches the wake's claim naming the work key), a host restart is a second `CoreWriter` instance's
//! `adopt`, and the core answers the wake from its own turn (R4).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::super::channel_wake::{ChannelWake, Seen, attempt_over};
use super::begin;
use crate::api::rig::Rig;
use crate::core_front::{TestDaemon, call_host_claims, call_owner, serve_for_test};
use crate::terminal::core_writer::{CoreWriter, RunClaim};
use crate::wake::{self, Request};
use kith_kernel::paths::Paths;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Arc;
use std::time::Duration;

const WK: &str = "tg:7#house";
const SID: &str = "sess-a";

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
        let db = self.folders.machine.join("core/core.db");
        let conn =
            rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).expect("core.db");
        let mut stmt = conn.prepare("SELECT id, state FROM turns WHERE conversation = ?1 ORDER BY id").unwrap();
        stmt.query_map([conversation], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(Result::unwrap).collect()
    }

    fn call(&self, method: &str, params: &Value) -> Value {
        match call_owner(&self.folders, method, params, false) {
            crate::core_front::Answer::Done(v) => v,
            _ => panic!("{method}: the core did not answer"),
        }
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
    core.writer("host-1").queued_from(&["wake"]).expect("queued")
}

/// The launch the host starts, played by `child`: the session's own claim first, then the wake's claim naming the work key.
fn launch(core: &Real, claim: i64, child: &Child, conversation: &str) -> Value {
    let pid = child.id();
    let started = crate::terminal::proc_start(pid).expect("start stamp");
    core.call("claim.external", &json!({"conversation": SID, "pid": pid, "started": started, "person": ""}));
    core.call("claim.attach", &json!({"claimId": claim, "conversation": conversation, "pid": pid, "started": started}))
}

fn sleeper() -> Child {
    Command::new("sleep").arg("60").spawn().expect("sleep")
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
    let v = core.call("hostclaim.show", &json!({"claimId": claim}));
    v["claim"]["state"].as_str().unwrap_or("none").to_owned()
}

fn settle(core: &Real) {
    let _ = call_host_claims(&core.folders, "hostclaim.settle", &json!({}));
    let _ = wake::tick(core.state());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_channel_wake_begins_the_cores_turn_once_and_the_core_answers_it_when_the_group_is_gone() {
    let core = Real::start();
    let r = hub_on(&core).await;
    let t = owed(&core, WK);
    let turn = t["turn"].as_i64().unwrap();
    let host = core.writer("host-1");
    let RunClaim::Claimed(claim) = host.claim_run_as("wakeRun", WK, "", turn) else { panic!("claimed") };
    assert_eq!(core.turns(WK).len(), 1, "one turn row for the wake");
    assert!(listed(&core).is_empty(), "a claimed turn is not listed again");
    assert_eq!(claim_state(&core, claim), "claimed");

    // The launch: a claim that names another turn's conversation is refused, the work key attaches.
    let mut child = sleeper();
    let started = crate::terminal::proc_start(child.id()).unwrap();
    let wrong = core.call(
        "claim.attach",
        &json!({"claimId": claim, "conversation": "other#house", "pid": child.id(), "started": started}),
    );
    assert_ne!(wrong["attached"], true, "{wrong}");
    assert_eq!(launch(&core, claim, &child, WK)["attached"], true);
    let rows = core.turns(WK);
    assert_eq!(rows.len(), 1, "the attach begins the same turn: {rows:?}");
    assert_eq!(rows[0].1, "running");
    assert_eq!(core.turns(SID).len(), 0, "the session's claim is a claim, not a second turn");
    assert_eq!(claim_state(&core, claim), "running");

    // The worker's end of the attempt reads the claim's state, not the exit code, and reports the process's end.
    let w = ChannelWake::for_test("run-1", turn, claim, WK);
    assert!(attempt_over(&r.hub, &w, &Seen { pid: Some(child.id()), code: Some(75) }).await);
    child.kill().unwrap();
    child.wait().unwrap();
    until("the wake is answered by the core", || {
        settle(&core);
        wake::status(core.state(), "card:c1").as_deref() == Some("answered")
    })
    .await;
    assert_eq!(core.turns(WK).len(), 1, "still one turn row for the run");
    assert!(listed(&core).is_empty(), "never begun again");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_before_the_attach_requeues_the_turn_which_runs_once_and_a_late_attach_is_refused() {
    let core = Real::start();
    let t = owed(&core, WK);
    let turn = t["turn"].as_i64().unwrap();
    let RunClaim::Claimed(first) = core.writer("host-1").claim_run_as("wakeRun", WK, "", turn) else { panic!() };
    // The host restarts: its adopt gives back a claim nobody attached.
    core.writer("host-2").adopt().expect("adopt");
    assert_eq!(claim_state(&core, first), "ended");
    assert_eq!(listed(&core).len(), 1, "the turn is queued again, not redeemed");
    assert_eq!(listed(&core)[0]["redeemedBefore"], false);

    // A launch spawned before the restart attaches now: refused, and nothing is redeemed.
    let mut late = sleeper();
    assert_ne!(launch(&core, first, &late, WK)["attached"], true);
    late.kill().unwrap();
    late.wait().unwrap();
    assert_eq!(listed(&core).len(), 1);

    // The clock claims it again and it runs once.
    let RunClaim::Claimed(second) = core.writer("host-2").claim_run_as("wakeRun", WK, "", turn) else { panic!() };
    let mut child = sleeper();
    assert_eq!(launch(&core, second, &child, WK)["attached"], true);
    assert_eq!(core.turns(WK).len(), 1, "one turn row for the whole story: {:?}", core.turns(WK));
    assert!(listed(&core).is_empty());
    child.kill().unwrap();
    child.wait().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_after_the_attach_ends_the_claim_once_the_group_is_gone_and_never_begins_it_again() {
    let core = Real::start();
    let t = owed(&core, WK);
    let turn = t["turn"].as_i64().unwrap();
    let RunClaim::Claimed(claim) = core.writer("host-1").claim_run_as("wakeRun", WK, "", turn) else { panic!() };
    let mut child = sleeper();
    assert_eq!(launch(&core, claim, &child, WK)["attached"], true);
    // The host restarts: an attached claim is left alone, the process lives on.
    core.writer("host-2").adopt().expect("adopt");
    assert_eq!(claim_state(&core, claim), "running");
    assert!(listed(&core).is_empty(), "not listed, so not begun again");
    child.kill().unwrap();
    child.wait().unwrap();
    until("the claim ends and the wake is answered", || {
        settle(&core);
        claim_state(&core, claim) == "ended" && wake::status(core.state(), "card:c1").as_deref() == Some("answered")
    })
    .await;
    assert!(listed(&core).is_empty());
    assert_eq!(core.turns(WK).len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bot_turn_redeemed_then_cut_by_a_restart_is_cancelled_and_the_core_answers_it() {
    let core = Real::start();
    let r = hub_on(&core).await;
    let conversation = "acp:house:";
    let t = owed(&core, conversation);
    let turn = t["turn"].as_i64().unwrap();
    // The actor's begin: the core's own turn, claimed and redeemed by the stub's process.
    let child = sleeper();
    let started = crate::terminal::proc_start(child.id()).unwrap();
    let began = match call_host_claims(
        &core.folders,
        "hostclaim.turn",
        &json!({"conversation": conversation, "turn": turn, "person": "", "revokeSeen": 0, "cwd": "/tmp",
                "sessionId": "bot-sess-1", "holderInstance": core.writer("host-1").instance()}),
    ) {
        crate::core_front::Answer::Done(v) => v,
        _ => panic!("hostclaim.turn was not answered"),
    };
    let claim = began["claimId"].as_i64().unwrap_or_else(|| panic!("{began}"));
    let redeemed = call_host_claims(
        &core.folders,
        "hostclaim.redeem",
        &json!({"claimId": claim, "pid": child.id(), "started": started, "holderInstance": core.writer("host-1").instance()}),
    );
    assert!(matches!(redeemed, crate::core_front::Answer::Done(_)), "hostclaim.redeem was not answered");
    assert_eq!(core.turns(conversation).len(), 1, "no second turn row for the bot's wake");

    // The host restarts before the prompt went: the turn is listed with its earlier claim, and the door cancels it.
    core.writer("host-2").adopt().expect("adopt");
    let again = listed(&core).into_iter().find(|t| t["conversation"] == conversation);
    let Some(again) = again else {
        // The core already holds it as cut off: nothing is begun, and the tick answers it.
        until("answered", || {
            settle(&core);
            wake::status(core.state(), "card:c1").as_deref() == Some("answered")
        })
        .await;
        return;
    };
    assert_eq!(again["redeemedBefore"], true);
    begin(&r.hub, &core.writer("host-2"), core.state(), &again).await;
    until("the turn is cancelled and the wake answered by the core", || {
        settle(&core);
        core.turns(conversation).iter().all(|(_, s)| s == "cancelled")
            && wake::status(core.state(), "card:c1").as_deref() == Some("answered")
    })
    .await;
    assert_eq!(core.turns(conversation).len(), 1);
    drop(child);
}
