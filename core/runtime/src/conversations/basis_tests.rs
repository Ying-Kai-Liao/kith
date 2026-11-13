#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::many_single_char_names,
    clippy::assert_is_empty
)]
//! The room basis, the turn budget, the conversation key and the warm processes (migration 82) on a real core.db: the
//! rules of `conversations::basis` and `conversations::warm`, and what `claims` does with them.

use super::basis::{self, Basis, Chain, Level, OWNER};
use super::claims::{
    Kind, NewClaim, NewTurn, claim, different_holder, end_gone, enqueue_turn, redeem,
    revoke_person, stop,
};
use super::tests::{Procs, count, open, state, turn_state};
use super::warm::{self, WarmProc};
use crate::db::Core;
use crate::error::Error;

fn new_turn<'a>(conv: &'a str, person: &'a str) -> NewTurn<'a> {
    NewTurn {
        conversation: conv,
        agent: "echo",
        engine: "claude",
        source: "room",
        person,
        prompt: "hi",
        deadline: None,
        revoke_seen: 0,
    }
}

fn room_basis(people: &[&str], author: &str, chain: Chain) -> Basis {
    Basis {
        people: people.iter().map(|p| ((*p).to_owned(), 0)).collect(),
        room: Some("r1".into()),
        room_gen: Some(3),
        level: Level::Room,
        join_epoch: Some(1),
        author: Some(author.into()),
        chain,
        key: None,
    }
}

fn room_turn(c: &mut Core, conv: &str, people: &[&str], chain: Chain) -> i64 {
    let author = people.first().copied().unwrap_or("bot:1");
    basis::enqueue(
        c,
        &new_turn(conv, "room-r1"),
        &room_basis(people, author, chain),
        10,
    )
    .expect("room turn")
}

fn refused(r: crate::error::Result<i64>) -> String {
    match r {
        Err(Error::Refused(w)) => w,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn claim_turn(c: &mut Core, conv: &str, turn: i64, person: &str) -> i64 {
    let req = NewClaim {
        conversation: conv,
        kind: Kind::Turn,
        person,
        turn: Some(turn),
        holder_instance: Some("kith-serve-host/1-a"),
        account: None,
        process: None,
        revoke_seen: 0,
    };
    claim(c, &req, 11).expect("claim").claim_id
}

fn proc_of<'a>(pid: u32, started: &'a str, person: &'a str, conv: Option<&'a str>) -> WarmProc<'a> {
    WarmProc {
        pid,
        started,
        instance: "kith-serve-host/1-a",
        person,
        agent: "echo",
        conversation: conv,
        revoke_seen: None,
    }
}

fn budget_left(c: &Core, turn: i64) -> Option<i64> {
    c.conn()
        .query_row("SELECT budget_left FROM turns WHERE id = ?1", [turn], |r| {
            r.get(0)
        })
        .expect("budget")
}

#[test]
fn revoke_of_a_basis_person_ends_the_room_claim_and_the_warm_processes() {
    let (_t, mut c) = open();
    let t1 = room_turn(&mut c, "room-c1", &["ann", "bob"], Chain::Root(3));
    let claim_id = claim_turn(&mut c, "room-c1", t1, "room-r1");
    redeem(&mut c, claim_id, 100, "e100", "kith-serve-host/1-a", 12).expect("redeem");
    // The room's process, an idle guest process of ann's, and bob's process in another conversation.
    warm::register(&mut c, &proc_of(200, "e200", "room-r1", Some("room-c1"))).expect("room proc");
    warm::register(&mut c, &proc_of(300, "e300", "ann", None)).expect("idle proc");
    warm::register(&mut c, &proc_of(400, "e400", "bob", Some("bob-c"))).expect("bob proc");
    // Someone outside the basis: nothing of the room is touched.
    let other = revoke_person(&mut c, "zed", 20).expect("revoke zed");
    assert!(other.kill.is_empty(), "{other:?}");
    assert_eq!(state(&c, claim_id), "running");
    let r = revoke_person(&mut c, "ann", 21).expect("revoke ann");
    assert_eq!(state(&c, claim_id), "ending", "the room claim ends");
    assert_eq!(turn_state(&c, t1), "cancelled");
    let pids: Vec<_> = r.kill.iter().filter_map(|k| k.pid).collect();
    assert!(pids.contains(&100), "the claim's process: {pids:?}");
    assert!(pids.contains(&200), "the room's warm process: {pids:?}");
    assert!(pids.contains(&300), "ann's idle process: {pids:?}");
    assert!(!pids.contains(&400), "bob is not revoked: {pids:?}");
    // Twice: the same kills, nothing new.
    let again = revoke_person(&mut c, "ann", 22).expect("again");
    assert!(again.kill.iter().any(|k| k.pid == Some(300)));
    // The marked rows go once their processes are gone; bob's stays.
    let live = Procs::default();
    live.spawn(200, "e200");
    live.spawn(300, "e300");
    live.spawn(100, "e100");
    assert_eq!(
        warm::settle(&mut c, &live).expect("settle"),
        0,
        "alive: kept"
    );
    live.kill(200);
    live.kill(300);
    live.kill(100);
    assert_eq!(warm::settle(&mut c, &live).expect("settle"), 2);
    assert_eq!(end_gone(&mut c, &live, 30).expect("gone"), vec![claim_id]);
    assert_eq!(count(&c, "SELECT count(*) FROM warm_procs"), 1);
    assert_eq!(
        warm::loaded(&c, 400, "e400").expect("bob"),
        Some("bob-c".into())
    );
}

