//! The stray sweep (`core_strays.rs`) against a scripted core: it cancels the host turns no item holds, and only those.

use super::*;
use crate::terminal::core_claims::tests::{Fake, register as script};
use kith_kernel::paths::Paths;
use serde_json::Value;
use std::sync::Arc;

/// A scripted core with these waiting host turns `(turn, key)`; it records every cancel.
fn core(listed: Vec<(i64, String)>, answers: bool) -> (CoreWriter, Arc<Mutex<Vec<i64>>>, std::path::PathBuf) {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "kith-strays-{}-{}",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ));
    let state = dir.join("state");
    std::fs::create_dir_all(&state).unwrap();
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
    let cancelled = Arc::new(Mutex::new(Vec::new()));
    let c = cancelled.clone();
    let fake: Fake = Arc::new(move |method, p| match method {
        _ if !answers => Answer::Down { why: "the core is down".into(), lost: false },
        "hostclaim.strays" => {
            let turns: Vec<Value> =
                listed.iter().map(|(t, k)| json!({"turn": t, "conversation": "s", "key": k})).collect();
            Answer::Done(json!({"turns": turns}))
        }
        "hostclaim.cancel" => {
            c.lock().unwrap().push(p["turn"].as_i64().unwrap());
            Answer::Done(json!({"cancelled": true}))
        }
        other => Answer::Refused(format!("no {other}")),
    });
    script(&state, fake);
    (CoreWriter::new(folders), cancelled, dir)
}

fn key() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[test]
fn a_turn_whose_key_no_item_holds_is_cancelled_and_the_ones_items_hold_are_not() {
    let (held, unbound, dead) = (key(), key(), key());
    register(&held);
    bind(&held, 11);
    register(&unbound); // made at accept, before its enqueue: the turn may be listed before the reply is handled
    let (w, cancelled, dir) = core(vec![(11, held), (12, unbound), (13, dead)], true);
    assert_eq!(w.sweep_strays(), 1);
    assert_eq!(*cancelled.lock().unwrap(), vec![13], "only the turn of an item that is gone");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn a_turn_whose_end_was_reported_or_cancelled_is_forgotten_and_a_requeue_of_it_is_swept() {
    let k = key();
    register(&k);
    bind(&k, 21);
    forget_turn(21);
    let (w, cancelled, dir) = core(vec![(21, k)], true);
    assert_eq!(w.sweep_strays(), 1);
    assert_eq!(*cancelled.lock().unwrap(), vec![21]);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn a_core_that_does_not_answer_cancels_nothing() {
    let (w, cancelled, dir) = core(vec![(31, key())], false);
    assert_eq!(w.sweep_strays(), 0);
    assert!(cancelled.lock().unwrap().is_empty());
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn register_never_unbinds_a_key_that_has_its_turn() {
    let k = key();
    register(&k);
    bind(&k, 41);
    register(&k); // the next begin of the same item registers again before its enqueue
    let (w, cancelled, dir) = core(vec![(41, k)], true);
    assert_eq!(w.sweep_strays(), 0);
    assert!(cancelled.lock().unwrap().is_empty());
    std::fs::remove_dir_all(dir).ok();
}
