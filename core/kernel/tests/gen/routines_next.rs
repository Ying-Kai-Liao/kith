// Makes tests/routines_next.json: what serve/host/src/routines/schedule.rs answers for each (trigger, zone, after).
// Not a cargo target. To refresh the file, make a throwaway crate (edition 2024) with this as src/main.rs and the
// dependencies anyhow = "1", chrono = "0.4.45", chrono-tz = "0.10.4", croner = "3", serde (derive) and serde_json, then
//   cargo run > core/kernel/tests/routines_next.json
// The `#[path]` is relative to src/main.rs: point it at the host file in your checkout.
#[path = "../../kith/serve/host/src/routines/schedule.rs"]
mod schedule;
use schedule::Trigger;
use serde_json::json;
fn main() {
    let zones = ["UTC", "Asia/Taipei", "America/New_York", "Europe/London", "Australia/Sydney", "Asia/Kolkata"];
    let exprs = ["* * * * *", "*/15 * * * *", "0 9 * * 1-5", "30 8 * * 0", "30 8 * * 7", "0 0 1 * 1", "0 0 29 2 *",
        "0 0 31 * *", "30 2 * * *", "30 1 * * *", "0 0 * * *", "0 12 * * 5-7", "5/15 3/10 * * *", "0 0 31 2 *"];
    let afters: [i64; 7] = [1_767_225_600_000, 1_772_930_000_000, 1_774_700_000_123, 1_790_000_000_000, 1_792_800_000_000, 1_793_500_000_000, 1_804_000_000_999];
    let mut cases = vec![];
    for tz in zones { for e in exprs { for a in afters {
        let t = Trigger::Cron { expression: e.into(), time_zone: tz.into() };
        cases.push(json!({"trigger": {"type":"cron","expr":e,"tz":tz}, "after": a, "next": t.next(a).ok().flatten()}));
    }}}
    for s in [1i64, 59, 60, 61, 900, 3600, 86_400] { for a in afters {
        let t = Trigger::Interval { seconds: s };
        cases.push(json!({"trigger": {"type":"interval","every":s}, "after": a, "next": t.next(a).ok().flatten()}));
    }}
    for (at, a) in [(afters[3], afters[2]), (afters[3] + 30_000, afters[3]), (afters[3] - 1, afters[3]), (afters[3], afters[3]), (afters[3] + 1, afters[3])] {
        let t = Trigger::Once { at };
        cases.push(json!({"trigger": {"type":"once","at":at}, "after": a, "next": t.next(a).ok().flatten()}));
    }
    println!("{}", serde_json::to_string_pretty(&json!({"cases": cases})).unwrap());
}
