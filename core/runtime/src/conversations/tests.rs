#![allow(clippy::items_after_statements, clippy::assert_is_empty)]
//! The claim code on a real core.db (no mocks of SQLite). Processes are made up: `Procs` answers the probes the real
//! system would.

use std::cell::RefCell;
use std::collections::HashMap;

use tempfile::TempDir;

use super::claims::{
    Kill, Kind, NewClaim, Redeemed, Reported, claim, different_holder, end_gone, reconcile, redeem,
    report, revoke_person, stop,
};
use crate::db::{Core, MIGRATIONS, RecordedProc};
use crate::error::Error;
use crate::gate::Liveness;
use crate::layout::Layout;
use crate::start_with;
use crate::tests::machine;

/// Made-up processes: pid -> start time. A pid not in the map is gone.
#[derive(Default)]
pub(super) struct Procs(pub RefCell<HashMap<u32, String>>);

impl Procs {
    pub(super) fn spawn(&self, pid: u32, started: &str) {
        self.0.borrow_mut().insert(pid, started.into());
    }
    pub(super) fn kill(&self, pid: u32) {
        self.0.borrow_mut().remove(&pid);
    }
    pub(super) fn alive(&self, pid: u32) -> bool {
        self.0.borrow().contains_key(&pid)
    }
}

impl Liveness for Procs {
    fn start(&self, pid: u32) -> Option<String> {
        self.0.borrow().get(&pid).cloned()
    }
    fn gone(&self, pid: u32) -> bool {
        !self.0.borrow().contains_key(&pid)
    }
}

/// A core whose reconcile has run, so claims are open.
pub(super) fn open() -> (TempDir, Core) {
    let (t, m) = machine();
    let l = Layout::new(&m);
    let mut c = start_with(&l, MIGRATIONS).expect("start");
    reconcile(&mut c, &[], &[], i64::MAX, 1).expect("reconcile");
    (t, c)
}

/// Simulates a new generation's start (the reconcile marker is per generation), so a test can reconcile again.
pub(super) fn unreconcile(c: &Core) {
    c.conn()
        .execute(
            "DELETE FROM meta WHERE key = ?1",
            [super::claims::RECONCILED],
        )
        .expect("unreconcile");
}

pub(super) fn conversation(c: &Core, id: &str) {
    c.conn()
        .execute(
            "INSERT OR IGNORE INTO conversations (id, agent, engine, created_at, updated_at) VALUES (?1, 'a', 'claude', 1, 1)",
            [id],
        )
        .expect("conversation");
}

pub(super) fn turn(c: &Core, conv: &str, person: &str) -> i64 {
    c.conn()
        .execute(
            "INSERT INTO turns (conversation, person, source, state, queued_at) VALUES (?1, ?2, 'app', 'queued', 1)",
            [conv, person],
        )
        .expect("turn");
    c.conn().last_insert_rowid()
}

pub(super) fn state(c: &Core, id: i64) -> String {
    c.conn()
        .query_row("SELECT state FROM claims WHERE claim_id = ?1", [id], |r| {
            r.get(0)
        })
        .expect("claim state")
}

pub(super) fn turn_state(c: &Core, id: i64) -> String {
    c.conn()
        .query_row("SELECT state FROM turns WHERE id = ?1", [id], |r| r.get(0))
        .expect("turn state")
}

pub(super) fn count(c: &Core, sql: &str) -> i64 {
    c.conn().query_row(sql, [], |r| r.get(0)).expect("count")
}

pub(super) fn new_claim(conv: &str, turn: Option<i64>) -> NewClaim<'_> {
    NewClaim {
        conversation: conv,
        kind: Kind::Turn,
        person: "sam",
        turn,
        holder_instance: Some("h1"),
        account: None,
        process: None,
        revoke_seen: 0,
    }
}

/// Claims `conv` with a queued turn.
fn take(c: &mut Core, conv: &str) -> (i64, i64) {
    conversation(c, conv);
    let t = turn(c, conv, "sam");
    let id = claim(c, &new_claim(conv, Some(t)), 10)
        .expect("claim")
        .claim_id;
    (id, t)
}

fn running(c: &mut Core, conv: &str, pid: u32) -> (i64, i64) {
    let (id, t) = take(c, conv);
    let r = redeem(c, id, pid, &format!("e{pid}"), "h1", 20).expect("redeem");
    assert_eq!(r, Redeemed::Running { again: false });
    (id, t)
}

// ---- step 1

#[test]
fn a_claim_takes_the_conversation_and_marks_the_turn_claimed() {
    let (_t, mut c) = open();
    let (id, t) = take(&mut c, "c1");
    assert_eq!(state(&c, id), "claimed");
    assert_eq!(turn_state(&c, t), "claimed");
    let gen_: i64 = c
        .conn()
        .query_row(
            "SELECT generation FROM claims WHERE claim_id = ?1",
            [id],
            |r| r.get(0),
        )
        .expect("generation");
    assert_eq!(gen_, c.generation());
}

#[test]
fn claim_ids_are_never_reused() {
    let (_t, mut c) = open();
    let (a, _) = take(&mut c, "c1");
    report(&mut c, a, "done", "{}", 30).expect("done");
    let (b, _) = take(&mut c, "c1");
    assert!(b > a);
}

#[test]
fn a_second_claim_is_refused_while_the_first_is_claimed_running_or_ending() {
    let (_t, mut c) = open();
    let (id, _) = take(&mut c, "c1");
    let again = |c: &mut Core| claim(c, &new_claim("c1", None), 11);
    assert!(matches!(again(&mut c), Err(Error::Refused(m)) if m.contains("claimed")));
    redeem(&mut c, id, 100, "e100", "h1", 12).expect("redeem");
    assert!(matches!(again(&mut c), Err(Error::Refused(m)) if m.contains("running")));
    stop(&mut c, id, "stop", false, 13).expect("stop");
    assert_eq!(state(&c, id), "ending");
    assert!(matches!(again(&mut c), Err(Error::Refused(m)) if m.contains("ending")));
    // Another conversation is free.
    conversation(&c, "c2");
    claim(&mut c, &new_claim("c2", None), 14).expect("other conversation");
}

