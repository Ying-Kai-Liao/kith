#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::{
    INVITEE_SLOTS, Liveness, State, Take, call, list, park, pid_gone, release, take, unpark,
};
use crate::db::Core;
use crate::layout::Layout;
use crate::tests::{machine, open_all as open};

fn core() -> (tempfile::TempDir, Layout, Core) {
    let (t, m) = machine();
    let l = Layout::new(&m);
    let c = open(&l);
    (t, l, c)
}

/// A system where no start time can be read and no process is ever reported gone: holders are never freed by it.
struct Nobody;

impl Liveness for Nobody {
    fn start(&self, _: u32) -> Option<String> {
        None
    }
    fn gone(&self, _: u32) -> bool {
        false
    }
}

/// Start times unreadable (a failed `ps`), but the real system answers whether a pid exists.
struct NoPs;

impl Liveness for NoPs {
    fn start(&self, _: u32) -> Option<String> {
        None
    }
    fn gone(&self, pid: u32) -> bool {
        pid_gone(pid)
    }
}

fn t(holder: &str, max: i64) -> Take<'_> {
    Take {
        holder,
        rt: holder,
        invitee: false,
        max: Some(max),
        pid: None,
    }
}

fn invitee(holder: &str, max: i64) -> Take<'_> {
    Take {
        invitee: true,
        ..t(holder, max)
    }
}

fn waiting(position: usize) -> State {
    State::Waiting { position }
}

fn running(c: &mut Core) -> i64 {
    list(c, &Nobody).expect("list")["running"].as_i64().unwrap()
}

#[test]
fn a_free_slot_is_granted_and_a_full_gate_makes_a_line() {
    let (_t, _l, mut c) = core();
    assert_eq!(take(&mut c, &t("a", 1), &Nobody).unwrap(), State::Granted);
    assert_eq!(take(&mut c, &t("b", 1), &Nobody).unwrap(), waiting(1));
    assert_eq!(take(&mut c, &t("c", 1), &Nobody).unwrap(), waiting(2));
}

#[test]
fn the_line_is_served_in_the_order_it_formed() {
    let (_t, _l, mut c) = core();
    take(&mut c, &t("a", 1), &Nobody).unwrap();
    take(&mut c, &t("b", 1), &Nobody).unwrap();
    take(&mut c, &t("c", 1), &Nobody).unwrap();
    assert!(release(&mut c, "a", &Nobody).unwrap());
    assert_eq!(take(&mut c, &t("b", 1), &Nobody).unwrap(), State::Granted);
    assert_eq!(take(&mut c, &t("c", 1), &Nobody).unwrap(), waiting(1));
    assert!(release(&mut c, "b", &Nobody).unwrap());
    assert_eq!(take(&mut c, &t("c", 1), &Nobody).unwrap(), State::Granted);
}