#[test]
fn an_epoch_moved_for_one_basis_person_refuses_the_enqueue_and_the_claim() {
    let (_t, mut c) = open();
    revoke_person(&mut c, "bob", 5).expect("bob revoked");
    // ann's epoch is current, bob's was read before his revoke.
    let e = refused(basis::enqueue(
        &mut c,
        &new_turn("room-c1", "room-r1"),
        &room_basis(&["ann", "bob"], "ann", Chain::Root(2)),
        10,
    ));
    assert!(e.contains("bob was revoked"), "{e}");
    assert_eq!(count(&c, "SELECT count(*) FROM turns"), 0);
    // Read at the current epoch it goes in, then a revoke slipping in before the claim is refused by the fence.
    let mut b = room_basis(&["ann", "bob"], "ann", Chain::Root(2));
    b.people[1].1 = 1;
    let t1 = basis::enqueue(&mut c, &new_turn("room-c1", "room-r1"), &b, 10).expect("queued");
    c.conn()
        .execute("UPDATE meta SET value = '2' WHERE key = 'revoked:bob'", [])
        .expect("bump");
    let req = NewClaim {
        conversation: "room-c1",
        kind: Kind::Turn,
        person: "room-r1",
        turn: Some(t1),
        holder_instance: None,
        account: None,
        process: None,
        revoke_seen: 0,
    };
    let e = claim(&mut c, &req, 11).expect_err("fenced");
    assert!(e.to_string().contains("was revoked"), "{e}");
    assert_eq!(
        turn_state(&c, t1),
        "queued",
        "the revoke's sweep cancels it, not the fence"
    );
}

#[test]
fn a_room_turn_with_no_basis_refuses_at_the_enqueue_and_at_the_claim() {
    let (_t, mut c) = open();
    let e = refused(basis::enqueue(
        &mut c,
        &new_turn("room-c1", "room-r1"),
        &room_basis(&[], "ann", Chain::Root(2)),
        10,
    ));
    assert!(e.contains("no basis"), "{e}");
    // A row that got in without one (an old binary, a hand edit) is not claimed either.
    super::tests::conversation(&c, "room-c2");
    c.conn()
        .execute(
            "INSERT INTO turns (conversation, person, source, state, queued_at, level) VALUES ('room-c2', 'room-r1', 'room', 'queued', 1, 'room')",
            [],
        )
        .expect("bare");
    let t = c.conn().last_insert_rowid();
    let req = NewClaim {
        conversation: "room-c2",
        kind: Kind::Turn,
        person: "room-r1",
        turn: Some(t),
        holder_instance: None,
        account: None,
        process: None,
        revoke_seen: 0,
    };
    let e = claim(&mut c, &req, 11).expect_err("no basis");
    assert!(e.to_string().contains("has no basis"), "{e}");
}

