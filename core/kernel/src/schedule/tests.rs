#![allow(clippy::unwrap_used)]

use chrono::{DateTime, Utc};
use serde_json::json;

use super::*;

fn ms(rfc: &str) -> i64 {
    DateTime::parse_from_rfc3339(rfc)
        .unwrap()
        .timestamp_millis()
}

fn cron(expr: &str, tz: &str) -> Trigger {
    Trigger::Cron {
        expr: expr.into(),
        tz: Zone::parse(tz).unwrap(),
    }
}

#[test]
fn serde_shapes_are_the_documented_ones() {
    for (value, trigger) in [
        (
            json!({"type":"cron","expr":"0 9 * * 1-5","tz":"local"}),
            cron("0 9 * * 1-5", "local"),
        ),
        (
            json!({"type":"cron","expr":"@daily","tz":"Asia/Taipei"}),
            cron("@daily", "Asia/Taipei"),
        ),
        (
            json!({"type":"interval","every":900}),
            Trigger::Interval { every: 900 },
        ),
        (
            json!({"type":"once","at":1_793_000_000_000_i64}),
            Trigger::Once {
                at: 1_793_000_000_000,
            },
        ),
        (json!({"type":"webhook"}), Trigger::Webhook),
        (
            json!({"type":"event","source":"github","event":"push","filters":{"repo":"o/r"}}),
            Trigger::Event {
                source: "github".into(),
                event: "push".into(),
                filters: [("repo".to_owned(), "o/r".to_owned())].into(),
            },
        ),
    ] {
        assert_eq!(serde_json::to_value(&trigger).unwrap(), value);
        assert_eq!(serde_json::from_value::<Trigger>(value).unwrap(), trigger);
    }
    // filters may be left out
    let t: Trigger =
        serde_json::from_value(json!({"type":"event","source":"slack","event":"*"})).unwrap();
    t.validate().unwrap();
}

#[test]
fn an_unknown_zone_is_refused() {
    let bad = json!({"type":"cron","expr":"* * * * *","tz":"Mars/Olympus"});
    assert!(serde_json::from_value::<Trigger>(bad).is_err());
    assert!(
        Zone::parse("Local").is_err(),
        "only the lower-case word means local"
    );
}

#[test]
fn validate_refuses_what_it_should() {
    assert!(cron("* * * * *", "UTC").validate().is_ok());
    assert!(
        Trigger::Cron {
            expr: "* * * * * * * *".into(),
            tz: Zone::Local
        }
        .validate()
        .is_err()
    );
    assert!(Trigger::Interval { every: 0 }.validate().is_err());
    assert!(Trigger::Interval { every: 31_536_001 }.validate().is_err());
    assert!(Trigger::Interval { every: 1 }.validate().is_ok());
    // Once in the past is a valid shape that never fires
    assert!(Trigger::Once { at: 1 }.validate().is_ok());
    let event = |source: &str| Trigger::Event {
        source: source.into(),
        event: "x".into(),
        filters: BTreeMap::new(),
    };
    assert!(event("github").validate().is_ok());
    assert!(event("carrier-pigeon").validate().is_err());
}

#[test]
fn iana_zones_read_the_clock_there() {
    // Sunday 14:20 in Taipei (UTC+8), the host's own test
    let t = cron("20 14 * * 0", "Asia/Taipei");
    assert_eq!(
        t.next_after_in(ms("2026-09-26T12:00:00Z"), &Utc),
        Some(ms("2026-09-27T06:20:00Z"))
    );
}

#[test]
fn local_follows_the_zone_it_is_given() {
    let plus8 = chrono::FixedOffset::east_opt(8 * 3600).unwrap();
    let t = cron("0 9 * * *", "local");
    assert_eq!(
        t.next_after_in(ms("2026-11-04T00:00:00Z"), &plus8),
        Some(ms("2026-11-04T01:00:00Z"))
    );
    assert_eq!(
        t.next_after_in(ms("2026-11-04T00:00:00Z"), &Utc),
        Some(ms("2026-11-04T09:00:00Z"))
    );
}

#[test]
fn impossible_and_far_dates() {
    let after = ms("2026-03-01T00:00:00Z");
    assert_eq!(cron("0 0 31 2 *", "UTC").next_after_in(after, &Utc), None);
    assert_eq!(cron("5-3 * * * *", "UTC").next_after_in(after, &Utc), None);
    // no 366-day horizon: the next Feb 29 is in 2028
    assert_eq!(
        cron("0 0 29 2 *", "UTC").next_after_in(after, &Utc),
        Some(ms("2028-02-29T00:00:00Z"))
    );
}

