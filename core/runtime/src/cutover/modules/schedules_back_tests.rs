#![allow(clippy::unwrap_used)]
//! The schedules rollback's hand-back (M4-5 handover, H4) on the `schedules_tests` world: each case of the per-job rule,
//! the state.json write-back, the running-run rule, and the host and kith-sched reading what was written.

use std::collections::BTreeSet;

use rusqlite::Connection;
use serde_json::{Value, json};

use super::schedules_tests::{World, find, opts, routine, world, write_state};
use crate::cutover::{rollback, state, step};
use crate::schedules::{local_minute, ms_of_slot};

const HOUR: i64 = 3_600_000;

/// 2026-01-05T08:00:00Z, long before any real clock the tests run on.
fn t0() -> i64 {
    ms_of_slot("2026-01-05T08:00:00Z").unwrap()
}

fn slot(ms: i64) -> String {
    crate::schedules::slot_of_ms(ms).unwrap()
}

fn hourly() -> Value {
    json!({"type": "cron", "expression": "0 * * * *", "timeZone": "UTC"})
}

/// A routine of bot b1 with its triggers and the host's next runs.
fn rt(id: &str, triggers: &Value, next: &Value) -> Value {
    let mut r = routine(id, "b1", true, triggers, "whk-test-key");
    r["nextRuns"] = next.clone();
    r
}

/// A world whose host has only these routines, stepped, with `wm` as the core's watermark (ms).
fn stepped(routines: &[Value], wm: Option<i64>) -> World {
    let w = world(false);
    w.set_serve_routines(&json!({"routines": routines, "runs": []}));
    step(&w.reg(), &w.env, "schedules", opts()).unwrap();
    let core = w.core();
    // The index the tick keeps (the rollback reads a job's kind from it).
    for j in w.jobs() {
        core.conn()
            .execute(
                "INSERT OR REPLACE INTO sched_jobs (agent, job, def, paused, first_seen) VALUES ('mel', ?1, ?2, 0, ?3)",
                rusqlite::params![j["id"].as_str().unwrap(), j.to_string(), t0()],
            )
            .unwrap();
    }
    if let Some(at) = wm {
        core.conn()
            .execute(
                "INSERT OR REPLACE INTO sched_watermark (mac, at, updated_at) VALUES (?1, ?2, 0)",
                rusqlite::params![w.mac(), at],
            )
            .unwrap();
    }
    drop(core);
    w
}

fn exec(w: &World, sql: &str) {
    let core = w.core();
    core.conn()
        .execute_batch(sql)
        .unwrap_or_else(|e| panic!("the rig's rows no longer fit the schema: {e}"));
}

/// A decided slot whose turn finished.
fn decided(w: &World, job: &str, at: i64) {
    exec(
        w,
        &format!(
            "INSERT INTO sched_fires (agent, job, slot, outcome, turn_id, at) VALUES ('mel', '{job}', '{}', 'fired', NULL, 0);",
            slot(at)
        ),
    );
}

/// A schedule turn in `state` for the job's slot (its fire row points at it). `claim`: a process for it.
fn turn(w: &World, job: &str, at: i64, state: &str, claim: Option<(i64, &str)>) {
    let parked = if state == "parked" {
        "'lock_busy'"
    } else {
        "NULL"
    };
    let conv = format!("schedule:mel:{job}:claude");
    let mut sql = format!(
        "INSERT OR IGNORE INTO conversations (id, agent, engine, created_at, updated_at) VALUES ('{conv}', 'mel', 'claude', 0, 0);
         INSERT INTO turns (conversation, source, state, parked_reason, queued_at) VALUES ('{conv}', 'schedule', '{state}', {parked}, 0);
         INSERT INTO sched_fires (agent, job, slot, outcome, turn_id, at)
           VALUES ('mel', '{job}', '{}', 'fired', (SELECT max(id) FROM turns), 0);",
        slot(at)
    );
    if let Some((pid, started)) = claim {
        let _ = write!(
            sql,
            "INSERT INTO claims (conversation, generation, kind, state, turn, pid, pid_started, created_at, claimed_at, running_at)
               VALUES ('{conv}', 1, 'turn', '{state}', (SELECT max(id) FROM turns), {pid}, '{started}', {at}, {at}, {at});
             UPDATE turns SET claim_id = (SELECT max(claim_id) FROM claims) WHERE id = (SELECT max(id) FROM turns);"
        );
    }
    exec(w, &sql);
}

