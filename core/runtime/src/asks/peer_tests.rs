//! Asks across computers (design 2026-11-24-ask-ledger §9, A5a): one test per rule, through the daemon's own dispatch, on
//! the ledger tests' rig. The fence tests are the model's variants (`PeerFence.tla`): each fence point refuses alone, and the
//! sweep alone is enough.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

use kith_api::Principal;
use serde_json::{Value, json};

use super::ledger_tests::{Rig, ms, turn_of};
use crate::conversations::sched_run;
use crate::error::Error;

pub(super) const MAC: &str = "macB";
pub(super) const DAY: i64 = 24 * 3600 * 1000;

pub(super) fn exec(r: &Rig, sql: &'static str, arg: i64) {
    r.d.shared()
        .with_core(|c| {
            c.conn().execute(sql, [arg])?;
            Ok(())
        })
        .unwrap();
}

/// The host's pairing epoch is a hash with no order: the tests use names.
pub(super) fn ep(n: i64) -> String {
    format!("epoch-{n}")
}

pub(super) fn link_dir(r: &Rig, dir: &str, epoch: i64) -> Value {
    r.host(
        "ask.peerLink",
        &json!({"computer": MAC, "dir": dir, "epoch": ep(epoch)}),
    )
}

/// The pairing the computer's asks to this one come under.
pub(super) fn link_in(r: &Rig, epoch: i64) -> Value {
    link_dir(r, "in", epoch)
}

/// The pairing this one's asks to the computer go under.
pub(super) fn link_out(r: &Rig, epoch: i64) -> Value {
    link_dir(r, "out", epoch)
}

pub(super) fn end_in(r: &Rig) -> Value {
    r.host("ask.endPeer", &json!({"computer": MAC, "dir": "in"}))
}

pub(super) fn end_out(r: &Rig) -> Value {
    r.host("ask.endPeer", &json!({"computer": MAC, "dir": "out"}))
}

/// The pairing ends behind the asks' back: the row only, no sweep.
fn drop_fence(r: &Rig, dir: &str) {
    r.d.shared()
        .with_core(|c| {
            c.conn()
                .execute("UPDATE ask_peers SET live = 0 WHERE dir = ?1", [dir])?;
            Ok(())
        })
        .unwrap();
}

/// A peer ask of `boss`, in `sess-1`, for `echo` on `computer`.
pub(super) fn peer(r: &Rig, id: &str, computer: &str) -> Value {
    r.open(
        id,
        "peer",
        &json!({"peer": computer, "task": "do it there"}),
        &[],
    )
}

pub(super) fn origin(r: &Rig, id: &str, key: &str, epoch: i64) -> Value {
    r.host("ask.open", &origin_params(r, id, key, epoch, &[]))
}

pub(super) fn origin_params(
    r: &Rig,
    id: &str,
    key: &str,
    epoch: i64,
    env: &[(&str, &str)],
) -> Value {
    let mut p = r.env(env);
    p["id"] = json!(id);
    p["asked"] = json!("echo");
    p["mode"] = json!("bg");
    p["task"] = json!("do it here");
    p["originComputer"] = json!(MAC);
    p["originKey"] = json!(key);
    p["originEpoch"] = json!(ep(epoch));
    p
}

pub(super) fn apply(
    r: &Rig,
    id: &str,
    epoch: i64,
    rows: &Value,
    state: Option<&str>,
    cost: Option<f64>,
) -> Value {
    let mut p = r.env(&[]);
    p["id"] = json!(id);
    p["epoch"] = json!(ep(epoch));
    p["rows"] = rows.clone();
    if let Some(s) = state {
        p["state"] = json!(s);
    }
    if let Some(c) = cost {
        p["cost"] = json!(c);
    }
    r.host("ask.peerApply", &p)
}

/// The host's claim says no as an answer (`{refused}`) or as an error.
pub(super) fn was_refused(said: &Result<Value, Error>) -> bool {
    match said {
        Err(_) => true,
        Ok(v) => v.get("refused").is_some(),
    }
}

pub(super) fn try_claim(r: &Rig, id: &str, turn: i64) -> Result<Value, Error> {
    r.call(
        Principal::Host,
        "hostclaim.claim",
        &json!({"conversation": format!("ask:{id}"), "kind": "askRun", "turn": turn, "person": "owner",
                "revokeSeen": 0, "holderInstance": "kith-serve-host/1-a"}),
    )
}

pub(super) fn attach(r: &Rig, id: &str, claim: i64) -> Value {
    let pid = std::process::id();
    let started = kith_kernel::procstart::start_stamp(pid).expect("a start stamp");
    r.d.shared()
        .with_core(|c| sched_run::attach(c, claim, Some(&format!("ask:{id}")), pid, &started, ms()))
        .unwrap()
}

pub(super) fn count(r: &Rig, sql: &str, arg: &str) -> i64 {
    r.sql(sql, arg)
}

