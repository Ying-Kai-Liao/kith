//! The routines' timing, frozen (M4-0). Every row asks `Trigger` when it fires next after a given moment (and, for
//! intervals, where `advance` lands after missed runs); the answers are stored in
//! `kernel/tests/difftest/golden/routines-next.json`. The M4 Rust Trigger type has to give these same answers, and
//! this host's code must not drift before then: the test fails when any answer changes.
//!
//! Re-record on purpose (and say why in the PR): `KITH_RECORD_ROUTINES_NEXT=1 cargo test -p kith-serve --lib schedule_golden`.
use super::Trigger;
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use serde_json::{Value, json};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../kernel/tests/difftest/golden/routines-next.json");

fn ms(rfc3339: &str) -> i64 {
    DateTime::parse_from_rfc3339(rfc3339).unwrap().timestamp_millis()
}

fn iso(ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(ms).unwrap().to_rfc3339()
}

fn cron(expression: &str, zone: &str) -> Trigger {
    Trigger::Cron { expression: expression.into(), time_zone: zone.into() }
}

/// One answer as data: the instant in UTC (and in the trigger's own zone for a cron), null for "never", or the error text.
fn answer(trigger: &Trigger, r: anyhow::Result<Option<i64>>) -> Value {
    match r {
        Err(e) => json!({ "error": format!("{e:#}") }),
        Ok(None) => json!({ "next": null }),
        Ok(Some(t)) => {
            let mut v = json!({ "next": t, "utc": iso(t) });
            if let Trigger::Cron { time_zone, .. } = trigger {
                let zone: Tz = time_zone.parse().unwrap();
                v["local"] =
                    json!(DateTime::<Utc>::from_timestamp_millis(t).unwrap().with_timezone(&zone).to_rfc3339());
            }
            v
        }
    }
}

