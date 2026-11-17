//! A cron job's turn claim (`sched_run.rs`): the check-and-set before the run's engine, what a host restart does with the run in
//! each state, the paused job's turn, and the revoke that kills a guest's run. Real core.db with every lane's tables.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::json;
use tempfile::TempDir;

use super::{attach, claim_run, paused_in};
use crate::conversations::claims::{self, NewTurn, revoke_person};
use crate::conversations::external::call_at;
use crate::conversations::tests::Procs;
use crate::conversations::warm;
use crate::db::Core;
use crate::error::Error;
use crate::layout::Layout;
use crate::tests::{machine, open_all};

const OLD: &str = "kith-serve-host/1-a";
const NEW: &str = "kith-serve-host/2-b";
const NOW: i64 = 1_900_000_000_000;

fn world() -> (TempDir, Core) {
    let (t, m) = machine();
    let mut c = open_all(&Layout::new(&m));
    claims::reconcile(&mut c, &[], &[], i64::MAX, 1).unwrap();
    (t, c)
}

/// A fired turn of job `job` for `person`, as the fire leaves it: the turn, its fire row (with the settings), the job's row.
fn fired(c: &mut Core, job: &str, person: &str, paused: bool) -> (String, i64) {
    let conversation = format!("schedule:mel:{job}:claude");
    let turn = claims::enqueue_turn(
        c,
        &NewTurn {
            conversation: &conversation,
            agent: "mel",
            engine: "claude",
            source: "schedule",
            person,
            prompt: "do it",
            deadline: None,
            revoke_seen: claims::revoke_epoch(c, person).unwrap(),
        },
        NOW,
    )
    .unwrap();
    c.conn()
        .execute(
            "INSERT INTO sched_fires (agent, job, slot, outcome, turn_id, at, settings)
             VALUES ('mel', ?1, '2026-11-18T08:00:00Z', 'fired', ?2, ?3, '{\"model\":\"m1\"}')",
            rusqlite::params![job, turn.to_string(), NOW],
        )
        .unwrap();
    c.conn()
        .execute(
            "INSERT INTO sched_jobs (agent, job, def, paused, person, first_seen) VALUES ('mel', ?1, '{}', ?2, NULL, 1)",
            rusqlite::params![job, i64::from(paused)],
        )
        .unwrap();
    (conversation, turn)
}

fn claim_for(c: &mut Core, conversation: &str, person: &str, turn: i64) -> Result<i64, Error> {
    let seen = claims::revoke_epoch(c, person).unwrap();
    claim_run(c, conversation, person, turn, Some(OLD), seen, NOW + 1).map(|c| c.claim_id)
}

fn turn_state(c: &Core, turn: i64) -> String {
    c.conn()
        .query_row("SELECT state FROM turns WHERE id = ?1", [turn], |r| {
            r.get(0)
        })
        .unwrap()
}

