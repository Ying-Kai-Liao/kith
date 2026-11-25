//! The wake run door by turn id, on a real hub with a scripted core (`terminal::core_claims_tests::Fake`; the core's own rules for
//! these rows are tested on a real core in `core/runtime`): which rows the door calls in which order, what it starts, and what
//! it refuses to start. The process it starts is a stand-in `kith` script.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::{ENGINE_GAP, begin, door, engine_gap, old_door, run_of};
use crate::api::rig::Rig;
use crate::core_front::Answer;
use crate::terminal::core_claims::tests::register;
use crate::terminal::core_writer::CoreWriter;
use crate::wake;
use kith_kernel::paths::Paths;
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

type Log = Arc<Mutex<Vec<(String, Value)>>>;

/// A writer over a scripted core: `answer` says what each row answers (the epoch and anything it leaves alone answer well), and
/// the log is every call it took.
fn scripted(answer: impl Fn(&str, &Value) -> Option<Answer> + Send + Sync + 'static) -> (Arc<CoreWriter>, Log) {
    let dir = std::env::temp_dir().join(format!("kith-wake-clock-{}", uuid::Uuid::new_v4()));
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
                _ => Answer::Done(json!({})),
            })
        }),
    );
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

/// A hub with the owner's bot of `house`, whose claims folder is a temp one of its own (never Kai's).
async fn rig() -> Rig {
    let r = Rig::new().await;
    r.owner_bot("house");
    r.hub.terminal.set_env(crate::terminal::Env { state: r.dir.join("claims"), sessions: None });
    r
}

const NOTE: &str = "[kith wake run-1] the staging box is ready";

/// The turn `hostclaim.queued` lists for a wake of session `sess-a`, as the core makes it.
fn listed(engine: &str) -> Value {
    json!({"turn": 7, "source": "wake", "conversation": "sess-a", "agent": "house", "engine": engine, "person": "owner",
           "owner": true, "door": "run", "requestIds": ["ask:job-9/1"], "note": NOTE, "prompt": NOTE,
           "redeemedBefore": false, "parkedReason": null})
}

/// What the queue's item says that turn is: the request an ask answer left, for the owner.
fn carried(person: Option<&str>) -> wake::Turn {
    wake::Turn {
        run: "run-1".to_owned(),
        session: "sess-a".to_owned(),
        agent: "house".to_owned(),
        person: person.map(str::to_owned),
        ids: vec!["ask:job-9/1".to_owned()],
        requests: vec![wake::Asked {
            id: "ask:job-9/1".to_owned(),
            why: String::new(),
            text: "the staging box is ready".to_owned(),
        }],
        note: NOTE.to_owned(),
    }
}

/// A `kith` that writes the claim it was given and its words to `out` and ends.
fn stub_kith(out: &Path) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kith-wake-stub-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("kith");
    std::fs::write(&file, format!("#!/bin/sh\necho \"$KITH_TURN_CLAIM|$*\" >> {}\n", out.display())).unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
    file
}

#[test]
fn a_wake_turns_run_is_named_by_its_note_and_a_bot_or_channel_session_keeps_its_old_door() {
    assert_eq!(run_of(NOTE), Some("run-1"));
    assert_eq!(run_of("no marker"), None);
    assert_eq!(run_of("[kith wake ] x"), None);
    assert!(old_door("acp:chat:thread"), "a bot chat");
    assert!(old_door("owner/repo#12"), "a channel thread");
    assert!(!old_door("3f2a9c1e-claude-session"), "a Claude session is the run door's");
}

#[test]
fn only_a_claude_engine_runs_detached_until_sched_turn_lands() {
    assert_eq!(engine_gap(&listed("claude"), None), None);
    assert_eq!(engine_gap(&listed("codex"), None), Some(ENGINE_GAP));
    assert_eq!(
        engine_gap(&json!({"turn": 1}), None),
        Some(ENGINE_GAP),
        "an engine the core did not name is not assumed"
    );
    assert_eq!(engine_gap(&listed("claude"), Some("codex")), Some(ENGINE_GAP), "the agent's harness decides");
    assert_eq!(engine_gap(&listed("codex"), Some("claude")), None);
    assert_eq!(ENGINE_GAP, "this agent's engine cannot run detached turns yet");
}

#[tokio::test]
async fn an_owed_wake_is_claimed_once_and_runs_kith_resume_with_its_claim_and_is_reported_done() {
    let r = rig().await;
    let out = std::env::temp_dir().join(format!("kith-wake-out-{}", uuid::Uuid::new_v4()));
    *crate::agent::hq::TEST_KERNEL_BIN.lock().unwrap() = Some(stub_kith(&out));
    let (core, log) = scripted(|m, _| (m == "hostclaim.claim").then(|| Answer::Done(json!({"claimId": 41}))));
    door(&r.hub, &core, &listed("claude"), 7, &carried(None)).await;
    until("the run's end is reported", || !calls(&log, "hostclaim.report").is_empty()).await;
    *crate::agent::hq::TEST_KERNEL_BIN.lock().unwrap() = None;

    let claims = calls(&log, "hostclaim.claim");
    assert_eq!(claims.len(), 1, "claimed once: {claims:?}");
    assert_eq!((claims[0]["kind"].as_str(), claims[0]["turn"].as_i64()), (Some("wakeRun"), Some(7)));
    assert_eq!(claims[0]["conversation"], "sess-a", "on the session the wake resumes");
    let ran = std::fs::read_to_string(&out).unwrap();
    assert_eq!(ran.lines().count(), 1, "the engine ran once: {ran}");
    assert!(
        ran.starts_with("41|house --resume sess-a [kith wake run-1] the staging box is ready"),
        "KITH_TURN_CLAIM is the claim's id, and the words are `kith AGENT --resume SESSION NOTE`: {ran}"
    );
    let reports = calls(&log, "hostclaim.report");
    assert_eq!((reports[0]["claimId"].as_i64(), reports[0]["kind"].as_str()), (Some(41), Some("done")));
    assert!(calls(&log, "hostclaim.cancel").is_empty(), "a run that ended well is not cancelled");
}