fn rows() -> Vec<Value> {
    let mut out = Vec::new();
    let mut next = |trigger: Trigger, after: &str| {
        let a = ms(after);
        let mut row =
            json!({ "op": "next", "trigger": serde_json::to_value(&trigger).unwrap(), "after": a, "afterUtc": iso(a) });
        row["answer"] = answer(&trigger, trigger.next(a));
        row["validate"] = match trigger.validate(a) {
            Ok(()) => json!("ok"),
            Err(e) => json!(format!("{e:#}")),
        };
        out.push(row);
    };

    // Cron: the expression dialect, in one zone (UTC) and in several.
    for e in [
        "* * * * *",
        "*/15 * * * *",
        "5/15 * * * *",
        "0,30 9-17 * * 1-5",
        "0 9 * * 0",
        "0 9 * * 7",
        "0 9 * * 5-7",
        "0 9 * * 0-7",
        "0 9 13 * 5",
        "0 9 13 * *",
        "0 0 29 2 *",
        "0 0 31 4 *",
        "59 23 31 12 *",
        "0 0 1 1 *",
        "0 9 * * MON-FRI",
        "0 9 * JAN *",
        "0 12 * * 6,7",
        "*/0 * * * *",
        "60 * * * *",
        "0 0 * 13 *",
        "5-1 * * * *",
        "1-2-3 * * * *",
        "x y z a b",
        "* * * * * *",
        "0 0 L * *",
        "0 0 * * 5#2",
        "@hourly",
        "@daily",
        "@midnight",
        "@weekly",
        "@monthly",
        "@yearly",
        "",
    ] {
        next(cron(e, "UTC"), "2026-11-03T08:00:00Z");
    }
    next(cron("0 0 29 2 *", "UTC"), "2027-03-01T00:00:00Z");
    next(cron("0 0 * * *", "UTC"), "2026-12-31T23:59:30Z");
    // after sitting exactly on a fire time, and a second before it: the next one is strictly later
    next(cron("30 8 * * *", "UTC"), "2026-11-03T08:30:00Z");
    next(cron("30 8 * * *", "UTC"), "2026-11-03T08:29:59Z");
    next(cron("30 8 * * *", "UTC"), "2026-11-03T08:30:00.001Z");

    // Zones.
    for z in [
        "Asia/Taipei",
        "Asia/Kolkata",
        "Asia/Kathmandu",
        "Europe/London",
        "Europe/Berlin",
        "America/New_York",
        "America/Los_Angeles",
        "America/Sao_Paulo",
        "Australia/Sydney",
        "Australia/Lord_Howe",
        "Pacific/Auckland",
        "Pacific/Kiritimati",
        "UTC",
        "Etc/GMT+5",
    ] {
        next(cron("0 9 * * *", z), "2026-11-03T08:00:00Z");
        next(cron("20 14 * * 0", z), "2026-09-26T12:00:00Z");
    }
    next(cron("0 9 * * *", "Mars/Olympus"), "2026-11-03T08:00:00Z");
    next(cron("0 9 * * *", ""), "2026-11-03T08:00:00Z");
    next(cron("0 9 * * *", "utc"), "2026-11-03T08:00:00Z");
    next(cron("0 9 * * *", "EST"), "2026-11-03T08:00:00Z");

    // DST. New York springs forward 2026-03-08 02:00 -> 03:00 and falls back 2026-11-01 02:00 -> 01:00.
    for (e, after) in [
        ("30 2 * * *", "2026-03-07T12:00:00Z"), // 02:30 does not exist on the 8th
        ("30 2 * * *", "2026-03-08T12:00:00Z"),
        ("0 2 8 3 *", "2026-01-01T00:00:00Z"),
        ("59 1 * * *", "2026-03-07T12:00:00Z"),
        ("0 3 * * *", "2026-03-07T12:00:00Z"),
        ("*/30 * * * *", "2026-03-08T06:45:00Z"), // across the gap
        ("30 1 * * *", "2026-10-31T12:00:00Z"),   // 01:30 happens twice on 11-01
        ("30 1 * * *", "2026-11-01T05:45:00Z"),   // after the first 01:30 (EDT)
        ("30 1 * * *", "2026-11-01T06:00:00Z"),
        ("30 1 * * *", "2026-11-01T06:45:00Z"), // after the second 01:30 (EST)
        ("0 * * * *", "2026-11-01T04:30:00Z"),  // hourly across the repeated hour
        ("0 * * * *", "2026-11-01T05:30:00Z"),
        ("0 * * * *", "2026-11-01T06:30:00Z"),
        ("0 0 * * *", "2026-11-01T03:00:00Z"),
        ("0 9 * * *", "2026-11-01T12:00:00Z"), // the day after: 09:00 is an hour later in UTC
    ] {
        next(cron(e, "America/New_York"), after);
    }
    // London: springs forward 2026-03-29 01:00 -> 02:00, back 2026-10-25 02:00 -> 01:00.
    next(cron("30 1 * * *", "Europe/London"), "2026-03-28T12:00:00Z");
    next(cron("30 1 * * *", "Europe/London"), "2026-10-24T12:00:00Z");
    next(cron("30 1 * * *", "Europe/London"), "2026-10-25T00:45:00Z");
    // Sydney: springs forward 2026-10-04 02:00 -> 03:00, back 2026-04-05 03:00 -> 02:00.
    next(cron("30 2 * * *", "Australia/Sydney"), "2026-10-03T12:00:00Z");
    next(cron("30 2 * * *", "Australia/Sydney"), "2026-04-04T12:00:00Z");
    // Lord Howe changes by thirty minutes.
    next(cron("45 1 * * *", "Australia/Lord_Howe"), "2026-10-03T12:00:00Z");
    // A weekly job over the New York change, and day-of-month 31 over a short month.
    next(cron("0 9 * * 0", "America/New_York"), "2026-03-07T00:00:00Z");
    next(cron("0 9 * * 0", "America/New_York"), "2026-03-08T14:00:00Z");
    next(cron("0 9 31 * *", "America/New_York"), "2026-11-01T00:00:00Z");

    // Interval: after + seconds, whatever the clock does.
    for s in [1, 59, 60, 3600, 86_400, 31_536_000] {
        next(Trigger::Interval { seconds: s }, "2026-11-03T08:00:00Z");
    }
    for s in [0, -1, 31_536_001, i64::MAX] {
        next(Trigger::Interval { seconds: s }, "2026-11-03T08:00:00Z");
    }
    next(Trigger::Interval { seconds: 86_400 }, "2026-03-07T12:00:00Z");

    // Once: the moment itself when it is still ahead, else never.
    for at in ["2026-11-03T08:00:01Z", "2026-11-03T08:00:00Z", "2026-11-03T07:59:59Z", "2027-01-01T00:00:00Z"] {
        next(Trigger::Once { at: ms(at) }, "2026-11-03T08:00:00Z");
    }
    next(Trigger::Once { at: 0 }, "1970-01-01T00:00:00Z");

    // No clock at all.
    next(Trigger::Webhook, "2026-11-03T08:00:00Z");
    next(
        Trigger::Event {
            source: "github".into(),
            event: "*".into(),
            filters: [("repo".to_owned(), "a/b".to_owned())].into(),
        },
        "2026-11-03T08:00:00Z",
    );
    next(
        Trigger::Event { source: "nope".into(), event: "*".into(), filters: Default::default() },
        "2026-11-03T08:00:00Z",
    );

    // advance(due, now): intervals skip the runs missed while the host was off, keeping the original cadence; the
    // others look for the next time after now.
    let mut advance = |trigger: Trigger, due: &str, now: &str| {
        let (d, n) = (ms(due), ms(now));
        let mut row = json!({ "op": "advance", "trigger": serde_json::to_value(&trigger).unwrap(), "due": d, "dueUtc": iso(d), "now": n, "nowUtc": iso(n) });
        row["answer"] = answer(&trigger, trigger.advance(d, n));
        out.push(row);
    };
    for (due, now) in [
        ("2026-11-03T08:00:00Z", "2026-11-03T08:00:00Z"),
        ("2026-11-03T08:00:00Z", "2026-11-03T08:00:30Z"),
        ("2026-11-03T08:00:00Z", "2026-11-03T08:01:00Z"),
        ("2026-11-03T08:00:00Z", "2026-11-03T08:59:59Z"),
        ("2026-11-03T08:00:00Z", "2026-11-04T08:00:00Z"),
        ("2026-11-03T08:00:00Z", "2026-11-03T07:00:00Z"),
    ] {
        advance(Trigger::Interval { seconds: 60 }, due, now);
        advance(Trigger::Interval { seconds: 3600 }, due, now);
    }
    advance(Trigger::Interval { seconds: 0 }, "2026-11-03T08:00:00Z", "2026-11-03T08:05:00Z");
    advance(cron("30 8 * * *", "UTC"), "2026-11-03T08:30:00Z", "2026-11-03T08:30:00Z");
    advance(cron("30 8 * * *", "UTC"), "2026-11-03T08:30:00Z", "2026-11-05T20:00:00Z");
    advance(cron("*/15 * * * *", "Asia/Taipei"), "2026-11-03T08:30:00Z", "2026-11-03T08:31:00Z");
    advance(Trigger::Once { at: ms("2026-11-03T08:30:00Z") }, "2026-11-03T08:30:00Z", "2026-11-03T08:30:01Z");
    advance(Trigger::Webhook, "2026-11-03T08:30:00Z", "2026-11-03T08:30:01Z");
    out
}

#[test]
fn routines_next_fire_times_are_frozen() {
    let got = Value::Array(rows());
    if std::env::var_os("KITH_RECORD_ROUTINES_NEXT").is_some() {
        std::fs::write(FIXTURE, serde_json::to_string_pretty(&got).unwrap() + "\n").unwrap();
        return;
    }
    let want: Value =
        serde_json::from_str(&std::fs::read_to_string(FIXTURE).expect("golden/routines-next.json is missing")).unwrap();
    let (got, want) = (got.as_array().unwrap(), want.as_array().expect("an array of rows"));
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        assert_eq!(g, w, "row {i} changed (the routines' timing is frozen; re-record on purpose)");
    }
    assert_eq!(got.len(), want.len(), "the number of rows changed");
}