#[test]
fn the_index_itself_refuses_a_second_writer_even_past_the_code_check() {
    let (_t, mut c) = open();
    let (_id, _) = take(&mut c, "c1");
    let e = c.conn().execute(
        "INSERT INTO claims (claim_id, conversation, generation, kind, state, created_at) VALUES (999, 'c1', 1, 'turn', 'claimed', 1)",
        [],
    );
    assert!(e.is_err());
}

#[test]
fn a_claim_is_refused_until_the_generation_reconciled() {
    let (_t, m) = machine();
    let l = Layout::new(&m);
    let mut c = start_with(&l, MIGRATIONS).expect("start");
    conversation(&c, "c1");
    let e = claim(&mut c, &new_claim("c1", None), 1).expect_err("not reconciled");
    assert!(matches!(e, Error::Refused(m) if m.contains("reconcile")));
    assert_eq!(count(&c, "SELECT count(*) FROM claims"), 0);
    reconcile(&mut c, &[], &[], i64::MAX, 2).expect("reconcile");
    claim(&mut c, &new_claim("c1", None), 3).expect("after reconcile");
}

#[test]
fn a_claim_for_an_unknown_conversation_or_a_turn_not_waiting_changes_nothing() {
    let (_t, mut c) = open();
    assert!(claim(&mut c, &new_claim("nope", None), 1).is_err());
    conversation(&c, "c1");
    let t = turn(&c, "c1", "sam");
    c.conn()
        .execute("UPDATE turns SET state = 'done' WHERE id = ?1", [t])
        .expect("done");
    assert!(claim(&mut c, &new_claim("c1", Some(t)), 2).is_err());
    assert_eq!(count(&c, "SELECT count(*) FROM claims"), 0);
    // The counter rolled back with the transaction.
    assert_eq!(
        count(&c, "SELECT value FROM counters WHERE name = 'claim_id'"),
        0
    );
}

#[test]
fn an_external_claim_needs_its_process() {
    let (_t, mut c) = open();
    conversation(&c, "c1");
    let mut req = new_claim("c1", None);
    req.kind = Kind::External;
    assert!(matches!(claim(&mut c, &req, 1), Err(Error::BadParams(_))));
    req.process = Some((4242, "Tue Nov 4 10:00:00 2026"));
    let id = claim(&mut c, &req, 1).expect("external").claim_id;
    let pid: i64 = c
        .conn()
        .query_row("SELECT pid FROM claims WHERE claim_id = ?1", [id], |r| {
            r.get(0)
        })
        .expect("pid");
    assert_eq!(pid, 4242);
}

#[test]
fn a_stale_generation_is_refused_by_every_step() {
    let (_t, mut c) = open();
    let (id, _) = take(&mut c, "c1");
    // A newer core took over.
    c.conn()
        .execute(
            "UPDATE counters SET value = value + 1 WHERE name = 'generation'",
            [],
        )
        .expect("bump");
    conversation(&c, "c2");
    assert!(matches!(
        claim(&mut c, &new_claim("c2", None), 1),
        Err(Error::StaleGeneration(_))
    ));
    assert!(matches!(
        redeem(&mut c, id, 1, "s", "h1", 1),
        Err(Error::StaleGeneration(_))
    ));
    assert!(matches!(
        report(&mut c, id, "done", "{}", 1),
        Err(Error::StaleGeneration(_))
    ));
    assert!(matches!(
        stop(&mut c, id, "x", false, 1),
        Err(Error::StaleGeneration(_))
    ));
    assert!(matches!(
        revoke_person(&mut c, "sam", 1),
        Err(Error::StaleGeneration(_))
    ));
    assert!(matches!(
        reconcile(&mut c, &[], &[], i64::MAX, 1),
        Err(Error::StaleGeneration(_))
    ));
    assert!(matches!(
        different_holder(&mut c, "h2", &[], &[], i64::MAX, 1),
        Err(Error::StaleGeneration(_))
    ));
    assert_eq!(state(&c, id), "claimed");
}

// ---- step 3: redeem