pub(super) fn wakes(r: &Rig) -> usize {
    r.owner("wake.queueList", &json!({}))["items"]
        .as_array()
        .map_or(0, |items| {
            items
                .iter()
                .map(|i| i["requests"].as_array().map_or(0, Vec::len))
                .sum()
        })
}

// ---- opening ----

#[test]
fn a_peer_open_needs_a_live_pairing_and_makes_no_turn() {
    let r = Rig::new();
    let none = r.call(Principal::Owner, "ask.open", &{
        let mut p = r.env(&[]);
        p["id"] = json!("p0");
        p["asked"] = json!("echo");
        p["mode"] = json!("peer");
        p["peer"] = json!(MAC);
        p["task"] = json!("t");
        p
    });
    assert!(matches!(none, Err(Error::Refused(_))), "{none:?}");
    link_out(&r, 1);
    let opened = peer(&r, "p1", MAC);
    assert_eq!(opened["turn"], Value::Null, "{opened}");
    assert_eq!(r.ask_state("p1"), "open");
    let turn: Option<i64> = r.sql("SELECT turn FROM asks WHERE id = ?1", "p1");
    assert_eq!(turn, None);
    let epoch: String = r.sql("SELECT peer_epoch FROM asks WHERE id = ?1", "p1");
    assert_eq!(epoch, ep(1));
    // It crosses: one cross hop, whatever the caller said.
    let cross: i64 = r.sql("SELECT cross_hops FROM asks WHERE id = ?1", "p1");
    assert_eq!(cross, 1);
    // The same open again finds the row.
    let again = peer(&r, "p1", MAC);
    assert_eq!(again["replayed"], json!(true));
    assert_eq!(
        count(&r, "SELECT count(*) FROM asks WHERE peer = ?1", MAC),
        1
    );
}

#[test]
fn a_peer_open_is_the_owners_only() {
    let r = Rig::new();
    link_out(&r, 1);
    let ask = |env: &[(&str, &str)]| {
        let mut p = r.env(env);
        p["id"] = json!("g1");
        p["asked"] = json!("echo");
        p["mode"] = json!("peer");
        p["peer"] = json!(MAC);
        p["task"] = json!("t");
        p
    };
    // A guest's own run.
    let guest = r.call(
        Principal::Owner,
        "ask.open",
        &ask(&[("KITH_PERSON", "guest-1")]),
    );
    assert!(matches!(guest, Err(Error::Refused(_))), "{guest:?}");
    // A run token never reaches the row.
    let token = r.call(Principal::RunToken, "ask.open", &ask(&[]));
    assert!(matches!(token, Err(Error::NotAllowed(_))), "{token:?}");
    // An owner agent asked on a guest's behalf can't cross either: its ask carries the guest's person.
    r.open(
        "up",
        "bg",
        &json!({"task": "t"}),
        &[("KITH_PERSON", "guest-1")],
    );
    let mut p = ask(&[("KITH_ASK_ID", "up")]);
    p["id"] = json!("g2");
    let chained = r.call(Principal::Owner, "ask.open", &p);
    assert!(matches!(chained, Err(Error::Refused(_))), "{chained:?}");
    assert_eq!(
        count(&r, "SELECT count(*) FROM asks WHERE peer = ?1", MAC),
        0
    );
    // The owner's own, with a person that is "owner" or none, opens.
    let mut p = ask(&[("KITH_PERSON", "owner")]);
    p["id"] = json!("g3");
    r.owner("ask.open", &p);
}

#[test]
fn an_origin_open_makes_a_bg_turn_and_the_same_key_again_finds_the_row() {
    let r = Rig::new();
    link_in(&r, 1);
    let first = origin(&r, "o1", "k1", 1);
    let turn = turn_of(&first);
    assert_eq!(r.turn_state(turn), "queued");
    // No local asker: nothing here to wake.
    let session: Option<String> = r.sql("SELECT reply_session FROM asks WHERE id = ?1", "o1");
    assert_eq!(session, None);
    // A resent peerAsk names the same key (the host picks another id): the row it made answers, at any epoch.
    let again = origin(&r, "o2", "k1", 1);
    assert_eq!(again["replayed"], json!(true));
    assert_eq!(turn_of(&again), turn);
    assert_eq!(
        count(&r, "SELECT count(*) FROM asks WHERE origin_key = ?1", "k1"),
        1
    );
    assert_eq!(
        count(&r, "SELECT count(*) FROM turns WHERE source = ?1", "ask"),
        1
    );
}

#[test]
fn an_origin_open_at_an_old_or_ended_epoch_is_a_refused_row_it_can_read() {
    let r = Rig::new();
    link_in(&r, 2);
    let old = origin(&r, "o1", "k1", 1);
    assert_eq!(old["refused"], json!(true), "{old}");
    assert_eq!(r.ask_state("o1"), "refused");
    assert_eq!(r.rows("o1"), [("lost".to_owned(), false)]);
    let turn: Option<i64> = r.sql("SELECT turn FROM asks WHERE id = ?1", "o1");
    assert_eq!(turn, None);
    // The same send again answers the same row.
    assert_eq!(origin(&r, "o9", "k1", 2)["replayed"], json!(true));
    // An epoch that was ended refuses too, and a computer never paired.
    end_in(&r);
    assert_eq!(origin(&r, "o3", "k3", 2)["refused"], json!(true));
    let mut p = origin_params(&r, "o4", "k4", 1, &[]);
    p["originComputer"] = json!("never-paired");
    assert_eq!(r.host("ask.open", &p)["refused"], json!(true));
    assert_eq!(
        count(&r, "SELECT count(*) FROM turns WHERE source = ?1", "ask"),
        0
    );
}