#[test]
fn a_double_take_changes_nothing_and_a_double_release_is_a_no_op() {
    let (_t, _l, mut c) = core();
    assert_eq!(take(&mut c, &t("a", 2), &Nobody).unwrap(), State::Granted);
    assert_eq!(take(&mut c, &t("a", 2), &Nobody).unwrap(), State::Granted);
    assert_eq!(running(&mut c), 1);
    assert_eq!(take(&mut c, &t("w", 1), &Nobody).unwrap(), waiting(1));
    assert_eq!(take(&mut c, &t("w", 1), &Nobody).unwrap(), waiting(1));
    assert_eq!(
        list(&mut c, &Nobody).unwrap()["waiting"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(release(&mut c, "a", &Nobody).unwrap());
    assert!(!release(&mut c, "a", &Nobody).unwrap(), "second release");
    assert!(
        !release(&mut c, "nobody-ever", &Nobody).unwrap(),
        "unknown holder"
    );
    assert_eq!(take(&mut c, &t("w", 1), &Nobody).unwrap(), State::Granted);
}

#[test]
fn a_holder_that_leaves_the_line_frees_its_place() {
    let (_t, _l, mut c) = core();
    take(&mut c, &t("a", 1), &Nobody).unwrap();
    take(&mut c, &t("b", 1), &Nobody).unwrap();
    take(&mut c, &t("c", 1), &Nobody).unwrap();
    assert!(release(&mut c, "b", &Nobody).unwrap());
    assert_eq!(take(&mut c, &t("c", 1), &Nobody).unwrap(), waiting(1));
}

#[test]
fn the_owners_turns_go_ahead_of_an_invited_persons() {
    let (_t, _l, mut c) = core();
    take(&mut c, &t("a", 1), &Nobody).unwrap();
    assert_eq!(
        take(&mut c, &invitee("guest", 1), &Nobody).unwrap(),
        waiting(1)
    );
    assert_eq!(take(&mut c, &t("owner", 1), &Nobody).unwrap(), waiting(2));
    release(&mut c, "a", &Nobody).unwrap();
    assert_eq!(
        take(&mut c, &t("owner", 1), &Nobody).unwrap(),
        State::Granted
    );
    assert_eq!(
        take(&mut c, &invitee("guest", 1), &Nobody).unwrap(),
        waiting(1)
    );
}

#[test]
fn invited_people_together_hold_at_most_their_share_and_never_block_the_owner() {
    assert_eq!(INVITEE_SLOTS, 1);
    let (_t, _l, mut c) = core();
    assert_eq!(
        take(&mut c, &invitee("g1", 3), &Nobody).unwrap(),
        State::Granted
    );
    assert_eq!(
        take(&mut c, &invitee("g2", 3), &Nobody).unwrap(),
        waiting(1)
    );
    // Room is free, but g2 may not use it; the owner's turn behind g2 goes by.
    assert_eq!(take(&mut c, &t("o1", 3), &Nobody).unwrap(), State::Granted);
    assert_eq!(take(&mut c, &t("o2", 3), &Nobody).unwrap(), State::Granted);
    assert_eq!(
        take(&mut c, &invitee("g2", 3), &Nobody).unwrap(),
        waiting(1)
    );
    release(&mut c, "g1", &Nobody).unwrap();
    assert_eq!(
        take(&mut c, &invitee("g2", 3), &Nobody).unwrap(),
        State::Granted
    );
}

#[test]
fn a_parked_turn_gives_its_slot_up_and_comes_back_ahead_of_new_turns() {
    let (_t, _l, mut c) = core();
    take(&mut c, &t("a", 1), &Nobody).unwrap();
    assert_eq!(take(&mut c, &t("b", 1), &Nobody).unwrap(), waiting(1));
    assert!(park(&mut c, "a", &Nobody).unwrap());
    // b, whom a waits on, took the only slot.
    assert_eq!(take(&mut c, &t("b", 1), &Nobody).unwrap(), State::Granted);
    assert_eq!(take(&mut c, &t("c", 1), &Nobody).unwrap(), waiting(1));
    assert_eq!(
        unpark(&mut c, "a", &Nobody).unwrap(),
        waiting(1),
        "ahead of c"
    );
    assert_eq!(take(&mut c, &t("c", 1), &Nobody).unwrap(), waiting(2));
    release(&mut c, "b", &Nobody).unwrap();
    assert_eq!(take(&mut c, &t("a", 1), &Nobody).unwrap(), State::Granted);
    assert_eq!(take(&mut c, &t("c", 1), &Nobody).unwrap(), waiting(1));
}

#[test]
fn asks_that_run_side_by_side_share_one_parking() {
    let (_t, _l, mut c) = core();
    take(&mut c, &t("a", 1), &Nobody).unwrap();
    assert!(park(&mut c, "a", &Nobody).unwrap());
    assert!(park(&mut c, "a", &Nobody).unwrap());
    assert_eq!(running(&mut c), 0);
    assert_eq!(
        unpark(&mut c, "a", &Nobody).unwrap(),
        State::Parked,
        "one ask is still out"
    );
    assert_eq!(
        unpark(&mut c, "a", &Nobody).unwrap(),
        State::Granted,
        "the last takes it back"
    );
    assert_eq!(
        unpark(&mut c, "a", &Nobody).unwrap(),
        State::Granted,
        "a double unpark changes nothing"
    );
    assert_eq!(running(&mut c), 1);
}

#[test]
fn parking_or_unparking_a_holder_that_holds_nothing_does_nothing() {
    let (_t, _l, mut c) = core();
    assert!(!park(&mut c, "ghost", &Nobody).unwrap());
    assert_eq!(unpark(&mut c, "ghost", &Nobody).unwrap(), State::Gone);
    take(&mut c, &t("w", 1), &Nobody).unwrap();
    take(&mut c, &t("x", 1), &Nobody).unwrap();
    assert!(
        !park(&mut c, "x", &Nobody).unwrap(),
        "a waiting turn holds no slot to give up"
    );
}

#[test]
fn a_release_while_parked_ends_the_turn_for_good() {
    let (_t, _l, mut c) = core();
    take(&mut c, &t("a", 1), &Nobody).unwrap();
    park(&mut c, "a", &Nobody).unwrap();
    assert_eq!(unpark(&mut c, "a", &Nobody).unwrap(), State::Granted);
    park(&mut c, "a", &Nobody).unwrap();
    assert!(release(&mut c, "a", &Nobody).unwrap());
    assert_eq!(unpark(&mut c, "a", &Nobody).unwrap(), State::Gone);
    assert_eq!(list(&mut c, &Nobody).unwrap()["slots"], json!([]));
}

#[test]
fn fewer_slots_than_holders_stops_new_grants_and_evicts_nobody() {
    let (_t, _l, mut c) = core();
    take(&mut c, &t("a", 2), &Nobody).unwrap();
    take(&mut c, &t("b", 2), &Nobody).unwrap();
    assert_eq!(take(&mut c, &t("c", 1), &Nobody).unwrap(), waiting(1));
    assert_eq!(running(&mut c), 2, "nobody was evicted");
    release(&mut c, "a", &Nobody).unwrap();
    assert_eq!(
        take(&mut c, &t("c", 1), &Nobody).unwrap(),
        waiting(1),
        "one running, one slot"
    );
    release(&mut c, "b", &Nobody).unwrap();
    assert_eq!(take(&mut c, &t("c", 1), &Nobody).unwrap(), State::Granted);
}

#[test]
fn no_slots_at_all_is_refused() {
    let (_t, _l, mut c) = core();
    assert!(take(&mut c, &t("a", 0), &Nobody).is_err());
    assert!(take(&mut c, &t("a", -3), &Nobody).is_err());
    assert_eq!(list(&mut c, &Nobody).unwrap()["waiting"], json!([]));
}

#[test]
fn a_take_without_max_uses_the_remembered_one_and_is_refused_when_none_is() {
    let (_t, _l, mut c) = core();
    let bare = |holder| Take {
        max: None,
        ..t(holder, 1)
    };
    assert!(
        take(&mut c, &bare("a"), &Nobody).is_err(),
        "none remembered"
    );
    assert_eq!(list(&mut c, &Nobody).unwrap()["waiting"], json!([]));
    take(&mut c, &t("a", 1), &Nobody).unwrap();
    assert_eq!(take(&mut c, &bare("b"), &Nobody).unwrap(), waiting(1));
}

#[test]
fn an_invited_persons_resume_waits_for_the_invitee_slot_and_the_owner_passes_it() {
    let (_t, _l, mut c) = core();
    take(&mut c, &invitee("g1", 3), &Nobody).unwrap();
    park(&mut c, "g1", &Nobody).unwrap();
    // g2 now holds the invited people's one slot.
    assert_eq!(
        take(&mut c, &invitee("g2", 3), &Nobody).unwrap(),
        State::Granted
    );
    assert_eq!(unpark(&mut c, "g1", &Nobody).unwrap(), waiting(1));
    // There is room (2 of 3), but g1 may not take the invitee slot; the owner's turn goes by.
    assert_eq!(take(&mut c, &t("o", 3), &Nobody).unwrap(), State::Granted);
    assert_eq!(
        take(&mut c, &invitee("g1", 3), &Nobody).unwrap(),
        waiting(1)
    );
    release(&mut c, "g2", &Nobody).unwrap();
    assert_eq!(
        take(&mut c, &invitee("g1", 3), &Nobody).unwrap(),
        State::Granted
    );
}

#[test]
fn two_turns_coming_back_are_served_the_later_one_first() {
    // As the host does: a returning turn goes to the front of the line, so the last to return is first served.
    let (_t, _l, mut c) = core();
    take(&mut c, &t("a", 1), &Nobody).unwrap();
    park(&mut c, "a", &Nobody).unwrap();
    take(&mut c, &t("b", 1), &Nobody).unwrap();
    park(&mut c, "b", &Nobody).unwrap();
    take(&mut c, &t("c", 1), &Nobody).unwrap();
    assert_eq!(unpark(&mut c, "a", &Nobody).unwrap(), waiting(1));
    assert_eq!(unpark(&mut c, "b", &Nobody).unwrap(), waiting(1));
    assert_eq!(take(&mut c, &t("a", 1), &Nobody).unwrap(), waiting(2));
    release(&mut c, "c", &Nobody).unwrap();
    assert_eq!(take(&mut c, &t("b", 1), &Nobody).unwrap(), State::Granted);
    assert_eq!(take(&mut c, &t("a", 1), &Nobody).unwrap(), waiting(1));
}

#[test]
fn a_new_ask_while_in_line_to_come_back_leaves_the_line() {
    let (_t, _l, mut c) = core();
    take(&mut c, &t("a", 1), &Nobody).unwrap();
    park(&mut c, "a", &Nobody).unwrap();
    take(&mut c, &t("b", 1), &Nobody).unwrap();
    assert_eq!(unpark(&mut c, "a", &Nobody).unwrap(), waiting(1));
    assert!(park(&mut c, "a", &Nobody).unwrap());
    assert_eq!(list(&mut c, &Nobody).unwrap()["waiting"], json!([]));
    // b ends: a, waiting on a reply, is not handed a slot it cannot use.
    release(&mut c, "b", &Nobody).unwrap();
    assert_eq!(running(&mut c), 0);
    assert_eq!(take(&mut c, &t("a", 1), &Nobody).unwrap(), State::Parked);
    assert_eq!(unpark(&mut c, "a", &Nobody).unwrap(), State::Granted);
}

/// A table of "processes": pid -> start time. Removing a pid ends it; changing the time is a reused pid.
#[derive(Clone, Default)]
struct Procs(Arc<Mutex<HashMap<u32, String>>>);

impl Procs {
    fn start(&self, pid: u32, started: &str) {
        self.0.lock().unwrap().insert(pid, started.to_owned());
    }
    fn end(&self, pid: u32) {
        self.0.lock().unwrap().remove(&pid);
    }
}

impl Liveness for Procs {
    fn start(&self, pid: u32) -> Option<String> {
        self.0.lock().unwrap().get(&pid).cloned()
    }
    fn gone(&self, pid: u32) -> bool {
        !self.0.lock().unwrap().contains_key(&pid)
    }
}

fn with_pid(holder: &str, max: i64, pid: u32) -> Take<'_> {
    Take {
        pid: Some(pid),
        ..t(holder, max)
    }
}

#[test]
fn the_crash_of_a_holder_frees_its_slot_on_the_next_take() {
    let (_t, _l, mut c) = core();
    let procs = Procs::default();
    procs.start(100, "Mon Nov  3 10:00:00 2026");
    let of = procs.clone();
    assert_eq!(
        take(&mut c, &with_pid("a", 1, 100), &of).unwrap(),
        State::Granted
    );
    assert_eq!(
        take(&mut c, &t("b", 1), &of).unwrap(),
        waiting(1),
        "alive: still held"
    );
    procs.end(100);
    assert_eq!(take(&mut c, &t("b", 1), &of).unwrap(), State::Granted);
    assert!(!release(&mut c, "a", &of).unwrap(), "already freed");
}

#[test]
fn a_reused_pid_is_another_process_and_does_not_keep_the_slot() {
    let (_t, _l, mut c) = core();
    let procs = Procs::default();
    procs.start(100, "Mon Nov  3 10:00:00 2026");
    let of = procs.clone();
    take(&mut c, &with_pid("a", 1, 100), &of).unwrap();
    procs.start(100, "Tue Nov  4 09:00:00 2026");
    assert_eq!(take(&mut c, &t("b", 1), &of).unwrap(), State::Granted);
}

#[test]
fn a_waiter_whose_process_died_loses_its_place_in_line() {
    let (_t, _l, mut c) = core();
    let procs = Procs::default();
    procs.start(7, "s7");
    let of = procs.clone();
    take(&mut c, &t("a", 1), &of).unwrap();
    take(&mut c, &with_pid("dead", 1, 7), &of).unwrap();
    take(&mut c, &t("live", 1), &of).unwrap();
    procs.end(7);
    release(&mut c, "a", &of).unwrap();
    assert_eq!(take(&mut c, &t("live", 1), &of).unwrap(), State::Granted);
    assert_eq!(list(&mut c, &of).unwrap()["waiting"], json!([]));
}

#[test]
fn a_failed_start_time_probe_never_frees_a_process_that_exists() {
    let (_t, _l, mut c) = core();
    // `ps` gives nothing, but the holder (this test process) is there.
    let me = std::process::id();
    take(&mut c, &with_pid("a", 1, me), &NoPs).unwrap();
    assert_eq!(take(&mut c, &t("b", 1), &NoPs).unwrap(), waiting(1));
    assert_eq!(
        list(&mut c, &NoPs).unwrap()["slots"][0]["holder"],
        json!("a")
    );
}

#[cfg(unix)]
#[test]
fn an_unreadable_start_time_does_not_keep_a_process_that_is_gone() {
    let (_t, _l, mut c) = core();
    let mut child = std::process::Command::new("true").spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    assert!(pid_gone(pid), "the child was reaped");
    take(&mut c, &with_pid("a", 1, pid), &NoPs).unwrap();
    // The holder's slot was taken while the pid was gone already: the next call frees it.
    assert_eq!(take(&mut c, &t("b", 1), &NoPs).unwrap(), State::Granted);
    assert!(!release(&mut c, "a", &NoPs).unwrap());
}

#[cfg(unix)]
#[test]
fn the_real_probe_tells_a_live_process_from_a_gone_one() {
    assert!(!pid_gone(std::process::id()));
    assert!(!pid_gone(1), "init exists, even if we may not signal it");
}

#[test]
fn the_gate_survives_a_daemon_restart() {
    let (_t, l, mut c) = core();
    take(&mut c, &t("a", 1), &Nobody).unwrap();
    take(&mut c, &t("b", 1), &Nobody).unwrap();
    take(&mut c, &t("c", 1), &Nobody).unwrap();
    drop(c);
    let mut c = open(&l);
    let seen = list(&mut c, &Nobody).unwrap();
    assert_eq!(seen["max"], json!(1));
    assert_eq!(seen["slots"][0]["holder"], json!("a"));
    assert_eq!(seen["waiting"][0]["holder"], json!("b"));
    assert_eq!(seen["waiting"][1]["holder"], json!("c"));
    release(&mut c, "a", &Nobody).unwrap();
    assert_eq!(take(&mut c, &t("b", 1), &Nobody).unwrap(), State::Granted);
}

/// Checks the rules that must hold after every step, from the list the gate gives.
fn invariants(c: &mut Core, max: i64) {
    let seen = list(c, &Nobody).unwrap();
    let slots = seen["slots"].as_array().unwrap();
    let active = slots.iter().filter(|s| s["state"] == "active").count();
    let guests = slots
        .iter()
        .filter(|s| s["state"] == "active" && s["invitee"] == true)
        .count();
    assert!(
        i64::try_from(active).unwrap() <= max,
        "{active} active, {max} slots: {seen}"
    );
    assert!(i64::try_from(guests).unwrap() <= INVITEE_SLOTS, "{seen}");
    let mut seen_holders = std::collections::HashSet::new();
    for w in seen["waiting"].as_array().unwrap() {
        assert!(
            seen_holders.insert(w["holder"].as_str().unwrap().to_owned()),
            "twice in line: {seen}"
        );
        if w["kind"] == "turn" {
            assert!(
                !slots.iter().any(|s| s["holder"] == w["holder"]),
                "waits and holds: {seen}"
            );
        }
    }
    // Room and a waiter who may take it never coexist for long: after a pump the head of the line is blocked.
    if i64::try_from(active).unwrap() < max {
        for w in seen["waiting"].as_array().unwrap() {
            assert!(
                w["invitee"] == true && guests >= 1,
                "a waiter was left with room free: {seen}"
            );
        }
    }
}

#[test]
fn never_more_holders_than_slots_whatever_the_order_of_calls() {
    // A fixed pseudo-random walk over calls, once per slot count (a count that changes is its own test: nobody is evicted).
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = move |n: u64| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x % n
    };
    let names = ["a", "b", "c", "d", "e", "f"];
    for max in 1..=3_i64 {
        let (_t, _l, mut c) = core();
        for _ in 0..400 {
            let h = names[usize::try_from(next(6)).unwrap()];
            walk_step(&mut c, h, max, &mut next);
            invariants(&mut c, max);
        }
    }
}