#[test]
fn only_a_basis_persons_post_opens_a_budget() {
    let (_t, mut c) = open();
    let e = refused(basis::enqueue(
        &mut c,
        &new_turn("room-c1", "room-r1"),
        &room_basis(&["ann"], "bot:7", Chain::Root(5)),
        10,
    ));
    assert!(e.contains("only a person's post opens"), "{e}");
    let e = refused(basis::enqueue(
        &mut c,
        &new_turn("room-c1", "room-r1"),
        &room_basis(&["ann"], "ann", Chain::None),
        10,
    ));
    assert!(
        e.contains("turn budget"),
        "a room turn names a budget or a parent: {e}"
    );
    // A person outside the basis cannot open one either.
    let e = refused(basis::enqueue(
        &mut c,
        &new_turn("room-c1", "room-r1"),
        &room_basis(&["ann"], "cat", Chain::Root(5)),
        10,
    ));
    assert!(e.contains("not a person of the basis"), "{e}");
    assert_eq!(count(&c, "SELECT count(*) FROM turns"), 0);
}

#[test]
fn a_follow_up_spends_one_round_and_is_refused_at_none() {
    let (_t, mut c) = open();
    let root = room_turn(&mut c, "room-c1", &["ann"], Chain::Root(2));
    assert_eq!(budget_left(&c, root), Some(2));
    let follow = |c: &mut Core, from: i64, key: Option<&str>| {
        let mut b = room_basis(&["ann"], "bot:7", Chain::Follow(from));
        b.key = key.map(str::to_owned);
        basis::enqueue(c, &new_turn("room-c1", "room-r1"), &b, 11)
    };
    let one = follow(&mut c, root, Some("k1")).expect("first follow-up");
    assert_eq!(budget_left(&c, one), Some(1));
    let root_of: Option<i64> = c
        .conn()
        .query_row("SELECT budget_root FROM turns WHERE id = ?1", [one], |r| {
            r.get(0)
        })
        .expect("root");
    assert_eq!(
        root_of,
        Some(root),
        "the chain names the message that opened it"
    );
    // The same enqueue again after a lost reply: the same turn, no second row.
    assert_eq!(follow(&mut c, root, Some("k1")).expect("retry"), one);
    assert_eq!(count(&c, "SELECT count(*) FROM turns"), 2);
    let two = follow(&mut c, one, None).expect("second follow-up");
    assert_eq!(budget_left(&c, two), Some(0));
    let root_of: Option<i64> = c
        .conn()
        .query_row("SELECT budget_root FROM turns WHERE id = ?1", [two], |r| {
            r.get(0)
        })
        .expect("root");
    assert_eq!(root_of, Some(root), "still the first message");
    let e = refused(follow(&mut c, two, None));
    assert!(e.contains("turn budget"), "{e}");
    assert_eq!(
        count(&c, "SELECT count(*) FROM turns"),
        3,
        "the refusal wrote nothing"
    );
    // A turn with no budget has nothing to follow.
    let plain = enqueue_turn(&mut c, &new_turn("plain", "owner"), 12).expect("plain");
    let e = refused(follow(&mut c, plain, None));
    assert!(e.contains("no turn budget"), "{e}");
}

#[test]
fn a_requeue_keeps_the_budget_and_a_cancel_gives_nothing_back() {
    let (_t, mut c) = open();
    let root = room_turn(&mut c, "room-c1", &["ann"], Chain::Root(2));
    let b = room_basis(&["ann"], "bot:7", Chain::Follow(root));
    let t = basis::enqueue(&mut c, &new_turn("room-c1", "room-r1"), &b, 11).expect("follow-up");
    assert_eq!(budget_left(&c, t), Some(1));
    let id = claim_turn(&mut c, "room-c1", t, "room-r1");
    // A stop that requeues (a busy park, a failover, a holder change): the same row, the same budget.
    stop(&mut c, id, "failover", true, 12).expect("stop");
    assert_eq!(turn_state(&c, t), "queued");
    assert_eq!(budget_left(&c, t), Some(1));
    let id = claim_turn_again(&mut c, t);
    // A cancel (a stop, a revoke) returns nothing to the chain: the parent still has what it had.
    stop(&mut c, id, "stop", false, 13).expect("cancel");
    assert_eq!(turn_state(&c, t), "cancelled");
    assert_eq!(budget_left(&c, t), Some(1));
    assert_eq!(budget_left(&c, root), Some(2));
    // The columns are fixed for good: the table refuses a change.
    let e = c
        .conn()
        .execute("UPDATE turns SET budget_left = 5 WHERE id = ?1", [t])
        .expect_err("fixed");
    assert!(e.to_string().contains("are fixed"), "{e}");
    let e = c
        .conn()
        .execute("UPDATE turn_people SET person = 'x' WHERE turn = ?1", [t])
        .expect_err("fixed");
    assert!(e.to_string().contains("fixed"), "{e}");
}

