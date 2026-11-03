#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::cell::Cell;
use std::sync::{Arc, Mutex};

use serde_json::json;

use super::{
    Clock, LEASE_MS, call, changed, drop_lease, held_in, lease_held_for_non_owner, link, mark,
    pulled, renew, restart, status, unlink,
};
use crate::db::Core;
use crate::layout::Layout;
use crate::tests::{machine, open};

fn core() -> (tempfile::TempDir, Layout, Core) {
    let (t, m) = machine();
    let l = Layout::new(&m);
    let c = open(&l);
    (t, l, c)
}

/// A clock the test moves by hand.
struct At(Cell<i64>);

impl Clock for At {
    fn now_ms(&self) -> i64 {
        self.0.get()
    }
}

fn clock(ms: i64) -> At {
    At(Cell::new(ms))
}

/// A linked Mac that pulled at `clk`'s time: held for `LEASE_MS`.
fn synced(c: &mut Core, clk: &At) {
    link(c, clk).unwrap();
    let m = mark(c).unwrap();
    assert!(pulled(c, clk, m, LEASE_MS).unwrap());
}

fn held(c: &mut Core, clk: &At) -> bool {
    lease_held_for_non_owner(c, clk)
}

#[test]
fn a_mac_never_told_about_the_cloud_is_not_held() {
    let (_t, _l, mut c) = core();
    let clk = clock(1_000);
    assert!(!held(&mut c, &clk), "unknown fails closed");
    // Nothing about the lease makes unknown held: not a drop, a renew, a change, a pull.
    drop_lease(&mut c, &clk).unwrap();
    assert!(!renew(&mut c, &clk, LEASE_MS).unwrap());
    changed(&mut c, &clk).unwrap();
    assert!(
        !pulled(&mut c, &clk, 1, LEASE_MS).unwrap(),
        "a pull needs a linked cloud"
    );
    assert!(!held(&mut c, &clk));
    assert_eq!(status(&mut c, &clk).unwrap()["cloud"], "unknown");
}

#[test]
fn linking_alone_is_not_held_until_a_pull_succeeds() {
    let (_t, _l, mut c) = core();
    let clk = clock(1_000);
    link(&mut c, &clk).unwrap();
    assert!(!held(&mut c, &clk));
    let m = mark(&mut c).unwrap();
    assert!(pulled(&mut c, &clk, m, LEASE_MS).unwrap());
    assert!(held(&mut c, &clk));
    // Linking again keeps the lease.
    link(&mut c, &clk).unwrap();
    assert!(held(&mut c, &clk));
}

#[test]
fn the_lease_runs_out_by_itself() {
    let (_t, _l, mut c) = core();
    let clk = clock(10_000);
    synced(&mut c, &clk);
    clk.0.set(10_000 + LEASE_MS - 1);
    assert!(held(&mut c, &clk));
    clk.0.set(10_000 + LEASE_MS);
    assert!(!held(&mut c, &clk), "until is the first instant it is gone");
    let s = status(&mut c, &clk).unwrap();
    assert_eq!(s["held"], false);
    assert_eq!(s["until"], 10_000 + LEASE_MS);
}

#[test]
fn a_renew_runs_a_held_lease_out_again() {
    let (_t, _l, mut c) = core();
    let clk = clock(0);
    synced(&mut c, &clk);
    clk.0.set(100_000);
    assert!(renew(&mut c, &clk, LEASE_MS).unwrap());
    clk.0.set(100_000 + LEASE_MS - 1);
    assert!(held(&mut c, &clk), "renewed from 100 s");
    clk.0.set(100_000 + LEASE_MS);
    assert!(!held(&mut c, &clk));
}

#[test]
fn a_renew_never_shortens_a_longer_lease() {
    let (_t, _l, mut c) = core();
    let clk = clock(0);
    link(&mut c, &clk).unwrap();
    let m = mark(&mut c).unwrap();
    assert!(pulled(&mut c, &clk, m, 600_000).unwrap());
    assert!(renew(&mut c, &clk, 1_000).unwrap());
    clk.0.set(500_000);
    assert!(held(&mut c, &clk));
}