#[test]
fn a_cap_refusal_of_an_origin_open_is_a_row_too() {
    let r = Rig::new();
    link_out(&r, 1);
    // The chain the other computer sent comes in `lineage`; the host's own environment is nobody's here.
    let mut p = origin_params(&r, "o1", "k1", 1, &[]);
    p["lineage"] = json!({"chain": "c1", "hop": 0, "cross": 99, "from": "x", "cost": 0.0});
    let said = r.host("ask.open", &p);
    assert_eq!(said["refused"], json!(true), "{said}");
    assert_eq!(r.ask_state("o1"), "refused");
    // A local open with the same numbers is still an error: only the other computer's ask is answered with a row.
    let mut q = r.env(&[("KITH_ASK_CHAIN", "c1"), ("KITH_ASK_CROSS", "99")]);
    q["id"] = json!("l1");
    q["asked"] = json!("echo");
    q["mode"] = json!("bg");
    q["task"] = json!("t");
    assert!(matches!(
        r.call(Principal::Owner, "ask.open", &q),
        Err(Error::Refused(_))
    ));
}

// ---- the fence ----

#[test]
fn end_peer_refuses_queued_and_claimed_and_stops_running_in_one_step() {
    let r = Rig::new();
    link_in(&r, 1);
    let queued = turn_of(&origin(&r, "q", "kq", 1));
    let claimed = turn_of(&origin(&r, "c", "kc", 1));
    let running = turn_of(&origin(&r, "r", "kr", 1));
    let held = r.claim("c", claimed);
    let live = r.start("r", running);
    let ended = end_in(&r);
    assert_eq!(ended["ended"], json!(["c", "q", "r"]), "{ended}");
    assert_eq!(r.ask_state("q"), "refused");
    assert_eq!(r.ask_state("c"), "refused");
    assert_eq!(r.ask_state("r"), "stopped");
    for id in ["q", "c"] {
        assert_eq!(r.rows(id), [("lost".to_owned(), false)], "{id}");
    }
    assert_eq!(r.turn_state(queued), "cancelled");
    assert_eq!(r.turn_state(claimed), "cancelled");
    // The running one's claim goes to `ending` and the kill goes to the waiter that holds it.
    let claim_state = |id: i64| -> String {
        r.sql(
            "SELECT state FROM claims WHERE claim_id = ?1",
            &id.to_string(),
        )
    };
    assert_eq!(claim_state(held), "ended");
    assert_eq!(claim_state(live), "ending");
    let kills = ended["kill"].as_array().unwrap();
    assert_eq!(kills.len(), 1, "{ended}");
    assert_eq!(kills[0]["claimId"], json!(live));
    // The claimed turn's child can no longer attach: its claim is gone (the sweep alone is enough).
    assert_eq!(attach(&r, "c", held)["refused"], json!("gone"));
    assert_eq!(r.turn_state(claimed), "cancelled");
    // Twice is the same.
    let twice = end_in(&r);
    assert_eq!(twice["ended"], json!([]));
    assert_eq!(r.ask_state("r"), "stopped");
}

#[test]
fn after_end_peer_nothing_opens_claims_or_attaches() {
    let r = Rig::new();
    link_in(&r, 1);
    let turn = turn_of(&origin(&r, "a", "ka", 1));
    end_in(&r);
    // Opening: refused as a row, no turn.
    assert_eq!(origin(&r, "b", "kb", 1)["refused"], json!(true));
    // Claiming: the sweep cancelled the turn; the claim is refused.
    let refused = try_claim(&r, "a", turn);
    assert!(was_refused(&refused), "{refused:?}");
    assert_eq!(
        count(
            &r,
            "SELECT count(*) FROM turns WHERE source = ?1 AND state = 'running'",
            "ask"
        ),
        0
    );
}

#[test]
fn the_claim_fence_alone_refuses_a_turn_of_an_ended_epoch() {
    // No sweep: the epoch is ended behind the ask's back (PeerFence variant v7 without the sweep, here for the claim).
    let r = Rig::new();
    link_in(&r, 1);
    let turn = turn_of(&origin(&r, "a", "ka", 1));
    drop_fence(&r, "in");
    let refused = try_claim(&r, "a", turn);
    assert!(was_refused(&refused), "{refused:?}");
    assert_eq!(r.turn_state(turn), "cancelled");
}

