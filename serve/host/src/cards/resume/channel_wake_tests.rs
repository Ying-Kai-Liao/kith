//! The channel door by turn id (`WK4c`), on a real hub with a scripted core: which rows each end of an attempt calls. The core's own
//! rules for the same rows (the attach naming the work key, the requeue, R4's answer) are tested in `core/runtime`; the launch's
//! side (the writer claim on the session first) in `core/kernel`; the channel's batch in `channels/tests.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::{ChannelWake, Pull, Seen, attempt_over, door, unattached};
use crate::api::rig::Rig;
use crate::core_front::Answer;
use crate::terminal::core_claims::tests::register;
use crate::terminal::core_writer::CoreWriter;
use crate::wake;
use kith_kernel::paths::Paths;
use serde_json::{Value, json};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

type Log = Arc<Mutex<Vec<(String, Value)>>>;

const WK: &str = "tg:7#house";
const NOTE: &str = "[kith wake run-1] the staging box is ready";

/// A writer over a scripted core; the hub's claims folder is its state, so what the door reads is what it answers.
fn scripted(
    r: &Rig,
    answer: impl Fn(&str, &Value) -> Option<Answer> + Send + Sync + 'static,
) -> (Arc<CoreWriter>, Log) {
    let dir = std::env::temp_dir().join(format!("kith-channel-wake-{}", uuid::Uuid::new_v4()));
    let state = dir.join("state");
    let folders = Paths {
        home: dir.clone(),
        state: state.clone(),
        agents: dir.join("agents"),
        home_real: dir.clone(),
        state_real: state.clone(),
        agents_real: dir.join("agents"),
        machine: dir.join("machine"),
        secrets: dir.join("secrets"),
        logs: dir.join("logs"),
        cache: dir.join("cache"),
        store: dir.join("store"),
    };
    let log: Log = Arc::default();
    let seen = log.clone();
    register(
        &state,
        Arc::new(move |method: &str, p: &Value| {
            seen.lock().unwrap().push((method.to_owned(), p.clone()));
            answer(method, p).unwrap_or_else(|| match method {
                "hostclaim.epoch" => Answer::Done(json!({"epoch": 0})),
                "cutover.mayServe" => Answer::Done(json!({"ok": true})),
                _ => Answer::Done(json!({})),
            })
        }),
    );
    crate::terminal::core_claims::enable(&state, folders.clone());
    r.hub.terminal.set_env(crate::terminal::Env { state, sessions: None });
    (Arc::new(CoreWriter::with_instance(folders, "test".to_owned())), log)
}

fn calls(log: &Log, method: &str) -> Vec<Value> {
    log.lock().unwrap().iter().filter(|(m, _)| m == method).map(|(_, p)| p.clone()).collect()
}

async fn until(what: &str, f: impl Fn() -> bool) {
    for _ in 0..200 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("never happened: {what}");
}

fn turn() -> wake::Turn {
    wake::Turn {
        run: "run-1".to_owned(),
        session: WK.to_owned(),
        agent: "house".to_owned(),
        person: None,
        ids: vec!["card:c1".to_owned()],
        requests: vec![wake::Asked {
            id: "card:c1".to_owned(),
            why: String::new(),
            text: "the staging box is ready".to_owned(),
        }],
        note: NOTE.to_owned(),
    }
}

fn claimed() -> ChannelWake {
    ChannelWake {
        run: "run-1".to_owned(),
        turn: 7,
        claim: 41,
        wk: WK.to_owned(),
        cards: Vec::new(),
        ids: vec!["card:c1".to_owned()],
        settled: AtomicBool::new(false),
        refused: AtomicBool::new(false),
        sid: Mutex::new(Some("sess-a".to_owned())),
    }
}

async fn rig() -> Rig {
    let r = Rig::new().await;
    r.owner_bot("house");
    r
}

#[tokio::test]
async fn the_wake_is_claimed_on_the_work_key_and_a_connection_that_is_down_gives_the_claim_back_queued() {
    let r = rig().await;
    let (core, log) = scripted(&r, |m, _| (m == "hostclaim.claim").then(|| Answer::Done(json!({"claimId": 41}))));
    let pull = Pull { core: &core, turn: 7, person: String::new() };
    door(&r.hub, &pull, &turn(), &[], WK, NOTE).await;
    let claims = calls(&log, "hostclaim.claim");
    assert_eq!(claims.len(), 1, "{claims:?}");
    assert_eq!((claims[0]["kind"].as_str(), claims[0]["turn"].as_i64()), (Some("wakeRun"), Some(7)));
    assert_eq!(claims[0]["conversation"], WK, "the wake's turn is on the thread's work key, not on its session");
    // No channel connection in this process: the claim goes back and the turn is pulled again, not cancelled.
    until("the requeue", || !calls(&log, "hostclaim.stop").is_empty()).await;
    let stops = calls(&log, "hostclaim.stop");
    assert_eq!((stops[0]["claimId"].as_i64(), stops[0]["requeue"].as_bool()), (Some(41), Some(true)));
    assert_eq!(calls(&log, "hostclaim.cancel"), Vec::<Value>::new());
}

#[tokio::test]
async fn a_thread_whose_wake_still_holds_its_key_leaves_the_second_turn_queued() {
    let r = rig().await;
    let (core, log) = scripted(&r, |m, _| (m == "hostclaim.claim").then(|| Answer::Done(json!({"refused": "held"}))));
    let pull = Pull { core: &core, turn: 8, person: String::new() };
    door(&r.hub, &pull, &turn(), &[], WK, NOTE).await;
    assert_eq!(calls(&log, "hostclaim.claim").len(), 1);
    for m in ["hostclaim.stop", "hostclaim.cancel", "hostclaim.report"] {
        assert!(calls(&log, m).is_empty(), "{m}: one wake rides in a thread at a time");
    }
}

