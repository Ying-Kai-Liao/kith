//! What the link carries beyond A5a's rows (design 2026-11-24-ask-ledger §9, A5b), through the daemon's own dispatch on the ledger
//! tests' rig: the callee's follow and stop, the places a computer may hold here, the day an unstarted ask waits, the caller's
//! `--to`, and a paired computer asked by its name.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

use kith_api::Principal;
use serde_json::{Value, json};

use super::ledger_tests::{Rig, ms, turn_of};
use super::peer_tests::{
    DAY, MAC, ep, exec, link_in, link_out, origin, peer, try_claim, was_refused,
};
use crate::error::Error;

fn thread(r: &Rig, key: &str, epoch: i64, after: i64) -> Result<Value, Error> {
    r.call(
        Principal::Host,
        "ask.peerThread",
        &json!({"computer": MAC, "key": key, "epoch": ep(epoch), "after": after}),
    )
}

fn stop(r: &Rig, id: &str, key: &str, epoch: i64) -> Result<Value, Error> {
    r.call(
        Principal::Host,
        "ask.peerStop",
        &json!({"id": id, "computer": MAC, "key": key, "epoch": ep(epoch)}),
    )
}

// ---- the callee's follow ----

#[test]
fn peer_thread_reads_rows_state_and_cost_and_writes_nothing() {
    let r = Rig::new();
    link_in(&r, 1);
    let turn = turn_of(&origin(&r, "o1", "k1", 1));
    let claim = r.start("o1", turn);
    r.owner(
        "ask.post",
        &json!({"id": "o1", "text": "halfway", "kind": "progress"}),
    );
    let first = thread(&r, "k1", 1, 0).unwrap();
    assert_eq!(first["found"], json!(true), "{first}");
    assert_eq!(first["state"], json!("open"));
    assert_eq!(first["rows"].as_array().unwrap().len(), 1, "{first}");
    assert_eq!(first["rows"][0]["kind"], json!("progress"));
    // Read only: the row is the same again, and still unread here.
    assert_eq!(thread(&r, "k1", 1, 0).unwrap()["rows"], first["rows"]);
    assert_eq!(thread(&r, "k1", 1, 1).unwrap()["rows"], json!([]));
    assert_eq!(r.rows("o1"), [("progress".to_owned(), false)]);
    // A key never seen is not found; an epoch that is not the live one is refused.
    assert_eq!(thread(&r, "nope", 1, 0).unwrap()["found"], json!(false));
    assert!(thread(&r, "k1", 7, 0).is_err());
    r.report(claim, "done");
}

// ---- the callee's stop ----

#[test]
fn peer_stop_ends_a_queued_ask_so_it_never_starts() {
    let r = Rig::new();
    link_in(&r, 1);
    let turn = turn_of(&origin(&r, "o1", "k1", 1));
    let said = stop(&r, "x1", "k1", 1).unwrap();
    assert_eq!(said["state"], json!("stopped"), "{said}");
    assert_eq!(r.ask_state("o1"), "stopped");
    assert_eq!(r.turn_state(turn), "cancelled");
    let claimed = try_claim(&r, "o1", turn);
    assert!(was_refused(&claimed), "{claimed:?}");
    // Again: the same answer, nothing more.
    assert_eq!(stop(&r, "x2", "k1", 1).unwrap()["state"], json!("stopped"));
}

#[test]
fn a_stop_that_comes_first_leaves_a_row_the_ask_finds() {
    let r = Rig::new();
    link_in(&r, 1);
    assert_eq!(
        stop(&r, "x1", "late", 1).unwrap()["state"],
        json!("stopped")
    );
    assert_eq!(r.ask_state("x1"), "stopped");
    // The ask arrives afterwards (another id of the host's making): it finds the stopped row and starts nothing.
    let said = origin(&r, "o9", "late", 1);
    assert_eq!(said["replayed"], json!(true), "{said}");
    assert!(said["turn"].is_null(), "{said}");
    assert_eq!(
        r.sql::<i64>("SELECT count(*) FROM turns WHERE source = ?1", "ask"),
        0
    );
}

#[test]
fn a_stop_of_an_ask_that_is_over_answers_its_state() {
    let r = Rig::new();
    link_in(&r, 1);
    let turn = turn_of(&origin(&r, "o1", "k1", 1));
    let claim = r.start("o1", turn);
    r.report(claim, "exit");
    r.tick();
    assert_eq!(r.ask_state("o1"), "lost");
    assert_eq!(stop(&r, "x1", "k1", 1).unwrap()["state"], json!("lost"));
}

#[test]
fn a_stop_under_an_ended_pairing_is_refused() {
    let r = Rig::new();
    link_in(&r, 1);
    origin(&r, "o1", "k1", 1);
    r.host("ask.endPeer", &json!({"computer": MAC, "dir": "in"}));
    assert!(matches!(stop(&r, "x1", "k1", 1), Err(Error::Refused(_))));
}

// ---- the places ----

