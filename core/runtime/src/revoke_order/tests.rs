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
    let mut core = open_all(&Layout::new(&t.path().join("core")))
        .with_folders(&crate::tests::paths(&t.path().join("machine")));
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
    // the authority removal: the bots are recorded, then the person's and every bot's row is deleted
    let rec: Vec<String> = bots.iter().filter(|b| *b != "mia").cloned().collect();
    super::record_removal(f.core.conn(), "mia", &rec, 1.0).unwrap();
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
    // kai's side is untouched; the record is spent, so stale `--bots` name nobody (no row, no record) and are
    // ignored rather than refused, so a retry of a call that finished is not an error
    assert_eq!(revoke_epoch(&f.core, "kai").unwrap(), 0);
    revoke_everywhere(&mut f.core, "mia", &bots, NOW + 1).unwrap();
    let again = revoke_everywhere(&mut f.core, "mia", &[], NOW + 1).unwrap();
    assert!(again.ended.is_empty() && again.cancelled_turns == 0);
}

fn open_rows(f: &Fixture) -> i64 {
    f.core
        .conn()
        .query_row("SELECT count(*) FROM open_removals", [], |r| r.get(0))
        .unwrap()
}

fn two_deep(f: &mut Fixture) -> i64 {
    f.core
        .conn()
        .execute(
            "INSERT INTO people (id, bot_of) VALUES ('mia-b1-b', 'mia-b1')",
            [],
        )
        .unwrap();
    ask(&f.core, "a1", None, "mia", "c-a1");
    ask(&f.core, "a9", None, "mia-b1-b", "c-a9");
    run(&mut f.core, "c-b1b", "mia-b1-b", "app", 13)
}

#[test]
fn the_removal_runs_the_order_itself_and_clears_its_record() {
    let mut f = fixture();
    let live = two_deep(&mut f);
    let out =
        crate::authority::call::call(&mut f.core, "people.removePerson", &json!({"id": "mia"}))
            .unwrap();
    assert!(
        !out["kill"].is_null(),
        "the writers to kill come back: {out}"
    );
    assert_eq!(state_of(&f.core, "asks", "id", "a1"), "stopped");
    assert_eq!(state_of(&f.core, "asks", "id", "a9"), "stopped");
    assert_eq!(state_of(&f.core, "claims", "claim_id", live), "ending");
    assert_eq!(open_rows(&f), 0);
}

#[test]
fn a_crash_between_the_removal_and_the_order_loses_no_bot_and_the_replay_finishes_it() {
    let mut f = fixture();
    let live = two_deep(&mut f);
    // the removal committed (rows gone, record written in the same transaction) and the core died before the order
    let bots: Vec<String> = ["mia-b1", "mia-b1-b", "mia-b2"].map(str::to_owned).to_vec();
    super::record_removal(f.core.conn(), "mia", &bots, 1.0).unwrap();
    f.core
        .conn()
        .execute("DELETE FROM people WHERE id LIKE 'mia%'", [])
        .unwrap();
    let (kill, failed) = super::replay_open(&mut f.core, NOW).unwrap();
    assert!(failed.is_empty(), "{failed:?}");
    assert_ne!(kill.len(), 0);
    assert_eq!(state_of(&f.core, "asks", "id", "a1"), "stopped");
    assert_eq!(state_of(&f.core, "asks", "id", "a9"), "stopped");
    assert_eq!(state_of(&f.core, "claims", "claim_id", live), "ending");
    for p in ["mia", "mia-b1", "mia-b1-b", "mia-b2"] {
        assert_eq!(revoke_epoch(&f.core, p).unwrap(), 1, "{p}");
    }
    // finished: the record is cleared, and a second pass is a no-op
    assert_eq!(open_rows(&f), 0);
    let (kill, failed) = super::replay_open(&mut f.core, NOW + 1).unwrap();
    assert!(kill.is_empty() && failed.is_empty());
}

#[test]
fn a_removal_with_no_bots_is_recorded_and_its_own_claim_is_fenced_by_the_replay() {
    let mut f = fixture();
    f.core
        .conn()
        .execute("DELETE FROM people WHERE bot_of = 'kai'", [])
        .unwrap();
    let live = run(&mut f.core, "c-k1", "kai", "app", 20);
    super::record_removal(f.core.conn(), "kai", &[], 1.0).unwrap();
    assert_eq!(open_rows(&f), 1, "a record exists even with no bots");
    f.core
        .conn()
        .execute("DELETE FROM people WHERE id = 'kai'", [])
        .unwrap();
    let (_, failed) = super::replay_open(&mut f.core, NOW).unwrap();
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(state_of(&f.core, "claims", "claim_id", live), "ending");
    assert_eq!(open_rows(&f), 0);
}