#[test]
fn the_attach_fence_alone_refuses_the_run_and_its_engine_never_starts() {
    let r = Rig::new();
    link_in(&r, 1);
    let turn = turn_of(&origin(&r, "a", "ka", 1));
    let claim = r.claim("a", turn);
    assert_eq!(r.turn_state(turn), "claimed");
    // The pairing ends between the claim and the child's attach; no sweep ran.
    drop_fence(&r, "in");
    assert_eq!(attach(&r, "a", claim)["refused"], json!("gone"));
    assert_eq!(r.turn_state(turn), "cancelled");
    let state: String = r.sql(
        "SELECT state FROM claims WHERE claim_id = ?1",
        &claim.to_string(),
    );
    assert_eq!(state, "ended");
    // The ask has no running turn; the tick settles it lost (nothing was ever started).
    r.tick();
    assert_eq!(r.ask_state("a"), "lost");
}

#[test]
fn a_new_epoch_replaces_the_old_and_ends_what_came_under_it_in_that_direction_only() {
    let r = Rig::new();
    link_in(&r, 1);
    link_out(&r, 1);
    let turn = turn_of(&origin(&r, "a", "ka", 1));
    peer(&r, "p", MAC);
    // A different epoch for `in` ends the live one first, in the same step: its asks go, the other direction's stay.
    let said = link_in(&r, 2);
    assert_eq!(said["ended"], json!(["a"]), "{said}");
    assert_eq!(r.ask_state("a"), "refused");
    assert_eq!(r.turn_state(turn), "cancelled");
    assert_eq!(r.ask_state("p"), "open");
    // The old epoch opens nothing; the new one does.
    assert_eq!(origin(&r, "b", "kb", 1)["refused"], json!(true));
    assert!(origin(&r, "c", "kc", 2)["turn"].is_i64());
    // An epoch that was ended is refused, the live one again changes nothing.
    let again = r.call(
        Principal::Host,
        "ask.peerLink",
        &json!({"computer": MAC, "dir": "in", "epoch": ep(1)}),
    );
    assert!(matches!(again, Err(Error::Refused(_))), "{again:?}");
    link_in(&r, 2);
    assert!(origin(&r, "e", "ke", 2)["turn"].is_i64());
    // `out` goes the same way and cuts off what was sent under the old one.
    let said = link_out(&r, 2);
    assert_eq!(said["ended"], json!(["p"]), "{said}");
    assert_eq!(r.ask_state("p"), "cut_off");
    end_in(&r);
    assert_eq!(origin(&r, "d", "kd", 2)["refused"], json!(true));
    let ended = r.call(
        Principal::Host,
        "ask.peerLink",
        &json!({"computer": MAC, "dir": "in", "epoch": ep(2)}),
    );
    assert!(matches!(ended, Err(Error::Refused(_))), "{ended:?}");
}

#[test]
fn end_peer_with_an_epoch_ends_only_that_pairing_never_a_newer_one() {
    let r = Rig::new();
    link_out(&r, 1);
    peer(&r, "p", MAC);
    // The owner re-linked since the caller read epoch 1: its end of epoch 1 changes nothing of epoch 2's.
    let said = link_out(&r, 2);
    assert_eq!(said["ended"], json!(["p"]), "{said}");
    peer(&r, "q", MAC);
    let stale = r.host(
        "ask.endPeer",
        &json!({"computer": MAC, "dir": "out", "epoch": ep(1)}),
    );
    assert_eq!(stale["ended"], json!([]), "{stale}");
    assert_eq!(r.ask_state("q"), "open");
    assert_eq!(
        r.sql::<i64>("SELECT live FROM ask_peers WHERE epoch = ?1", &ep(2)),
        1
    );
    // The live epoch named ends it.
    let live = r.host(
        "ask.endPeer",
        &json!({"computer": MAC, "dir": "out", "epoch": ep(2)}),
    );
    assert_eq!(live["ended"], json!(["q"]), "{live}");
    assert_eq!(r.ask_state("q"), "cut_off");
}

#[test]
fn two_computers_paired_both_ways_end_one_direction_and_the_other_goes_on() {
    let r = Rig::new();
    link_in(&r, 1);
    link_out(&r, 5);
    let turn = turn_of(&origin(&r, "a", "ka", 1));
    peer(&r, "p", MAC);
    // `in` ends: the asks that computer sent here are ended; the ask sent there is untouched, and still owed.
    let ended = end_in(&r);
    assert_eq!(ended["ended"], json!(["a"]), "{ended}");
    assert_eq!(r.ask_state("p"), "open");
    assert_eq!(r.turn_state(turn), "cancelled");
    assert_eq!(
        r.host("ask.peerOwed", &json!({}))["send"][0]["id"],
        json!("p")
    );
    // A new ask still opens `out`, and the other way refuses.
    peer(&r, "p2", MAC);
    assert_eq!(origin(&r, "b", "kb", 1)["refused"], json!(true));
    // The other way round: `out` ends, and what came `in` opens and runs.
    link_in(&r, 3);
    let live = turn_of(&origin(&r, "c", "kc", 3));
    end_out(&r);
    assert_eq!(r.ask_state("p"), "cut_off");
    assert_eq!(r.ask_state("p2"), "cut_off");
    let claim = r.start("c", live);
    assert_eq!(r.turn_state(live), "running");
    assert!(claim > 0);
    assert_eq!(r.ask_state("c"), "open");
    // The `out` pairing is over: nothing to open there.
    let mut p = r.env(&[]);
    p["id"] = json!("p3");
    p["asked"] = json!("echo");
    p["mode"] = json!("peer");
    p["peer"] = json!(MAC);
    p["task"] = json!("t");
    assert!(matches!(
        r.call(Principal::Owner, "ask.open", &p),
        Err(Error::Refused(_))
    ));
}