#[test]
fn a_renew_after_expiry_or_loss_does_not_bring_the_lease_back() {
    let (_t, _l, mut c) = core();
    let clk = clock(0);
    synced(&mut c, &clk);
    // Expired (the Mac slept): a late ping is not a pull.
    clk.0.set(LEASE_MS + 5_000);
    assert!(!renew(&mut c, &clk, LEASE_MS).unwrap());
    assert!(!held(&mut c, &clk));
    // Lost mid-renewal: the cloud changed after the pull, before the ping.
    clk.0.set(LEASE_MS + 6_000);
    let m = mark(&mut c).unwrap();
    assert!(pulled(&mut c, &clk, m, LEASE_MS).unwrap());
    changed(&mut c, &clk).unwrap();
    assert!(!renew(&mut c, &clk, LEASE_MS).unwrap());
    assert!(!held(&mut c, &clk));
}

#[test]
fn a_cloud_change_ends_the_lease_and_a_pull_that_started_before_it_does_not_bring_it_back() {
    let (_t, _l, mut c) = core();
    let clk = clock(0);
    synced(&mut c, &clk);
    let before = mark(&mut c).unwrap();
    let after = changed(&mut c, &clk).unwrap();
    assert_eq!(after, before + 1);
    assert!(!held(&mut c, &clk), "ends the moment the cloud changes");
    assert!(
        !pulled(&mut c, &clk, before, LEASE_MS).unwrap(),
        "a pull that read the state before the change is stale"
    );
    assert!(!held(&mut c, &clk));
    assert!(pulled(&mut c, &clk, after, LEASE_MS).unwrap());
    assert!(held(&mut c, &clk));
}

#[test]
fn a_second_pull_just_runs_the_lease_from_the_later_time() {
    let (_t, _l, mut c) = core();
    let clk = clock(0);
    synced(&mut c, &clk);
    clk.0.set(50_000);
    let m = mark(&mut c).unwrap();
    assert!(pulled(&mut c, &clk, m, LEASE_MS).unwrap());
    clk.0.set(50_000 + LEASE_MS - 1);
    assert!(held(&mut c, &clk));
}

#[test]
fn a_dropped_connection_ends_the_lease_and_keeps_the_change_count() {
    let (_t, _l, mut c) = core();
    let clk = clock(0);
    synced(&mut c, &clk);
    let m = changed(&mut c, &clk).unwrap();
    drop_lease(&mut c, &clk).unwrap();
    assert!(!held(&mut c, &clk));
    assert_eq!(mark(&mut c).unwrap(), m);
    drop_lease(&mut c, &clk).unwrap();
    assert!(!held(&mut c, &clk), "dropping twice is harmless");
}

#[test]
fn a_cloud_that_is_off_waits_for_nothing_and_a_relink_starts_unsynced() {
    let (_t, _l, mut c) = core();
    let clk = clock(0);
    synced(&mut c, &clk);
    unlink(&mut c, &clk).unwrap();
    assert!(
        held(&mut c, &clk),
        "off on purpose: nothing waits on the cloud"
    );
    let s = status(&mut c, &clk).unwrap();
    assert_eq!(
        (s["cloud"].as_str(), s["synced"].as_bool()),
        (Some("off"), Some(false))
    );
    let m = mark(&mut c).unwrap();
    assert!(
        !pulled(&mut c, &clk, m, LEASE_MS).unwrap(),
        "no pull to acquire while off"
    );
    link(&mut c, &clk).unwrap();
    assert!(
        !held(&mut c, &clk),
        "the old lease did not survive being off"
    );
}

#[test]
fn a_word_from_the_cloud_turns_an_off_cloud_on_and_not_held() {
    for event in 0..3 {
        let (_t, _l, mut c) = core();
        let clk = clock(0);
        unlink(&mut c, &clk).unwrap();
        assert!(held(&mut c, &clk));
        match event {
            0 => {
                changed(&mut c, &clk).unwrap();
            }
            1 => drop_lease(&mut c, &clk).unwrap(),
            _ => {
                let m = mark(&mut c).unwrap();
                assert!(
                    !pulled(&mut c, &clk, m, LEASE_MS).unwrap(),
                    "not held on this call"
                );
            }
        }
        assert!(
            !held(&mut c, &clk),
            "event {event}: the stale off no longer passes"
        );
        assert_eq!(status(&mut c, &clk).unwrap()["cloud"], "linked");
    }
}

#[test]
fn a_restart_leaves_an_off_cloud_off() {
    let (_t, _l, mut c) = core();
    let clk = clock(0);
    unlink(&mut c, &clk).unwrap();
    restart(&mut c, &clk).unwrap();
    assert!(held(&mut c, &clk));
}