#[test]
fn dom_and_dow_both_restricted_fire_on_either() {
    // 2026-11-04 is a Wednesday; the 1st of December is a Tuesday
    let t = cron("0 0 1 * 1", "UTC");
    assert_eq!(
        t.next_after_in(ms("2026-11-04T00:00:00Z"), &Utc),
        Some(ms("2026-11-09T00:00:00Z")),
        "the next Monday comes before the 1st"
    );
    // a day-of-week that names every day is still restricted
    let all = cron("0 0 15 * 0-6", "UTC");
    assert_eq!(
        all.next_after_in(ms("2026-11-04T00:00:00Z"), &Utc),
        Some(ms("2026-11-05T00:00:00Z"))
    );
}

#[test]
fn interval_and_once_round_up_to_the_tick() {
    let after = ms("2026-11-04T10:00:10Z");
    assert_eq!(
        Trigger::Interval { every: 1 }.next_after_in(after, &Utc),
        Some(ms("2026-11-04T10:01:00Z"))
    );
    assert_eq!(
        Trigger::Interval { every: 60 }.next_after_in(ms("2026-11-04T10:00:00Z"), &Utc),
        Some(ms("2026-11-04T10:01:00Z"))
    );
    let once = Trigger::Once {
        at: ms("2026-11-04T10:05:30Z"),
    };
    assert_eq!(
        once.next_after_in(after, &Utc),
        Some(ms("2026-11-04T10:06:00Z"))
    );
    assert_eq!(once.next_after_in(ms("2026-11-04T10:05:30Z"), &Utc), None);
    assert_eq!(Trigger::Webhook.next_after_in(after, &Utc), None);
}

#[test]
fn next_strictly_increases() {
    let after = ms("2026-03-07T22:17:41Z");
    for tz in [
        "UTC",
        "America/New_York",
        "Europe/London",
        "Australia/Sydney",
        "local",
    ] {
        for expr in [
            "* * * * *",
            "*/7 * * * *",
            "30 2 * * *",
            "30 1 * * *",
            "0 */3 * * *",
            "0 0 * * 0",
            "@hourly",
        ] {
            let fires = cron(expr, tz).next_n_in(after, 200, &Utc);
            assert_eq!(fires.len(), 200, "{expr} {tz}");
            assert!(fires.windows(2).all(|w| w[0] < w[1]), "{expr} {tz}");
            assert!(fires[0] > after);
        }
    }
    let fires = Trigger::Interval { every: 61 }.next_n_in(after, 50, &Utc);
    assert!(fires.windows(2).all(|w| w[0] < w[1]));
}

#[test]
fn dst_gap_and_overlap_in_new_york() {
    // Pinned instants (a croner bump that changes the rule must show here). 2026-03-08 02:30 does not exist in New
    // York: croner fires at 03:00 EDT. 2026-11-01 01:30 happens twice: only the first (EDT) fires.
    let t = cron("30 2 * * *", "America/New_York");
    assert_eq!(
        t.next_n_in(ms("2026-03-06T12:00:00Z"), 4, &Utc),
        vec![
            ms("2026-03-07T07:30:00Z"),
            ms("2026-03-08T07:00:00Z"),
            ms("2026-03-09T06:30:00Z"),
            ms("2026-03-10T06:30:00Z"),
        ]
    );
    let t = cron("30 1 * * *", "America/New_York");
    assert_eq!(
        t.next_n_in(ms("2026-10-30T12:00:00Z"), 4, &Utc),
        vec![
            ms("2026-10-31T05:30:00Z"),
            ms("2026-11-01T05:30:00Z"),
            ms("2026-11-02T06:30:00Z"),
            ms("2026-11-03T06:30:00Z"),
        ]
    );
}

#[test]
fn local_on_a_dst_zone_follows_that_zone() {
    // `Local` is whatever zone it is handed; a DST zone behaves as the IANA spelling does.
    let ny: Tz = "America/New_York".parse().unwrap();
    let local = cron("30 2 * * *", "local");
    let named = cron("30 2 * * *", "America/New_York");
    for after in ["2026-03-07T12:00:00Z", "2026-10-31T12:00:00Z"] {
        assert_eq!(
            local.next_n_in(ms(after), 4, &ny),
            named.next_n_in(ms(after), 4, &Utc)
        );
    }
    assert_eq!(
        local.next_after_in(ms("2026-03-08T00:00:00Z"), &ny),
        Some(ms("2026-03-08T07:00:00Z"))
    );
}

#[test]
fn sub_second_after_is_floored_to_the_minute() {
    let t = cron("* * * * *", "UTC");
    assert_eq!(
        t.next_after(ms("2026-03-08T12:59:59.999Z")),
        Some(ms("2026-03-08T13:00:00Z"))
    );
    assert_eq!(
        t.next_after(ms("2026-03-08T12:34:00.123Z")),
        Some(ms("2026-03-08T12:35:00Z"))
    );
    let t = cron("0 * * * *", "UTC");
    assert_eq!(
        t.next_after(ms("2026-03-08T12:59:59.999Z")),
        Some(ms("2026-03-08T13:00:00Z"))
    );
}

