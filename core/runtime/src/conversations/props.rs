//! Generated step sequences against the real claim code and a real core.db: whatever order claims, redeems, reports,
//! stops, revokes, holder changes and process deaths come in, a conversation never has two writers, and a claim is never
//! `ended` while the process its redeem recorded is alive. The holder is honest the way the real one is: it reports
//! `done` or `exit` for a pid only after that process is gone. Run only these: `cargo test -p kith-core conversations::props`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use proptest::prelude::*;
use proptest::test_runner::{Config, TestCaseError, TestRunner};

use super::claims::{
    Redeemed, claim, different_holder, end_gone, reconcile, redeem, report, revoke_person, stop,
};
use super::tests::{Procs, conversation, count, new_claim, open};
use crate::db::Core;

#[derive(Clone, Debug)]
enum Step {
    Claim(usize),
    Redeem(usize),
    Done(usize),
    Exit(usize),
    Busy(usize),
    Stop(usize, bool),
    Revoke,
    Different(bool),
    Reconcile(bool),
    Die(usize),
    Gone,
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        3 => (0..2usize).prop_map(Step::Claim),
        3 => (0..8usize).prop_map(Step::Redeem),
        2 => (0..8usize).prop_map(Step::Done),
        2 => (0..8usize).prop_map(Step::Exit),
        1 => (0..8usize).prop_map(Step::Busy),
        2 => ((0..8usize), any::<bool>()).prop_map(|(i, r)| Step::Stop(i, r)),
        1 => Just(Step::Revoke),
        2 => any::<bool>().prop_map(Step::Different),
        1 => any::<bool>().prop_map(Step::Reconcile),
        2 => (0..8usize).prop_map(Step::Die),
        3 => Just(Step::Gone),
    ]
}

fn recorded_pid(c: &Core, id: i64) -> Option<u32> {
    c.conn()
        .query_row("SELECT pid FROM claims WHERE claim_id = ?1", [id], |r| {
            r.get::<_, Option<i64>>(0)
        })
        .unwrap()
        .map(|p| u32::try_from(p).unwrap())
}

fn invariants(c: &Core, procs: &Procs) -> Result<(), TestCaseError> {
    let two = count(
        c,
        "SELECT count(*) FROM (SELECT conversation FROM claims
         WHERE state IN ('claimed', 'running', 'ending') GROUP BY conversation HAVING count(*) > 1)",
    );
    prop_assert_eq!(two, 0, "two writers on one conversation");
    let mut st = c
        .conn()
        .prepare("SELECT claim_id, pid FROM claims WHERE state = 'ended' AND pid IS NOT NULL")
        .unwrap();
    let ended: Vec<(i64, i64)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    for (id, pid) in ended {
        let alive = procs.alive(u32::try_from(pid).unwrap());
        prop_assert!(!alive, "claim {} ended while its pid {} is alive", id, pid);
    }
    Ok(())
}

fn run(steps: &[Step]) -> Result<(), TestCaseError> {
    let (_t, mut c) = open();
    let procs = Procs::default();
    conversation(&c, "c0");
    conversation(&c, "c1");
    let mut ids: Vec<i64> = Vec::new();
    let mut next_pid = 1000u32;
    let mut now = 100i64;
    let mut instance = 1u32;
    for s in steps {
        now += 1;
        let pick = |i: usize| (!ids.is_empty()).then(|| ids[i % ids.len()]);
        match s {
            Step::Claim(conv) => {
                let name = format!("c{conv}");
                let mut req = new_claim(&name, None);
                let inst = format!("h{instance}");
                req.holder_instance = Some(&inst);
                if let Ok(got) = claim(&mut c, &req, now) {
                    prop_assert!(ids.iter().all(|&i| i < got.claim_id), "claim ids grow");
                    ids.push(got.claim_id);
                }
            }
            Step::Redeem(i) => {
                if let Some(id) = pick(*i) {
                    next_pid += 1;
                    let inst = format!("h{instance}");
                    let r =
                        redeem(&mut c, id, next_pid, &format!("s{next_pid}"), &inst, now).unwrap();
                    // A refused stub exits; only a redeemed one goes on to run the engine.
                    if matches!(r, Redeemed::Running { again: false }) {
                        procs.spawn(next_pid, &format!("s{next_pid}"));
                    }
                }
            }
            Step::Done(i) | Step::Exit(i) => {
                if let Some(id) = pick(*i) {
                    let pid = recorded_pid(&c, id);
                    if let Some(p) = pid {
                        procs.kill(p);
                    }
                    let body = pid.map_or("{}".into(), |p| format!(r#"{{"pid":{p}}}"#));
                    let kind = if matches!(s, Step::Done(_)) {
                        "done"
                    } else {
                        "exit"
                    };
                    report(&mut c, id, kind, &body, now).unwrap();
                }
            }
            Step::Busy(i) => {
                if let Some(id) = pick(*i) {
                    report(&mut c, id, "busy", "{}", now).unwrap();
                }
            }
            Step::Stop(i, requeue) => {
                if let Some(id) = pick(*i) {
                    stop(&mut c, id, "stop", *requeue, now).unwrap();
                }
            }
            Step::Revoke => {
                revoke_person(&mut c, "sam", now).unwrap();
            }
            Step::Different(list_all) => {
                instance += 1;
                let listed: Vec<i64> = if *list_all { ids.clone() } else { Vec::new() };
                different_holder(&mut c, &format!("h{instance}"), &listed, &[], now).unwrap();
            }
            Step::Reconcile(list_all) => {
                let listed: Vec<i64> = if *list_all { ids.clone() } else { Vec::new() };
                reconcile(&mut c, &listed, &[], now).unwrap();
            }
            Step::Die(i) => {
                if let Some(p) = pick(*i).and_then(|id| recorded_pid(&c, id)) {
                    procs.kill(p);
                }
            }
            Step::Gone => {
                end_gone(&mut c, &procs, now).unwrap();
            }
        }
        invariants(&c, &procs)?;
    }
    Ok(())
}

#[test]
fn any_order_of_steps_keeps_one_writer_and_never_ends_a_claim_under_a_live_pid() {
    let mut config = Config {
        failure_persistence: None,
        ..Config::default()
    };
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = 64;
    }
    TestRunner::new(config)
        .run(&proptest::collection::vec(step(), 1..40), |steps| {
            run(&steps)
        })
        .unwrap();
}