#[test]
fn a_computer_has_at_most_ten_asks_waiting_here() {
    let r = Rig::new();
    link_in(&r, 1);
    for n in 0..10 {
        let said = origin(&r, &format!("o{n}"), &format!("k{n}"), 1);
        assert!(said["turn"].is_i64(), "{said}");
    }
    let eleventh = origin(&r, "o10", "k10", 1);
    assert_eq!(eleventh["refused"], json!(true), "{eleventh}");
    assert!(
        eleventh["reason"].as_str().unwrap().contains("10 asks"),
        "{eleventh}"
    );
    // The same key again is the row it made, however many wait.
    assert_eq!(origin(&r, "o11", "k3", 1)["replayed"], json!(true));
}

#[test]
fn a_computer_runs_at_most_two_at_once_and_the_third_waits_queued() {
    let r = Rig::new();
    link_in(&r, 1);
    let turns: Vec<i64> = (0..3)
        .map(|n| turn_of(&origin(&r, &format!("o{n}"), &format!("k{n}"), 1)))
        .collect();
    let c0 = r.start("o0", turns[0]);
    r.start("o1", turns[1]);
    let third = try_claim(&r, "o2", turns[2]);
    assert!(was_refused(&third), "{third:?}");
    assert_eq!(
        r.turn_state(turns[2]),
        "queued",
        "it waits, never cancelled"
    );
    r.report(c0, "done");
    let again = try_claim(&r, "o2", turns[2]);
    assert!(!was_refused(&again), "{again:?}");
}

#[test]
fn a_pairing_not_confirmed_since_the_core_started_starts_nothing_until_the_host_tells_it_again() {
    let r = Rig::new();
    link_in(&r, 1);
    let turn = turn_of(&origin(&r, "o1", "k1", 1));
    // The core was restarted: the host has not told it of the pairing in this process yet.
    r.d.shared()
        .with_core(|c| {
            c.conn()
                .execute("UPDATE ask_peers SET boot = 'an older core'", [])?;
            Ok(())
        })
        .unwrap();
    let held = try_claim(&r, "o1", turn);
    assert!(was_refused(&held), "{held:?}");
    assert_eq!(r.turn_state(turn), "queued", "it waits, never cancelled");
    link_in(&r, 1); // the reconcile tells it again
    let again = try_claim(&r, "o1", turn);
    assert!(!was_refused(&again), "{again:?}");
}

#[test]
fn the_run_door_lists_which_computer_sent_an_ask() {
    let r = Rig::new();
    link_in(&r, 1);
    origin(&r, "o1", "k1", 1);
    r.open_bg("local");
    let listed = r.host("hostclaim.queued", &json!({"sources": ["ask"]}));
    let of = |id: &str| {
        listed["turns"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["ask"] == json!(id))
            .unwrap()["originComputer"]
            .clone()
    };
    assert_eq!(of("o1"), json!(MAC));
    assert_eq!(of("local"), Value::Null);
}

// ---- the day ----

#[test]
fn an_ask_nobody_started_in_a_day_expires_lost_and_never_starts() {
    let r = Rig::new();
    link_in(&r, 1);
    let old = turn_of(&origin(&r, "old", "ko", 1));
    let fresh = turn_of(&origin(&r, "fresh", "kf", 1));
    let running = turn_of(&origin(&r, "run", "kr", 1));
    r.start("run", running);
    exec(
        &r,
        "UPDATE asks SET at_ms = ?1 WHERE id = 'old'",
        ms() - DAY - 1000,
    );
    exec(
        &r,
        "UPDATE asks SET at_ms = ?1 WHERE id = 'fresh'",
        ms() - DAY + 3_600_000,
    );
    exec(
        &r,
        "UPDATE asks SET at_ms = ?1 WHERE id = 'run'",
        ms() - 3 * DAY,
    );
    let ticked = r.tick();
    assert!(
        ticked["asks"].as_array().unwrap().contains(&json!("old")),
        "{ticked}"
    );
    assert_eq!(r.ask_state("old"), "lost");
    assert_eq!(r.turn_state(old), "expired");
    assert_eq!(r.rows("old"), [("lost".to_owned(), false)]);
    assert_eq!(r.ask_state("fresh"), "open");
    assert_eq!(r.turn_state(fresh), "queued");
    assert_eq!(r.ask_state("run"), "open", "a run is not an unstarted ask");
    let claimed = try_claim(&r, "old", old);
    assert!(was_refused(&claimed), "{claimed:?}");
    // A second tick adds nothing.
    assert_eq!(r.tick()["asks"], json!([]));
    assert_eq!(r.rows("old").len(), 1);
}

#[test]
fn an_ask_that_already_ran_and_was_put_back_does_not_expire() {
    let r = Rig::new();
    link_in(&r, 1);
    let turn = turn_of(&origin(&r, "ran", "kr", 1));
    let claim = r.start("ran", turn);
    // The run crashed and its turn went back in the queue (`requeue_turn`), long after the ask came.
    exec(
        &r,
        "UPDATE asks SET at_ms = ?1 WHERE id = 'ran'",
        ms() - 2 * DAY,
    );
    r.d.shared()
        .with_core(|c| {
            c.conn().execute(
                "UPDATE turns SET state = 'queued', claim_id = NULL, requeues = requeues + 1, started_at = NULL WHERE id = ?1",
                [turn],
            )?;
            c.conn().execute("UPDATE claims SET state = 'ended' WHERE id = ?1", [claim])?;
            Ok(())
        })
        .unwrap();
    assert_eq!(r.tick()["asks"], json!([]));
    assert_eq!(r.ask_state("ran"), "open");
    assert_eq!(r.turn_state(turn), "queued");
}