#[test]
fn a_clock_stepping_back_never_extends_a_lease() {
    let (_t, _l, mut c) = core();
    let clk = clock(1_000_000);
    synced(&mut c, &clk);
    // An hour back: the old reading would make the lease look young again.
    clk.0.set(1_000_000 - 3_600_000);
    assert!(!held(&mut c, &clk), "behind its last reading: fails closed");
    let m = mark(&mut c).unwrap();
    assert!(!pulled(&mut c, &clk, m, LEASE_MS).unwrap());
    assert!(!renew(&mut c, &clk, LEASE_MS).unwrap());
    // A change made while the clock is behind still ends things, and does not lower the reading.
    drop_lease(&mut c, &clk).unwrap();
    clk.0.set(1_000_000 + 1);
    assert!(!held(&mut c, &clk));
    // Back at the reading, a pull acquires normally.
    assert!(pulled(&mut c, &clk, m, LEASE_MS).unwrap());
    assert!(held(&mut c, &clk));
}

#[test]
fn a_small_step_back_after_a_reading_does_not_hold_either() {
    let (_t, _l, mut c) = core();
    let clk = clock(5_000);
    synced(&mut c, &clk);
    clk.0.set(4_999);
    assert!(!held(&mut c, &clk));
    clk.0.set(5_000);
    assert!(held(&mut c, &clk));
}

#[test]
fn the_lease_survives_a_reopen_and_expires_by_the_clock_and_a_restart_ends_it() {
    let (_t, l, mut c) = core();
    let clk = clock(0);
    synced(&mut c, &clk);
    drop(c);
    let mut c = open(&l);
    clk.0.set(LEASE_MS - 1);
    assert!(held(&mut c, &clk), "the row is core.db's, not memory");
    clk.0.set(LEASE_MS);
    assert!(
        !held(&mut c, &clk),
        "and it expires by the clock after a reopen"
    );

    // The daemon starting again: the relay connection of the last run is gone.
    clk.0.set(LEASE_MS + 10);
    let m = mark(&mut c).unwrap();
    assert!(pulled(&mut c, &clk, m, LEASE_MS).unwrap());
    drop(c);
    let mut c = open(&l);
    restart(&mut c, &clk).unwrap();
    assert!(
        !held(&mut c, &clk),
        "lost across the restart, long before it ran out"
    );
    let m = mark(&mut c).unwrap();
    assert!(
        pulled(&mut c, &clk, m, LEASE_MS).unwrap(),
        "and a pull gets it back"
    );
    assert!(held(&mut c, &clk));
}

#[test]
fn the_claim_path_reads_it_inside_its_own_transaction() {
    let (_t, _l, mut c) = core();
    let clk = clock(0);
    synced(&mut c, &clk);
    let tx = c.claim_tx().unwrap();
    assert!(held_in(&tx, 1).unwrap());
    assert!(!held_in(&tx, LEASE_MS).unwrap());
}

#[test]
fn rows_refuse_nonsense_and_answer_every_state() {
    let (_t, _l, mut c) = core();
    let clk = clock(1_000);
    let mut ask = |m: &str, p: serde_json::Value| call(&mut c, m, &p, &clk);
    assert_eq!(
        ask("syncLease.status", json!({})).unwrap()["cloud"],
        "unknown"
    );
    ask("syncLease.link", json!({})).unwrap();
    let m = ask("syncLease.mark", json!({})).unwrap()["mark"].clone();
    for ms in [0, -5, 3_600_001] {
        assert!(
            ask("syncLease.pulled", json!({"mark": m, "ms": ms})).is_err(),
            "{ms}"
        );
        assert!(ask("syncLease.renew", json!({"ms": ms})).is_err(), "{ms}");
    }
    assert!(
        ask("syncLease.pulled", json!({"ms": 10})).is_err(),
        "mark is required"
    );
    assert!(ask("syncLease.pulled", json!({"mark": -1})).is_err());
    assert_eq!(
        ask("syncLease.pulled", json!({"mark": m})).unwrap()["held"],
        true
    );
    assert_eq!(ask("syncLease.status", json!({})).unwrap()["held"], true);
    assert_eq!(
        ask("syncLease.renew", json!({"ms": 5})).unwrap()["renewed"],
        true
    );
    let m2 = ask("syncLease.changed", json!({})).unwrap()["mark"].clone();
    assert_eq!(m2, json!(m.as_i64().unwrap() + 1));
    assert_eq!(
        ask("syncLease.pulled", json!({"mark": m})).unwrap()["held"],
        false
    );
    assert_eq!(ask("syncLease.renew", json!({})).unwrap()["renewed"], false);
    assert_eq!(ask("syncLease.drop", json!({})).unwrap()["held"], false);
    assert_eq!(ask("syncLease.unlink", json!({})).unwrap()["cloud"], "off");
    assert!(ask("syncLease.nope", json!({})).is_err());
}