fn walk_step(c: &mut Core, h: &str, max: i64, next: &mut impl FnMut(u64) -> u64) {
    match next(5) {
        0 | 1 => {
            let who = Take {
                invitee: next(3) == 0,
                ..t(h, max)
            };
            take(c, &who, &Nobody).unwrap();
        }
        2 => {
            release(c, h, &Nobody).unwrap();
        }
        3 => {
            park(c, h, &Nobody).unwrap();
        }
        _ => {
            unpark(c, h, &Nobody).unwrap();
        }
    }
}

#[test]
fn threads_taking_and_releasing_never_hold_more_than_the_slots() {
    const SLOTS: usize = 3;
    let (_t, _l, c) = core();
    let c = Arc::new(Mutex::new(c));
    let inflight = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(AtomicUsize::new(0));
    let workers: Vec<_> = (0..8)
        .map(|n| {
            let (c, inflight, done) = (c.clone(), inflight.clone(), done.clone());
            std::thread::spawn(move || {
                let me = format!("w{n}");
                for _ in 0..15 {
                    loop {
                        let state = {
                            let mut core = c.lock().unwrap();
                            take(&mut core, &t(&me, 3), &Nobody).unwrap()
                        };
                        if state == State::Granted {
                            break;
                        }
                        std::thread::yield_now();
                    }
                    let now = inflight.fetch_add(1, Ordering::SeqCst) + 1;
                    assert!(now <= SLOTS, "{now} turns in flight, {SLOTS} slots");
                    std::thread::yield_now();
                    inflight.fetch_sub(1, Ordering::SeqCst);
                    release(&mut c.lock().unwrap(), &me, &Nobody).unwrap();
                    done.fetch_add(1, Ordering::SeqCst);
                }
            })
        })
        .collect();
    for w in workers {
        w.join().expect("worker");
    }
    assert_eq!(done.load(Ordering::SeqCst), 8 * 15);
    let seen = list(&mut c.lock().unwrap(), &Nobody).unwrap();
    assert_eq!(seen["slots"], json!([]));
    assert_eq!(seen["waiting"], json!([]));
}