#[test]
fn a_host_cannot_open_a_peer_ask() {
    let r = Rig::new();
    link_out(&r, 1);
    let mut p = r.env(&[]);
    p["id"] = json!("p1");
    p["asked"] = json!("echo");
    p["mode"] = json!("peer");
    p["peer"] = json!(MAC);
    p["task"] = json!("t");
    let said = r.call(Principal::Host, "ask.open", &p);
    assert!(matches!(said, Err(Error::NotAllowed(_))), "{said:?}");
    assert_eq!(
        count(&r, "SELECT count(*) FROM asks WHERE peer = ?1", MAC),
        0
    );
}

// ---- following ----

#[test]
fn peer_owed_lists_the_send_then_the_follow_and_the_stop() {
    let r = Rig::new();
    link_out(&r, 1);
    peer(&r, "p1", MAC);
    let owed = r.host("ask.peerOwed", &json!({}));
    assert_eq!(owed["send"][0]["id"], json!("p1"), "{owed}");
    assert_eq!(owed["send"][0]["agent"], json!("echo"));
    assert_eq!(owed["send"][0]["task"], json!("do it there"));
    assert_eq!(owed["send"][0]["lineage"]["cross"], json!(1));
    assert_eq!(owed["follow"], json!([]));
    // The computer answered the send: from now on it is followed.
    apply(&r, "p1", 1, &json!([]), Some("open"), None);
    let owed = r.host("ask.peerOwed", &json!({}));
    assert_eq!(owed["send"], json!([]));
    assert_eq!(owed["follow"][0]["after"], json!(0), "{owed}");
    // A stop is owed until the computer says the run is over.
    r.owner("ask.end", &json!({"roots": ["p1"]}));
    let owed = r.host("ask.peerOwed", &json!({}));
    assert_eq!(owed["stop"][0]["key"], json!("p1"), "{owed}");
    assert_eq!(owed["follow"], json!([]));
    apply(&r, "p1", 1, &json!([]), Some("stopped"), None);
    assert_eq!(r.host("ask.peerOwed", &json!({}))["stop"], json!([]));
    // An unpaired computer is owed nothing.
    peer(&r, "p2", MAC);
    end_out(&r);
    let owed = r.host("ask.peerOwed", &json!({}));
    assert_eq!(
        owed,
        json!({"send": [], "follow": [], "stop": [], "to": []})
    );
}

#[test]
fn an_eleventh_ask_is_held_back_here_and_goes_when_one_of_the_ten_ends() {
    let r = Rig::new();
    link_out(&r, 1);
    for n in 0..11 {
        peer(&r, &format!("q{n:02}"), MAC);
    }
    let listed = |r: &Rig| -> Vec<String> {
        r.host("ask.peerOwed", &json!({}))["send"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["id"].as_str().unwrap().to_owned())
            .collect()
    };
    let first = listed(&r);
    assert_eq!(first.len(), 10, "{first:?}");
    let held_back = (0..11)
        .map(|n| format!("q{n:02}"))
        .find(|id| !first.contains(id))
        .unwrap();
    // Ten of them open there: still ten, the same ones are owed.
    apply(&r, &first[0], 1, &json!([]), Some("open"), None);
    let second = listed(&r);
    assert_eq!(second.len(), 9, "{second:?}");
    assert!(!second.contains(&held_back), "{second:?}");
    // One ends: the held-back ask is owed now.
    apply(&r, &first[0], 1, &json!([]), Some("answered"), None);
    let third = listed(&r);
    assert!(third.contains(&held_back), "{third:?}");
    assert_eq!(
        r.ask_state(&held_back),
        "open",
        "never refused or cut off for waiting"
    );
}

