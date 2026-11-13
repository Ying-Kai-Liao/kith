#![allow(clippy::unwrap_used)]
//! The four edits and the checks on their input, held to what `kernel/lib/kith_accounts.py` does (the difftest's
//! `account-state` cases hold the whole commands to it).

use super::Map;
use super::change::{Change, OUT_FOR, apply, parse_until, reason_ok, validate_usage};
use serde_json::{Value, json};

fn accts(v: &Value) -> Map {
    v.as_object().unwrap().clone()
}

fn out(id: &str, until: i64, extend_only: bool) -> Change {
    Change::Out {
        id: id.to_owned(),
        until: Some(until),
        reason: "rate_limit".to_owned(),
        extend_only,
    }
}

#[test]
fn out_sets_the_mark_and_keeps_since_while_still_out() {
    let mut a = Map::new();
    apply(&mut a, &out("claude:work", 2000, false), 1000);
    assert_eq!(
        Value::Object(a.clone()),
        json!({"claude:work": {"since": 1000, "out_until": 2000, "reason": "rate_limit"}})
    );
    // Out again, later: `since` stays while it was still out.
    apply(&mut a, &out("claude:work", 3000, false), 1500);
    assert_eq!(a["claude:work"]["since"], 1000);
    assert_eq!(a["claude:work"]["out_until"], 3000);
    // Out after the old mark ran out: a new `since`.
    apply(&mut a, &out("claude:work", 9000, false), 5000);
    assert_eq!(a["claude:work"]["since"], 5000);
}

#[test]
fn extend_only_leaves_a_longer_mark_but_still_makes_the_entry() {
    let mut a = accts(&json!({"claude:work": {"out_until": 5000, "reason": "manual", "since": 1}}));
    apply(&mut a, &out("claude:work", 3000, true), 1000);
    assert_eq!(
        a["claude:work"],
        json!({"out_until": 5000, "reason": "manual", "since": 1})
    );
    // The Python makes the entry before it looks, so an unknown account gets one (here with the mark, as 0 < 3000).
    let mut b = Map::new();
    apply(&mut b, &out("codex:b", 3000, true), 1000);
    assert_eq!(b["codex:b"]["out_until"], 3000);
    let mut c = accts(&json!({"codex:b": {"out_until": 9000}}));
    apply(&mut c, &out("codex:b", 3000, true), 1000);
    assert_eq!(c["codex:b"], json!({"out_until": 9000}));
}

#[test]
fn no_time_means_an_hour_from_now() {
    let mut a = Map::new();
    let c = Change::Out {
        id: "claude".to_owned(),
        until: None,
        reason: "x".to_owned(),
        extend_only: true,
    };
    apply(&mut a, &c, 100);
    assert_eq!(a["claude"]["out_until"], 100 + OUT_FOR);
}

#[test]
fn clear_forgets_the_mark_and_notes_the_time_and_the_hosts_mark() {
    let mut a = accts(
        &json!({"claude:work": {"out_until": 5000, "reason": "r", "since": 1, "last_started": 7}}),
    );
    apply(
        &mut a,
        &Change::Clear {
            id: "claude:work".to_owned(),
            host_until: 4000,
        },
        2000,
    );
    assert_eq!(
        a["claude:work"],
        json!({"last_started": 7, "cleared_at": 2000, "cleared_host_until": 4000})
    );
}

#[test]
fn started_and_usage_set_their_key() {
    let mut a = Map::new();
    apply(
        &mut a,
        &Change::Started {
            id: "claude".to_owned(),
        },
        42,
    );
    assert_eq!(a["claude"]["last_started"], 42);
    let usage = json!({"read_at": 1, "windows": [{"name": "w", "percent": 5.0}]});
    apply(
        &mut a,
        &Change::Usage {
            id: "claude".to_owned(),
            usage: usage.clone(),
        },
        43,
    );
    assert_eq!(a["claude"]["usage"], usage);
    assert_eq!(a["claude"]["last_started"], 42);
}

#[test]
fn a_reading_is_checked_and_stored_whole() {
    let ok = validate_usage(
        &json!({"windows": [{"name": "five_hour", "percent": 71, "resets_at": 1_900_000_000}, {"name": "weekly", "percent": 20.5}]}),
        1234,
    )
    .unwrap();
    assert_eq!(
        ok,
        json!({"read_at": 1234, "windows": [
            {"name": "five_hour", "percent": 71.0, "resets_at": 1_900_000_000},
            {"name": "weekly", "percent": 20.5}]})
    );
    for (bad, why) in [
        (json!([]), "a usage reading is"),
        (json!({"windows": []}), "1 to 8"),
        (
            json!({"windows": [{"name": "w", "percent": 101}]}),
            "0 to 100",
        ),
        (
            json!({"windows": [{"name": "w", "percent": true}]}),
            "0 to 100",
        ),
        (
            json!({"windows": [{"name": "a/b", "percent": 1}]}),
            "window name",
        ),
        (
            json!({"windows": [{"name": "w", "percent": 1, "resets_at": 0}]}),
            "resets_at",
        ),
        (
            json!({"windows": [{"name": "w", "percent": 1, "x": 1}]}),
            "a window is",
        ),
        (
            json!({"windows": [{"name": "w", "percent": 1}], "x": 1}),
            "a usage reading is",
        ),
        (
            json!({"windows": [{"name": "w", "percent": 1}], "read_at": -3}),
            "read_at",
        ),
    ] {
        let e = validate_usage(&bad, 1).unwrap_err();
        assert!(e.contains(why), "{bad}: {e}");
    }
}

#[test]
fn reasons_are_short_lower_case_words() {
    assert!(reason_ok("rate_limit"));
    assert!(reason_ok("weekly limit"));
    assert!(reason_ok("9am"));
    assert!(!reason_ok("Bad"));
    assert!(!reason_ok(""));
    assert!(!reason_ok(" x"));
    assert!(!reason_ok(&"a".repeat(41)));
    assert!(reason_ok(&"a".repeat(40)));
}

#[test]
fn until_is_a_time_a_span_or_a_local_date() {
    assert_eq!(parse_until("4102444800", 5).unwrap(), 4_102_444_800);
    assert_eq!(parse_until("90m", 1000).unwrap(), 1000 + 5400);
    assert_eq!(parse_until("+30s", 1000).unwrap(), 1030);
    assert_eq!(parse_until("2 h", 1000).unwrap(), 1000 + 7200);
    assert_eq!(parse_until(" 1d ", 0).unwrap(), 86_400);
    for ok in ["2026-11-04 09:30", "2026-11-04T09:30:15", "2026-11-04"] {
        assert!(parse_until(ok, 0).unwrap() > 1_700_000_000, "{ok}");
    }
    for bad in ["yesterday", "5", "", "h", "12 parsecs", "2026-13-01"] {
        let e = parse_until(bad, 0).unwrap_err();
        assert!(e.starts_with(&format!("'{bad}' is not a time")), "{e}");
    }
}
