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
            expr: "* * * * * *".into(),
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
    // 2026-03-08 02:30 does not exist in New York; 2026-11-01 01:30 happens twice. Whatever croner does, a daily job
    // fires once a day, and always strictly later.
    let t = cron("30 2 * * *", "America/New_York");
    let fires = t.next_n_in(ms("2026-03-06T12:00:00Z"), 4, &Utc);
    assert_eq!(fires.len(), 4);
    let days: Vec<_> = fires.iter().map(|f| f.div_euclid(86_400_000)).collect();
    assert!(days.windows(2).all(|w| w[0] < w[1]));
    let t = cron("30 1 * * *", "America/New_York");
    let fires = t.next_n_in(ms("2026-10-30T12:00:00Z"), 4, &Utc);
    let days: Vec<_> = fires.iter().map(|f| f.div_euclid(86_400_000)).collect();
    assert_eq!(days.len(), 4);
    assert!(
        days.windows(2).all(|w| w[0] < w[1]),
        "the doubled hour fires once, not twice"
    );
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