#[test]
fn a_failed_try_shows_waiting_on_the_computer_and_a_call_it_answered_clears_it() {
    let r = Rig::new();
    link_out(&r, 1);
    peer(&r, "p1", MAC);
    let waiting = |r: &Rig| -> Value {
        r.owner("ask.delegations", &json!({}))["delegations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["id"] == json!("p1"))
            .unwrap()["waiting"]
            .clone()
    };
    assert_eq!(waiting(&r), Value::Null);
    let wakes_before = wakes(&r);
    let mut p = r.env(&[]);
    p["id"] = json!("p1");
    p["epoch"] = json!(ep(1));
    p["away"] = json!(true);
    let said = r.host("ask.peerApply", &p);
    assert_eq!(said["applied"], json!(false), "{said}");
    let w = waiting(&r);
    assert_eq!(
        (&w["on"], &w["computer"]),
        (&json!("computer"), &json!(MAC)),
        "{w}"
    );
    let since = w["since"].clone();
    assert!(since.is_i64(), "{w}");
    // Nothing was settled and nothing was written: the ask is open, still owed its send, with no row and no wake.
    assert_eq!(r.ask_state("p1"), "open");
    assert!(r.rows("p1").is_empty());
    assert_eq!(wakes(&r), wakes_before);
    assert_eq!(
        r.host("ask.peerOwed", &json!({}))["send"][0]["id"],
        json!("p1")
    );
    // A second failed try keeps the first one's time.
    r.host("ask.peerApply", &p);
    assert_eq!(waiting(&r)["since"], since);
    // The computer answered: the mark is gone.
    apply(&r, "p1", 1, &json!([]), Some("open"), None);
    assert_eq!(waiting(&r), Value::Null);
    // An ask that is over waits on nothing, and a failed try does not mark it.
    apply(&r, "p1", 1, &json!([]), Some("stopped"), None);
    r.host("ask.peerApply", &p);
    assert_eq!(waiting(&r), Value::Null);
}

#[test]
fn peer_apply_copies_each_row_once_with_one_wake_per_row_and_books_the_cost_once() {
    let r = Rig::new();
    link_out(&r, 1);
    peer(&r, "p1", MAC);
    let rows = json!([
        {"n": 1, "as": "echo", "kind": "progress", "text": "halfway"},
        {"n": 2, "as": "echo", "kind": "answer", "text": "forty-two"},
    ]);
    let first = apply(&r, "p1", 1, &rows, Some("answered"), Some(0.5));
    assert_eq!(first["applied"], json!(true), "{first}");
    assert_eq!(first["rows"], json!([1, 2]));
    assert_eq!(r.ask_state("p1"), "answered");
    assert_eq!(
        r.rows("p1"),
        [("progress".to_owned(), false), ("answer".to_owned(), false)]
    );
    // One wake for the answer; a progress row waits for `--wait`.
    assert_eq!(wakes(&r), 1);
    assert_eq!(r.wake_state("p1", 2), "queued");
    let fetched: i64 = r.sql("SELECT fetched FROM asks WHERE id = ?1", "p1");
    assert_eq!(fetched, 2);
    // The same rows again are no-ops: no row, no wake, no second cost.
    let second = apply(&r, "p1", 1, &rows, Some("answered"), Some(0.5));
    assert_eq!(second["applied"], json!(false), "{second}");
    assert_eq!(r.rows("p1").len(), 2);
    assert_eq!(wakes(&r), 1);
    let cost: f64 = r.sql("SELECT cost FROM asks WHERE id = ?1", "p1");
    assert!((cost - 0.5).abs() < 1e-9, "{cost}");
    // Nothing after a final state.
    apply(
        &r,
        "p1",
        1,
        &json!([{"n": 3, "kind": "answer", "text": "late"}]),
        None,
        None,
    );
    assert_eq!(r.rows("p1").len(), 2);
    // `--wait` reads what the wake did not take, and withdraws the wake (R5 holds on the copied rows).
    let read = r.owner("ask.read", &json!({"id": "p1"}));
    assert_eq!(read["rows"].as_array().unwrap().len(), 2);
    assert_eq!(wakes(&r), 0);
}

#[test]
fn peer_apply_in_pieces_copies_from_after_the_last_row() {
    let r = Rig::new();
    link_out(&r, 1);
    peer(&r, "p1", MAC);
    let one = json!([{"n": 1, "kind": "progress", "text": "a"}]);
    apply(&r, "p1", 1, &one, Some("open"), None);
    let both = json!([{"n": 1, "kind": "progress", "text": "a"}, {"n": 2, "kind": "progress", "text": "b"}]);
    let said = apply(&r, "p1", 1, &both, Some("open"), None);
    assert_eq!(said["rows"], json!([2]), "{said}");
    assert_eq!(r.ask_state("p1"), "open");
    assert_eq!(r.rows("p1").len(), 2);
    // A reply from another epoch is dropped.
    let stale = apply(
        &r,
        "p1",
        7,
        &json!([{"n": 3, "kind": "answer", "text": "x"}]),
        Some("answered"),
        None,
    );
    assert_eq!(stale["stale"], json!(true));
    assert_eq!(r.ask_state("p1"), "open");
    // Only an ask of another computer takes rows.
    r.open_bg("local");
    let local = r.call(Principal::Host, "ask.peerApply", &{
        let mut p = r.env(&[]);
        p["id"] = json!("local");
        p["epoch"] = json!(ep(1));
        p
    });
    assert!(matches!(local, Err(Error::Refused(_))), "{local:?}");
}

#[test]
fn a_computer_that_stopped_it_on_its_own_tells_the_asker_once() {
    let r = Rig::new();
    link_out(&r, 1);
    peer(&r, "p1", MAC);
    apply(&r, "p1", 1, &json!([]), Some("stopped"), None);
    assert_eq!(r.ask_state("p1"), "stopped");
    assert_eq!(r.rows("p1"), [("lost".to_owned(), false)]);
    assert_eq!(wakes(&r), 1);
    apply(&r, "p1", 1, &json!([]), Some("stopped"), None);
    assert_eq!(r.rows("p1").len(), 1);
}

#[test]
fn nothing_reaches_the_asker_after_a_stop_but_the_cost_and_the_confirmation_do() {
    let r = Rig::new();
    link_out(&r, 1);
    peer(&r, "p1", MAC);
    apply(&r, "p1", 1, &json!([]), Some("open"), None);
    r.owner("ask.end", &json!({"roots": ["p1"]}));
    assert_eq!(r.ask_state("p1"), "stopped");
    let owed: i64 = r.sql("SELECT stop_owed FROM asks WHERE id = ?1", "p1");
    assert_eq!(owed, 1);
    let said = apply(
        &r,
        "p1",
        1,
        &json!([{"n": 1, "kind": "answer", "text": "too late"}]),
        Some("answered"),
        Some(0.3),
    );
    assert_eq!(said["applied"], json!(false), "{said}");
    assert_eq!(r.rows("p1").len(), 0);
    assert_eq!(wakes(&r), 0);
    assert_eq!(r.ask_state("p1"), "stopped");
    // Its cost joins the chain once, and the confirmed end clears the owed stop.
    assert!((r.sql::<f64>("SELECT cost FROM asks WHERE id = ?1", "p1") - 0.3).abs() < 1e-9);
    let owed: i64 = r.sql("SELECT stop_owed FROM asks WHERE id = ?1", "p1");
    assert_eq!(owed, 0);
    apply(&r, "p1", 1, &json!([]), Some("answered"), Some(0.3));
    assert!((r.sql::<f64>("SELECT cost FROM asks WHERE id = ?1", "p1") - 0.3).abs() < 1e-9);
}

#[test]
fn a_stop_of_a_tree_owes_a_stop_to_every_peer_ask_in_it() {
    let r = Rig::new();
    link_out(&r, 1);
    r.open_bg("root");
    r.open(
        "mid",
        "bg",
        &json!({"task": "t"}),
        &[("KITH_ASK_ID", "root")],
    );
    r.open(
        "far",
        "peer",
        &json!({"peer": MAC, "task": "t"}),
        &[("KITH_ASK_ID", "mid")],
    );
    r.open(
        "near",
        "peer",
        &json!({"peer": MAC, "task": "t"}),
        &[("KITH_ASK_ID", "root")],
    );
    r.owner("ask.end", &json!({"roots": ["root"]}));
    for id in ["far", "near"] {
        let owed: i64 = r.sql("SELECT stop_owed FROM asks WHERE id = ?1", id);
        assert_eq!(owed, 1, "{id}");
        assert_eq!(r.ask_state(id), "stopped");
    }
    let owed = r.host("ask.peerOwed", &json!({}));
    assert_eq!(owed["stop"].as_array().unwrap().len(), 2, "{owed}");
}

// ---- the cut-off ----

#[test]
fn the_tick_cuts_off_a_peer_ask_past_the_wait_once_and_the_stop_stays_owed() {
    let r = Rig::new();
    link_out(&r, 1);
    peer(&r, "late", MAC);
    peer(&r, "fresh", MAC);
    exec(
        &r,
        "UPDATE asks SET peer_seen_ms = ?1 WHERE id = 'late'",
        ms() - 8 * DAY,
    );
    exec(
        &r,
        "UPDATE asks SET peer_seen_ms = ?1 WHERE id = 'fresh'",
        ms() - 6 * DAY,
    );
    r.tick();
    assert_eq!(r.ask_state("late"), "cut_off");
    assert_eq!(r.ask_state("fresh"), "open");
    assert_eq!(r.rows("late"), [("lost".to_owned(), false)]);
    assert_eq!(r.wake_state("late", 1), "queued");
    let owed: i64 = r.sql("SELECT stop_owed FROM asks WHERE id = ?1", "late");
    assert_eq!(owed, 1);
    let list = r.host("ask.peerOwed", &json!({}));
    assert_eq!(list["stop"][0]["id"], json!("late"), "{list}");
    // A later tick adds nothing; and the computer's own final row never joins the notice.
    r.tick();
    assert_eq!(r.rows("late").len(), 1);
    let said = apply(
        &r,
        "late",
        1,
        &json!([{"n": 1, "kind": "answer", "text": "done after all"}]),
        Some("answered"),
        None,
    );
    assert_eq!(said["applied"], json!(false), "{said}");
    assert_eq!(r.rows("late").len(), 1);
    assert_eq!(r.ask_state("late"), "cut_off");
    // Its confirmation of the stop ends the owing.
    let owed: i64 = r.sql("SELECT stop_owed FROM asks WHERE id = ?1", "late");
    assert_eq!(owed, 0);
    // An ask whose computer answered is never cut off (the answer starts the wait over).
    let said = apply(&r, "fresh", 1, &json!([]), Some("open"), None);
    assert_eq!(said["applied"], json!(true));
}

#[test]
fn an_unpaired_computers_asks_are_cut_off_at_once_with_one_notice() {
    let r = Rig::new();
    link_out(&r, 1);
    peer(&r, "p1", MAC);
    peer(&r, "p2", MAC);
    // The link ends through the row: both are cut off in the same step, one row and one wake each, nothing owed.
    let ended = end_out(&r);
    assert_eq!(ended["ended"], json!(["p1", "p2"]), "{ended}");
    for id in ["p1", "p2"] {
        assert_eq!(r.ask_state(id), "cut_off");
        assert_eq!(r.rows(id).len(), 1);
        let owed: i64 = r.sql("SELECT stop_owed FROM asks WHERE id = ?1", id);
        assert_eq!(owed, 0);
    }
    assert_eq!(wakes(&r), 2);
    end_out(&r);
    r.tick();
    assert_eq!(r.rows("p1").len(), 1);
    assert_eq!(wakes(&r), 2);
}

#[test]
fn the_tick_cuts_off_an_ask_whose_pairing_ended_behind_its_back() {
    let r = Rig::new();
    link_out(&r, 1);
    peer(&r, "p1", MAC);
    drop_fence(&r, "out");
    r.tick();
    assert_eq!(r.ask_state("p1"), "cut_off");
    r.tick();
    assert_eq!(r.rows("p1").len(), 1);
}

// ---- the table ----

#[test]
fn every_peer_row_checks_its_params_and_stays_off_the_run_socket() {
    let r = Rig::new();
    for (method, who) in [
        ("ask.peerLink", Principal::Host),
        ("ask.peerLink", Principal::Owner),
        ("ask.endPeer", Principal::Host),
        ("ask.peerApply", Principal::Host),
        ("ask.peerApply", Principal::Owner),
        ("ask.peerThread", Principal::Host),
        ("ask.peerStop", Principal::Host),
    ] {
        let unknown = r.call(
            who,
            method,
            &json!({"computer": "x", "dir": "in", "id": "x", "epoch": "e", "zzz": 1}),
        );
        assert!(
            matches!(unknown, Err(Error::BadParams(_))),
            "{method}: {unknown:?}"
        );
        let none = r.call(who, method, &json!({}));
        assert!(
            matches!(none, Err(Error::BadParams(_))),
            "{method}: {none:?}"
        );
    }
    for method in ["ask.peerOwed", "ask.peerList"] {
        let unknown = r.call(Principal::Host, method, &json!({"zzz": 1}));
        assert!(matches!(unknown, Err(Error::BadParams(_))), "{unknown:?}");
    }
    // No guest reaches them.
    for (method, params) in [
        (
            "ask.peerLink",
            json!({"computer": "x", "dir": "in", "epoch": "e"}),
        ),
        ("ask.endPeer", json!({"computer": "x", "dir": "out"})),
        ("ask.peerApply", json!({"id": "x", "epoch": "e"})),
        ("ask.peerOwed", json!({})),
        ("ask.peerList", json!({})),
        (
            "ask.peerThread",
            json!({"computer": "x", "key": "k", "epoch": "e"}),
        ),
        (
            "ask.peerStop",
            json!({"id": "i", "computer": "x", "key": "k", "epoch": "e"}),
        ),
    ] {
        let denied = r.call(Principal::RunToken, method, &params);
        assert!(
            matches!(denied, Err(Error::NotAllowed(_))),
            "{method}: {denied:?}"
        );
    }
    // Every param the table names reaches the code.
    link_out(&r, 1);
    peer(&r, "p1", MAC);
    let said = r.call(Principal::Host, "ask.peerApply", &{
        let mut p = r.env(&[]);
        p["id"] = json!("p1");
        p["epoch"] = json!(ep(1));
        p["rows"] = json!([{"n": 1, "as": "echo", "kind": "progress", "text": "t", "at": 5}]);
        p["state"] = json!("open");
        p["cost"] = json!(0.1);
        p
    });
    assert!(said.is_ok(), "{said:?}");
    let bad_state = r.call(Principal::Host, "ask.peerApply", &{
        let mut p = r.env(&[]);
        p["id"] = json!("p1");
        p["epoch"] = json!(ep(1));
        p["state"] = json!("sideways");
        p
    });
    assert!(
        matches!(bad_state, Err(Error::BadParams(_))),
        "{bad_state:?}"
    );
    let partial = r.call(Principal::Owner, "ask.open", &{
        let mut p = origin_params(&r, "x1", "k", 1, &[]);
        p.as_object_mut().unwrap().remove("originEpoch");
        p
    });
    assert!(matches!(partial, Err(Error::BadParams(_))), "{partial:?}");
}
