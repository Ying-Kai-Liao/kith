//! Rule K of step 5 (`reconcile.rs`): a core restart leaves the host's live claims alone, on a real core.db with made-up
//! processes ([`Procs`]).

use super::claims::{Kind, Redeemed, claim, redeem, report};
use super::reconcile::reconcile_with;
use super::tests::{Procs, conversation, new_claim, open, state, turn, turn_state, unreconcile};
use crate::db::Core;

const HOST: &str = "kith-serve-host/1-e1";

/// A queued turn of `conv` claimed for `instance`, redeemed on `pid` (started `e<pid>`) when one is given.
fn take_as(c: &mut Core, conv: &str, instance: &str, pid: Option<u32>) -> (i64, i64) {
    conversation(c, conv);
    let t = turn(c, conv, "sam");
    let mut req = new_claim(conv, Some(t));
    req.holder_instance = Some(instance);
    let id = claim(c, &req, 10).expect("claim").claim_id;
    if let Some(pid) = pid {
        let r = redeem(c, id, pid, &format!("e{pid}"), instance, 20).expect("redeem");
        assert_eq!(r, Redeemed::Running { again: false });
    }
    (id, t)
}

#[test]
fn a_core_restart_leaves_a_live_running_host_claim_and_a_claimed_one() {
    let (_t, mut c) = open();
    let procs = Procs::default();
    procs.spawn(200, "e200");
    let (running, running_turn) = take_as(&mut c, "a", HOST, Some(200));
    let (claimed, claimed_turn) = take_as(&mut c, "b", HOST, None);
    unreconcile(&c);
    let out = reconcile_with(&mut c, &procs, &[], &[], i64::MAX, 50).expect("reconcile");
    assert_eq!(state(&c, running), "running", "its process is alive: the host's to end");
    assert_eq!(turn_state(&c, running_turn), "running", "not put back: nothing runs it twice");
    assert_eq!(state(&c, claimed), "claimed", "the host's redeem or stop decides it");
    assert_eq!(turn_state(&c, claimed_turn), "claimed");
    assert!(out.kill.is_empty() && out.holder_kill.is_empty() && out.ended.is_empty(), "{out:?}");
    // The host reports the turn done afterwards, as it would have without the restart.
    report(&mut c, running, "done", r#"{"pid":200}"#, 60).expect("done");
    assert_eq!(state(&c, running), "ended");
    assert_eq!(turn_state(&c, running_turn), "done");
}

#[test]
fn a_host_claim_whose_process_is_gone_is_reconciled_as_before() {
    let (_t, mut c) = open();
    let procs = Procs::default();
    // Never spawned in `procs`: the system says pid 300 is gone.
    let (gone, gone_turn) = take_as(&mut c, "a", HOST, Some(300));
    // A pid reused by another process (another start time) is gone too.
    procs.spawn(301, "e9999");
    let (reused, _) = take_as(&mut c, "b", HOST, Some(301));
    unreconcile(&c);
    let out = reconcile_with(&mut c, &procs, &[], &[], i64::MAX, 50).expect("reconcile");
    assert_eq!(state(&c, gone), "ending", "never straight to ended");
    assert_eq!(state(&c, reused), "ending");
    assert_eq!(turn_state(&c, gone_turn), "queued", "the turn is put back, as before");
    let killed: Vec<_> = out.kill.iter().filter_map(|k| k.claim_id).collect();
    assert!(killed.contains(&gone) && killed.contains(&reused), "{out:?}");
}

#[test]
fn holder_and_external_claims_are_unchanged() {
    let (_t, mut c) = open();
    let procs = Procs::default();
    procs.spawn(400, "e400");
    procs.spawn(401, "e401");
    // A holder's live claims the holder does not list: step 5 takes them, live process or not.
    let (held_running, _) = take_as(&mut c, "a", "h1", Some(400));
    let (held_claimed, held_turn) = take_as(&mut c, "b", "h1", None);
    // A `--direct` run: left alone by step 5.
    conversation(&c, "c");
    let mut req = new_claim("c", None);
    req.kind = Kind::External;
    req.holder_instance = None;
    req.process = Some((401, "e401"));
    let external = claim(&mut c, &req, 10).expect("external").claim_id;
    unreconcile(&c);
    let out = reconcile_with(&mut c, &procs, &[], &[], i64::MAX, 50).expect("reconcile");
    assert_eq!(state(&c, held_running), "ending");
    assert_eq!(state(&c, held_claimed), "ended");
    assert_eq!(turn_state(&c, held_turn), "queued");
    assert_eq!(state(&c, external), "claimed");
    assert!(out.kill.iter().any(|k| k.claim_id == Some(held_running)), "{out:?}");
    assert!(out.kill.iter().all(|k| k.claim_id != Some(external)), "{out:?}");
}