#[test]
fn redeem_moves_claimed_to_running_and_records_the_process() {
    let (_t, mut c) = open();
    let (id, t) = take(&mut c, "c1");
    let r = redeem(&mut c, id, 321, "Tue Nov 4 10:00:00 2026", "h9", 20).expect("redeem");
    assert_eq!(r, Redeemed::Running { again: false });
    assert_eq!(state(&c, id), "running");
    assert_eq!(turn_state(&c, t), "running");
    let (pid, started, inst, at): (i64, String, String, i64) = c
        .conn()
        .query_row(
            "SELECT pid, pid_started, holder_instance, running_at FROM claims WHERE claim_id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .expect("row");
    assert_eq!(
        (pid, started.as_str(), inst.as_str(), at),
        (321, "Tue Nov 4 10:00:00 2026", "h9", 20)
    );
}

#[test]
fn a_retried_redeem_is_idempotent_and_another_process_is_refused() {
    let (_t, mut c) = open();
    let (id, _) = running(&mut c, "c1", 100);
    assert_eq!(
        redeem(&mut c, id, 100, "e100", "h1", 21).expect("again"),
        Redeemed::Running { again: true }
    );
    assert_eq!(
        redeem(&mut c, id, 101, "e101", "h1", 22).expect("other pid"),
        Redeemed::Refused("running".into())
    );
    // The same pid with another start time is another process.
    assert_eq!(
        redeem(&mut c, id, 100, "s-other", "h1", 23).expect("reused pid"),
        Redeemed::Refused("running".into())
    );
    let (pid, at): (i64, i64) = c
        .conn()
        .query_row(
            "SELECT pid, running_at FROM claims WHERE claim_id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("row");
    assert_eq!((pid, at), (100, 20), "the first redeem stands");
}

#[test]
fn a_redeem_of_an_unknown_or_ended_claim_changes_nothing() {
    let (_t, mut c) = open();
    assert_eq!(
        redeem(&mut c, 77, 1, "s", "h1", 1).expect("unknown"),
        Redeemed::Refused("unknown".into())
    );
    let (id, _) = take(&mut c, "c1");
    report(&mut c, id, "exit", "{}", 30).expect("exit");
    assert_eq!(state(&c, id), "ended");
    assert_eq!(
        redeem(&mut c, id, 5, "e5", "h1", 31).expect("late"),
        Redeemed::Refused("ended".into())
    );
    assert_eq!(
        count(&c, "SELECT count(*) FROM claims WHERE pid IS NOT NULL"),
        0
    );
}

#[test]
fn a_redeem_after_a_release_on_a_new_holder_instance_fails() {
    let (_t, mut c) = open();
    let (id, t) = take(&mut c, "c1");
    let out = different_holder(&mut c, "h2", &[], &[], i64::MAX, 30).expect("new instance");
    assert_eq!(out.ended, vec![id]);
    assert_eq!(state(&c, id), "ended");
    assert_eq!(turn_state(&c, t), "queued", "the turn is given back");
    assert_eq!(
        redeem(&mut c, id, 100, "e100", "h1", 31).expect("late redeem"),
        Redeemed::Refused("ended".into())
    );
    assert_eq!(state(&c, id), "ended");
    assert_eq!(
        count(&c, "SELECT count(*) FROM claims WHERE pid IS NOT NULL"),
        0
    );
}

#[test]
fn a_claim_the_new_instance_lists_is_left_alone() {
    let (_t, mut c) = open();
    let (id, _) = take(&mut c, "c1");
    let out = different_holder(&mut c, "h2", &[id], &[], i64::MAX, 30).expect("new instance");
    assert_eq!(out, super::claims::Released::default());
    assert_eq!(state(&c, id), "claimed");
    // And a claim already of the new instance is not the old holder's to end.
    let (_t2, mut c2) = open();
    let (id2, _) = take(&mut c2, "c1");
    let out = different_holder(&mut c2, "h1", &[], &[], i64::MAX, 30).expect("same instance");
    assert!(out.ended.is_empty());
    assert_eq!(state(&c2, id2), "claimed");
}

// ---- reports

#[test]
fn busy_ends_the_claim_and_parks_the_turn_without_a_claim() {
    let (_t, mut c) = open();
    let (id, t) = take(&mut c, "c1");
    let r = report(&mut c, id, "busy", "{}", 30).expect("busy");
    assert_eq!(
        r,
        Reported::Stored {
            applied: Some("busy")
        }
    );
    assert_eq!(state(&c, id), "ended");
    let (st, reason, claim_id): (String, String, Option<i64>) = c
        .conn()
        .query_row(
            "SELECT state, parked_reason, claim_id FROM turns WHERE id = ?1",
            [t],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("turn");
    assert_eq!(
        (st.as_str(), reason.as_str(), claim_id),
        ("parked", "lock_busy", None)
    );
    // The conversation is free for the next claim.
    claim(&mut c, &new_claim("c1", None), 31).expect("free");
}

#[test]
fn a_busy_for_a_running_claim_changes_nothing() {
    let (_t, mut c) = open();
    let (id, t) = running(&mut c, "c1", 100);
    let r = report(&mut c, id, "busy", "{}", 30).expect("busy");
    assert_eq!(r, Reported::Stored { applied: None });
    assert_eq!(state(&c, id), "running");
    assert_eq!(turn_state(&c, t), "running");
}

#[test]
fn done_ends_the_claim_and_finishes_the_turn_and_exit_requeues_it() {
    let (_t, mut c) = open();
    let (id, t) = running(&mut c, "c1", 100);
    report(&mut c, id, "done", r#"{"pid":100}"#, 30).expect("done");
    assert_eq!(state(&c, id), "ended");
    assert_eq!(turn_state(&c, t), "done");

    let (id2, t2) = running(&mut c, "c2", 200);
    report(&mut c, id2, "exit", r#"{"pid":200}"#, 30).expect("exit");
    assert_eq!(state(&c, id2), "ended");
    assert_eq!(turn_state(&c, t2), "queued");
    let (requeues, claim_id): (i64, Option<i64>) = c
        .conn()
        .query_row(
            "SELECT requeues, claim_id FROM turns WHERE id = ?1",
            [t2],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("turn");
    assert_eq!((requeues, claim_id), (1, None));
}

#[test]
fn a_retried_report_is_idempotent_and_a_late_busy_cannot_replace_a_done() {
    let (_t, mut c) = open();
    let (id, t) = running(&mut c, "c1", 100);
    let first = report(&mut c, id, "done", r#"{"pid":100}"#, 30).expect("done");
    assert_eq!(
        first,
        Reported::Stored {
            applied: Some("done")
        }
    );
    let again = report(&mut c, id, "done", r#"{"pid":100}"#, 31).expect("retry");
    assert_eq!(again, Reported::Stored { applied: None });
    report(&mut c, id, "busy", "{}", 32).expect("late busy");
    assert_eq!(state(&c, id), "ended");
    assert_eq!(turn_state(&c, t), "done");
    assert_eq!(
        count(
            &c,
            &format!("SELECT count(*) FROM reports WHERE claim_id = {id}")
        ),
        2,
        "done and busy are both kept, keyed by kind"
    );
    assert_eq!(
        count(&c, "SELECT count(*) FROM reports WHERE acked_at IS NULL"),
        0
    );
    let end_reason: String = c
        .conn()
        .query_row(
            "SELECT end_reason FROM claims WHERE claim_id = ?1",
            [id],
            |r| r.get(0),
        )
        .expect("reason");
    assert_eq!(end_reason, "done");
}

#[test]
fn a_report_for_an_unknown_claim_id_is_rejected_and_stores_nothing() {
    let (_t, mut c) = open();
    assert_eq!(
        report(&mut c, 12345, "done", "{}", 1).expect("unknown"),
        Reported::Unknown
    );
    assert_eq!(count(&c, "SELECT count(*) FROM reports"), 0);
    assert!(matches!(
        report(&mut c, 1, "nonsense", "{}", 1),
        Err(Error::BadParams(_))
    ));
}

#[test]
fn a_report_for_an_old_claim_never_lands_on_the_current_one() {
    let (_t, mut c) = open();
    let (old, _) = running(&mut c, "c1", 100);
    report(&mut c, old, "done", r#"{"pid":100}"#, 30).expect("done");
    let (new, t) = take(&mut c, "c1");
    // A late busy and a late exit for the old id: kept under the old id, acked, nothing applied to the new claim.
    for kind in ["busy", "exit"] {
        let r = report(&mut c, old, kind, "{}", 40).expect("late");
        assert_eq!(r, Reported::Stored { applied: None }, "{kind}");
    }
    assert_eq!(state(&c, new), "claimed");
    assert_eq!(turn_state(&c, t), "claimed");
    assert_eq!(
        count(
            &c,
            &format!("SELECT count(*) FROM reports WHERE claim_id = {new}")
        ),
        0
    );
}

#[test]
fn an_exit_of_another_process_of_the_claim_does_not_end_the_writer() {
    let (_t, mut c) = open();
    let (id, _) = running(&mut c, "c1", 100);
    // A respawn (pid 101) that lost the lock exits; the redeemed pid 100 still writes.
    let r = report(&mut c, id, "exit", r#"{"pid":101}"#, 30).expect("exit");
    assert_eq!(r, Reported::Stored { applied: None });
    assert_eq!(state(&c, id), "running");
}

#[test]
fn precedence_is_done_then_exit_then_busy_then_spawned() {
    let (_t, mut c) = open();
    let (id, t) = take(&mut c, "c1");
    // Insert the unacked reports directly (as a retry storm after a crash would leave them), then settle with one more.
    for k in ["spawned", "busy", "exit"] {
        c.conn()
            .execute(
                "INSERT INTO reports (claim_id, kind, body, received_at) VALUES (?1, ?2, '{}', 1)",
                rusqlite::params![id, k],
            )
            .expect("insert");
    }
    let r = report(&mut c, id, "done", "{}", 30).expect("done");
    assert_eq!(
        r,
        Reported::Stored {
            applied: Some("done")
        }
    );
    assert_eq!(turn_state(&c, t), "done", "done won over exit and busy");
    assert_eq!(
        count(&c, "SELECT count(*) FROM reports WHERE acked_at IS NULL"),
        0,
        "the rest were acked with it"
    );
}

#[test]
fn spawned_records_the_stub_so_a_lost_redeem_still_leaves_a_pid() {
    let (_t, mut c) = open();
    let (id, _) = take(&mut c, "c1");
    let r = report(&mut c, id, "spawned", r#"{"pid":55,"started":"e55"}"#, 30).expect("spawned");
    assert_eq!(
        r,
        Reported::Stored {
            applied: Some("spawned")
        }
    );
    let (pid, started): (i64, String) = c
        .conn()
        .query_row(
            "SELECT pid, pid_started FROM claims WHERE claim_id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("row");
    assert_eq!((pid, started.as_str()), (55, "e55"));
    assert_eq!(state(&c, id), "claimed");
}

// ---- stop, ending and gone

#[test]
fn a_running_claim_keeps_one_writer_through_ending_until_the_pid_is_gone() {
    let (_t, mut c) = open();
    let procs = Procs::default();
    procs.spawn(100, "e100");
    let (id, t) = running(&mut c, "c1", 100);
    let kill = stop(&mut c, id, "revoked", false, 30)
        .expect("stop")
        .expect("kill");
    assert_eq!(
        kill,
        Kill {
            claim_id: Some(id),
            pid: Some(100),
            started: Some("e100".into())
        }
    );
    assert_eq!(state(&c, id), "ending");
    assert_eq!(turn_state(&c, t), "cancelled");
    // The kill is re-sent (level-triggered).
    assert!(
        stop(&mut c, id, "revoked", false, 31)
            .expect("again")
            .is_some()
    );
    // Alive: stays ending, and a second claim is still refused.
    assert!(end_gone(&mut c, &procs, 32).expect("probe").is_empty());
    assert_eq!(state(&c, id), "ending");
    assert!(claim(&mut c, &new_claim("c1", None), 33).is_err());
    // Gone: ended, and the conversation is free.
    procs.kill(100);
    assert_eq!(end_gone(&mut c, &procs, 34).expect("probe"), vec![id]);
    assert_eq!(state(&c, id), "ended");
    claim(&mut c, &new_claim("c1", None), 35).expect("free");
}

#[test]
fn a_reused_pid_counts_as_gone_and_a_probe_that_cannot_tell_does_not() {
    let (_t, mut c) = open();
    let procs = Procs::default();
    procs.spawn(100, "e100");
    let (id, _) = running(&mut c, "c1", 100);
    stop(&mut c, id, "stop", true, 30).expect("stop");
    // The pid now belongs to a stranger.
    procs.spawn(100, "e999");
    assert_eq!(end_gone(&mut c, &procs, 31).expect("probe"), vec![id]);

    // A probe that neither reads a start time nor says "gone".
    struct Unsure;
    impl Liveness for Unsure {
        fn start(&self, _: u32) -> Option<String> {
            None
        }
        fn gone(&self, _: u32) -> bool {
            false
        }
    }
    let (id2, _) = running(&mut c, "c2", 200);
    stop(&mut c, id2, "stop", true, 32).expect("stop");
    assert!(end_gone(&mut c, &Unsure, 33).expect("probe").is_empty());
    assert_eq!(state(&c, id2), "ending");
}

#[test]
fn stop_requeues_or_cancels_the_turn_and_ignores_a_claim_that_is_not_live() {
    let (_t, mut c) = open();
    let (id, t) = running(&mut c, "c1", 100);
    stop(&mut c, id, "take-over", true, 30).expect("stop");
    assert_eq!(turn_state(&c, t), "queued");
    report(&mut c, id, "exit", r#"{"pid":100}"#, 31).expect("exit confirms");
    assert_eq!(state(&c, id), "ended");
    assert_eq!(
        turn_state(&c, t),
        "queued",
        "an ending claim's exit leaves the turn as stop left it"
    );
    assert!(
        stop(&mut c, id, "again", false, 32)
            .expect("ended")
            .is_none()
    );
    assert!(
        stop(&mut c, 999, "x", false, 32)
            .expect("unknown")
            .is_none()
    );
}

// ---- step 6

#[test]
fn revoking_a_person_cancels_their_turns_and_ends_their_claims_in_order() {
    let (_t, mut c) = open();
    let (run, t_run) = running(&mut c, "c1", 100);
    let (held, t_held) = take(&mut c, "c2");
    let t_queued = turn(&c, "c1", "sam");
    let t_parked = turn(&c, "c2", "sam");
    c.conn()
        .execute(
            "UPDATE turns SET state = 'parked', parked_reason = 'terminal_live' WHERE id = ?1",
            [t_parked],
        )
        .expect("park");
    // Another person's work is untouched.
    conversation(&c, "c3");
    let other = turn(&c, "c3", "eve");
    let (eve_claim, _) = {
        let id = claim(
            &mut c,
            &NewClaim {
                person: "eve",
                ..new_claim("c3", Some(other))
            },
            12,
        )
        .expect("eve")
        .claim_id;
        (id, other)
    };
    let out = revoke_person(&mut c, "sam", 40).expect("revoke");
    assert_eq!(out.cancelled_turns, 2);
    assert_eq!(turn_state(&c, t_queued), "cancelled");
    assert_eq!(turn_state(&c, t_parked), "cancelled");
    assert_eq!(turn_state(&c, t_run), "cancelled");
    assert_eq!(turn_state(&c, t_held), "cancelled");
    assert_eq!(state(&c, run), "ending");
    assert_eq!(
        state(&c, held),
        "ended",
        "never spawned: nothing to wait for"
    );
    assert_eq!(out.kill.len(), 1);
    assert_eq!(state(&c, eve_claim), "claimed");
    assert_eq!(turn_state(&c, other), "claimed");
    // Still the writer until gone: a new claim for the revoked person's conversation is refused.
    assert!(claim(&mut c, &new_claim("c1", None), 41).is_err());
    // Re-sent by any running core.
    assert_eq!(
        revoke_person(&mut c, "sam", 42).expect("again").kill.len(),
        1
    );
}

// ---- steps 5 and 7

#[test]
fn reconcile_ends_unlisted_claimed_claims_and_holds_unlisted_running_ones_as_ending() {
    let (_t, mut c) = open();
    let (kept, _) = running(&mut c, "k", 10);
    let (claimed, t_claimed) = take(&mut c, "a");
    let (run, t_run) = running(&mut c, "b", 20);
    // The new core bumps the generation (simulated) and reconciles with a holder that lists only `kept`.
    unreconcile(&c);
    let out = reconcile(&mut c, &[kept, 4242], &[], i64::MAX, 50).expect("reconcile");
    assert_eq!(out.ended, vec![claimed]);
    assert_eq!(state(&c, claimed), "ended");
    assert_eq!(turn_state(&c, t_claimed), "queued");
    assert_eq!(state(&c, run), "ending", "never straight to ended");
    assert_eq!(turn_state(&c, t_run), "queued");
    assert_eq!(
        out.kill,
        vec![Kill {
            claim_id: Some(run),
            pid: Some(20),
            started: Some("e20".into())
        }]
    );
    assert_eq!(out.kill_listed, vec![4242], "listed with no row");
    assert_eq!(state(&c, kept), "running", "re-adopted");
}

#[test]
fn reconcile_lists_a_holder_entry_whose_claim_is_ended() {
    let (_t, mut c) = open();
    let (id, _) = running(&mut c, "c1", 100);
    report(&mut c, id, "done", r#"{"pid":100}"#, 30).expect("done");
    unreconcile(&c);
    let out = reconcile(&mut c, &[id], &[], i64::MAX, 31).expect("reconcile");
    assert_eq!(out.kill_listed, vec![id]);
}

#[test]
fn a_different_holder_instance_kills_recorded_processes_with_no_redeem() {
    let (_t, mut c) = open();
    let (id, _) = running(&mut c, "c1", 100);
    let strays = [
        RecordedProc {
            pid: 100,
            started: "e100".into(),
            conv: "c1".into(),
        },
        RecordedProc {
            pid: 300,
            started: "e300".into(),
            conv: "c9".into(),
        },
    ];
    let out = different_holder(&mut c, "h2", &[], &strays, i64::MAX, 30).expect("new instance");
    assert_eq!(
        out.kill_procs,
        vec![Kill {
            claim_id: None,
            pid: Some(300),
            started: Some("e300".into())
        }],
        "the redeemed pid is the claim's kill, not a stray"
    );
    assert_eq!(out.kill.len(), 1);
    assert_eq!(state(&c, id), "ending");
}

/// The holder-crash-donetruth sequence of the writer-lock skill: claim 1, the spawn reply is lost, the holder crashes,
/// the orphan stub redeems claim 1, the new holder instance does not list it.
#[test]
fn holder_crash_donetruth_the_orphan_redeems_and_the_claim_goes_to_ending_not_ended() {
    let (_t, mut c) = open();
    let procs = Procs::default();
    let (id, t) = take(&mut c, "c1");
    procs.spawn(500, "e500");
    // The holder crashed; the orphan stub (pid 500) takes the lock and redeems claim 1, which is still current.
    assert_eq!(
        redeem(&mut c, id, 500, "e500", "h1", 20).expect("orphan redeem"),
        Redeemed::Running { again: false }
    );
    // The core meets holder instance h2, which lists nothing.
    let out = different_holder(&mut c, "h2", &[], &[], i64::MAX, 30).expect("new instance");
    assert!(out.ended.is_empty(), "not released under a running engine");
    assert_eq!(state(&c, id), "ending");
    assert_eq!(out.kill.len(), 1);
    assert_eq!(out.kill[0].pid, Some(500));
    assert_eq!(turn_state(&c, t), "queued");
    // While the pid lives the claim stays the writer.
    assert!(end_gone(&mut c, &procs, 31).expect("probe").is_empty());
    assert_eq!(state(&c, id), "ending");
    assert!(claim(&mut c, &new_claim("c1", None), 32).is_err());
    // Only the confirmed death releases it.
    procs.kill(500);
    assert_eq!(end_gone(&mut c, &procs, 33).expect("probe"), vec![id]);
    assert_eq!(state(&c, id), "ended");
}

/// The other order of the same race: the new instance meets the claim first, so the orphan's redeem fails and the engine
/// is never started.
#[test]
fn holder_crash_donetruth_other_order_the_release_wins_the_race() {
    let (_t, mut c) = open();
    let (id, _) = take(&mut c, "c1");
    different_holder(&mut c, "h2", &[], &[], i64::MAX, 30).expect("new instance");
    assert_eq!(state(&c, id), "ended");
    assert_eq!(
        redeem(&mut c, id, 500, "e500", "h1", 31).expect("orphan"),
        Redeemed::Refused("ended".into())
    );
}

#[test]
fn a_second_connection_that_releases_between_claim_and_redeem_wins_the_cas() {
    // Two handles on one core.db: a raw connection stands for another process's release landing first.
    let (t, mut c) = open();
    let (id, _) = take(&mut c, "c1");
    let db = Layout::new(&t.path().join("machine")).db;
    let other = rusqlite::Connection::open(db).expect("second connection");
    other
        .busy_timeout(std::time::Duration::from_secs(5))
        .expect("timeout");
    let n = other
        .execute(
            "UPDATE claims SET state = 'ended' WHERE claim_id = ?1 AND state = 'claimed'",
            [id],
        )
        .expect("release");
    assert_eq!(n, 1);
    assert_eq!(
        redeem(&mut c, id, 7, "e7", "h1", 40).expect("redeem"),
        Redeemed::Refused("ended".into())
    );
}

// ---- turns

fn new_turn<'a>(
    conv: &'a str,
    source: &'a str,
    deadline: Option<i64>,
) -> super::claims::NewTurn<'a> {
    super::claims::NewTurn {
        conversation: conv,
        agent: "a",
        engine: "claude",
        source,
        person: "sam",
        prompt: "hi",
        deadline,
        revoke_seen: 0,
    }
}

#[test]
fn enqueue_creates_the_conversation_and_validates_the_source() {
    use super::claims::enqueue_turn;
    let (_t, mut c) = open();
    let t = enqueue_turn(&mut c, &new_turn("fresh", "wake", None), 5).expect("enqueue");
    assert_eq!(turn_state(&c, t), "queued");
    assert_eq!(
        count(&c, "SELECT count(*) FROM conversations WHERE id = 'fresh'"),
        1
    );
    assert!(matches!(
        enqueue_turn(&mut c, &new_turn("fresh", "bogus", None), 6),
        Err(Error::BadParams(_))
    ));
    // The queued turn can be claimed.
    claim(&mut c, &new_claim("fresh", Some(t)), 7).expect("claim");
}

#[test]
fn park_wake_and_expire() {
    use super::claims::{enqueue_turn, expire_parked, park, unpark_ready};
    let (_t, mut c) = open();
    let a = enqueue_turn(&mut c, &new_turn("c1", "app", None), 1).expect("a");
    let b = enqueue_turn(&mut c, &new_turn("c1", "schedule", Some(100)), 1).expect("b");
    assert!(matches!(
        park(&mut c, a, "nope", 2),
        Err(Error::BadParams(_))
    ));
    assert!(park(&mut c, a, "lock_busy", 2).expect("park"));
    assert!(!park(&mut c, a, "lock_busy", 3).expect("already parked"));
    assert!(park(&mut c, b, "terminal_live", 2).expect("park b"));
    // A live writer keeps them parked.
    let (w, _) = running(&mut c, "c1", 100);
    assert!(
        unpark_ready(&mut c, "c1", true, false, 4)
            .expect("held")
            .is_empty()
    );
    report(&mut c, w, "done", r#"{"pid":100}"#, 5).expect("done");
    // The probes decide per reason.
    assert_eq!(
        unpark_ready(&mut c, "c1", false, true, 6).expect("none"),
        Vec::<i64>::new()
    );
    assert_eq!(
        unpark_ready(&mut c, "c1", true, true, 7).expect("lock"),
        vec![a]
    );
    assert_eq!(turn_state(&c, a), "queued");
    assert_eq!(turn_state(&c, b), "parked");
    // q218: b expires at its deadline.
    assert!(expire_parked(&mut c, 99).expect("early").is_empty());
    assert_eq!(expire_parked(&mut c, 100).expect("expire"), vec![b]);
    assert_eq!(turn_state(&c, b), "expired");
    assert_eq!(turn_state(&c, a), "queued", "no deadline: never expires");
}

// ---- the independent and architect reviews of #739

fn procs_with(pids: &[(u32, &str)]) -> Procs {
    let p = Procs::default();
    for (pid, s) in pids {
        p.spawn(*pid, s);
    }
    p
}

#[test]
fn the_writers_exit_ends_the_claim_after_another_process_exit_was_acked() {
    let (_t, mut c) = open();
    let (id, _) = running(&mut c, "c1", 100);
    let r = report(&mut c, id, "exit", r#"{"pid":101}"#, 30).expect("other");
    assert_eq!(r, Reported::Stored { applied: None });
    assert_eq!(state(&c, id), "running");
    let r = report(&mut c, id, "exit", r#"{"pid":100}"#, 31).expect("recorded");
    assert_eq!(
        r,
        Reported::Stored {
            applied: Some("exit")
        }
    );
    assert_eq!(state(&c, id), "ended");
    // The same for done, and a retry of the same report is idempotent.
    let (id2, _) = running(&mut c, "c2", 200);
    report(&mut c, id2, "done", r#"{"pid":201}"#, 32).expect("other done");
    report(&mut c, id2, "done", r#"{"pid":201}"#, 33).expect("retry");
    assert_eq!(state(&c, id2), "running");
    report(&mut c, id2, "done", r#"{"pid":200}"#, 34).expect("recorded done");
    assert_eq!(state(&c, id2), "ended");
}

#[test]
fn a_report_with_no_pid_moves_a_claim_with_a_recorded_pid_to_ending_and_end_gone_ends_it() {
    let (_t, mut c) = open();
    let (id, _) = running(&mut c, "c1", 100);
    report(&mut c, id, "done", "{}", 30).expect("done");
    // Still the writer: nothing else may claim the conversation, and a later nameless exit changes nothing.
    assert_eq!(state(&c, id), "ending");
    report(&mut c, id, "exit", "{}", 31).expect("exit");
    assert_eq!(state(&c, id), "ending");
    assert!(matches!(
        claim(&mut c, &new_claim("c1", None), 32),
        Err(Error::Refused(_))
    ));
    let alive = procs_with(&[(100, "e100")]);
    assert!(end_gone(&mut c, &alive, 33).expect("alive").is_empty());
    assert_eq!(state(&c, id), "ending");
    assert_eq!(
        end_gone(&mut c, &Procs::default(), 34).expect("gone"),
        vec![id]
    );
    assert_eq!(state(&c, id), "ended");
    // An exit nameless on a claim still `claimed` with a pid from `spawned` goes the same way.
    let (id2, _) = take(&mut c, "c2");
    report(
        &mut c,
        id2,
        "spawned",
        r#"{"pid":200,"started":"e200"}"#,
        40,
    )
    .expect("spawned");
    report(&mut c, id2, "exit", "{}", 41).expect("exit");
    assert_eq!(state(&c, id2), "ending");
    assert_eq!(
        end_gone(&mut c, &Procs::default(), 42).expect("gone"),
        vec![id2]
    );
    assert_eq!(state(&c, id2), "ended");
}

/// The body the holder link sends for a process's exit (`holder_link::exit_body`): the pid, how it ended and
/// `out_of_usage: false`. It follows the pid rules of any report, and a repeat of it changes nothing.
#[test]
fn the_holder_links_exit_body_ends_the_claim_of_its_pid_only() {
    let (_t, mut c) = open();
    let (id, turn) = running(&mut c, "c1", 100);
    let body = |pid: u32| {
        format!(r#"{{"out_of_usage":false,"pid":{pid},"code":0,"signal":null,"lapsed":false}}"#)
    };
    report(&mut c, id, "exit", &body(101), 30).expect("another process");
    assert_eq!(state(&c, id), "running", "a respawn's exit ends nothing");
    report(&mut c, id, "exit", &body(100), 31).expect("its own");
    assert_eq!(state(&c, id), "ended");
    assert_eq!(turn_state(&c, turn), "queued", "the turn goes back");
    report(&mut c, id, "exit", &body(100), 32).expect("repeat");
    assert_eq!(state(&c, id), "ended");
    assert_eq!(
        count(&c, "SELECT COUNT(*) FROM reports WHERE kind = 'exit'"),
        2,
        "one report per process, a repeat is the same one"
    );
}

#[test]
fn a_report_with_no_pid_on_a_claim_with_no_pid_ends_a_claimed_one_only() {
    let (_t, mut c) = open();
    let (id, _) = take(&mut c, "c1");
    report(&mut c, id, "exit", "{}", 30).expect("exit");
    assert_eq!(state(&c, id), "ended");
    let (id2, _) = running(&mut c, "c2", 100);
    stop(&mut c, id2, "stop", false, 32).expect("stop");
    report(&mut c, id2, "exit", "{}", 33).expect("exit");
    assert_eq!(state(&c, id2), "ending");
}

#[test]
fn a_busy_naming_another_process_leaves_the_claim_alone() {
    let (_t, mut c) = open();
    let (id, _) = take(&mut c, "c1");
    report(&mut c, id, "spawned", r#"{"pid":100,"started":"e100"}"#, 20).expect("spawned");
    report(&mut c, id, "busy", r#"{"pid":999}"#, 21).expect("busy");
    assert_eq!(state(&c, id), "claimed");
    report(&mut c, id, "busy", "{}", 22).expect("nameless busy");
    assert_eq!(state(&c, id), "claimed");
    report(&mut c, id, "busy", r#"{"pid":100}"#, 23).expect("busy");
    assert_eq!(state(&c, id), "ended");
}

#[test]
fn a_claim_past_its_turns_deadline_is_refused() {
    use super::claims::enqueue_turn;
    let (_t, mut c) = open();
    let t = enqueue_turn(&mut c, &new_turn("c1", "schedule", Some(100)), 5).expect("enqueue");
    assert!(matches!(
        claim(&mut c, &new_claim("c1", Some(t)), 100),
        Err(Error::Refused(_))
    ));
    assert_eq!(count(&c, "SELECT count(*) FROM claims"), 0);
    claim(&mut c, &new_claim("c1", Some(t)), 99).expect("in time");
}

#[test]
fn a_claim_for_one_person_cannot_run_another_persons_turn() {
    let (_t, mut c) = open();
    conversation(&c, "c1");
    let t = turn(&c, "c1", "alex");
    assert!(matches!(
        claim(&mut c, &new_claim("c1", Some(t)), 10),
        Err(Error::Refused(_))
    ));
    assert_eq!(turn_state(&c, t), "queued");
}

#[test]
fn a_revoke_between_the_may_check_and_the_claim_refuses_it_and_one_before_the_redeem_leaves_no_claimed_row()
 {
    use super::claims::revoke_epoch;
    let (_t, mut c) = open();
    conversation(&c, "c1");
    let seen = revoke_epoch(&c, "sam").expect("epoch");
    revoke_person(&mut c, "sam", 5).expect("revoke");
    let mut req = new_claim("c1", None);
    req.revoke_seen = seen;
    assert!(matches!(claim(&mut c, &req, 6), Err(Error::Refused(m)) if m.contains("revoked")));
    assert_eq!(count(&c, "SELECT count(*) FROM claims"), 0);
    // A check made after the revoke goes through.
    req.revoke_seen = revoke_epoch(&c, "sam").expect("epoch");
    let got = claim(&mut c, &req, 7).expect("claim");
    // A revoke after the claim ends the unspawned claim, so the redeem finds nothing to take and is refused; the
    // claim is never left `claimed` (a stuck writer slot).
    revoke_person(&mut c, "sam", 8).expect("revoke");
    assert!(matches!(
        redeem(&mut c, got.claim_id, 100, "e100", "h1", 9).expect("redeem"),
        Redeemed::Refused(_)
    ));
    assert_ne!(state(&c, got.claim_id), "claimed");
    assert_eq!(state(&c, got.claim_id), "ended");
}

fn direct(c: &mut Core, conv: &str, pid: u32) -> i64 {
    conversation(c, conv);
    let started = format!("e{pid}");
    let mut req = new_claim(conv, None);
    req.kind = Kind::External;
    req.holder_instance = None;
    req.process = Some((pid, &started));
    claim(c, &req, 10).expect("direct claim").claim_id
}

#[test]
fn a_direct_claim_survives_reconcile_and_a_holder_change_and_ends_when_its_pid_is_gone() {
    let (_t, mut c) = open();
    let id = direct(&mut c, "c1", 700);
    let out = different_holder(&mut c, "h2", &[], &[], i64::MAX, 20).expect("holder change");
    assert_eq!(out, super::claims::Released::default());
    unreconcile(&c);
    let out = reconcile(&mut c, &[], &[], i64::MAX, 21).expect("reconcile");
    assert_eq!(out, super::claims::Released::default());
    assert_eq!(state(&c, id), "claimed");
    // A stray stub cannot take its pid over.
    assert_eq!(
        redeem(&mut c, id, 701, "e701", "h1", 22).expect("stray"),
        Redeemed::Refused("claimed".into())
    );
    let alive = procs_with(&[(700, "e700")]);
    assert!(end_gone(&mut c, &alive, 23).expect("alive").is_empty());
    assert_eq!(state(&c, id), "claimed");
    let none = Procs::default();
    assert_eq!(end_gone(&mut c, &none, 24).expect("gone"), vec![id]);
    assert_eq!(state(&c, id), "ended");
}

#[test]
fn a_second_reconcile_in_one_generation_is_refused() {
    let (_t, mut c) = open();
    assert!(matches!(
        reconcile(&mut c, &[], &[], i64::MAX, 5),
        Err(Error::Refused(m)) if m.contains("already reconciled")
    ));
}

#[test]
fn the_high_water_mark_keeps_a_claim_the_holders_list_could_not_know() {
    use super::claims::claim_high_water;
    let (_t, mut c) = open();
    let (old, _) = take(&mut c, "a");
    let mark = claim_high_water(&c).expect("mark");
    // Taken after the mark was read, before the holder's list arrives.
    let (newer, _) = take(&mut c, "b");
    let out = different_holder(&mut c, "h2", &[], &[], mark, 30).expect("holder change");
    assert_eq!(out.ended, vec![old]);
    assert_eq!(state(&c, newer), "claimed");
}

#[test]
fn an_old_instance_process_of_an_ended_claim_is_still_killed() {
    let (_t, mut c) = open();
    let (id, _) = running(&mut c, "c1", 100);
    report(&mut c, id, "exit", r#"{"pid":100}"#, 25).expect("exit");
    let strays = [RecordedProc {
        pid: 100,
        started: "e100".into(),
        conv: "c1".into(),
    }];
    let out = different_holder(&mut c, "h2", &[], &strays, i64::MAX, 30).expect("holder change");
    assert_eq!(out.kill_procs.len(), 1);
}

#[test]
fn enqueue_refuses_another_agent_or_engine_and_an_empty_person() {
    use super::claims::{SOURCES, enqueue_turn};
    let (_t, mut c) = open();
    enqueue_turn(&mut c, &new_turn("c1", "app", None), 1).expect("first");
    let mut other = new_turn("c1", "app", None);
    other.agent = "b";
    assert!(matches!(
        enqueue_turn(&mut c, &other, 2),
        Err(Error::Refused(_))
    ));
    let mut other = new_turn("c1", "app", None);
    other.engine = "codex";
    assert!(matches!(
        enqueue_turn(&mut c, &other, 2),
        Err(Error::Refused(_))
    ));
    let mut nobody = new_turn("c1", "app", None);
    nobody.person = "";
    assert!(matches!(
        enqueue_turn(&mut c, &nobody, 2),
        Err(Error::BadParams(_))
    ));
    for s in ["channel", "room", "bot"] {
        assert!(SOURCES.contains(&s));
        enqueue_turn(&mut c, &new_turn("c1", s, None), 3).expect(s);
    }
}

#[test]
fn a_claimed_claim_with_no_process_stopped_is_ended_and_its_late_redeem_fails() {
    let (_t, mut c) = open();
    let (id, t) = take(&mut c, "c1");
    assert!(
        stop(&mut c, id, "revoked", false, 30)
            .expect("stop")
            .is_none()
    );
    assert_eq!(state(&c, id), "ended", "no pid for end_gone to wait on");
    assert_eq!(turn_state(&c, t), "cancelled");
    assert_eq!(
        redeem(&mut c, id, 100, "e100", "h1", 31).expect("late"),
        Redeemed::Refused("ended".into())
    );
    claim(&mut c, &new_claim("c1", None), 32).expect("claim again");
}

#[test]
fn a_claimed_claim_with_a_spawned_process_stopped_is_ending_until_it_is_gone() {
    let (_t, mut c) = open();
    let (id, _) = take(&mut c, "c1");
    report(&mut c, id, "spawned", r#"{"pid":100,"started":"e100"}"#, 20).expect("spawned");
    let kill = stop(&mut c, id, "stop", false, 30)
        .expect("stop")
        .expect("kill");
    assert_eq!(kill.pid, Some(100));
    assert_eq!(state(&c, id), "ending");
}

/// Every claim path takes its id from the same counter, which a fresh db moves above the epoch-ms ids older cores sent a
/// surviving holder (it answers every lower claim `Stale`): a turn claim, not only a terminal's, gets one.
#[test]
fn a_claim_on_a_fresh_db_gets_an_id_above_the_clock_and_the_next_follows() {
    let (_t, mut c) = open();
    let (id, _) = take(&mut c, "c1");
    assert!(id > 1_000_000_000_000, "{id}");
    let (id2, _) = take(&mut c, "c2");
    assert_eq!(id2, id + 1);
}