/// Claims `t` again once its first claim has ended (the stop's kill is not needed for an unspawned claim).
fn claim_turn_again(c: &mut Core, t: i64) -> i64 {
    let conv: String = c
        .conn()
        .query_row("SELECT conversation FROM turns WHERE id = ?1", [t], |r| {
            r.get(0)
        })
        .expect("conversation");
    claim_turn(c, &conv, t, "room-r1")
}

#[test]
fn a_second_conversation_for_one_agent_room_and_epoch_refuses() {
    let (_t, mut c) = open();
    basis::ensure_room(&mut c, "c1", "echo", "claude", "r1", 1, 10).expect("first");
    // The same id again is the same conversation.
    basis::ensure_room(&mut c, "c1", "echo", "claude", "r1", 1, 11).expect("again");
    let e = basis::ensure_room(&mut c, "c2", "echo", "claude", "r1", 1, 12).expect_err("second");
    assert!(e.to_string().contains("a second one is refused"), "{e}");
    // Another epoch, another room, another agent: each its own.
    basis::ensure_room(&mut c, "c3", "echo", "claude", "r1", 2, 13).expect("next epoch");
    basis::ensure_room(&mut c, "c4", "echo", "claude", "r2", 1, 14).expect("other room");
    basis::ensure_room(&mut c, "c5", "other", "claude", "r1", 1, 15).expect("other agent");
    // An id that names another epoch's conversation is refused.
    let e =
        basis::ensure_room(&mut c, "c1", "echo", "claude", "r1", 2, 16).expect_err("wrong epoch");
    assert!(e.to_string().contains("is not the conversation"), "{e}");
    // An enqueue for a new id under a taken key is refused too, and wrote no turn.
    let e = refused(basis::enqueue(
        &mut c,
        &new_turn("c9", "room-r1"),
        &room_basis(&["ann"], "ann", Chain::Root(1)),
        17,
    ));
    assert!(e.contains("a second one is refused"), "{e}");
    assert_eq!(count(&c, "SELECT count(*) FROM turns"), 0);
}

#[test]
fn an_older_epoch_conversation_takes_the_owners_turns_only() {
    let (_t, mut c) = open();
    basis::ensure_room(&mut c, "old", "echo", "claude", "r1", 1, 10).expect("epoch 1");
    // The room is at epoch 2 now: a room turn names it and goes to its own conversation, not the old one.
    let mut b = room_basis(&["ann"], "ann", Chain::Root(1));
    b.join_epoch = Some(2);
    let e = refused(basis::enqueue(&mut c, &new_turn("old", "room-r1"), &b, 11));
    assert!(e.contains("its own epoch"), "{e}");
    // A person's own turn to it is refused as well.
    let mut p = room_basis(&[], "ann", Chain::None);
    p.level = Level::Owner;
    p.join_epoch = Some(2);
    let e = refused(basis::enqueue(&mut c, &new_turn("old", "ann"), &p, 11));
    assert!(e.contains("only the owner"), "{e}");
    // The owner's goes through.
    let t = basis::enqueue(&mut c, &new_turn("old", OWNER), &p, 12).expect("owner's turn");
    assert_eq!(turn_state(&c, t), "queued");
    // The current epoch's conversation takes the room turn.
    let t = basis::enqueue(&mut c, &new_turn("new", "room-r1"), &b, 13).expect("epoch 2");
    assert_eq!(turn_state(&c, t), "queued");
    // A row that was never a room conversation is no room's.
    super::tests::conversation(&c, "plain");
    let e = refused(basis::enqueue(
        &mut c,
        &new_turn("plain", "room-r1"),
        &b,
        14,
    ));
    assert!(e.contains("not a room conversation"), "{e}");
}