#[tokio::test]
async fn an_attempt_that_attached_reports_its_end_once_and_later_attempts_carry_nothing() {
    let r = rig().await;
    let (core, log) = scripted(&r, |m, _| {
        (m == "hostclaim.show").then(|| {
            Answer::Done(json!({"claim": {"claimId": 41, "state": "running", "kind": "external", "pid": 4242}}))
        })
    });
    let _ = &core;
    let w = claimed();
    assert!(attempt_over(&r.hub, &w, &Seen { pid: Some(4242), code: Some(0) }).await);
    assert!(w.settled(), "the backup loop's next attempt runs without the variables");
    until("the report", || !calls(&log, "hostclaim.report").is_empty()).await;
    let reports = calls(&log, "hostclaim.report");
    assert_eq!((reports[0]["claimId"].as_i64(), reports[0]["kind"].as_str()), (Some(41), Some("done")));
    // Not `answered` by the host: the core answers it from its turn.
    assert_eq!(calls(&log, "wake.done"), Vec::<Value>::new());
}

#[tokio::test]
async fn an_exit_code_is_not_read_for_attachment_only_the_claims_state() {
    let r = rig().await;
    // The engine exited 75 itself, but the claim says this process attached: it is reported as an exit, not given back.
    let (_core, log) = scripted(&r, |m, _| {
        (m == "hostclaim.show").then(|| {
            Answer::Done(json!({"claim": {"claimId": 41, "state": "ending", "kind": "external", "pid": 4242}}))
        })
    });
    let w = claimed();
    assert!(attempt_over(&r.hub, &w, &Seen { pid: Some(4242), code: Some(75) }).await);
    until("the report", || !calls(&log, "hostclaim.report").is_empty()).await;
    assert_eq!(calls(&log, "hostclaim.report")[0]["kind"], "exit");
    assert!(calls(&log, "hostclaim.stop").is_empty() && calls(&log, "hostclaim.cancel").is_empty());
}

#[tokio::test]
async fn a_launch_that_stopped_before_the_attach_leaves_the_claim_open_for_the_next_attempt() {
    let r = rig().await;
    let (_core, log) = scripted(&r, |m, _| {
        (m == "hostclaim.show")
            .then(|| Answer::Done(json!({"claim": {"claimId": 41, "state": "claimed", "kind": "turn", "pid": null}})))
    });
    let w = claimed();
    assert!(!attempt_over(&r.hub, &w, &Seen { pid: Some(4242), code: Some(10) }).await);
    assert!(!w.settled(), "the next attempt carries it again");
    // A run that never spawned (a refusal before it, a spawn error, Windows' early NotStarted) has no pid: the same branch.
    assert!(!attempt_over(&r.hub, &w, &Seen::default()).await);
    assert_eq!(calls(&log, "hostclaim.report"), Vec::<Value>::new());
}

#[tokio::test]
async fn a_wake_nobody_attached_is_requeued_when_a_writer_holds_the_session_with_no_failed_try() {
    let r = rig().await;
    let me = std::process::id();
    let (_core, log) = scripted(&r, move |m, p| match m {
        "hostclaim.show" if p["conversation"] == "sess-a" => Some(Answer::Done(
            json!({"claim": {"claimId": 9, "state": "running", "kind": "terminal", "pid": me, "person": ""}}),
        )),
        _ => None,
    });
    let w = claimed();
    unattached(&r.hub, &w, false).await;
    until("the requeue", || !calls(&log, "hostclaim.stop").is_empty()).await;
    let stops = calls(&log, "hostclaim.stop");
    assert_eq!((stops[0]["claimId"].as_i64(), stops[0]["requeue"].as_bool()), (Some(41), Some(true)));
    assert!(calls(&log, "hostclaim.cancel").is_empty(), "a held session is not a failed try");
}

#[tokio::test]
async fn a_wake_nobody_attached_and_no_writer_holds_is_freed_and_after_a_stop_withdrawn() {
    let r = rig().await;
    let (_core, log) = scripted(&r, |_, _| None);
    let w = claimed();
    unattached(&r.hub, &w, false).await;
    until("the cancel", || !calls(&log, "hostclaim.cancel").is_empty()).await;
    assert_eq!(calls(&log, "hostclaim.cancel")[0]["turn"], 7);
    assert_eq!(calls(&log, "hostclaim.stop"), Vec::<Value>::new());

    // A Stop before the attach is one `wake.withdraw` (the wake module's own call, which cancels the unredeemed turn and withdraws
    // its requests in one transaction), not a cancel and a done as two calls: a restart between them gives back what the Stop took.
    let (_core, log) = scripted(&r, |_, _| None);
    let w = claimed();
    unattached(&r.hub, &w, true).await;
    assert!(w.settled());
    for m in ["hostclaim.cancel", "hostclaim.stop", "hostclaim.report"] {
        assert!(calls(&log, m).is_empty(), "{m}");
    }
}

#[tokio::test]
async fn a_launch_too_old_to_name_the_work_key_is_refused_by_the_core_and_the_wake_is_requeued_not_dropped() {
    let r = rig().await;
    let (_core, log) = scripted(&r, |m, _| {
        (m == "hostclaim.show")
            .then(|| Answer::Done(json!({"claim": {"claimId": 41, "state": "claimed", "kind": "turn", "pid": null}})))
    });
    let w = claimed();
    assert!(!attempt_over(&r.hub, &w, &Seen { pid: Some(4242), code: Some(10) }).await);
    unattached(&r.hub, &w, false).await;
    until("the requeue", || !calls(&log, "hostclaim.stop").is_empty()).await;
    assert!(calls(&log, "hostclaim.cancel").is_empty(), "never freed: that would count a failed try for ever");
}