#[test]
fn a_name_in_an_open_record_can_not_join_again_until_the_order_has_run() {
    let mut f = fixture();
    super::record_removal(f.core.conn(), "zed", &["zed-b".to_owned()], 1.0).unwrap();
    for (m, params) in [
        ("people.addPerson", json!({"id": "zed"})),
        ("people.addBot", json!({"id": "zed-b", "of": "kai"})),
    ] {
        let err = crate::authority::call::call(&mut f.core, m, &params).unwrap_err();
        assert!(
            matches!(&err, Error::Refused(msg) if msg.contains("finishing the revoke of zed first")),
            "{m}: {err}"
        );
    }
    let gone: i64 = f
        .core
        .conn()
        .query_row(
            "SELECT count(*) FROM people WHERE id LIKE 'zed%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(gone, 0, "the refused add wrote nothing");
    // after the order the id is free again
    super::replay_open(&mut f.core, NOW).unwrap();
    crate::authority::call::call(
        &mut f.core,
        "people.addBot",
        &json!({"id": "zed-b", "of": "kai"}),
    )
    .unwrap();
}

#[test]
fn a_recorded_name_that_joined_again_elsewhere_is_refused_not_skipped() {
    let mut f = fixture();
    // a broken invariant: mia's record names kai-b, which is in people under kai
    super::record_removal(f.core.conn(), "mia", &["kai-b".to_owned()], 1.0).unwrap();
    f.core
        .conn()
        .execute("DELETE FROM people WHERE id LIKE 'mia%'", [])
        .unwrap();
    ask(&f.core, "k2", None, "kai-b", "c-k2");
    let err = revoke_everywhere(&mut f.core, "mia", &[], NOW).unwrap_err();
    assert!(
        matches!(&err, Error::Refused(m) if m.contains("kai-b") && m.contains("mia")),
        "{err}"
    );
    assert_eq!(state_of(&f.core, "asks", "id", "k2"), "open");
    assert_eq!(open_rows(&f), 1, "the record stays");
}

#[test]
fn an_unreadable_record_stops_the_order_and_the_add_check_naming_the_row() {
    let mut f = fixture();
    f.core
        .conn()
        .execute(
            "INSERT INTO open_removals (person, bots, at) VALUES ('mia', '', 1)",
            [],
        )
        .unwrap();
    let err = revoke_everywhere(&mut f.core, "mia", &[], NOW).unwrap_err();
    assert!(
        matches!(&err, Error::Refused(m) if m.contains("open_removals row of mia")),
        "{err}"
    );
    let err = super::open_removal_naming(f.core.conn(), "anyone").unwrap_err();
    assert!(
        matches!(&err, Error::Refused(m) if m.contains("open_removals row of mia")),
        "{err}"
    );
    assert_eq!(open_rows(&f), 1);
}

#[test]
fn forged_bots_are_refused_and_nothing_is_ended() {
    let mut f = fixture();
    world(&mut f);
    f.core
        .conn()
        .execute("INSERT INTO people (id, owner) VALUES ('boss', 1)", [])
        .unwrap();
    ask(&f.core, "o1", None, "boss", "c-o1");
    for forged in ["boss", "kai-b", "kai"] {
        let err = revoke_everywhere(&mut f.core, "mia", &[forged.to_owned()], NOW).unwrap_err();
        assert!(
            matches!(&err, Error::Refused(m) if m.contains(forged)),
            "{forged}: {err}"
        );
    }
    // a forged name next to a true one still refuses the lot
    let mixed = ["mia-b1".to_owned(), "kai-b".to_owned()];
    revoke_everywhere(&mut f.core, "mia", &mixed, NOW).unwrap_err();
    // the owner can not be the revoked person either
    revoke_everywhere(&mut f.core, "boss", &[], NOW).unwrap_err();
    for id in ["a1", "a2", "a3", "k1", "o1"] {
        assert_eq!(state_of(&f.core, "asks", "id", id), "open", "{id}");
    }
    for p in ["mia", "mia-b1", "kai-b", "boss"] {
        assert_eq!(revoke_epoch(&f.core, p).unwrap(), 0, "{p}");
    }
    // a name with no row and no record is nobody's: ignored, not an error
    revoke_everywhere(&mut f.core, "mia", &["nobody-known".to_owned()], NOW).unwrap();
    // the true bots are accepted
    let ok = ["mia-b1".to_owned(), "mia-b2".to_owned()];
    revoke_everywhere(&mut f.core, "mia", &ok, NOW).unwrap();
}

fn is_revoked(f: &Fixture, id: &str) -> i64 {
    f.core
        .conn()
        .query_row("SELECT revoked FROM people WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .unwrap()
}

#[test]
fn a_suspend_runs_the_order_sweeps_the_person_and_their_bots_and_clears_its_record() {
    let mut f = fixture();
    let live = world(&mut f);
    let out =
        crate::authority::call::call(&mut f.core, "people.suspend", &json!({"id": "mia"})).unwrap();
    assert!(out["revokePending"].is_null(), "{out}");
    assert!(
        out["kill"].as_array().is_some_and(|k| !k.is_empty()),
        "the writers to kill come back: {out}"
    );
    assert!(out.get("suspendedPerson").is_none(), "{out}");
    assert_eq!(is_revoked(&f, "mia"), 1, "the suspend stands");
    for (conv, id) in &live {
        let want = if conv.starts_with("c-k") {
            "running"
        } else {
            "ending"
        };
        assert_eq!(state_of(&f.core, "claims", "claim_id", *id), want, "{conv}");
    }
    assert_eq!(state_of(&f.core, "asks", "id", "a1"), "stopped");
    assert_eq!(state_of(&f.core, "asks", "id", "a3"), "stopped");
    assert_eq!(state_of(&f.core, "asks", "id", "k1"), "open");
    for p in ["mia", "mia-b1", "mia-b2"] {
        assert_eq!(revoke_epoch(&f.core, p).unwrap(), 1, "{p}");
    }
    assert_eq!(open_rows(&f), 0);
}

#[test]
fn a_crash_between_the_suspend_and_the_order_is_finished_by_the_replay() {
    let mut f = fixture();
    let live = world(&mut f);
    // the suspend committed with its record and the core died before the order
    f.core
        .conn()
        .execute("UPDATE people SET revoked = 1 WHERE id = 'mia'", [])
        .unwrap();
    super::record_suspend(f.core.conn(), "mia", 1.0).unwrap();
    let (kill, failed) = super::replay_open(&mut f.core, NOW).unwrap();
    assert!(failed.is_empty(), "{failed:?}");
    assert_ne!(kill.len(), 0);
    assert_eq!(state_of(&f.core, "asks", "id", "a3"), "stopped");
    assert_eq!(state_of(&f.core, "claims", "claim_id", live[3].1), "ending");
    assert_eq!(open_rows(&f), 0);
}

#[test]
fn an_undo_before_the_order_has_run_drops_the_record_and_sweeps_nobody() {
    let mut f = fixture();
    let live = world(&mut f);
    f.core
        .conn()
        .execute("UPDATE people SET revoked = 1 WHERE id = 'mia'", [])
        .unwrap();
    super::record_suspend(f.core.conn(), "mia", 1.0).unwrap();
    let out =
        crate::authority::call::call(&mut f.core, "people.undo", &json!({"id": "mia"})).unwrap();
    assert!(out["kill"].is_null(), "{out}");
    assert_eq!(is_revoked(&f, "mia"), 0);
    assert_eq!(open_rows(&f), 1, "undo leaves the record for the order");
    let (kill, failed) = super::replay_open(&mut f.core, NOW).unwrap();
    assert!(kill.is_empty() && failed.is_empty());
    assert_eq!(open_rows(&f), 0, "dropped without acting");
    assert_eq!(state_of(&f.core, "asks", "id", "a1"), "open");
    assert_eq!(
        state_of(&f.core, "claims", "claim_id", live[1].1),
        "running"
    );
    assert_eq!(revoke_epoch(&f.core, "mia").unwrap(), 0);
}

#[test]
fn a_repeat_suspend_merges_the_record_and_runs_the_order_again_harmlessly() {
    let mut f = fixture();
    world(&mut f);
    for _ in 0..2 {
        let out =
            crate::authority::call::call(&mut f.core, "people.suspend", &json!({"id": "mia"}))
                .unwrap();
        assert!(out["revokePending"].is_null(), "{out}");
        assert_eq!(open_rows(&f), 0);
    }
    assert_eq!(state_of(&f.core, "asks", "id", "a1"), "stopped");
    // suspend, undo, suspend again: the second suspend sweeps what started in between
    crate::authority::call::call(&mut f.core, "people.undo", &json!({"id": "mia"})).unwrap();
    let seen = revoke_epoch(&f.core, "mia").unwrap();
    let mut nt = new_turn("c-again", "app", "mia");
    nt.revoke_seen = seen;
    let t = enqueue_turn(&mut f.core, &nt, 2).unwrap();
    let again = claim(&mut f.core, &new_claim("c-again", "mia", t, seen), 3)
        .unwrap()
        .claim_id;
    redeem(&mut f.core, again, 30, "e30", "h1", 4).unwrap();
    crate::authority::call::call(&mut f.core, "people.suspend", &json!({"id": "mia"})).unwrap();
    assert_eq!(state_of(&f.core, "claims", "claim_id", again), "ending");
    assert_eq!(open_rows(&f), 0);
}

#[test]
fn the_owner_is_refused_a_suspend_and_leaves_no_record() {
    let mut f = fixture();
    f.core
        .conn()
        .execute("UPDATE people SET owner = 1 WHERE id = 'kai'", [])
        .unwrap();
    let err = crate::authority::call::call(&mut f.core, "people.suspend", &json!({"id": "kai"}))
        .unwrap_err();
    assert!(err.to_string().contains("owner"), "{err}");
    assert_eq!(open_rows(&f), 0);
}