fn claim_row(c: &Core, claim: i64) -> (String, String, Option<i64>) {
    c.conn()
        .query_row(
            "SELECT kind, state, pid FROM claims WHERE claim_id = ?1",
            [claim],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
}

/// The rollback's and `live`'s rule for "a run still going": a process and a state that is not over (`CLAIM_GOING`).
fn going(c: &Core) -> i64 {
    c.conn()
        .query_row(
            "SELECT count(*) FROM claims c WHERE c.pid IS NOT NULL AND c.state IN ('claimed', 'running', 'ending')",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

#[test]
fn the_run_attaches_its_own_pid_once_and_only_to_a_claim_with_no_process() {
    let (_t, mut c) = world();
    let (conv, turn) = fired(&mut c, "daily", "kai", false);
    let id = claim_for(&mut c, &conv, "kai", turn).unwrap();
    assert_eq!(
        claim_row(&c, id),
        ("turn".into(), "claimed".into(), None),
        "no process yet: not a run going"
    );
    assert_eq!(going(&c), 0);
    assert_eq!(
        attach(&mut c, id, 4242, "e1", NOW + 2).unwrap(),
        json!({"attached": true})
    );
    assert_eq!(
        claim_row(&c, id),
        ("external".into(), "running".into(), Some(4242))
    );
    assert_eq!(turn_state(&c, turn), "running");
    assert_eq!(going(&c), 1, "now the rollback hand-back sees it");
    // The same process asking again is answered; another one (two hosts spawned run-turn twice) loses the check-and-set.
    assert_eq!(
        attach(&mut c, id, 4242, "e1", NOW + 3).unwrap(),
        json!({"attached": true, "again": true})
    );
    assert_eq!(
        attach(&mut c, id, 5555, "e9", NOW + 3).unwrap(),
        json!({"refused": "gone", "state": "running"})
    );
    assert_eq!(
        claim_row(&c, id).2,
        Some(4242),
        "the first one's pid stands"
    );
}

#[test]
fn a_cancelled_or_ended_claim_cannot_be_attached_so_nothing_runs_unseen() {
    let (_t, mut c) = world();
    // The rollback's and the pause's cancel of a claim with no process yet: the later attach fails.
    let (conv, turn) = fired(&mut c, "daily", "kai", false);
    let id = claim_for(&mut c, &conv, "kai", turn).unwrap();
    let tx = c.claim_tx().unwrap();
    assert_eq!(
        claims::cancel_waiting_in(&tx, &[turn], "rollback", NOW + 2).unwrap(),
        vec![turn]
    );
    tx.commit().unwrap();
    assert_eq!(
        attach(&mut c, id, 4242, "e1", NOW + 3).unwrap(),
        json!({"refused": "gone", "state": "ended"})
    );
    assert_eq!(turn_state(&c, turn), "cancelled");
    assert_eq!(going(&c), 0);
    // A revoke between the claim and the attach.
    let (conv, turn) = fired(&mut c, "weekly", "sam", false);
    let id = claim_for(&mut c, &conv, "sam", turn).unwrap();
    revoke_person(&mut c, "sam", NOW + 4).unwrap();
    assert_eq!(
        attach(&mut c, id, 4243, "e2", NOW + 5).unwrap()["refused"],
        "gone"
    );
    // An id that never was.
    assert_eq!(
        attach(&mut c, 999, 1, "e", NOW).unwrap()["state"],
        "unknown"
    );
}

#[test]
fn only_a_schedule_turn_is_claimed_this_way_and_a_claim_with_no_turn_cannot_attach() {
    let (_t, mut c) = world();
    let (conv, _) = fired(&mut c, "daily", "kai", false);
    let other = claims::enqueue_turn(
        &mut c,
        &NewTurn {
            conversation: &conv,
            agent: "mel",
            engine: "claude",
            source: "app",
            person: "kai",
            prompt: "hi",
            deadline: None,
            revoke_seen: 0,
        },
        NOW,
    )
    .unwrap();
    let e = claim_for(&mut c, &conv, "kai", other).unwrap_err();
    assert!(e.to_string().contains("not waiting"), "{e}");
}

#[test]
fn a_host_restart_requeues_a_claim_with_no_process_and_leaves_a_run_that_is_alive() {
    let (_t, mut c) = world();
    let procs = Procs::default();
    let (conv_a, a) = fired(&mut c, "a", "kai", false);
    let (conv_b, b) = fired(&mut c, "b", "kai", false);
    let no_pid = claim_for(&mut c, &conv_a, "kai", a).unwrap();
    let running = claim_for(&mut c, &conv_b, "kai", b).unwrap();
    attach(&mut c, running, 700, "e7", NOW + 2).unwrap();
    procs.spawn(700, "e7");
    let adopted = warm::adopt(&mut c, NEW, NOW + 3).unwrap();
    assert_eq!(
        adopted.ended,
        vec![no_pid],
        "never attached: nothing can be running, the turn goes back"
    );
    assert!(adopted.kill.is_empty(), "the run is not killed");
    assert_eq!(turn_state(&c, a), "queued");
    assert_eq!(
        claim_row(&c, running),
        ("external".into(), "running".into(), Some(700))
    );
    assert_eq!(turn_state(&c, b), "running");
    assert!(
        claims::end_gone(&mut c, &procs, NOW + 4)
            .unwrap()
            .is_empty(),
        "alive: kept"
    );
}

#[test]
fn a_run_that_died_after_it_attached_is_ended_and_its_turn_failed_never_requeued() {
    let (_t, mut c) = world();
    let procs = Procs::default();
    let (conv, turn) = fired(&mut c, "daily", "kai", false);
    let id = claim_for(&mut c, &conv, "kai", turn).unwrap();
    attach(&mut c, id, 700, "e7", NOW + 2).unwrap();
    warm::adopt(&mut c, NEW, NOW + 3).unwrap();
    // The process is gone (killed with the Mac, or ended while the host was down) and nobody reported.
    assert_eq!(claims::end_gone(&mut c, &procs, NOW + 4).unwrap(), vec![id]);
    assert_eq!(claim_row(&c, id).1, "ended");
    assert_eq!(
        turn_state(&c, turn),
        "failed",
        "outcome unknown: never retried, its prompt may have gone out"
    );
    assert_eq!(
        c.conn()
            .query_row("SELECT requeues FROM turns WHERE id = ?1", [turn], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn the_host_reports_a_clean_exit_done_and_a_bad_one_failed() {
    let (_t, mut c) = world();
    let (conv_a, a) = fired(&mut c, "a", "kai", false);
    let (conv_b, b) = fired(&mut c, "b", "kai", false);
    let ok = claim_for(&mut c, &conv_a, "kai", a).unwrap();
    let bad = claim_for(&mut c, &conv_b, "kai", b).unwrap();
    attach(&mut c, ok, 700, "e7", NOW + 2).unwrap();
    attach(&mut c, bad, 701, "e8", NOW + 2).unwrap();
    claims::report(
        &mut c,
        ok,
        "done",
        &json!({"pid": 700}).to_string(),
        NOW + 3,
    )
    .unwrap();
    claims::report(
        &mut c,
        bad,
        "exit",
        &json!({"pid": 701}).to_string(),
        NOW + 3,
    )
    .unwrap();
    assert_eq!(
        (turn_state(&c, a).as_str(), claim_row(&c, ok).1.as_str()),
        ("done", "ended")
    );
    assert_eq!(
        turn_state(&c, b),
        "failed",
        "an exit of a scheduled run is not a requeue"
    );
}

#[test]
fn a_paused_jobs_turn_is_cancelled_at_the_claim_and_never_run() {
    let (_t, mut c) = world();
    let (conv, turn) = fired(&mut c, "daily", "kai", true);
    assert!(paused_in(c.conn(), turn).unwrap());
    let e = claim_for(&mut c, &conv, "kai", turn).unwrap_err();
    assert!(e.to_string().contains("is paused"), "{e}");
    assert_eq!(
        turn_state(&c, turn),
        "cancelled",
        "level-triggered: not left queued to be claimed again"
    );
    // A turn of another source in a paused job's conversation is not touched by the job's pause.
    let app = claims::enqueue_turn(
        &mut c,
        &NewTurn {
            conversation: &conv,
            agent: "mel",
            engine: "claude",
            source: "app",
            person: "kai",
            prompt: "hi",
            deadline: None,
            revoke_seen: 0,
        },
        NOW,
    )
    .unwrap();
    assert!(!paused_in(c.conn(), app).unwrap());
}

#[test]
fn a_host_change_gives_a_paused_jobs_claimed_turn_up_instead_of_requeueing_it() {
    let (_t, mut c) = world();
    let (conv, turn) = fired(&mut c, "daily", "kai", false);
    claim_for(&mut c, &conv, "kai", turn).unwrap();
    // The pause lands after the claim, as a flag only (the cancel of the pause's own write may have missed it).
    c.conn()
        .execute("UPDATE sched_jobs SET paused = 1", [])
        .unwrap();
    warm::adopt(&mut c, NEW, NOW + 3).unwrap();
    assert_eq!(
        turn_state(&c, turn),
        "cancelled",
        "pause wins over the host-change requeue"
    );
}

#[test]
fn an_out_of_usage_restart_of_a_paused_jobs_run_cancels_it_instead_of_putting_it_back() {
    let (_t, mut c) = world();
    let (conv, turn) = fired(&mut c, "daily", "kai", false);
    let id = claim_for(&mut c, &conv, "kai", turn).unwrap();
    // A bot-style run of the same turn: the claim ends on an exit report that would requeue it.
    c.conn().execute("UPDATE claims SET kind = 'turn', state = 'running', pid = 9, pid_started = 'e' WHERE claim_id = ?1", [id]).unwrap();
    c.conn()
        .execute("UPDATE turns SET state = 'running' WHERE id = ?1", [turn])
        .unwrap();
    c.conn()
        .execute("UPDATE sched_jobs SET paused = 1", [])
        .unwrap();
    claims::report(
        &mut c,
        id,
        "exit",
        &json!({"pid": 9, "out_of_usage": true}).to_string(),
        NOW + 3,
    )
    .unwrap();
    assert_eq!(turn_state(&c, turn), "cancelled");
}

#[test]
fn a_revoke_kills_a_guests_scheduled_run_because_its_claim_is_swept_by_person() {
    let (_t, mut c) = world();
    let (conv, turn) = fired(&mut c, "weekly", "sam", false);
    let id = claim_for(&mut c, &conv, "sam", turn).unwrap();
    attach(&mut c, id, 700, "e7", NOW + 2).unwrap();
    // Through the row the sweep uses (`kith-sessions sweep`, a host's revoke): the run's process is named to be signalled.
    let out = call_at(
        &mut c,
        "claim.sweepPerson",
        &json!({"persons": ["sam"]}),
        NOW + 3,
        &Procs::default(),
    )
    .unwrap();
    let kill = out["kill"].as_array().unwrap();
    assert_eq!(kill.len(), 1, "{out}");
    assert_eq!(
        (kill[0]["claimId"].as_i64(), kill[0]["pid"].as_i64()),
        (Some(id), Some(700))
    );
    assert_eq!(claim_row(&c, id).1, "ending");
    assert_eq!(turn_state(&c, turn), "cancelled");
}

#[test]
fn releasing_a_pid_never_ends_a_scheduled_runs_claim() {
    let (_t, mut c) = world();
    let (conv, turn) = fired(&mut c, "daily", "owner", false);
    let id = claim_for(&mut c, &conv, "owner", turn).unwrap();
    attach(&mut c, id, 4242, "e1", NOW + 2).unwrap();
    // The owner's run-chain asks for its own pid's claims; the scheduled run's pid is no run of that kind.
    let p = json!({"pid": 4242, "started": "e1"});
    let said = call_at(&mut c, "claim.release", &p, NOW + 3, &Procs::default()).unwrap();
    assert_eq!(said["ended"], json!([]), "{said}");
    assert_eq!(
        claim_row(&c, id),
        ("external".into(), "running".into(), Some(4242))
    );
    assert_eq!(turn_state(&c, turn), "running");
}

#[test]
fn claim_attach_is_served_as_a_row_and_answers_data() {
    let (_t, mut c) = world();
    let (conv, turn) = fired(&mut c, "daily", "kai", false);
    let id = claim_for(&mut c, &conv, "kai", turn).unwrap();
    assert!(crate::conversations::external::serves("claim.attach"));
    let p = json!({"claimId": id, "pid": 4242, "started": "e1"});
    assert_eq!(
        call_at(&mut c, "claim.attach", &p, NOW + 2, &Procs::default()).unwrap(),
        json!({"attached": true})
    );
    let again = json!({"claimId": id, "pid": 4243, "started": "e2"});
    assert_eq!(
        call_at(&mut c, "claim.attach", &again, NOW + 2, &Procs::default()).unwrap()["refused"],
        "gone"
    );
}