use std::fmt::Write as _;

fn next_runs(w: &World, id: &str) -> Value {
    let v = w.serve_routines();
    let r = v["routines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == id)
        .unwrap_or_else(|| panic!("no routine {id}"));
    r["nextRuns"].clone()
}

fn outcome(w: &World, job: &str, at: i64) -> Option<String> {
    Connection::open(&w.env.layout.db)
        .unwrap()
        .query_row(
            "SELECT outcome FROM sched_fires WHERE agent = 'mel' AND job = ?1 AND slot = ?2",
            rusqlite::params![job, slot(at)],
            |r| r.get(0),
        )
        .ok()
}

fn roll(w: &World) -> String {
    rollback(&w.reg(), &w.env, "schedules", opts())
        .unwrap()
        .join("\n")
}

fn state_json(w: &World) -> Value {
    serde_json::from_slice(&std::fs::read(w.state_file()).unwrap()).unwrap()
}

#[test]
fn a_cancelled_queued_turn_gives_its_slot_back_to_the_host_and_its_row_says_so() {
    let w = stepped(
        &[rt("a", &json!([hourly()]), &json!([t0() + 5 * HOUR]))],
        Some(t0() + 3 * HOUR),
    );
    decided(&w, "a", t0() + 2 * HOUR);
    turn(&w, "a", t0() + 3 * HOUR, "queued", None);
    let out = roll(&w);
    assert!(out.contains("the host runs it once it starts"), "{out}");
    assert_eq!(next_runs(&w, "a"), json!([t0() + 3 * HOUR]));
    assert_eq!(
        outcome(&w, "a", t0() + 3 * HOUR).as_deref(),
        Some("skipped:rollback")
    );
    // Earlier slots keep what they decided; the turn is cancelled, not left to strand.
    assert_eq!(outcome(&w, "a", t0() + 2 * HOUR).as_deref(), Some("fired"));
    let c = Connection::open(&w.env.layout.db).unwrap();
    let st: String = c
        .query_row("SELECT state FROM turns", [], |r| r.get(0))
        .unwrap();
    assert_eq!(st, "cancelled");
}

#[test]
fn a_parked_turn_is_cancelled_too_and_its_slot_goes_to_the_host() {
    let w = stepped(
        &[rt("a", &json!([hourly()]), &json!([t0() + 5 * HOUR]))],
        Some(t0() + 3 * HOUR),
    );
    turn(&w, "a", t0() + 3 * HOUR, "parked", None);
    roll(&w);
    assert_eq!(next_runs(&w, "a"), json!([t0() + 3 * HOUR]));
    assert_eq!(
        outcome(&w, "a", t0() + 3 * HOUR).as_deref(),
        Some("skipped:rollback")
    );
}

#[test]
fn a_cancelled_slot_of_a_cron_job_is_reported_and_not_replayed() {
    let w = stepped(&[], Some(t0() + 3 * HOUR));
    // The job `digest` is kith-sched's own (no routine block).
    let core = w.core();
    core.conn()
        .execute(
            "INSERT OR REPLACE INTO sched_jobs (agent, job, def, paused, first_seen) VALUES ('mel', 'digest', ?1, 0, 0)",
            [json!({"id": "digest", "cron": "0 * * * *", "task": "x"}).to_string()],
        )
        .unwrap();
    drop(core);
    turn(&w, "digest", t0() + 3 * HOUR, "queued", None);
    let out = roll(&w);
    assert!(
        out.contains("mel/digest") && out.contains("is not replayed"),
        "{out}"
    );
    assert_eq!(
        outcome(&w, "digest", t0() + 3 * HOUR).as_deref(),
        Some("skipped:rollback")
    );
    // kith-sched has one tick: it is the core's watermark, past the cancelled slot.
    assert_eq!(
        state_json(&w)["tick"],
        json!(local_minute(t0() + 3 * HOUR).unwrap())
    );
}

#[test]
fn an_anchor_the_core_never_reached_is_the_hosts_next_run_again() {
    // The host was down at the switch and the core's ticks never got to the anchor.
    let w = stepped(
        &[rt("a", &json!([hourly()]), &json!([t0() + 6 * HOUR]))],
        Some(t0() + 2 * HOUR),
    );
    decided(&w, "a", t0() + HOUR);
    roll(&w);
    assert_eq!(next_runs(&w, "a"), json!([t0() + 6 * HOUR]));
}

#[test]
fn a_used_anchor_gives_the_next_slot_after_what_the_core_decided() {
    let w = stepped(
        &[rt("a", &json!([hourly()]), &json!([t0() + HOUR]))],
        Some(t0() + 4 * HOUR),
    );
    decided(&w, "a", t0() + HOUR);
    decided(&w, "a", t0() + 4 * HOUR);
    roll(&w);
    // A cron counts from the watermark.
    assert_eq!(next_runs(&w, "a"), json!([t0() + 5 * HOUR]));
}

#[test]
fn an_interval_counts_from_its_last_slot_and_a_cron_from_the_watermark() {
    let w = stepped(
        &[
            rt(
                "i",
                &json!([{"type": "interval", "seconds": 1800}]),
                &json!([t0() + HOUR]),
            ),
            rt("c", &json!([hourly()]), &json!([t0() + HOUR])),
        ],
        Some(t0() + 6 * HOUR),
    );
    for job in ["i", "c"] {
        decided(&w, job, t0() + HOUR);
        decided(&w, job, t0() + 2 * HOUR);
    }
    roll(&w);
    // The interval: last slot + its period, whatever the watermark says. The cron: after the watermark.
    assert_eq!(next_runs(&w, "i"), json!([t0() + 2 * HOUR + HOUR / 2]));
    assert_eq!(next_runs(&w, "c"), json!([t0() + 7 * HOUR]));
}

#[test]
fn a_once_the_core_decided_gets_no_next_run_and_one_it_never_reached_keeps_its_time() {
    let later = 4_102_444_800_000_i64;
    let w = stepped(
        &[
            // The host fired this one (no next run, its time passed): the step decided its slot.
            rt("fired", &json!([{"type": "once", "at": t0()}]), &json!([])),
            rt(
                "pending",
                &json!([{"type": "once", "at": later}]),
                &json!([later]),
            ),
        ],
        Some(t0() + 3 * HOUR),
    );
    let c = Connection::open(&w.env.layout.db).unwrap();
    let n: i64 = c
        .query_row(
            "SELECT count(*) FROM sched_fires WHERE job = 'fired'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
    drop(c);
    roll(&w);
    assert!(next_runs(&w, "fired").get(0).is_none_or(Value::is_null));
    assert_eq!(next_runs(&w, "pending"), json!([later]));
}

#[test]
fn an_edited_trigger_is_dropped_the_host_keeps_its_own_trigger_and_it_is_said() {
    let w = stepped(
        &[rt(
            "a",
            &json!([{"type": "cron", "expression": "0 0 * * 1", "timeZone": "UTC"}]),
            &json!([t0() + 100 * HOUR]),
        )],
        Some(t0() + 2 * HOUR),
    );
    // After the switch the weekly became hourly (jobs.json by hand): the rollback does not write that edit back.
    let mut jobs = w.jobs();
    for j in &mut jobs {
        if j["id"] == "a" {
            j["trigger"] = serde_json::to_value(kith_kernel::schedule::Trigger::Cron {
                expr: "0 * * * *".into(),
                tz: kith_kernel::schedule::Zone::parse("UTC").unwrap(),
            })
            .unwrap();
        }
    }
    std::fs::write(w.mel_jobs(), serde_json::to_string_pretty(&jobs).unwrap()).unwrap();
    let out = roll(&w);
    // The next Monday 00:00Z after the watermark, not the hourly slot and not the old anchor.
    assert_eq!(next_runs(&w, "a"), json!([t0() + 160 * HOUR]));
    assert!(out.contains("the edit is dropped"), "{out}");
}

#[test]
fn a_cron_routine_with_an_empty_watermark_table_still_gets_a_next_run_from_the_tick_text() {
    let w = stepped(&[rt("a", &json!([hourly()]), &json!([t0() + HOUR]))], None);
    // The core decided the host's anchor, so the next run counts from the tick.
    decided(&w, "a", t0() + HOUR);
    let at = local_minute(t0() + 2 * HOUR).unwrap();
    exec(
        &w,
        &format!(
            "DELETE FROM sched_watermark; INSERT OR REPLACE INTO sched_tick (mac, tick, updated_at) VALUES ('{}', '{at}', 0);",
            w.mac()
        ),
    );
    roll(&w);
    assert_eq!(next_runs(&w, "a"), json!([t0() + 3 * HOUR]));
}

#[test]
fn a_webhook_trigger_is_left_alone_and_the_triggers_of_one_routine_are_each_ruled() {
    let w = stepped(
        &[rt(
            "m",
            &json!([hourly(), {"type": "webhook"}]),
            &json!([t0() + 9 * HOUR, null]),
        )],
        Some(t0() + HOUR),
    );
    turn(&w, "m-1", t0() + HOUR, "queued", None);
    roll(&w);
    assert_eq!(next_runs(&w, "m"), json!([t0() + HOUR, null]));
}

#[test]
fn state_json_takes_the_watermark_as_its_tick_and_a_running_core_run_over_an_older_entry() {
    let w = world(false);
    write_state(
        &w,
        &json!({"tick": "2026-01-05T07:00", "last": {
            "mel/digest": {"at": "2026-01-05T06:00", "pid": 42, "procStart": "p1", "why": "due", "log": "/x.log"},
            "mel/newer": {"at": "2026-01-05T20:00", "pid": 7, "procStart": "n1"},
        }}),
    );
    step(&w.reg(), &w.env, "schedules", opts()).unwrap();
    let wm = t0() + 3 * HOUR;
    {
        let core = w.core();
        core.conn()
            .execute(
                "INSERT OR REPLACE INTO sched_watermark (mac, at, updated_at) VALUES (?1, ?2, 0)",
                rusqlite::params![w.mac(), wm],
            )
            .unwrap();
        for job in ["digest", "newer"] {
            core.conn()
                .execute(
                    "INSERT OR REPLACE INTO sched_jobs (agent, job, def, paused, first_seen) VALUES ('mel', ?1, ?2, 0, 0)",
                    rusqlite::params![job, json!({"id": job, "cron": "0 * * * *"}).to_string()],
                )
                .unwrap();
        }
    }
    // A detached run the core started for each job: one newer than kith-sched's entry, one older.
    turn(&w, "digest", wm, "running", Some((4242, "stamp-4242")));
    turn(&w, "newer", t0(), "running", Some((77, "stamp-77")));
    let out = roll(&w);
    assert!(
        out.contains("2 still running in the core") || out.contains("1 still running in the core"),
        "{out}"
    );
    let s = state_json(&w);
    assert_eq!(s["tick"], json!(local_minute(wm).unwrap()));
    let d = &s["last"]["mel/digest"];
    assert_eq!(d["pid"], 4242);
    assert_eq!(d["procStart"], "stamp-4242");
    assert_eq!(d["at"], json!(local_minute(wm).unwrap()));
    assert!(d["why"].as_str().unwrap().contains("running in the core"));
    // kith-sched's own entry is the newer one here: it stays whole.
    assert_eq!(s["last"]["mel/newer"]["pid"], 7);
    assert_eq!(s["last"]["mel/newer"]["procStart"], "n1");
}

#[test]
fn a_detached_cron_run_does_not_refuse_the_rollback_but_a_running_routine_run_does() {
    let w = stepped(
        &[rt("a", &json!([hourly()]), &json!([t0() + HOUR]))],
        Some(t0()),
    );
    let core = w.core();
    core.conn()
        .execute(
            "INSERT OR REPLACE INTO sched_jobs (agent, job, def, paused, first_seen) VALUES ('mel', 'digest', ?1, 0, 0)",
            [json!({"id": "digest", "cron": "0 * * * *"}).to_string()],
        )
        .unwrap();
    drop(core);
    turn(&w, "a", t0(), "running", Some((99, "s")));
    let err = rollback(&w.reg(), &w.env, "schedules", opts())
        .unwrap_err()
        .to_string();
    assert!(err.contains("scheduled turn(s) are running"), "{err}");
    assert!(state::read(&w.env.layout, "schedules").unwrap().is_some());
    // The routine's run ends; a cron run with a process is only handed to kith-sched.
    exec(
        &w,
        "UPDATE turns SET state = 'done'; UPDATE claims SET state = 'ended';",
    );
    turn(&w, "digest", t0(), "running", Some((98, "s2")));
    roll(&w);
    assert_eq!(state_json(&w)["last"]["mel/digest"]["pid"], 98);
}

#[test]
fn a_rollback_without_routines_in_serve_db_still_cancels_what_waits() {
    let w = stepped(
        &[rt("a", &json!([hourly()]), &json!([t0() + HOUR]))],
        Some(t0()),
    );
    turn(&w, "a", t0(), "queued", None);
    Connection::open(&w.serve)
        .unwrap()
        .execute("DELETE FROM kv WHERE k = 'routines.v1'", [])
        .unwrap();
    roll(&w);
    assert_eq!(outcome(&w, "a", t0()).as_deref(), Some("skipped:rollback"));
}

/// The host and kith-sched read what the rollback wrote, and between them nothing fires twice and nothing the core
/// cancelled is lost.
#[test]
fn the_host_and_kith_sched_reading_the_write_back_fire_nothing_twice_and_lose_nothing() {
    let now = t0() + 5 * HOUR + HOUR / 2;
    let w = stepped(
        &[
            // Cancelled: the host takes the slot.
            rt("q", &json!([hourly()]), &json!([t0() + HOUR])),
            // Unused anchor: the host's own next run comes back.
            rt("u", &json!([hourly()]), &json!([t0() + 4 * HOUR])),
            // Used anchor: the core ran it, the host must not.
            rt("d", &json!([hourly()]), &json!([t0() + HOUR])),
        ],
        Some(t0() + 3 * HOUR),
    );
    for h in 1..=3 {
        decided(&w, "d", t0() + h * HOUR);
    }
    decided(&w, "q", t0() + HOUR);
    decided(&w, "q", t0() + 2 * HOUR);
    turn(&w, "q", t0() + 3 * HOUR, "queued", None);
    exec(
        &w,
        "INSERT OR REPLACE INTO sched_jobs (agent, job, def, paused, first_seen)
           VALUES ('mel', 'digest', '{\"id\":\"digest\",\"cron\":\"0 * * * *\"}', 0, 0);",
    );
    roll(&w);

    let c = Connection::open(&w.env.layout.db).unwrap();
    // What the core decided (a cancelled slot is not decided: it was handed back).
    let core_decided = |job: &str| -> BTreeSet<i64> {
        let mut st = c
            .prepare(
                "SELECT slot FROM sched_fires WHERE job = ?1 AND outcome != 'skipped:rollback'",
            )
            .unwrap();
        st.query_map([job], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|s| ms_of_slot(&s.unwrap()).unwrap())
            .collect()
    };
    // The host: every routine trigger whose next run is due fires once, at that slot.
    for (id, handed) in [("q", Some(t0() + 3 * HOUR)), ("u", None), ("d", None)] {
        let host: BTreeSet<i64> = next_runs(&w, id)
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_i64)
            .filter(|t| *t <= now)
            .collect();
        assert!(
            host.is_disjoint(&core_decided(id)),
            "{id}: the host replays {host:?} the core decided {:?}",
            core_decided(id)
        );
        if let Some(slot) = handed {
            assert!(host.contains(&slot), "{id}: the cancelled slot is lost");
        }
    }
    assert_eq!(next_runs(&w, "u"), json!([t0() + 4 * HOUR]));
    // kith-sched: its tick starts after the watermark; a cron job's slots from there are its own.
    let tick = state_json(&w)["tick"].as_str().unwrap().to_owned();
    assert_eq!(tick, local_minute(t0() + 3 * HOUR).unwrap());
    let digest = core_decided("digest");
    assert!(digest.iter().all(|s| *s <= t0() + 3 * HOUR));
}

#[test]
fn stepping_again_after_a_rollback_brings_the_cancelled_slot_back_to_the_core() {
    let w = stepped(
        &[rt("a", &json!([hourly()]), &json!([t0() + 5 * HOUR]))],
        Some(t0() + 3 * HOUR),
    );
    turn(&w, "a", t0() + 3 * HOUR, "queued", None);
    roll(&w);
    assert_eq!(next_runs(&w, "a"), json!([t0() + 3 * HOUR]));
    // The host never ran (it was not started): step again. The anchor is the written-back next run, and the
    // cancelled slot's row must not count as decided.
    step(&w.reg(), &w.env, "schedules", opts()).unwrap();
    let job = w.jobs().into_iter().find(|j| j["id"] == "a").unwrap();
    assert_eq!(job["routine"]["anchor"], json!(t0() + 3 * HOUR));
    assert_eq!(outcome(&w, "a", t0() + 3 * HOUR), None);
    let c = Connection::open(&w.env.layout.db).unwrap();
    assert!(crate::schedules::anchor::unused(&c, "mel", "a", t0() + 3 * HOUR).unwrap());
    // And a second rollback is a no-op that changes nothing it should not.
    drop(c);
    roll(&w);
    assert_eq!(next_runs(&w, "a"), json!([t0() + 3 * HOUR]));
    let again = rollback(&w.reg(), &w.env, "schedules", opts()).unwrap();
    assert!(again.join("\n").contains("not switched"));
    let _ = find(&w.jobs(), "digest");
}

#[test]
fn a_cron_run_the_refusal_lets_pass_lands_in_state_json_and_one_whose_claim_ended_refuses() {
    let w = stepped(
        &[rt("a", &json!([hourly()]), &json!([t0() + HOUR]))],
        Some(t0()),
    );
    exec(
        &w,
        &format!(
            "INSERT OR REPLACE INTO sched_jobs (agent, job, def, paused, first_seen) VALUES ('mel', 'digest', '{}', 0, 0);",
            json!({"id": "digest", "cron": "0 * * * *"})
        ),
    );
    // A claim that has ended but kept its pid is no run going: `live` counts the turn and refuses.
    turn(&w, "digest", t0(), "running", Some((97, "s")));
    exec(&w, "UPDATE claims SET state = 'ended';");
    let err = rollback(&w.reg(), &w.env, "schedules", opts())
        .unwrap_err()
        .to_string();
    assert!(err.contains("scheduled turn(s) are running"), "{err}");
    // An ending claim with a process is still a run going: `live` lets it pass and it is written to `last`.
    exec(&w, "UPDATE claims SET state = 'ending', pid = 91;");
    roll(&w);
    assert_eq!(state_json(&w)["last"]["mel/digest"]["pid"], 91);
}