#[test]
fn the_hosts_cron_superset_still_parses() {
    // What serve/host/src/routines accepts (`Cron::from_str`): the presets and tests there, and croner-only spellings.
    for expr in [
        "0 9 * * *",
        "*/15 * * * *",
        "47 * * * *",
        "47 23 * * 1,3,5",
        "47 23 31 * *",
        "37 * * * *",
        "5 23 * * 1,3,6",
        "45 8 31 * *",
        "*/7 * * * *",
        "20 14 * * 0",
        "0 9 * * 1,3,5",
        "0 9 31 * *",
        "* * * * *",
        "0 9 * * MON-FRI",
        "0 0 * JAN *",
        "0 0 L * *",
        "0 0 * * FRI#2",
        "0 0 15W * *",
        "0 0 ? * *",
        "@yearly",
        "@annually",
    ] {
        let t = cron(expr, "UTC");
        t.validate().expect(expr);
        assert!(t.next_after(ms("2026-11-04T10:00:00Z")).is_some(), "{expr}");
    }
    // croner's meaning, not kith-sched's.
    assert_eq!(
        cron("0 9 * * MON-FRI", "UTC").next_after(ms("2026-11-06T10:00:00Z")),
        Some(ms("2026-11-09T09:00:00Z"))
    );
    assert_eq!(
        cron("@yearly", "UTC").next_after(ms("2026-11-04T10:00:00Z")),
        Some(ms("2027-01-01T00:00:00Z"))
    );
    // A spec both refuse keeps kith-sched's message.
    for (expr, msg) in [
        ("61 9 * * *", "cron field '61' out of range 0-59"),
        (
            "a b c",
            "cron needs 5 fields (minute hour day month weekday), got 'a b c'",
        ),
    ] {
        assert_eq!(CronSpec::parse(expr).unwrap_err().0, msg);
    }
}

#[test]
fn catch_up_runs_once_for_a_miss_under_a_day() {
    let t = cron("*/5 * * * *", "UTC");
    let now = ms("2026-11-04T10:07:00Z");
    // slept through two slots: one run, the first one missed
    assert_eq!(
        t.due_in(Some(ms("2026-11-04T09:58:00Z")), now, &Utc),
        Some(ms("2026-11-04T10:00:00Z"))
    );
    // nothing due since the last tick
    assert_eq!(t.due_in(Some(ms("2026-11-04T10:05:00Z")), now, &Utc), None);
    // a miss of more than a day is forgotten back to the last day
    let daily = cron("0 9 * * *", "UTC");
    assert_eq!(
        daily.due_in(
            Some(ms("2026-11-01T08:00:00Z")),
            ms("2026-11-04T10:00:00Z"),
            &Utc
        ),
        Some(ms("2026-11-04T09:00:00Z"))
    );
    // no last tick: only the minute just gone is looked at
    assert_eq!(
        t.due_in(None, ms("2026-11-04T10:05:30Z"), &Utc),
        Some(ms("2026-11-04T10:05:00Z"))
    );
    assert_eq!(t.due_in(None, ms("2026-11-04T10:06:30Z"), &Utc), None);
}

#[test]
fn preview_lists_the_next_fires() {
    let out = preview(
        &json!({"trigger": {"type":"cron","expr":"0 12 * * *","tz":"UTC"}, "after": ms("2026-11-04T00:00:00Z"), "n": 3}),
        0,
    )
    .unwrap();
    let at: Vec<i64> = out["fires"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["at"].as_i64().unwrap())
        .collect();
    assert_eq!(
        at,
        vec![
            ms("2026-11-04T12:00:00Z"),
            ms("2026-11-05T12:00:00Z"),
            ms("2026-11-06T12:00:00Z")
        ]
    );
    assert_eq!(out["fires"][0]["iso"], "2026-11-04T12:00:00+00:00");
    // after defaults to now; a webhook has none; n is capped
    let out = preview(
        &json!({"trigger": {"type":"interval","every":3600}, "n": 1000}),
        ms("2026-11-04T00:00:00Z"),
    )
    .unwrap();
    assert_eq!(out["fires"].as_array().unwrap().len(), PREVIEW_MAX);
    assert_eq!(
        preview(&json!({"trigger": {"type":"webhook"}}), 0).unwrap(),
        json!({"fires": []})
    );
    assert!(
        preview(
            &json!({"trigger": {"type":"cron","expr":"nope","tz":"UTC"}}),
            0
        )
        .is_err()
    );
    assert!(preview(&json!({}), 0).is_err());
}
