#![allow(clippy::unwrap_used)]
//! `Trigger` against the two schedulers it replaces (M4-1).
//!
//! * `schedule_parity.json` is what `kith-sched`'s cron parser says (kernel/tests/kith-sched-cron-table). The clocks are
//!   naive local times, read here as UTC: an offset-zero `Local`. When python3 is on the PATH the table is made again
//!   and must equal the frozen one, so the file cannot go stale (under CI a missing python3 fails the test).
//!   The catch-up rows are tick's arithmetic copied into the script (kernel/bin/kith-sched, `tick`, around lines
//!   398-405); a change to tick is not seen by the table.
//! * `routines_next.json` is what serve/host/src/routines/schedule.rs answers for the same (trigger, zone, after). It
//!   was made by compiling that file as it is and printing `Trigger::next` (`tests/gen/routines_next.rs` says how). Cron
//!   must be equal to the minute (the host keeps an `after`'s sub-second part, this one cuts it off); Interval and Once
//!   are the host's value rounded up to the minute (the intended difference).

use std::path::PathBuf;
use std::process::Command;

use chrono::{DateTime, NaiveDateTime, Utc};
use kith_kernel::schedule::{CronSpec, Trigger, Zone};
use serde_json::Value;

fn here(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn naive_ms(text: &str) -> i64 {
    NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S")
        .unwrap()
        .and_utc()
        .timestamp_millis()
}

fn naive_text(ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(ms)
        .unwrap()
        .format("%Y-%m-%dT%H:%M:%S")
        .to_string()
}

fn check_python_table(table: &Value) {
    let cases = table["specs"].as_array().unwrap();
    assert!(cases.len() > 80);
    for case in cases {
        let spec = case["spec"].as_str().unwrap();
        if let Some(error) = case.get("error") {
            // kith-sched refuses it; croner may still take it (the host's superset), else the message is kith-sched's.
            match CronSpec::parse(spec) {
                Err(got) => assert_eq!(got.0, error.as_str().unwrap(), "{spec:?}"),
                Ok(parsed) => assert!(matches!(parsed, CronSpec::Croner(_)), "{spec:?}"),
            }
            continue;
        }
        let trigger = Trigger::Cron {
            expr: spec.to_owned(),
            tz: Zone::Local,
        };
        trigger.validate().expect(spec);
        for (clock, want) in case["next"].as_object().unwrap() {
            let want: Vec<&str> = want
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            let got: Vec<String> = trigger
                .next_n_in(naive_ms(clock), want.len() + 1, &Utc)
                .into_iter()
                .map(naive_text)
                .collect();
            // Python's chain stops when it finds nothing within five years; ours has no horizon, so it may go on.
            assert_eq!(&got[..want.len()], &want[..], "{spec:?} after {clock}");
            if want.len() < 4 {
                assert!(
                    got.len() == want.len()
                        || got[want.len()] > naive_text(naive_ms(clock) + 5 * 366 * 86_400_000),
                    "{spec:?} after {clock}"
                );
            }
        }
    }
    for c in table["catch_up"].as_array().unwrap() {
        let trigger = Trigger::Cron {
            expr: c["spec"].as_str().unwrap().to_owned(),
            tz: Zone::Local,
        };
        let got = trigger
            .due_in(
                Some(naive_ms(c["last"].as_str().unwrap())),
                naive_ms(c["now"].as_str().unwrap()),
                &Utc,
            )
            .map(naive_text);
        assert_eq!(got.as_deref(), c["due"].as_str(), "{c}");
    }
}

#[test]
fn matches_kith_sched_frozen_table() {
    let table: Value =
        serde_json::from_slice(&std::fs::read(here("tests/schedule_parity.json")).unwrap())
            .unwrap();
    check_python_table(&table);
}

#[test]
fn frozen_table_is_what_kith_sched_says_now() {
    let script = here("../../kernel/tests/kith-sched-cron-table");
    if !script.exists() || !cfg!(unix) {
        return;
    }
    let Ok(out) = Command::new("python3").arg(&script).output() else {
        // CI must not pass without the check.
        assert!(
            std::env::var_os("CI").is_none(),
            "python3 not found: the frozen table cannot be re-checked"
        );
        eprintln!("python3 not found: the frozen table is not re-checked");
        return;
    };
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let now: Value = serde_json::from_slice(&out.stdout).unwrap();
    let frozen: Value =
        serde_json::from_slice(&std::fs::read(here("tests/schedule_parity.json")).unwrap())
            .unwrap();
    assert_eq!(
        now, frozen,
        "rerun kernel/tests/kith-sched-cron-table --write"
    );
}

#[test]
fn matches_the_host_routines() {
    let table: Value =
        serde_json::from_slice(&std::fs::read(here("tests/routines_next.json")).unwrap()).unwrap();
    let cases = table["cases"].as_array().unwrap();
    assert!(cases.len() > 500);
    for case in cases {
        let trigger: Trigger = serde_json::from_value(case["trigger"].clone()).unwrap();
        let after = case["after"].as_i64().unwrap();
        let host = case["next"].as_i64();
        let got = trigger.next_after_in(after, &Utc);
        let want = match &trigger {
            // the host carries seconds; this one fires on the minute tick
            Trigger::Interval { .. } | Trigger::Once { .. } => {
                host.map(|t| (t + 59_999).div_euclid(60_000) * 60_000)
            }
            Trigger::Cron { .. } => host.map(|t| t.div_euclid(60_000) * 60_000),
            _ => host,
        };
        assert_eq!(got, want, "{} after {after}", case["trigger"]);
    }
}
