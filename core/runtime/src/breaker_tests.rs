use std::path::Path;
use std::time::{Duration, Instant};

use super::{Breakers, Gate};

fn s(n: u64) -> Duration {
    Duration::from_secs(n)
}

#[test]
fn opens_on_a_trip_holds_a_window_and_lets_one_test_call_through() {
    let b = Breakers::default();
    let core = Path::new("/a");
    let t0 = Instant::now();
    assert_eq!(b.gate(core, t0), Gate::Pass);
    b.end(core, true, t0);
    assert_eq!(b.gate(core, t0 + Duration::from_millis(999)), Gate::Hold);
    assert_eq!(b.gate(core, t0 + s(1)), Gate::Probe);
    // One test call per window: the rest hold while it is out.
    assert_eq!(b.gate(core, t0 + s(1)), Gate::Hold);
    assert_eq!(b.gate(core, t0 + s(5)), Gate::Hold);
    // It failed: the window doubles.
    b.end(core, true, t0 + s(2));
    assert_eq!(b.gate(core, t0 + s(3)), Gate::Hold);
    assert_eq!(b.gate(core, t0 + s(4)), Gate::Probe);
}

#[test]
fn an_answer_closes_it_and_the_wait_starts_over() {
    let b = Breakers::default();
    let core = Path::new("/a");
    let t0 = Instant::now();
    b.end(core, true, t0);
    assert_eq!(b.gate(core, t0 + s(1)), Gate::Probe);
    b.end(core, false, t0 + s(1));
    assert_eq!(b.gate(core, t0 + s(1)), Gate::Pass);
    b.end(core, true, t0 + s(2));
    assert_eq!(b.gate(core, t0 + s(3)), Gate::Probe, "1 s again, not 2");
}

#[test]
fn the_wait_doubles_up_to_thirty_seconds() {
    let b = Breakers::default();
    let core = Path::new("/a");
    let mut now = Instant::now();
    let mut waits = vec![];
    for _ in 0..8 {
        b.end(core, true, now);
        let mut w = 0;
        while b.gate(core, now + s(w)) == Gate::Hold {
            w += 1;
        }
        waits.push(w);
        now += s(w);
    }
    assert_eq!(waits, [1, 2, 4, 8, 16, 30, 30, 30]);
}

#[test]
fn a_late_trip_from_a_call_that_passed_before_it_opened_does_not_lengthen_the_window() {
    let b = Breakers::default();
    let core = Path::new("/a");
    let t0 = Instant::now();
    b.end(core, true, t0);
    b.end(core, true, t0 + Duration::from_millis(10));
    assert_eq!(b.gate(core, t0 + s(1)), Gate::Probe);
}

#[test]
fn each_core_has_its_own() {
    let b = Breakers::default();
    let t0 = Instant::now();
    b.end(Path::new("/a"), true, t0);
    assert_eq!(b.gate(Path::new("/b"), t0), Gate::Pass);
}
