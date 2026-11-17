//! The end of a cron run against a core that does not always answer (`core_cron.rs`, `until_told`): a lost call must not
//! leave the claim held until the host restarts.

#![allow(clippy::unwrap_used)]

use super::{Pace, over, until_told};
use crate::core_front::Answer;
use crate::terminal::core_claims::tests::{Fake, register};
use crate::terminal::core_writer::{CoreWriter, Told};
use kith_kernel::paths::Paths;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const FAST: Pace = Pace { first: Duration::from_millis(1), cap: Duration::from_millis(4) };

/// A core that does not answer its first `down` calls, then answers `Done`; the log is every call it took.
fn flaky_core(down: usize) -> (CoreWriter, Arc<Mutex<Vec<String>>>) {
    let dir = std::env::temp_dir().join(format!("kith-cron-core-{}", uuid::Uuid::new_v4()));
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
    let log = Arc::new(Mutex::new(Vec::new()));
    let seen = log.clone();
    let fake: Fake = Arc::new(move |method: &str, p: &Value| {
        let mut calls = seen.lock().unwrap();
        calls.push(format!("{method} {}", p["turn"].as_i64().or(p["claimId"].as_i64()).unwrap_or(0)));
        if calls.len() <= down { Answer::Down { why: "no core".into(), lost: false } } else { Answer::Done(json!({})) }
    });
    register(&state, fake);
    (CoreWriter::with_instance(folders, "test".to_owned()), log)
}

/// A child that has already said why it failed and exits with `code`.
fn finished(code: i32) -> std::process::Child {
    let mut cmd = std::process::Command::new("sh");
    cmd.args(["-c", &format!("echo 'nothing ran' >&2; exit {code}")]).stderr(std::process::Stdio::piped());
    cmd.spawn().unwrap()
}

#[test]
fn a_run_that_never_attached_ends_its_turn_and_claim_though_the_core_missed_the_first_calls() {
    let (writer, log) = flaky_core(3);
    over(&writer, finished(4), 9, 7, &FAST);
    assert_eq!(
        *log.lock().unwrap(),
        ["hostclaim.cancel 7", "hostclaim.cancel 7", "hostclaim.cancel 7", "hostclaim.cancel 7", "hostclaim.report 9"],
        "the cancel is asked until answered, then the report"
    );
}

#[test]
fn a_clean_run_is_reported_and_asked_until_the_core_answers() {
    let (writer, log) = flaky_core(2);
    over(&writer, finished(0), 9, 7, &FAST);
    assert_eq!(*log.lock().unwrap(), ["hostclaim.report 9", "hostclaim.report 9", "hostclaim.report 9"]);
}

#[test]
fn a_refusal_ends_the_asking_it_is_an_answer() {
    let mut asked = 0;
    let told = until_told("cancel", 7, &FAST, || {
        asked += 1;
        Told::Refused("the claim is gone".into())
    });
    assert_eq!((asked, told), (1, Told::Refused("the claim is gone".into())));
}

#[test]
fn the_asks_are_paced_not_tight() {
    let pace = Pace { first: Duration::from_millis(20), cap: Duration::from_millis(40) };
    let (mut n, start) = (0, std::time::Instant::now());
    until_told("report", 7, &pace, || {
        n += 1;
        if n < 4 { Told::Down("x".into()) } else { Told::Done }
    });
    assert!(start.elapsed() >= Duration::from_millis(20 + 40 + 40), "{:?}", start.elapsed());
}