#[test]
fn a_plain_turn_is_an_owner_level_turn_with_no_budget() {
    let (_t, mut c) = open();
    let t = enqueue_turn(&mut c, &new_turn("c1", "ann"), 10).expect("plain");
    let (level, left, room): (String, Option<i64>, Option<String>) = c
        .conn()
        .query_row(
            "SELECT level, budget_left, room FROM turns WHERE id = ?1",
            [t],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("row");
    assert_eq!((level.as_str(), left, room), ("owner", None, None));
    assert_eq!(count(&c, "SELECT count(*) FROM turn_people"), 0);
    // Its person's revoke still cancels it, by the person column as before.
    revoke_person(&mut c, "ann", 11).expect("revoke");
    assert_eq!(turn_state(&c, t), "cancelled");
}

fn host_claim(c: &mut Core, conv: &str, holder: &str, pid: Option<u32>) -> i64 {
    super::tests::conversation(c, conv);
    let t = super::tests::turn(c, conv, "owner");
    let req = NewClaim {
        conversation: conv,
        kind: Kind::Turn,
        person: "owner",
        turn: Some(t),
        holder_instance: Some(holder),
        account: None,
        process: None,
        revoke_seen: 0,
    };
    let id = claim(c, &req, 10).expect("claim").claim_id;
    if let Some(p) = pid {
        redeem(c, id, p, &format!("e{p}"), holder, 11).expect("redeem");
    }
    id
}

#[test]
fn a_new_host_instance_ends_the_old_hosts_claims_and_processes_and_nothing_else() {
    let (_t, mut c) = open();
    let me = "kith-serve-host/2-b";
    let old_claimed = host_claim(&mut c, "a", "kith-serve-host/1-a", None);
    let old_running = host_claim(&mut c, "b", "kith-serve-host", Some(100));
    let mine = host_claim(&mut c, "c", me, Some(200));
    let holder = host_claim(&mut c, "d", "h1", Some(300));
    warm::register(&mut c, &proc_of(400, "e400", "", Some("b"))).expect("old host's process");
    let mut own = proc_of(500, "e500", "", None);
    own.instance = me;
    warm::register(&mut c, &own).expect("own process");
    let out = warm::adopt(&mut c, me, 20).expect("adopt");
    assert_eq!(
        out.ended,
        vec![old_claimed],
        "never redeemed: ended at once"
    );
    assert_eq!(state(&c, old_claimed), "ended");
    assert_eq!(
        state(&c, old_running),
        "ending",
        "a process: the writer until gone"
    );
    assert_eq!(state(&c, mine), "running");
    assert_eq!(
        state(&c, holder),
        "running",
        "a holder's claim is its holder's"
    );
    let pids: Vec<_> = out.kill.iter().filter_map(|k| k.pid).collect();
    assert!(pids.contains(&100) && pids.contains(&400), "{pids:?}");
    assert!(
        !pids.contains(&200) && !pids.contains(&300) && !pids.contains(&500),
        "{pids:?}"
    );
    // The turns of the ended claims are back in the queue for whoever claims them next.
    assert_eq!(
        count(&c, "SELECT count(*) FROM turns WHERE state = 'queued'"),
        2
    );
    // Twice: nothing more ends.
    let again = warm::adopt(&mut c, me, 21).expect("again");
    assert!(again.ended.is_empty());
    assert_eq!(state(&c, mine), "running");
    let e = warm::adopt(&mut c, "h2", 22).expect_err("a holder is no host");
    assert!(e.to_string().contains("not a host instance"), "{e}");
}

#[test]
fn a_holders_hello_leaves_every_host_instances_claims_alone() {
    let (_t, mut c) = open();
    let a = host_claim(&mut c, "a", "kith-serve-host/1-a", Some(100));
    let b = host_claim(&mut c, "b", "kith-serve-host", Some(200));
    let h = host_claim(&mut c, "c", "h1", Some(300));
    let out = different_holder(&mut c, "h2", &[], &[], i64::MAX, 30).expect("hello");
    assert_eq!(state(&c, a), "running");
    assert_eq!(state(&c, b), "running");
    assert_eq!(state(&c, h), "ending", "the old holder's is ended");
    assert!(out.kill.iter().all(|k| k.pid == Some(300)), "{out:?}");
}

#[test]
fn a_process_is_not_registered_past_its_persons_revoke() {
    let (_t, mut c) = open();
    revoke_person(&mut c, "ann", 5).expect("revoke");
    let mut w = proc_of(300, "e300", "ann", None);
    w.revoke_seen = Some(0);
    let e = warm::register(&mut c, &w).expect_err("stale");
    assert!(e.to_string().contains("was revoked"), "{e}");
    w.revoke_seen = Some(1);
    warm::register(&mut c, &w).expect("current");
    assert!(warm::gone(&mut c, 300, "e300").expect("gone"));
    assert!(!warm::gone(&mut c, 300, "e300").expect("twice"));
}