fn answer(c: &mut Core, method: &str, params: &Value) -> Value {
    call(c, method, params, &Nobody).expect(method)
}

#[test]
fn the_rows_answer_in_json_and_refuse_bad_params() {
    let (_t, _l, mut c) = core();
    let got = answer(
        &mut c,
        "gate.take",
        &json!({"holder": "a", "max": 1, "pid": 4242}),
    );
    assert_eq!(got, json!({"state": "granted"}));
    // The remembered max is used when a take leaves it out.
    let got = answer(
        &mut c,
        "gate.take",
        &json!({"holder": "b", "pid": 4243, "rt": "bot:b"}),
    );
    assert_eq!(got, json!({"state": "waiting", "position": 1}));
    assert_eq!(
        answer(&mut c, "gate.park", &json!({"holder": "a"})),
        json!({"parked": true})
    );
    assert_eq!(
        answer(&mut c, "gate.unpark", &json!({"holder": "a"})),
        json!({"state": "waiting", "position": 1})
    );
    assert_eq!(
        answer(&mut c, "gate.release", &json!({"holder": "zz"})),
        json!({"released": false})
    );
    let seen = answer(&mut c, "gate.list", &json!({}));
    let b = seen["slots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["holder"] == "b")
        .unwrap();
    assert_eq!(b["rt"], json!("bot:b"));
    for bad in [
        json!({"holder": "x", "max": 1}),
        json!({"holder": "x", "max": 1, "pid": 0}),
        json!({"holder": "x", "max": 1, "pid": 1}),
        json!({"holder": "x", "max": 1, "pid": -5}),
        json!({"holder": "", "max": 1, "pid": 4242}),
        json!({"holder": "x", "max": 0, "pid": 4242}),
        json!({"holder": "x", "max": -1, "pid": 4242}),
    ] {
        assert!(call(&mut c, "gate.take", &bad, &Nobody).is_err(), "{bad}");
    }
}

#[test]
fn a_row_take_with_no_remembered_max_is_refused() {
    let (_t, _l, mut c) = core();
    let bad = json!({"holder": "x", "pid": 4242});
    assert!(call(&mut c, "gate.take", &bad, &Nobody).is_err());
}

#[test]
fn the_rows_are_in_the_table_for_the_owner_only() {
    for name in [
        "gate.list",
        "gate.take",
        "gate.release",
        "gate.park",
        "gate.unpark",
    ] {
        let row = kith_api::method(name).expect(name);
        assert!(row.allows(kith_api::Principal::Owner), "{name}");
        assert!(!row.allows(kith_api::Principal::RunToken), "{name}");
    }
}