// ---- the caller's --to ----

#[test]
fn a_to_is_kept_for_a_peer_ask_once_and_owed_until_the_computer_took_it() {
    let r = Rig::new();
    link_out(&r, 1);
    peer(&r, "p1", MAC);
    let to = |text: &str| r.owner("ask.to", &json!({"id": "p1", "text": text}));
    let said = to("and one more thing");
    assert_eq!(said["peer"], json!(MAC), "{said}");
    let k = said["k"].as_str().unwrap().to_owned();
    // The same words again are one message.
    assert_eq!(to("and one more thing")["k"], json!(k));
    // Not owed before the send was answered; then owed with its words.
    assert_eq!(r.host("ask.peerOwed", &json!({}))["to"], json!([]));
    super::peer_tests::apply(&r, "p1", 1, &json!([]), Some("open"), None);
    let owed = r.host("ask.peerOwed", &json!({}));
    assert_eq!(owed["to"].as_array().unwrap().len(), 1, "{owed}");
    assert_eq!(owed["to"][0]["text"], json!("and one more thing"));
    assert_eq!(owed["to"][0]["k"], json!(k));
    // It goes on being owed until the computer says it took it.
    super::peer_tests::apply(&r, "p1", 1, &json!([]), Some("open"), None);
    assert_eq!(r.host("ask.peerOwed", &json!({}))["to"][0]["k"], json!(k));
    let mut p = r.env(&[]);
    p["id"] = json!("p1");
    p["epoch"] = json!(ep(1));
    p["toSent"] = json!([k]);
    r.host("ask.peerApply", &p);
    assert_eq!(r.host("ask.peerOwed", &json!({}))["to"], json!([]));
    // A stopped ask takes no follow-up.
    r.owner("ask.end", &json!({"roots": ["p1"]}));
    let refused = r.call(
        Principal::Owner,
        "ask.to",
        &json!({"id": "p1", "text": "too late"}),
    );
    assert!(refused.is_err(), "{refused:?}");
}

#[test]
fn a_to_after_the_peer_ask_was_answered_is_refused_and_nothing_is_owed() {
    let r = Rig::new();
    link_out(&r, 1);
    peer(&r, "p1", MAC);
    r.owner("ask.to", &json!({"id": "p1", "text": "before"}));
    super::peer_tests::apply(&r, "p1", 1, &json!([]), Some("open"), None);
    assert_eq!(
        r.host("ask.peerOwed", &json!({}))["to"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    super::peer_tests::apply(&r, "p1", 1, &json!([]), Some("answered"), None);
    // What was owed before the answer is not owed after it, and a new one is refused with words.
    assert_eq!(r.host("ask.peerOwed", &json!({}))["to"], json!([]));
    let late = r.call(
        Principal::Owner,
        "ask.to",
        &json!({"id": "p1", "text": "after"}),
    );
    assert!(
        matches!(late, Err(Error::Refused(ref w)) if w.contains("already answered")),
        "{late:?}"
    );
}

#[test]
fn a_to_for_a_local_ask_keeps_nothing() {
    let r = Rig::new();
    r.open_bg("a1");
    let said = r.owner("ask.to", &json!({"id": "a1", "text": "hello"}));
    assert_eq!(said["ok"], json!(true));
    assert!(said.get("peer").is_none(), "{said}");
}

// ---- asked by name ----

#[test]
fn a_paired_computer_is_asked_by_the_name_its_pairing_gave() {
    let r = Rig::new();
    r.host(
        "ask.peerLink",
        &json!({"computer": MAC, "dir": "out", "epoch": ep(1), "name": "Mini"}),
    );
    let said = r.open(
        "p1",
        "peer",
        &json!({"peer": "mini", "task": "do it there"}),
        &[],
    );
    assert!(said["record"].is_object(), "{said}");
    assert_eq!(
        r.sql::<String>("SELECT peer FROM asks WHERE id = ?1", "p1"),
        MAC
    );
    // A name nobody has is no pairing.
    let none = r.call(Principal::Owner, "ask.open", &{
        let mut p = r.env(&[]);
        p["id"] = json!("p2");
        p["asked"] = json!("echo");
        p["mode"] = json!("peer");
        p["task"] = json!("x");
        p["peer"] = json!("laptop");
        p
    });
    assert!(matches!(none, Err(Error::Refused(_))), "{none:?}");
    // The list says what is live, with the name.
    let list = r.host("ask.peerList", &json!({}));
    assert_eq!(list["out"][0]["name"], json!("Mini"), "{list}");
    assert_eq!(list["in"], json!([]));
}
