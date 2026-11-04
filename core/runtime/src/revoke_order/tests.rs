//! The revoke order (M3-10d): a person with two bots and a two-deep ask tree, a double revoke, and a revoke racing a new
//! claim.
#![allow(clippy::unwrap_used, clippy::too_many_lines)]

use rusqlite::params;
use serde_json::json;

use super::revoke_everywhere;
use crate::asks::call::call_at;
use crate::conversations::claims::{
    Kind, NewClaim, NewTurn, claim, enqueue_turn, park, reconcile, redeem, revoke_epoch,
    revoke_person,
};
use crate::db::Core;
use crate::error::Error;
use crate::layout::Layout;
use crate::tests::open_all;

const NOW: i64 = 1_800_000_000_000;

struct Fixture {
    _t: tempfile::TempDir,
    core: Core,
}

fn fixture() -> Fixture {
    let t = tempfile::tempdir().unwrap();
    let mut core = open_all(&Layout::new(&t.path().join("core")));
    reconcile(&mut core, &[], &[], i64::MAX, 1).unwrap();
    for (id, bot_of) in [
        ("mia", None),
        ("mia-b1", Some("mia")),
        ("mia-b2", Some("mia")),
        ("kai", None),
        ("kai-b", Some("kai")),
    ] {
        core.conn()
            .execute(
                "INSERT INTO people (id, bot_of) VALUES (?1, ?2)",
                params![id, bot_of],
            )
            .unwrap();
    }
    Fixture { _t: t, core }
}

fn ask(core: &Core, id: &str, parent: Option<&str>, person: &str, conv: &str) {
    core.conn()
        .execute(
            "INSERT INTO asks (id, chain, parent, hop, caller_person, conversation, state, at_ms, cost)
             VALUES (?1, 'r', ?2, 0, ?3, ?4, 'open', 1, 0.0)",
            params![id, parent, person, conv],
        )
        .unwrap();
}

fn turn(core: &mut Core, conv: &str, source: &str, person: &str) -> i64 {
    enqueue_turn(
        core,
        &NewTurn {
            conversation: conv,
            agent: "a",
            engine: "claude",
            source,
            person,
            prompt: "p",
            deadline: None,
            revoke_seen: 0,
        },
        2,
    )
    .unwrap()
}

fn new_claim<'a>(conv: &'a str, person: &'a str, turn: i64, seen: i64) -> NewClaim<'a> {
    NewClaim {
        conversation: conv,
        kind: Kind::Turn,
        person,
        turn: Some(turn),
        holder_instance: Some("h1"),
        account: None,
        process: None,
        revoke_seen: seen,
    }
}

/// A running writer for `person` on `conv`.
fn run(core: &mut Core, conv: &str, person: &str, source: &str, pid: u32) -> i64 {
    let t = turn(core, conv, source, person);
    let id = claim(core, &new_claim(conv, person, t, 0), 3)
        .unwrap()
        .claim_id;
    redeem(core, id, pid, &format!("e{pid}"), "h1", 4).unwrap();
    id
}

fn state_of(core: &Core, table: &str, key: &str, id: impl rusqlite::ToSql) -> String {
    core.conn()
        .query_row(
            &format!("SELECT state FROM {table} WHERE {key} = ?1"),
            [id],
            |r| r.get(0),
        )
        .unwrap()
}

/// mia: an app turn, the root ask; bot 1 the child; bot 2 the grandchild and a schedule turn. kai's own tree and bot stay.
fn world(f: &mut Fixture) -> Vec<(String, i64)> {
    ask(&f.core, "a1", None, "mia", "c-a1");
    ask(&f.core, "a2", Some("a1"), "mia-b1", "c-a2");
    ask(&f.core, "a3", Some("a2"), "mia-b2", "c-a3");
    ask(&f.core, "k1", None, "kai", "c-k1");
    let mut live = Vec::new();
    for (conv, who, source, pid) in [
        ("c-app", "mia", "app", 10),
        ("c-a1", "mia", "ask", 11),
        ("c-a2", "mia-b1", "ask", 12),
        ("c-a3", "mia-b2", "ask", 13),
        ("c-sched", "mia-b2", "schedule", 14),
        ("c-k1", "kai", "ask", 20),
        ("c-kbot", "kai-b", "app", 21),
    ] {
        let id = run(&mut f.core, conv, who, source, pid);
        live.push((conv.to_owned(), id));
    }
    live
}

