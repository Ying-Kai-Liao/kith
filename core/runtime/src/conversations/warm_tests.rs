//! `warm_procs` is one row per (process, conversation) (migration 83): a host process that has loaded several sessions is
//! registered, forgotten, revoked and adopted per session, and killed once.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::claims::{Redeemed, report, revoke_person};
use super::tests::{Procs, count, open, running, state, turn_state};
use super::warm::{self, WarmProc};
use crate::db::Core;

const HOST: &str = "kith-serve-host/1-a";

fn proc_of<'a>(pid: u32, person: &'a str, conv: Option<&'a str>) -> WarmProc<'a> {
    WarmProc {
        pid,
        started: "e",
        instance: HOST,
        person,
        agent: "echo",
        conversation: conv,
        revoke_seen: Some(0),
    }
}

fn reg(c: &mut Core, pid: u32, conv: Option<&str>) {
    warm::register(c, &proc_of(pid, "", conv)).expect("register");
}

fn rows(c: &Core, pid: u32) -> i64 {
    count(
        c,
        &format!("SELECT count(*) FROM warm_procs WHERE pid = {pid}"),
    )
}

#[test]
fn a_process_registers_each_session_it_loads_and_the_placeholder_goes() {
    let (_t, mut c) = open();
    reg(&mut c, 10, None);
    assert_eq!(rows(&c, 10), 1);
    assert_eq!(warm::loaded_all(&c, 10, "e").unwrap(), Vec::<String>::new());
    reg(&mut c, 10, Some("main"));
    reg(&mut c, 10, Some("thread"));
    reg(&mut c, 10, Some("thread"));
    assert_eq!(
        rows(&c, 10),
        2,
        "the empty row went, the repeat changed nothing"
    );
    assert_eq!(
        warm::loaded_all(&c, 10, "e").unwrap(),
        vec!["main".to_owned(), "thread".to_owned()]
    );
    // Registering with no session names the process again without adding a row.
    reg(&mut c, 10, None);
    assert_eq!(rows(&c, 10), 2);
    // Whose process it is, is one fact for every row of it.
    warm::register(&mut c, &proc_of(10, "", Some("third"))).unwrap();
    c.conn()
        .execute(
            "UPDATE warm_procs SET agent = 'old' WHERE pid = 10 AND conversation = 'main'",
            [],
        )
        .unwrap();
    reg(&mut c, 10, Some("thread"));
    assert_eq!(
        count(
            &c,
            "SELECT count(DISTINCT agent) FROM warm_procs WHERE pid = 10"
        ),
        1,
        "a registration restates the process on every row"
    );
}

#[test]
fn forgetting_a_session_keeps_the_process_and_forgetting_the_process_takes_every_row() {
    let (_t, mut c) = open();
    reg(&mut c, 10, Some("main"));
    reg(&mut c, 10, Some("thread"));
    assert!(warm::gone(&mut c, 10, "e", Some("thread")).unwrap());
    assert!(
        !warm::gone(&mut c, 10, "e", Some("thread")).unwrap(),
        "twice is a no-op"
    );
    assert_eq!(
        warm::loaded_all(&c, 10, "e").unwrap(),
        vec!["main".to_owned()]
    );
    // The last session leaves the process known (a revoke still reaches it) with none loaded.
    assert!(warm::gone(&mut c, 10, "e", Some("main")).unwrap());
    assert_eq!(rows(&c, 10), 1);
    assert_eq!(warm::loaded_all(&c, 10, "e").unwrap(), Vec::<String>::new());
    reg(&mut c, 10, Some("again"));
    assert_eq!(rows(&c, 10), 1);
    reg(&mut c, 10, Some("more"));
    assert!(warm::gone(&mut c, 10, "e", None).unwrap());
    assert_eq!(rows(&c, 10), 0);
    assert!(
        !warm::gone(&mut c, 10, "e", None).unwrap(),
        "twice is a no-op"
    );
}