/// The rules that must hold after every step, whatever the order of calls: held implies linked, synced and unexpired
/// (or the cloud off), so a lease that is not synced or has run out is never held; and the lease never reaches past
/// the longest run any acquire or renew asked for from the latest reading.
fn invariants(c: &mut Core, clk: &At, longest: i64, latest: i64) {
    let s = status(c, clk).unwrap();
    let now = clk.0.get();
    let held = lease_held_for_non_owner(c, clk);
    assert_eq!(held, s["held"].as_bool().unwrap());
    if held && s["cloud"] == "linked" {
        assert!(s["synced"].as_bool().unwrap(), "held implies synced");
        assert!(
            now < s["until"].as_i64().unwrap(),
            "held implies not expired"
        );
        assert!(now >= latest, "held implies the clock has not stepped back");
        assert!(
            s["until"].as_i64().unwrap() <= latest + longest,
            "never reaches past the longest ask"
        );
    }
    if s["cloud"] == "unknown" {
        assert!(!held, "unknown is never held");
    }
    if !s["synced"].as_bool().unwrap() && s["cloud"] == "linked" {
        assert!(!held, "not synced is never held");
    }
}

#[test]
fn held_implies_not_expired_whatever_the_order_of_calls_and_clock_jumps() {
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = move |n: u64| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x % n
    };
    for _ in 0..4 {
        let (_t, _l, mut c) = core();
        let clk = clock(1_000_000);
        let mut longest = 0_i64;
        for _ in 0..400 {
            match next(10) {
                0 => link(&mut c, &clk).unwrap(),
                1 if next(8) == 0 => unlink(&mut c, &clk).unwrap(),
                2 => {
                    changed(&mut c, &clk).unwrap();
                }
                3 => drop_lease(&mut c, &clk).unwrap(),
                4 | 5 => {
                    let m = if next(3) == 0 {
                        0
                    } else {
                        mark(&mut c).unwrap()
                    };
                    let ms = 1 + i64::try_from(next(200_000)).unwrap();
                    if pulled(&mut c, &clk, m, ms).unwrap() {
                        longest = longest.max(ms);
                    }
                }
                6 => {
                    let ms = 1 + i64::try_from(next(200_000)).unwrap();
                    if renew(&mut c, &clk, ms).unwrap() {
                        longest = longest.max(ms);
                    }
                }
                7 => restart(&mut c, &clk).unwrap(),
                // The clock moves: mostly forward, sometimes back.
                _ => {
                    let step = i64::try_from(next(150_000)).unwrap();
                    let now = if next(4) == 0 {
                        clk.0.get() - step
                    } else {
                        clk.0.get() + step
                    };
                    clk.0.set(now.max(0));
                }
            }
            // The latest reading any change saw, from the row itself.
            let latest = row_at(&mut c);
            // `longest` can only grow, so it bounds `until - at` for every lease ever granted.
            invariants(&mut c, &clk, longest, latest);
        }
    }
}

fn row_at(c: &mut Core) -> i64 {
    let tx = c.claim_tx().unwrap();
    tx.query_row("SELECT at FROM sync_lease WHERE id = 1", [], |r| r.get(0))
        .unwrap_or(0)
}

#[test]
fn threads_pulling_dropping_and_renewing_never_leave_a_half_lease() {
    let (_t, _l, mut c) = core();
    link(&mut c, &clock(1_000)).unwrap();
    let c = Arc::new(Mutex::new(c));
    let workers: Vec<_> = (0..6)
        .map(|n| {
            let c = c.clone();
            std::thread::spawn(move || {
                let clk = clock(1_000);
                for i in 0..40 {
                    let mut g = c.lock().unwrap();
                    match (n + i) % 4 {
                        0 => {
                            let m = mark(&mut g).unwrap();
                            pulled(&mut g, &clk, m, LEASE_MS).unwrap();
                        }
                        1 => {
                            renew(&mut g, &clk, LEASE_MS).unwrap();
                        }
                        2 => {
                            changed(&mut g, &clk).unwrap();
                        }
                        _ => drop_lease(&mut g, &clk).unwrap(),
                    }
                    let s = status(&mut g, &clk).unwrap();
                    if s["held"] == true {
                        assert_eq!(s["synced"], true);
                        assert!(s["until"].as_i64().unwrap() > 1_000);
                    }
                }
            })
        })
        .collect();
    for w in workers {
        w.join().unwrap();
    }
}