#[test]
fn a_person_with_two_bots_and_a_two_deep_ask_tree_is_stopped_everywhere_and_others_are_not() {
    let mut f = fixture();
    let live = world(&mut f);
    // a queued turn of a bot, waiting behind nothing
    let queued = turn(&mut f.core, "c-queued", "schedule", "mia-b1");
    let out = revoke_everywhere(&mut f.core, "mia", &[], NOW).unwrap();
    assert_eq!(out.principals, ["mia", "mia-b1", "mia-b2"]);
    assert_eq!(out.ended, ["a1", "a2", "a3"]);
    assert_eq!(out.cancelled_turns, 1);
    assert_eq!(state_of(&f.core, "turns", "id", queued), "cancelled");
    for (conv, id) in &live {
        let want = if conv.starts_with("c-k") {
            "running"
        } else {
            "ending"
        };
        assert_eq!(state_of(&f.core, "claims", "claim_id", id), want, "{conv}");
    }
    assert_eq!(out.kill.len(), 5, "mia's five live writers, once each");
    assert_eq!(state_of(&f.core, "asks", "id", "k1"), "open");
    for p in ["mia", "mia-b1", "mia-b2"] {
        assert_eq!(revoke_epoch(&f.core, p).unwrap(), 1, "{p}");
    }
    for p in ["kai", "kai-b"] {
        assert_eq!(revoke_epoch(&f.core, p).unwrap(), 0, "{p}");
    }
}

#[test]
fn a_person_without_bots_is_revoked_alone() {
    let mut f = fixture();
    let out = revoke_everywhere(&mut f.core, "ghost", &[], NOW).unwrap();
    assert_eq!(out.principals, ["ghost"]);
    assert!(out.kill.is_empty() && out.ended.is_empty());
    ask(&f.core, "a1", None, "solo", "c-a1");
    run(&mut f.core, "c-a1", "solo", "ask", 11);
    let out = revoke_everywhere(&mut f.core, "solo", &[], NOW).unwrap();
    assert_eq!(out.principals, ["solo"]);
    assert_eq!(out.ended, ["a1"]);
    assert_eq!(out.kill.len(), 1);
}

#[test]
fn revoking_twice_ends_nothing_more_and_answers_the_kills_again_until_they_are_gone() {
    let mut f = fixture();
    world(&mut f);
    let first = revoke_everywhere(&mut f.core, "mia", &[], NOW).unwrap();
    let again = revoke_everywhere(&mut f.core, "mia", &[], NOW + 1).unwrap();
    assert_eq!(again.ended, Vec::<String>::new());
    assert_eq!(again.cancelled_turns, 0);
    assert_eq!(again.kill.len(), first.kill.len(), "re-sent until gone");
    assert_eq!(revoke_epoch(&f.core, "mia").unwrap(), 2);
    assert_eq!(state_of(&f.core, "asks", "id", "a3"), "stopped");
}

#[test]
fn a_retry_after_a_crash_between_the_steps_finishes_the_asks() {
    let mut f = fixture();
    world(&mut f);
    // the first step alone, as a crash after it leaves things
    revoke_person(&mut f.core, "mia", NOW).unwrap();
    assert_eq!(state_of(&f.core, "asks", "id", "a1"), "open");
    let out = revoke_everywhere(&mut f.core, "mia", &[], NOW + 1).unwrap();
    assert_eq!(out.ended, ["a1", "a2", "a3"]);
}

#[test]
fn a_claim_that_passed_its_may_check_before_the_revoke_is_refused_for_the_person_and_each_bot() {
    let mut f = fixture();
    let seen: Vec<(&str, i64)> = ["mia", "mia-b1", "mia-b2"]
        .into_iter()
        .map(|p| (p, revoke_epoch(&f.core, p).unwrap()))
        .collect();
    let turns: Vec<i64> = seen
        .iter()
        .map(|(p, _)| turn(&mut f.core, &format!("c-{p}"), "app", p))
        .collect();
    revoke_everywhere(&mut f.core, "mia", &[], NOW).unwrap();
    for ((p, e), t) in seen.iter().zip(&turns) {
        let err = claim(&mut f.core, &new_claim(&format!("c-{p}"), p, *t, *e), 5).unwrap_err();
        assert!(
            matches!(&err, Error::Refused(m) if m.contains("was revoked after the may check")),
            "{p}: {err}"
        );
        // a turn enqueued by someone who checked before the revoke is refused at the enqueue, in the insert's transaction
        let late = enqueue_turn(
            &mut f.core,
            &NewTurn {
                revoke_seen: *e,
                ..new_turn(&format!("c-{p}"), "app", p)
            },
            6,
        )
        .unwrap_err();
        assert!(
            matches!(&late, Error::Refused(m) if m.contains("was revoked after the may check")),
            "{p}: {late}"
        );
        // a turn queued with the new epoch is valid; a stale claimer's refusal leaves it queued for a fresh claim
        let now = revoke_epoch(&f.core, p).unwrap();
        let t = enqueue_turn(
            &mut f.core,
            &NewTurn {
                revoke_seen: now,
                ..new_turn(&format!("c-{p}"), "app", p)
            },
            6,
        )
        .unwrap();
        let err = claim(&mut f.core, &new_claim(&format!("c-{p}"), p, t, *e), 6).unwrap_err();
        assert!(matches!(err, Error::Refused(_)), "{p}: {err}");
        assert_eq!(state_of(&f.core, "turns", "id", t), "queued", "{p}");
        claim(&mut f.core, &new_claim(&format!("c-{p}"), p, t, now), 7).unwrap();
    }
}