#[tokio::test]
async fn a_session_a_terminal_holds_is_not_started_the_turn_stays_where_it_is() {
    let r = rig().await;
    let out = std::env::temp_dir().join(format!("kith-wake-out-{}", uuid::Uuid::new_v4()));
    *crate::agent::hq::TEST_KERNEL_BIN.lock().unwrap() = Some(stub_kith(&out));
    let (core, log) = scripted(|m, _| (m == "hostclaim.claim").then(|| Answer::Done(json!({"refused": "held"}))));
    door(&r.hub, &core, &listed("claude"), 7, &carried(None)).await;
    *crate::agent::hq::TEST_KERNEL_BIN.lock().unwrap() = None;
    assert_eq!(calls(&log, "hostclaim.claim").len(), 1);
    assert!(!out.exists(), "nothing started beside the terminal");
    assert!(calls(&log, "hostclaim.cancel").is_empty() && calls(&log, "hostclaim.report").is_empty(), "the turn waits");
}

#[tokio::test]
async fn a_person_whose_access_ended_at_the_door_is_withdrawn_and_nothing_is_started() {
    let r = rig().await;
    let _sam = crate::cards::tests::joined(&r, "member").await;
    r.auth(&["member", "house", "sam", "--rm"]);
    let out = std::env::temp_dir().join(format!("kith-wake-out-{}", uuid::Uuid::new_v4()));
    *crate::agent::hq::TEST_KERNEL_BIN.lock().unwrap() = Some(stub_kith(&out));
    let (core, log) = scripted(|_, _| None);
    door(&r.hub, &core, &listed("claude"), 7, &carried(Some("sam"))).await;
    *crate::agent::hq::TEST_KERNEL_BIN.lock().unwrap() = None;
    assert!(
        calls(&log, "hostclaim.claim").is_empty(),
        "no claim for a person who has no access: {:?}",
        log.lock().unwrap()
    );
    assert!(!out.exists(), "nothing was spawned");
}

#[tokio::test]
async fn a_non_claude_agents_turn_is_cancelled_with_the_reason_and_never_claimed_or_started() {
    let r = rig().await;
    let out = std::env::temp_dir().join(format!("kith-wake-out-{}", uuid::Uuid::new_v4()));
    *crate::agent::hq::TEST_KERNEL_BIN.lock().unwrap() = Some(stub_kith(&out));
    let (core, log) = scripted(|_, _| None);
    door(&r.hub, &core, &listed("codex"), 7, &carried(None)).await;
    until("the cancel", || !calls(&log, "hostclaim.cancel").is_empty()).await;
    *crate::agent::hq::TEST_KERNEL_BIN.lock().unwrap() = None;
    let cancels = calls(&log, "hostclaim.cancel");
    assert_eq!((cancels[0]["turn"].as_i64(), cancels[0]["reason"].as_str()), (Some(7), Some(ENGINE_GAP)));
    assert!(
        calls(&log, "hostclaim.claim").is_empty(),
        "never started without its claim: a restart would send the prompt twice"
    );
    assert!(!out.exists(), "nothing was spawned");
}

#[tokio::test]
async fn a_turn_whose_earlier_claim_ran_is_cancelled_interrupted_and_never_begun_again() {
    let r = rig().await;
    let (core, log) = scripted(|_, _| None);
    let mut t = listed("claude");
    t["redeemedBefore"] = true.into();
    begin(&r.hub, &core, &r.hub.terminal.env().state, &t).await;
    until("the cancel", || !calls(&log, "hostclaim.cancel").is_empty()).await;
    let cancels = calls(&log, "hostclaim.cancel");
    assert!(cancels[0]["reason"].as_str().is_some_and(|w| w.starts_with("interrupted")), "{cancels:?}");
    assert_eq!(calls(&log, "hostclaim.claim"), Vec::<Value>::new());
}

#[tokio::test]
async fn a_parked_turn_is_left_for_the_ticks_unpark() {
    let r = rig().await;
    let (core, log) = scripted(|_, _| None);
    let mut parked = listed("claude");
    parked["parkedReason"] = "terminal_live".into();
    begin(&r.hub, &core, &r.hub.terminal.env().state, &parked).await;
    assert!(log.lock().unwrap().is_empty(), "a parked turn is not claimed: {:?}", log.lock().unwrap());
}
