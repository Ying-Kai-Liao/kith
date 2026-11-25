//! WK4c on a real core (the daemon the host tests start, as `wake/tests.rs`): the rows the host calls, not a script's answers. A
//! channel wake's turn is counted in `turns`, and a host restart is a second host instance's `adopt`.
//!
//! The launch's own steps (`claim.external` on the session, then `claim.attach` naming the work key) are rows of the claims
//! module, which a test daemon does not have switched: they are tested on the core's tables in
//! `core/runtime/src/conversations/detached_tests.rs`, and on the kernel's call order in `core/kernel` `writer_turn_tests.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::super::channel_wake::{ChannelWake, Seen, attempt_over, unattached};
use crate::api::rig::Rig;
use crate::core_front::{TestDaemon, serve_for_test};
use crate::terminal::core_writer::{CoreWriter, RunClaim};
use crate::wake::{self, Request};
use kith_kernel::paths::Paths;
use serde_json::Value;
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
    assert!(listed(&core).is_empty());
    assert_eq!(core.turns(WK).len(), 1, "one turn row for the whole story: {:?}", core.turns(WK));
}