fn new_turn<'a>(conv: &'a str, source: &'a str, person: &'a str) -> NewTurn<'a> {
    NewTurn {
        conversation: conv,
        agent: "a",
        engine: "claude",
        source,
        person,
        prompt: "p",
        deadline: None,
        revoke_seen: 0,
    }
}

#[test]
fn a_turn_whose_enqueuer_checked_before_a_revoke_never_outlives_it() {
    let mut f = fixture();
    // c4-epochleak: the may check at epoch 0, the revoke, then the insert. The insert is refused; the other order is swept.
    let seen = revoke_epoch(&f.core, "mia").unwrap();
    let before = enqueue_turn(
        &mut f.core,
        &NewTurn {
            revoke_seen: seen,
            ..new_turn("c-m", "app", "mia")
        },
        2,
    )
    .unwrap();
    revoke_person(&mut f.core, "mia", NOW).unwrap();
    assert_eq!(state_of(&f.core, "turns", "id", before), "cancelled");
    let err = enqueue_turn(
        &mut f.core,
        &NewTurn {
            revoke_seen: seen,
            ..new_turn("c-m", "app", "mia")
        },
        3,
    )
    .unwrap_err();
    assert!(matches!(err, Error::Refused(_)), "{err}");
    let n: i64 = f
        .core
        .conn()
        .query_row(
            "SELECT count(*) FROM turns WHERE state IN ('queued', 'parked')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 0);
}

#[test]
fn a_revoked_refusal_leaves_the_turn_and_other_peoples_turns_in_the_conversation_alone() {
    let mut f = fixture();
    let kai = turn(&mut f.core, "c-shared", "app", "kai");
    revoke_person(&mut f.core, "mia", NOW).unwrap();
    let now = revoke_epoch(&f.core, "mia").unwrap();
    let mia = enqueue_turn(
        &mut f.core,
        &NewTurn {
            revoke_seen: now,
            ..new_turn("c-shared", "app", "mia")
        },
        5,
    )
    .unwrap();
    let err = claim(&mut f.core, &new_claim("c-shared", "mia", mia, 0), 6).unwrap_err();
    assert!(matches!(err, Error::Refused(_)), "{err}");
    assert_eq!(state_of(&f.core, "turns", "id", mia), "queued");
    assert_eq!(state_of(&f.core, "turns", "id", kai), "queued");
}

#[test]
fn a_fenced_parked_turn_is_cancelled_and_loses_its_park_reason() {
    let mut f = fixture();
    ask(&f.core, "a1", None, "mia", "c-a1");
    revoke_everywhere(&mut f.core, "mia", &[], NOW).unwrap();
    let t = turn(&mut f.core, "c-a1", "wake", "kai");
    assert!(park(&mut f.core, t, "terminal_live", 3).unwrap());
    let err = claim(&mut f.core, &new_claim("c-a1", "kai", t, 0), 5).unwrap_err();
    assert!(matches!(err, Error::Stopped(_)), "{err}");
    assert_eq!(state_of(&f.core, "turns", "id", t), "cancelled");
    let reason: Option<String> = f
        .core
        .conn()
        .query_row("SELECT parked_reason FROM turns WHERE id = ?1", [t], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(reason, None);
}

#[test]
fn a_stopped_asks_conversation_refuses_its_ask_turn_with_the_stopped_kind_not_the_busy_one() {
    let mut f = fixture();
    ask(&f.core, "a1", None, "mia", "c-a1");
    revoke_everywhere(&mut f.core, "mia", &[], NOW).unwrap();
    // arrives after the stop (the stop itself cancels only what was queued by then)
    let t = turn(&mut f.core, "c-a1", "wake", "kai");
    let err = claim(&mut f.core, &new_claim("c-a1", "kai", t, 0), 5).unwrap_err();
    assert!(matches!(err, Error::Stopped(_)), "{err}");
    // the fence never lifts: the turn is cancelled, and a second attempt (a racing claimer) changes nothing
    assert_eq!(state_of(&f.core, "turns", "id", t), "cancelled");
    let err = claim(&mut f.core, &new_claim("c-a1", "kai", t, 0), 5).unwrap_err();
    assert!(matches!(err, Error::Stopped(_)), "{err}");
    assert_eq!(state_of(&f.core, "turns", "id", t), "cancelled");
    // a busy conversation stays a plain refusal
    let live = run(&mut f.core, "c-busy", "kai", "app", 30);
    let t2 = turn(&mut f.core, "c-busy", "app", "kai");
    let err = claim(&mut f.core, &new_claim("c-busy", "kai", t2, 0), 6).unwrap_err();
    assert!(
        matches!(&err, Error::Refused(m) if m.contains("has a writer")),
        "{err}"
    );
    assert_eq!(state_of(&f.core, "claims", "claim_id", live), "running");
    // and its turn stays queued for the writer to finish
    assert_eq!(state_of(&f.core, "turns", "id", t2), "queued");
}

#[test]
fn a_follow_up_to_an_ended_ask_is_refused_at_send_and_one_to_an_open_ask_is_not() {
    let mut f = fixture();
    ask(&f.core, "live1", None, "kai", "c-live");
    ask(&f.core, "gone1", None, "mia", "c-gone");
    revoke_everywhere(&mut f.core, "mia", &[], NOW).unwrap();
    let ok = call_at(&mut f.core, "ask.to", &json!({"id": "live1"}), NOW).unwrap();
    assert_eq!(ok["ok"], true);
    let err = call_at(&mut f.core, "ask.to", &json!({"id": "gone1"}), NOW).unwrap_err();
    assert!(
        matches!(&err, Error::Stopped(m) if m.contains("was stopped; ask anew")),
        "{err}"
    );
    let err = call_at(&mut f.core, "ask.to", &json!({"id": "nope"}), NOW).unwrap_err();
    assert!(matches!(err, Error::Refused(_)), "{err}");
    // the row answers the whole order too
    let out = call_at(
        &mut f.core,
        "ask.revokeEverywhere",
        &json!({"person": "kai"}),
        NOW,
    )
    .unwrap();
    assert_eq!(out["principals"], json!(["kai", "kai-b"]));
    assert_eq!(out["ended"], json!(["live1"]));
}

#[test]
fn a_removed_person_whose_bot_rows_are_gone_is_revoked_through_the_bots_the_caller_passes() {
    let mut f = fixture();
    // a bot of a bot, with a live run and an ask of its own
    f.core
        .conn()
        .execute(
            "INSERT INTO people (id, bot_of) VALUES ('mia-b1-b', 'mia-b1')",
            [],
        )
        .unwrap();
    ask(&f.core, "a1", None, "mia", "c-a1");
    ask(&f.core, "a9", None, "mia-b1-b", "c-a9");
    let live = [
        run(&mut f.core, "c-app", "mia", "app", 10),
        run(&mut f.core, "c-b1", "mia-b1", "app", 12),
        run(&mut f.core, "c-b1b", "mia-b1-b", "app", 13),
    ];
    let bots = ["mia-b1", "mia-b1-b", "mia-b2", "mia"].map(str::to_owned);
    // the authority removal: the person's and every bot's row is deleted
    f.core
        .conn()
        .execute("DELETE FROM people WHERE id LIKE 'mia%'", [])
        .unwrap();
    let out = revoke_everywhere(&mut f.core, "mia", &bots, NOW).unwrap();
    assert_eq!(out.principals, ["mia", "mia-b1", "mia-b1-b", "mia-b2"]);
    assert_eq!(out.ended, ["a1", "a9"]);
    for id in live {
        assert_eq!(state_of(&f.core, "claims", "claim_id", id), "ending");
    }
    for p in &out.principals {
        assert_eq!(revoke_epoch(&f.core, p).unwrap(), 1, "{p}");
    }
    // kai's side is untouched, and a repeat with the same bots finds nothing new
    assert_eq!(revoke_epoch(&f.core, "kai").unwrap(), 0);
    let again = revoke_everywhere(&mut f.core, "mia", &bots, NOW + 1).unwrap();
    assert!(again.ended.is_empty() && again.cancelled_turns == 0);
}