#[test]
fn a_process_owed_a_kill_is_not_forgotten_and_its_new_sessions_are_marked_too() {
    let (_t, mut c) = open();
    reg(&mut c, 10, Some("main"));
    c.conn()
        .execute("UPDATE warm_procs SET kill_at = 5 WHERE pid = 10", [])
        .unwrap();
    assert!(!warm::gone(&mut c, 10, "e", Some("main")).unwrap());
    assert!(!warm::gone(&mut c, 10, "e", None).unwrap());
    reg(&mut c, 10, Some("thread"));
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM warm_procs WHERE pid = 10 AND kill_at = 5"
        ),
        2,
        "a session loaded after the mark is the marked process's"
    );
    // Once it is confirmed gone, settle takes every row.
    let live = Procs::default();
    assert_eq!(warm::settle(&mut c, &live).unwrap(), 2);
    assert_eq!(rows(&c, 10), 0);
}

#[test]
fn a_revoke_and_a_host_change_name_a_process_once_however_many_sessions_it_holds() {
    let (_t, mut c) = open();
    // A guest's process with two sessions.
    warm::register(&mut c, &proc_of(20, "ann", Some("a1"))).unwrap();
    warm::register(&mut c, &proc_of(20, "ann", Some("a2"))).unwrap();
    // The owner's process with two sessions, which an old host registered.
    reg(&mut c, 30, Some("o1"));
    reg(&mut c, 30, Some("o2"));
    let r = revoke_person(&mut c, "ann", 9).expect("revoke");
    let pids: Vec<_> = r.kill.iter().filter_map(|k| k.pid).collect();
    assert_eq!(pids, vec![20], "ann's process once: {pids:?}");
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM warm_procs WHERE kill_at IS NOT NULL"
        ),
        2,
        "both of its rows are marked"
    );
    let a = warm::adopt(&mut c, "kith-serve-host/2-b", 10).expect("adopt");
    let pids: Vec<_> = a.kill.iter().filter_map(|k| k.pid).collect();
    assert_eq!(pids.len(), 2, "each old process once: {pids:?}");
    assert!(pids.contains(&20) && pids.contains(&30));
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM warm_procs WHERE kill_at IS NOT NULL"
        ),
        4
    );
}

/// P7b: a guest's revoke kills the process that also holds the owner's session, and the owner's turn running there is not
/// cancelled with it: it stays the owner's, and when the process's exit is reported it goes back to the queue.
#[test]
fn an_owner_turn_killed_with_a_revoked_guests_process_is_requeued() {
    let (_t, mut c) = open();
    let (id, t) = running(&mut c, "o1", 100);
    let w = |conv| WarmProc {
        started: "e100",
        ..proc_of(100, "ann", Some(conv))
    };
    warm::register(&mut c, &w("a1")).unwrap();
    warm::register(&mut c, &w("o1")).unwrap();
    let r = revoke_person(&mut c, "ann", 40).expect("revoke");
    assert_eq!(
        r.kill.iter().filter_map(|k| k.pid).collect::<Vec<_>>(),
        vec![100]
    );
    assert_eq!(r.cancelled_turns, 0);
    assert_eq!(
        (state(&c, id).as_str(), turn_state(&c, t).as_str()),
        ("running", "running")
    );
    report(&mut c, id, "exit", r#"{"pid":100}"#, 41).expect("exit");
    assert_eq!(
        (state(&c, id).as_str(), turn_state(&c, t).as_str()),
        ("ended", "queued")
    );
}

/// P7b: a claim made while a process is marked for a kill is refused at the redeem on that process, whichever step made
/// it. The mark is read from any of the process's rows, including the placeholder row of a process whose last session
/// is gone.
#[test]
fn a_marked_process_refuses_a_redeem_even_through_its_placeholder_row() {
    let (_t, mut c) = open();
    super::tests::conversation(&c, "c1");
    let turn = super::tests::turn(&c, "c1", "sam");
    let id = super::claims::claim(&mut c, &super::tests::new_claim("c1", Some(turn)), 10)
        .unwrap()
        .claim_id;
    c.conn()
        .execute(
            "INSERT INTO warm_procs (pid, started, instance, person, agent, conversation, kill_at)
             VALUES (50, 'e50', ?1, '', 'echo', '', 5)",
            [HOST],
        )
        .unwrap();
    assert_eq!(
        super::claims::redeem(&mut c, id, 50, "e50", "h1", 20).unwrap(),
        Redeemed::Refused("killing".into())
    );
    assert_eq!(state(&c, id), "claimed");
}
