//! The schedule fire (M4-3), one test per requirement of the architect's list (see the PR body's table), the crash
//! cases, and the daylight-saving days. Real core.db, real authority tables, no mocks of `SQLite`.
#![allow(
    unknown_lints,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::assert_is_empty
)]

use std::cell::Cell;

use kith_kernel::paths::Paths;
use kith_kernel::schedule::Trigger;
use kith_kernel::secrets::{Ctx, Sandbox, ServeDir};
use serde_json::{Value, json};
use tempfile::TempDir;

use super::fire::{
    Fire, Kind, Missed, Outcome, Step, TTL_MS, advance_in, deadline_of, fire, kind_of, run_now,
    tick, watermark_in,
};
use super::sync::Read;
use super::{index, mac_id};
use crate::authority::call::call_with;
use crate::conversations::claims::{self, Kind as ClaimKind, NewClaim, Redeemed};
use crate::db::Core;
use crate::error::Error;
use crate::layout::Layout;
use crate::tests::{machine, open_all, paths};

const HOUR: i64 = 3_600_000;
const DAY: i64 = 24 * HOUR;

fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> i64 {
    chrono::NaiveDate::from_ymd_opt(y, mo, d)
        .unwrap()
        .and_hms_opt(h, mi, 0)
        .unwrap()
        .and_utc()
        .timestamp_millis()
}

struct Rig {
    _t: TempDir,
    p: Paths,
    core: Core,
    ctx: Ctx,
    mac: String,
}

#[allow(clippy::unnecessary_wraps, reason = "the shape of `Fire::after`")]
fn ok(_: Step) -> crate::error::Result<()> {
    Ok(())
}

impl Rig {
    fn new() -> Self {
        let (t, m) = machine();
        let mut p = paths(&m);
        p.agents = m.join("agents");
        p.state = m.join("state");
        p.secrets = m.join("secrets");
        for a in ["mel", "ops"] {
            let d = p.agents.join(a).join("role");
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("agent.md"), format!("---\nname: {a}\n---\n")).unwrap();
        }
        let mut core = open_all(&Layout::new(&m)).with_folders(&p);
        claims::reconcile(&mut core, &[], &[], i64::MAX, 1).unwrap();
        let ctx = Ctx {
            auth: m.join("secrets/auth.json"),
            agents: p.agents.clone(),
            kernel: m.join("kernel"),
            person: None,
            now_ms: 1_800_000_000_000.0,
            sandbox: Sandbox::Fixed(false),
            serve: ServeDir::Fixed(m.join("serve")),
        };
        let mac = mac_id(&m).unwrap();
        let mut r = Self {
            _t: t,
            p,
            core,
            ctx,
            mac,
        };
        r.say("people.addPerson", &json!({"id": "kai", "owner": true}));
        for who in ["sam", "amy"] {
            r.say("people.addPerson", &json!({"id": who}));
        }
        r.say(
            "people.grant",
            &json!({"agent": "mel", "person": "sam", "preset": "member"}),
        );
        r
    }

    fn say(&mut self, method: &str, params: &Value) -> Value {
        call_with(&mut self.core, &self.ctx, method, params)
            .unwrap_or_else(|e| panic!("{method}: {e}"))
    }

    fn jobs(&self, agent: &str, defs: &Value) {
        std::fs::write(
            self.p.agents.join(agent).join("jobs.json"),
            defs.to_string(),
        )
        .unwrap();
    }

    fn tick(&mut self, now: i64) -> Value {
        tick(&mut self.core, &self.p, &self.ctx, now, &ok).unwrap()
    }

    fn set_wm(&self, ms: i64) {
        self.core
            .conn()
            .execute(
                "INSERT INTO sched_watermark (mac, at, updated_at) VALUES (?1, ?2, 0)
                 ON CONFLICT (mac) DO UPDATE SET at = excluded.at",
                rusqlite::params![self.mac, ms],
            )
            .unwrap();
    }

    fn wm(&self) -> Option<i64> {
        watermark_in(self.core.conn(), &self.mac).unwrap()
    }

    fn n(&self, sql: &str) -> i64 {
        self.core.conn().query_row(sql, [], |r| r.get(0)).unwrap()
    }

    /// `(slot, outcome)` of the job's fire rows, oldest slot first.
    fn rows(&self, job: &str) -> Vec<(String, String)> {
        let mut st = self
            .core
            .conn()
            .prepare("SELECT slot, outcome FROM sched_fires WHERE job = ?1 ORDER BY slot")
            .unwrap();
        st.query_map([job], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    fn outcomes(&self, job: &str) -> Vec<String> {
        self.rows(job).into_iter().map(|r| r.1).collect()
    }

    fn turn_state(&self, id: i64) -> String {
        self.core
            .conn()
            .query_row("SELECT state FROM turns WHERE id = ?1", [id], |r| r.get(0))
            .unwrap()
    }

    fn finish_all(&self) {
        self.core
            .conn()
            .execute(
                "UPDATE turns SET state = 'done', claim_id = NULL, parked_reason = NULL
                 WHERE state IN ('queued', 'parked')",
                [],
            )
            .unwrap();
    }

    /// One fire of `slot` as the tick would make it (the person and epoch read first).
    fn fire(&mut self, agent: &str, job: &str, slot: &str, now: i64, seen: Option<i64>) -> Outcome {
        let idx = index(&self.p, Some(agent));
        let a = &idx.agents[0];
        let read = Read {
            agent: &a.agent,
            hash: &a.hash,
            jobs: &a.jobs,
            paths: Some(&self.p),
        };
        let def = a
            .jobs
            .iter()
            .find(|j| super::sync::job_id(j) == job)
            .unwrap();
        let auth = crate::authority::tables_within(self.core.conn()).unwrap();
        let person = super::fire::person_of(def.get("person").and_then(Value::as_str), &auth);
        let seen = seen.unwrap_or_else(|| claims::revoke_epoch(&self.core, &person).unwrap());
        fire(
            &mut self.core,
            &Fire {
                read: &read,
                job,
                slot: Some(slot),
                missed: None,
                now,
                mac: &self.mac,
                person: &person,
                revoke_seen: seen,
                advance_to: None,
                ctx: &self.ctx,
                after: &ok,
            },
        )
        .unwrap()
    }

    fn run_now(&mut self, agent: &str, job: &str, key: &str, now: i64) -> Value {
        run_now(&mut self.core, &self.p, &self.ctx, agent, job, key, now).unwrap()
    }
}

fn daily(id: &str, cron: &str) -> Value {
    json!({"id": id, "cron": cron, "tz": "UTC", "task": format!("do {id}")})
}

fn fired(report: &Value) -> Vec<String> {
    report["fires"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["outcome"] == "fired" && f["decided"].is_null())
        .map(|f| f["job"].as_str().unwrap().to_owned())
        .collect()
}

// ---- 1. one write transaction per fire

#[test]
fn a_due_job_fires_into_a_turn_with_its_deadline_and_moves_the_watermark() {
    let mut r = Rig::new();
    r.jobs("mel", &json!([daily("daily", "30 8 * * *")]));
    r.set_wm(at(2026, 11, 3, 8, 29));
    let rep = r.tick(at(2026, 11, 3, 8, 30));
    assert_eq!(fired(&rep), ["daily"]);
    let (conv, person, source, prompt, deadline, state): (
        String,
        String,
        String,
        String,
        i64,
        String,
    ) = r
        .core
        .conn()
        .query_row(
            "SELECT conversation, person, source, prompt, deadline, state FROM turns",
            [],
            |x| {
                Ok((
                    x.get(0)?,
                    x.get(1)?,
                    x.get(2)?,
                    x.get(3)?,
                    x.get(4)?,
                    x.get(5)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(conv, "schedule:mel:daily:claude");
    assert_eq!((person.as_str(), source.as_str()), ("kai", "schedule"));
    assert_eq!((prompt.as_str(), state.as_str()), ("do daily", "queued"));
    assert_eq!(
        deadline,
        at(2026, 11, 4, 8, 30),
        "a timed slot expires at its next slot"
    );
    let turn: String = r
        .core
        .conn()
        .query_row("SELECT turn_id FROM sched_fires", [], |x| x.get(0))
        .unwrap();
    assert_eq!(turn, "1");
    assert_eq!(r.wm(), Some(at(2026, 11, 3, 8, 30)));
    // The next minute has nothing due.
    assert!(fired(&r.tick(at(2026, 11, 3, 8, 31))).is_empty());
    assert_eq!(r.n("SELECT count(*) FROM turns"), 1);
}

#[test]
fn a_failure_between_any_two_steps_leaves_nothing_and_the_slot_fires_on_the_next_tick() {
    for step in [
        Step::Row,
        Step::Sweep,
        Step::Enqueue,
        Step::TurnId,
        Step::Watermark,
    ] {
        let mut r = Rig::new();
        r.jobs("mel", &json!([daily("daily", "30 8 * * *")]));
        let before = at(2026, 11, 3, 8, 29);
        r.set_wm(before);
        // The failure is the job's: the tick answers, with the error in `problems`.
        let rep = tick(&mut r.core, &r.p, &r.ctx, at(2026, 11, 3, 8, 30), &|s| {
            if s == step {
                Err(Error::Io("injected".into()))
            } else {
                Ok(())
            }
        })
        .unwrap();
        assert!(
            rep["problems"].to_string().contains("injected"),
            "{step:?}: {rep}"
        );
        for t in ["sched_fires", "turns", "conversations"] {
            assert_eq!(
                r.n(&format!("SELECT count(*) FROM {t}")),
                0,
                "{step:?}: {t}"
            );
        }
        assert_eq!(r.wm(), Some(before), "{step:?}: the watermark did not move");
        // The retry decides the slot once.
        assert_eq!(
            fired(&r.tick(at(2026, 11, 3, 8, 30))),
            ["daily"],
            "{step:?}"
        );
        assert_eq!(r.n("SELECT count(*) FROM turns"), 1, "{step:?}");
        assert_eq!(r.n("SELECT count(*) FROM sched_fires"), 1, "{step:?}");
    }
}

#[test]
fn a_crash_between_two_jobs_keeps_the_first_whole_and_the_retry_fires_only_the_rest() {
    let mut r = Rig::new();
    r.jobs(
        "mel",
        &json!([daily("a", "30 8 * * *"), daily("b", "30 8 * * *")]),
    );
    r.set_wm(at(2026, 11, 3, 8, 29));
    let seen = Cell::new(0);
    let rep = tick(&mut r.core, &r.p, &r.ctx, at(2026, 11, 3, 8, 30), &|s| {
        if s == Step::Enqueue {
            seen.set(seen.get() + 1);
            if seen.get() == 2 {
                return Err(Error::Io("injected".into()));
            }
        }
        Ok(())
    })
    .unwrap();
    assert!(rep["problems"].to_string().contains("injected"), "{rep}");
    assert_eq!(
        r.n("SELECT count(*) FROM turns"),
        1,
        "job a committed whole"
    );
    assert_eq!(
        r.wm(),
        Some(at(2026, 11, 3, 8, 29)),
        "the watermark waits for the last fire"
    );
    let rep = r.tick(at(2026, 11, 3, 8, 30));
    assert_eq!(fired(&rep), ["b"], "a is decided already, b fires");
    assert_eq!(r.n("SELECT count(*) FROM turns"), 2);
    assert_eq!(r.n("SELECT count(*) FROM sched_fires"), 2);
}

#[test]
fn a_second_tick_for_the_same_slot_and_a_second_mac_are_no_ops() {
    let mut r = Rig::new();
    r.jobs("mel", &json!([daily("daily", "30 8 * * *")]));
    r.set_wm(at(2026, 11, 3, 8, 29));
    r.tick(at(2026, 11, 3, 8, 30));
    // The watermark is lost (a restore, another core): the same slot is due again.
    r.core
        .conn()
        .execute("DELETE FROM sched_watermark", [])
        .unwrap();
    r.set_wm(at(2026, 11, 3, 8, 29));
    let rep = r.tick(at(2026, 11, 3, 8, 30));
    assert!(fired(&rep).is_empty());
    assert_eq!(rep["fires"][0]["decided"], true);
    assert_eq!(rep["fires"][0]["outcome"], "fired");
    assert_eq!(r.n("SELECT count(*) FROM turns"), 1);
    // The key has no machine in it: another Mac deciding the same slot changes nothing.
    let slot = super::slot_of_ms(at(2026, 11, 3, 8, 30)).unwrap();
    r.mac = "another-mac".into();
    let out = r.fire("mel", "daily", &slot, at(2026, 11, 3, 8, 30), None);
    assert!(matches!(out, Outcome::Decided { .. }), "{out:?}");
    assert_eq!(r.n("SELECT count(*) FROM turns"), 1);
}

// ---- 2. the may check

#[test]
fn a_revoke_between_the_read_and_the_transaction_is_refused_inside_it() {
    let mut r = Rig::new();
    r.jobs(
        "mel",
        &json!([{"id": "sams", "cron": "30 8 * * *", "tz": "UTC", "task": "t", "person": "sam"}]),
    );
    let slot = super::slot_of_ms(at(2026, 11, 3, 8, 30)).unwrap();
    let seen = claims::revoke_epoch(&r.core, "sam").unwrap();
    // After the authority revoke is visible, the sweep.
    claims::revoke_person(&mut r.core, "sam", 5).unwrap();
    let out = r.fire("mel", "sams", &slot, at(2026, 11, 3, 8, 30), Some(seen));
    assert_eq!(out, Outcome::Skipped("revoked".into()));
    assert_eq!(r.outcomes("sams"), ["skipped:revoked"]);
    assert_eq!(r.n("SELECT count(*) FROM turns"), 0);
}

#[test]
fn a_suspended_person_and_a_person_without_access_get_a_row_with_the_reason() {
    let mut r = Rig::new();
    r.jobs(
        "mel",
        &json!([
            {"id": "sams", "cron": "30 8 * * *", "tz": "UTC", "task": "t", "person": "sam"},
            {"id": "amys", "cron": "30 8 * * *", "tz": "UTC", "task": "t", "person": "amy"},
            {"id": "mine", "cron": "30 8 * * *", "tz": "UTC", "task": "t"},
        ]),
    );
    r.say("people.suspend", &json!({"id": "sam"}));
    r.set_wm(at(2026, 11, 3, 8, 29));
    r.tick(at(2026, 11, 3, 8, 30));
    assert_eq!(r.outcomes("sams"), ["skipped:revoked"]);
    let amy = &r.outcomes("amys")[0];
    assert!(amy.starts_with("skipped:denied: "), "{amy}");
    assert_eq!(r.outcomes("mine"), ["fired"], "the owner's job runs");
    assert_eq!(r.n("SELECT count(*) FROM turns"), 1);
}

// ---- 3. the definition: the index row decides, after the hash

#[test]
fn the_fire_reindexes_when_the_file_changed_and_decides_on_the_new_row() {
    let mut r = Rig::new();
    r.jobs("mel", &json!([daily("daily", "30 8 * * *")]));
    r.set_wm(at(2026, 11, 3, 8, 29));
    r.tick(at(2026, 11, 3, 8, 30));
    let h1: String = r
        .core
        .conn()
        .query_row(
            "SELECT hash FROM sched_index WHERE agent = 'mel'",
            [],
            |x| x.get(0),
        )
        .unwrap();
    r.finish_all();
    // Hand edit (or git sync, or kith-sched): paused. The next tick sees the new hash.
    let mut d = daily("daily", "30 8 * * *");
    d["paused"] = json!(true);
    r.jobs("mel", &json!([d]));
    r.tick(at(2026, 11, 4, 8, 30));
    let h2: String = r
        .core
        .conn()
        .query_row(
            "SELECT hash FROM sched_index WHERE agent = 'mel'",
            [],
            |x| x.get(0),
        )
        .unwrap();
    assert_ne!(h1, h2);
    assert_eq!(r.n("SELECT paused FROM sched_jobs WHERE job = 'daily'"), 1);
    assert_eq!(r.outcomes("daily"), ["fired", "skipped:paused"]);
    // A fire handed a file the index has not seen indexes it in its own transaction.
    r.jobs("mel", &json!([daily("daily", "30 8 * * *")]));
    let slot = super::slot_of_ms(at(2026, 11, 5, 8, 30)).unwrap();
    let out = r.fire("mel", "daily", &slot, at(2026, 11, 5, 8, 30), None);
    assert!(matches!(out, Outcome::Fired { .. }), "{out:?}");
    assert_eq!(r.n("SELECT paused FROM sched_jobs WHERE job = 'daily'"), 0);
}

#[test]
fn a_job_gone_from_the_file_is_gone_in_the_index_and_its_waiting_turn_is_cancelled() {
    let mut r = Rig::new();
    r.jobs("mel", &json!([daily("daily", "30 8 * * *")]));
    r.set_wm(at(2026, 11, 3, 8, 29));
    r.tick(at(2026, 11, 3, 8, 30));
    r.jobs("mel", &json!([]));
    r.tick(at(2026, 11, 3, 8, 40));
    assert_eq!(r.n("SELECT count(*) FROM sched_jobs"), 0);
    assert_eq!(r.turn_state(1), "cancelled");
}

// ---- 4. the skip rule

#[test]
fn any_unfinished_turn_of_the_job_skips_a_slot_manual_and_event_runs_included() {
    let mut r = Rig::new();
    r.jobs("mel", &json!([daily("hourly", "0 * * * *")]));
    r.set_wm(at(2026, 11, 3, 8, 59));
    let running = |r: &Rig, id: i64| {
        r.core
            .conn()
            .execute("UPDATE turns SET state = 'running' WHERE id = ?1", [id])
            .unwrap();
    };
    let done = |r: &Rig, id: i64| {
        r.core
            .conn()
            .execute("UPDATE turns SET state = 'done' WHERE id = ?1", [id])
            .unwrap();
    };
    // A run by hand leaves a turn; the next timed slot finds the job busy.
    let m = r.run_now("mel", "hourly", "k1", at(2026, 11, 3, 8, 59));
    assert_eq!(m["outcome"], "fired");
    running(&r, 1);
    r.tick(at(2026, 11, 3, 9, 0));
    let nine = super::slot_of_ms(at(2026, 11, 3, 9, 0)).unwrap();
    assert!(r.rows("hourly").contains(&(
        nine,
        "skipped:busy: the previous run has not finished".into()
    )));
    // Done: the next slot fires. A webhook that finds that turn unfinished is busy and writes no key; once it is done
    // the webhook fires, and its turn blocks the slot after it.
    done(&r, 1);
    assert_eq!(fired(&r.tick(at(2026, 11, 3, 10, 0))), ["hourly"]);
    running(&r, 2);
    let out = r.fire("mel", "hourly", "event:e1", at(2026, 11, 3, 10, 5), None);
    assert_eq!(out, Outcome::Busy);
    assert_eq!(
        r.n("SELECT count(*) FROM sched_fires WHERE slot = 'event:e1'"),
        0
    );
    done(&r, 2);
    let out = r.fire("mel", "hourly", "event:e1", at(2026, 11, 3, 10, 6), None);
    assert!(matches!(out, Outcome::Fired { .. }), "{out:?}");
    running(&r, 3);
    r.tick(at(2026, 11, 3, 11, 0));
    let eleven = super::slot_of_ms(at(2026, 11, 3, 11, 0)).unwrap();
    assert!(
        r.rows("hourly")
            .iter()
            .any(|x| x.0 == eleven && x.1.starts_with("skipped:busy"))
    );
}

#[test]
fn a_turn_past_its_deadline_is_swept_in_the_fire_and_does_not_block_it() {
    let mut r = Rig::new();
    r.jobs("mel", &json!([daily("hourly", "0 * * * *")]));
    r.set_wm(at(2026, 11, 3, 8, 59));
    r.tick(at(2026, 11, 3, 9, 0));
    // Nobody claimed it; its deadline is the next slot. At that slot the sweep expires it and the new slot fires.
    let rep = r.tick(at(2026, 11, 3, 10, 0));
    assert_eq!(fired(&rep), ["hourly"]);
    assert_eq!(r.turn_state(1), "expired");
}

// ---- 5. pause and revoke cancel

#[test]
fn every_index_write_of_a_paused_job_cancels_what_waits_and_ends_an_unspawned_claim() {
    let mut r = Rig::new();
    r.jobs(
        "mel",
        &json!([daily("a", "30 8 * * *"), daily("b", "30 8 * * *")]),
    );
    r.set_wm(at(2026, 11, 3, 8, 29));
    r.tick(at(2026, 11, 3, 8, 30));
    // a's turn is claimed (no process yet); b's is running; a third, parked, belongs to a too.
    let claim = |r: &mut Rig, conv: &str, turn: i64| {
        claims::claim(
            &mut r.core,
            &NewClaim {
                conversation: conv,
                kind: ClaimKind::Turn,
                person: "kai",
                turn: Some(turn),
                holder_instance: Some("h1"),
                account: None,
                process: None,
                revoke_seen: 0,
            },
            at(2026, 11, 3, 8, 31),
        )
        .unwrap()
        .claim_id
    };
    let a_claim = claim(&mut r, "schedule:mel:a:claude", 1);
    let b_claim = claim(&mut r, "schedule:mel:b:claude", 2);
    assert_eq!(
        claims::redeem(&mut r.core, b_claim, 4242, "e1", "h1", 20).unwrap(),
        Redeemed::Running { again: false }
    );
    // Pause both: a file edit, then any tick.
    let (mut a, mut b) = (daily("a", "30 8 * * *"), daily("b", "30 8 * * *"));
    a["paused"] = json!(true);
    b["paused"] = json!(true);
    r.jobs("mel", &json!([a.clone(), b.clone()]));
    r.tick(at(2026, 11, 3, 9, 0));
    assert_eq!(
        r.turn_state(1),
        "cancelled",
        "claimed with no process: cancelled"
    );
    let (state, why): (String, String) = r
        .core
        .conn()
        .query_row(
            "SELECT state, end_reason FROM claims WHERE claim_id = ?1",
            [a_claim],
            |x| Ok((x.get(0)?, x.get(1)?)),
        )
        .unwrap();
    assert_eq!((state.as_str(), why.as_str()), ("ended", "paused"));
    assert_eq!(
        claims::redeem(&mut r.core, a_claim, 99, "e2", "h1", 30).unwrap(),
        Redeemed::Refused("ended".into()),
        "a late redeem of the cancelled claim fails its check-and-set"
    );
    assert_eq!(r.turn_state(2), "running", "a running turn is left alone");
    // Not only on the flip: a queued turn the pause missed is caught by the next write of the same paused row.
    r.finish_all();
    let late = claims::enqueue_turn(
        &mut r.core,
        &claims::NewTurn {
            conversation: "schedule:mel:a:claude",
            agent: "mel",
            engine: "claude",
            source: "schedule",
            person: "kai",
            prompt: "late",
            deadline: None,
            revoke_seen: 0,
        },
        40,
    )
    .unwrap();
    r.core
        .conn()
        .execute(
            "INSERT INTO sched_fires (agent, job, slot, outcome, turn_id, at) VALUES ('mel', 'a', 'manual:late', 'fired', ?1, 40)",
            [late.to_string()],
        )
        .unwrap();
    r.jobs("mel", &json!([a, b, daily("c", "0 0 1 1 *")]));
    r.tick(at(2026, 11, 3, 9, 1));
    assert_eq!(r.turn_state(late), "cancelled");
}

#[test]
fn revoke_person_cancels_waiting_turns_and_the_next_fire_is_skipped() {
    let mut r = Rig::new();
    r.jobs(
        "mel",
        &json!([{"id": "sams", "cron": "30 8 * * *", "tz": "UTC", "task": "t", "person": "sam"}]),
    );
    r.set_wm(at(2026, 11, 3, 8, 29));
    r.tick(at(2026, 11, 3, 8, 30));
    assert_eq!(r.turn_state(1), "queued");
    r.say("people.suspend", &json!({"id": "sam"}));
    claims::revoke_person(&mut r.core, "sam", at(2026, 11, 3, 9, 0)).unwrap();
    assert_eq!(r.turn_state(1), "cancelled");
    r.tick(at(2026, 11, 4, 8, 30));
    assert_eq!(r.outcomes("sams"), ["fired", "skipped:revoked"]);
}

// ---- 6. the deadline

#[test]
fn deadlines_follow_the_kind_of_slot_and_are_always_after_now() {
    let cron = Trigger::Cron {
        expr: "0 9 * * *".into(),
        tz: kith_kernel::schedule::Zone::Iana("UTC".into()),
    };
    let nine = at(2026, 11, 3, 9, 0);
    let now = nine + 5 * 60_000;
    // A timed slot: its next slot, even when the fire is late.
    assert_eq!(deadline_of(Kind::Timed, Some(&cron), nine, now), nine + DAY);
    // Run-now and webhooks: the earlier of the next slot after now and now + TTL.
    assert_eq!(deadline_of(Kind::Manual, Some(&cron), 0, now), now + TTL_MS);
    let near = at(2026, 11, 3, 10, 0);
    let hourly = Trigger::Cron {
        expr: "0 * * * *".into(),
        tz: kith_kernel::schedule::Zone::Iana("UTC".into()),
    };
    assert_eq!(deadline_of(Kind::Event, Some(&hourly), 0, now), near);
    // No timed trigger, or one with nothing after: the TTL.
    assert_eq!(
        deadline_of(Kind::Event, Some(&Trigger::Webhook), 0, now),
        now + TTL_MS
    );
    assert_eq!(deadline_of(Kind::Manual, None, 0, now), now + TTL_MS);
    let once = Trigger::Once { at: nine };
    assert_eq!(
        deadline_of(Kind::Timed, Some(&once), nine, now),
        now + TTL_MS
    );
}

#[test]
fn the_deadline_is_checked_at_the_claim_only() {
    let mut r = Rig::new();
    r.jobs("mel", &json!([daily("daily", "30 8 * * *")]));
    r.set_wm(at(2026, 11, 3, 8, 29));
    r.tick(at(2026, 11, 3, 8, 30));
    let req = |seen| NewClaim {
        conversation: "schedule:mel:daily:claude",
        kind: ClaimKind::Turn,
        person: "kai",
        turn: Some(1),
        holder_instance: Some("h1"),
        account: None,
        process: None,
        revoke_seen: seen,
    };
    let late = at(2026, 11, 4, 8, 30);
    assert!(
        claims::claim(&mut r.core, &req(0), late).is_err(),
        "past the deadline"
    );
    claims::claim(&mut r.core, &req(0), late - 1).unwrap();
}

// ---- 7. rows with reasons, missed slots

#[test]
fn a_slot_missed_past_the_window_keeps_one_row_and_the_latest_in_window_fires() {
    let mut r = Rig::new();
    r.jobs("mel", &json!([daily("daily", "0 9 * * *")]));
    r.set_wm(at(2026, 11, 1, 9, 30));
    // The Mac slept from day 1 to the 5th, 10:00: slots on the 2nd, 3rd, 4th (older than a day) and the 5th.
    let rep = r.tick(at(2026, 11, 5, 10, 0));
    assert_eq!(fired(&rep), ["daily"]);
    let rows = r.rows("daily");
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(
        rows[0].0.starts_with("2026-11-04T09:00")
            && rows[0].1.starts_with("skipped:missed (3 slot(s)"),
        "{rows:?}"
    );
    assert!(
        rows[1].0.starts_with("2026-11-05T09:00") && rows[1].1 == "fired",
        "{rows:?}"
    );
    assert_eq!(r.wm(), Some(at(2026, 11, 5, 10, 0)));
    // A later sleep: the slot of the 6th is older than the window and kept as a row, the 7th fires.
    r.finish_all();
    r.set_wm(at(2026, 11, 5, 9, 30));
    let rep = r.tick(at(2026, 11, 7, 10, 0));
    assert_eq!(fired(&rep), ["daily"], "the 7th is inside the window");
}

#[test]
fn a_paused_revoked_and_busy_slot_each_keep_a_row_with_the_reason() {
    let mut r = Rig::new();
    let mut p = daily("paused", "30 8 * * *");
    p["paused"] = json!(true);
    r.jobs("mel", &json!([p, daily("busy", "30 8 * * *")]));
    r.set_wm(at(2026, 11, 3, 8, 29));
    r.tick(at(2026, 11, 3, 8, 30));
    // Still running a day later (a deadline would have swept a turn nobody took).
    r.core
        .conn()
        .execute("UPDATE turns SET state = 'running'", [])
        .unwrap();
    r.tick(at(2026, 11, 4, 8, 30));
    assert_eq!(r.outcomes("paused"), ["skipped:paused", "skipped:paused"]);
    assert_eq!(
        r.outcomes("busy"),
        ["fired", "skipped:busy: the previous run has not finished"]
    );
}

// ---- 8. run now

#[test]
fn run_now_busy_writes_no_key_and_a_decided_key_answers_what_was_decided() {
    let mut r = Rig::new();
    r.jobs("mel", &json!([daily("daily", "30 8 * * *")]));
    let now = at(2026, 11, 3, 12, 0);
    let first = r.run_now("mel", "daily", "k1", now);
    assert_eq!(first["outcome"], "fired");
    // Busy: no key, answered busy, and the retry later fires once the job is free.
    let busy = r.run_now("mel", "daily", "k2", now + 1000);
    assert_eq!(
        (busy["outcome"].as_str(), busy["retry"].as_bool()),
        (Some("busy"), Some(true))
    );
    assert_eq!(
        r.n("SELECT count(*) FROM sched_fires WHERE slot = 'manual:k2'"),
        0
    );
    // A retry of the decided key answers the recorded outcome and makes no second turn.
    let again = r.run_now("mel", "daily", "k1", now + 2000);
    assert_eq!(
        (again["decided"].as_bool(), again["outcome"].as_str()),
        (Some(true), Some("fired"))
    );
    assert_eq!(again["turn"], first["turn"]);
    assert_eq!(r.n("SELECT count(*) FROM turns"), 1);
    r.finish_all();
    assert_eq!(
        r.run_now("mel", "daily", "k2", now + 3000)["outcome"],
        "fired"
    );
    // The deadline of a run-now: the earlier of the next slot and now + TTL.
    let dl: i64 = r.n("SELECT deadline FROM turns WHERE id = 2");
    assert_eq!(dl, (now + 3000 + TTL_MS).min(at(2026, 11, 4, 8, 30)));
}

#[test]
fn run_now_on_a_paused_or_revoked_job_writes_the_row_and_answers_final() {
    let mut r = Rig::new();
    let mut p = daily("paused", "30 8 * * *");
    p["paused"] = json!(true);
    r.jobs(
        "mel",
        &json!([p, {"id": "sams", "cron": "30 8 * * *", "tz": "UTC", "task": "t", "person": "sam"}]),
    );
    let now = at(2026, 11, 3, 12, 0);
    let a = r.run_now("mel", "paused", "k1", now);
    assert_eq!(
        (a["outcome"].as_str(), a["final"].as_bool()),
        (Some("skipped:paused"), Some(true))
    );
    assert_eq!(r.outcomes("paused"), ["skipped:paused"]);
    r.say("people.suspend", &json!({"id": "sam"}));
    let b = r.run_now("mel", "sams", "k1", now);
    assert_eq!(
        (b["outcome"].as_str(), b["final"].as_bool()),
        (Some("skipped:revoked"), Some(true))
    );
    // The decided id answers the same on retry.
    let b2 = r.run_now("mel", "sams", "k1", now + 1);
    assert_eq!(
        (b2["outcome"].as_str(), b2["decided"].as_bool()),
        (Some("skipped:revoked"), Some(true))
    );
    assert_eq!(r.n("SELECT count(*) FROM turns"), 0);
}

#[test]
fn run_now_refuses_a_bad_key_and_an_unknown_job() {
    let mut r = Rig::new();
    r.jobs("mel", &json!([daily("daily", "30 8 * * *")]));
    let e = |r: &mut Rig, job: &str, key: &str| {
        run_now(&mut r.core, &r.p, &r.ctx, "mel", job, key, 1).unwrap_err()
    };
    assert!(matches!(e(&mut r, "daily", "a b"), Error::BadParams(_)));
    assert!(matches!(e(&mut r, "nope", "k"), Error::BadParams(_)));
}

// ---- 9. catch-up

#[test]
fn catch_up_fires_only_the_latest_due_slot_inside_the_window() {
    let mut r = Rig::new();
    r.jobs("mel", &json!([daily("hourly", "0 * * * *")]));
    r.set_wm(at(2026, 11, 3, 2, 0));
    let rep = r.tick(at(2026, 11, 3, 7, 30));
    assert_eq!(fired(&rep), ["hourly"]);
    let rows = r.rows("hourly");
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(
        rows[0].0.starts_with("2026-11-03T06:00")
            && rows[0].1.starts_with("skipped:missed (4 slot(s)"),
        "{rows:?}"
    );
    assert!(rows[1].0.starts_with("2026-11-03T07:00"));
    assert_eq!(r.n("SELECT count(*) FROM turns"), 1);
    // The edge of the day: a slot exactly a day old is forgotten, one a minute inside the window is run.
    let mut r = Rig::new();
    r.jobs(
        "mel",
        &json!([daily("edge", "30 8 2 11 *"), daily("inside", "31 8 2 11 *")]),
    );
    r.set_wm(at(2026, 11, 1, 8, 0));
    let rep = r.tick(at(2026, 11, 3, 8, 30));
    assert_eq!(fired(&rep), ["inside"]);
    assert!(r.outcomes("edge")[0].starts_with("skipped:missed (1 slot(s)"));
}

#[test]
fn the_first_tick_with_no_watermark_covers_the_last_minute_only() {
    let mut r = Rig::new();
    r.jobs("mel", &json!([daily("daily", "30 8 * * *")]));
    let rep = r.tick(at(2026, 11, 3, 8, 30));
    assert_eq!(fired(&rep), ["daily"]);
    let mut r = Rig::new();
    r.jobs("mel", &json!([daily("daily", "30 8 * * *")]));
    assert!(fired(&r.tick(at(2026, 11, 3, 9, 30))).is_empty());
    assert_eq!(r.wm(), Some(at(2026, 11, 3, 9, 30)));
}

// ---- 10. the watermark

#[test]
fn the_watermark_never_moves_backwards_and_reads_an_imported_local_tick() {
    let r = Rig::new();
    let tx = r.core.conn().unchecked_transaction().unwrap();
    advance_in(&tx, "m", 5_000, None, 1).unwrap();
    advance_in(&tx, "m", 3_000, None, 2).unwrap();
    advance_in(&tx, "m", 1_000, Some(5_000), 3).unwrap();
    tx.commit().unwrap();
    let got: i64 = r
        .core
        .conn()
        .query_row("SELECT at FROM sched_watermark WHERE mac = 'm'", [], |x| {
            x.get(0)
        })
        .unwrap();
    assert_eq!(got, 5_000);
    // A machine imported before migration 121 has only the local-minute text; it is converted when read.
    r.core
        .conn()
        .execute(
            "INSERT INTO sched_tick (mac, tick, updated_at) VALUES ('old', '2026-11-04T11:59', 0)",
            [],
        )
        .unwrap();
    let want = super::slot_of_local("2026-11-04T11:59")
        .and_then(|s| super::ms_of_slot(&s))
        .unwrap();
    assert_eq!(watermark_in(r.core.conn(), "old").unwrap(), Some(want));
    assert_eq!(watermark_in(r.core.conn(), "never").unwrap(), None);
}

#[test]
fn the_import_writes_the_watermark_beside_the_tick() {
    let (t, m) = machine();
    let mut p = paths(&m);
    p.agents = m.join("agents");
    std::fs::create_dir_all(m.join("sched")).unwrap();
    std::fs::write(
        m.join("sched/state.json"),
        r#"{"tick": "2026-11-04T11:59"}"#,
    )
    .unwrap();
    crate::import::schedules::run(&Layout::new(&m), &m, false).unwrap();
    let core = open_all(&Layout::new(&m));
    let mac = mac_id(&m).unwrap();
    let want = super::slot_of_local("2026-11-04T11:59")
        .and_then(|s| super::ms_of_slot(&s))
        .unwrap();
    let got: i64 = core
        .conn()
        .query_row(
            "SELECT at FROM sched_watermark WHERE mac = ?1",
            [&mac],
            |x| x.get(0),
        )
        .unwrap();
    assert_eq!(got, want);
    drop(t);
}

// ---- 11. nothing runs it

#[test]
fn the_rows_are_experimental_core_rows_and_the_plain_call_does_not_fire() {
    for m in ["schedule.tick", "schedule.runNow"] {
        let row = kith_api::method(m).unwrap();
        assert_eq!(row.stability, kith_api::Stability::Experimental, "{m}");
        assert_eq!(row.served, kith_api::Served::Core, "{m}");
        assert!(super::serves_write(m) && !super::serves(m));
    }
    let r = Rig::new();
    let e = r
        .core
        .call(kith_api::Principal::Owner, "schedule.tick", &json!({}))
        .unwrap_err();
    assert!(matches!(e, Error::NotServed(_)), "{e}");
}

#[test]
fn slots_have_a_kind() {
    assert_eq!(kind_of("2026-11-03T08:30:00Z"), Kind::Timed);
    assert_eq!(kind_of("manual:k"), Kind::Manual);
    assert_eq!(kind_of("event:e"), Kind::Event);
    let _ = Missed {
        key_ms: 0,
        first_ms: 0,
        count: 0,
        more: false,
    };
}

// ---- daylight saving, in the job's zone (Trigger) with a UTC watermark

/// Ticks every `step` ms over `[from, to]`, finishing each turn, and returns the fired slots.
fn run_over(r: &mut Rig, from: i64, to: i64, step: i64) -> Vec<String> {
    let mut t = from;
    while t <= to {
        r.tick(t);
        r.finish_all();
        t += step;
    }
    r.rows("half-two")
        .into_iter()
        .filter(|x| x.1 == "fired")
        .map(|x| x.0)
        .collect()
}

fn sydney(id: &str, cron: &str) -> Value {
    json!({"id": id, "cron": cron, "tz": "Australia/Sydney", "task": "t"})
}

#[test]
fn sydney_spring_forward_fires_the_skipped_half_two_once() {
    // 2026-10-04: 02:00 AEST jumps to 03:00 AEDT, so 02:30 does not exist.
    let mut r = Rig::new();
    r.jobs("mel", &json!([sydney("half-two", "30 2 * * *")]));
    let from = at(2026, 10, 3, 12, 0);
    r.set_wm(from);
    let slots = run_over(&mut r, from + 60_000, at(2026, 10, 4, 12, 0), 10 * 60_000);
    assert_eq!(slots.len(), 1, "once, not lost: {slots:?}");
    assert!(
        slots[0].starts_with("2026-10-03T16:00"),
        "03:00 AEDT is 16:00Z the day before: {slots:?}"
    );
}

#[test]
fn sydney_fall_back_fires_half_two_once_not_twice() {
    // 2026-04-05: 03:00 AEDT goes back to 02:00 AEST, so 02:30 happens twice (15:30Z and 16:30Z).
    let mut r = Rig::new();
    r.jobs("mel", &json!([sydney("half-two", "30 2 * * *")]));
    let from = at(2026, 4, 4, 14, 0);
    r.set_wm(from);
    let slots = run_over(&mut r, from + 60_000, at(2026, 4, 5, 3, 0), 10 * 60_000);
    let on_day: Vec<_> = slots
        .iter()
        .filter(|s| s.starts_with("2026-04-04T1"))
        .collect();
    assert_eq!(on_day.len(), 1, "{slots:?}");
    assert!(
        on_day[0].starts_with("2026-04-04T15:30"),
        "the first 02:30: {slots:?}"
    );
    // The watermark is an instant, so the repeated hour does not make it move back.
    let wm = r.wm().unwrap();
    assert!(
        wm > at(2026, 4, 5, 2, 0) && wm <= at(2026, 4, 5, 3, 0),
        "{wm}"
    );
}

#[test]
fn a_tick_that_slept_through_the_repeated_hour_still_fires_once() {
    let mut r = Rig::new();
    r.jobs("mel", &json!([sydney("half-two", "30 2 * * *")]));
    r.set_wm(at(2026, 4, 4, 14, 0));
    r.tick(at(2026, 4, 4, 17, 0));
    assert_eq!(r.outcomes("half-two"), ["fired"]);
}

// ---- 12. the trigger never returns a time at or before `after` (the host routines' fall-back bug)

#[test]
fn new_york_fall_back_never_returns_a_time_at_or_before_after() {
    let t = Trigger::Cron {
        expr: "30 1 * * *".into(),
        tz: kith_kernel::schedule::Zone::Iana("America/New_York".into()),
    };
    // 2026-11-01: 01:30 EDT is 05:30Z, 01:30 EST (the second pass) would be 06:30Z.
    let after = at(2026, 11, 1, 6, 0);
    let next = t.next_after(after).unwrap();
    assert!(next > after, "{next} <= {after}");
    // Whatever a tick stores as the next time is after the clock it read.
    for now in [at(2026, 11, 1, 5, 30), at(2026, 11, 1, 6, 10)] {
        assert!(t.next_after(now).unwrap() > now);
    }
    // The fire: a slot due at 05:30Z, ticked at 06:10Z, gets a deadline after now.
    let d = deadline_of(
        Kind::Timed,
        Some(&t),
        at(2026, 11, 1, 5, 30),
        at(2026, 11, 1, 6, 10),
    );
    assert!(d > at(2026, 11, 1, 6, 10));
}

// ---- the architect's round on #762

fn paused_daily() -> Value {
    json!({"id": "daily", "cron": "30 8 * * *", "tz": "UTC", "task": "t", "paused": true})
}

#[test]
fn a_stale_read_applied_after_a_fresh_one_never_unpauses_the_job() {
    let mut r = Rig::new();
    r.jobs("mel", &json!([daily("daily", "30 8 * * *")]));
    r.tick(at(2026, 11, 3, 8, 0));
    // A run-now reads v1 (active); kith-sched writes v2 (paused); a tick applies v2.
    let old = index(&r.p, Some("mel"));
    r.jobs("mel", &json!([paused_daily()]));
    r.tick(at(2026, 11, 3, 8, 1));
    let paused = |r: &Rig| r.n("SELECT paused FROM sched_jobs WHERE job = 'daily'");
    assert_eq!(paused(&r), 1);
    // The run-now's transaction now applies its v1 read: refused, nothing written.
    let a = &old.agents[0];
    let read = Read {
        agent: &a.agent,
        hash: &a.hash,
        jobs: &a.jobs,
        paths: Some(&r.p),
    };
    let slot = super::slot_of_ms(at(2026, 11, 3, 8, 30)).unwrap();
    let seen = claims::revoke_epoch(&r.core, "kai").unwrap();
    let out = fire(
        &mut r.core,
        &Fire {
            read: &read,
            job: "daily",
            slot: Some(&slot),
            missed: None,
            now: at(2026, 11, 3, 8, 30),
            mac: &r.mac,
            person: "kai",
            revoke_seen: seen,
            advance_to: None,
            ctx: &r.ctx,
            after: &ok,
        },
    )
    .unwrap();
    assert_eq!(out, Outcome::Stale);
    assert_eq!(paused(&r), 1, "still paused");
    assert_eq!(r.n("SELECT count(*) FROM turns"), 0);
    assert_eq!(r.n("SELECT count(*) FROM sched_fires"), 0);
    // The retry reads the file again and finds the job paused.
    let v = r.run_now("mel", "daily", "k1", at(2026, 11, 3, 8, 31));
    assert_eq!(v["outcome"], "skipped:paused");
    // And a removed job stays removed.
    r.jobs("mel", &json!([]));
    r.tick(at(2026, 11, 3, 8, 32));
    assert_eq!(r.n("SELECT count(*) FROM sched_jobs"), 0);
    let read = Read {
        agent: &a.agent,
        hash: &a.hash,
        jobs: &a.jobs,
        paths: Some(&r.p),
    };
    let out = fire(
        &mut r.core,
        &Fire {
            read: &read,
            job: "daily",
            slot: Some(&slot),
            missed: None,
            now: at(2026, 11, 3, 8, 33),
            mac: &r.mac,
            person: "kai",
            revoke_seen: seen,
            advance_to: None,
            ctx: &r.ctx,
            after: &ok,
        },
    )
    .unwrap();
    assert_eq!(out, Outcome::Stale);
    assert_eq!(r.n("SELECT count(*) FROM sched_jobs"), 0, "not revived");
}

fn every(id: &str, secs: u64) -> Value {
    json!({"id": id, "trigger": {"type": "interval", "every": secs}, "task": format!("do {id}")})
}

#[test]
fn a_five_minute_interval_fires_at_minute_five_and_leaves_rows() {
    let mut r = Rig::new();
    r.jobs("mel", &json!([every("iv", 300)]));
    // No fire yet: the first slot is the time the file was indexed plus the interval.
    for m in 0..5 {
        assert!(
            fired(&r.tick(at(2026, 11, 3, 8, m))).is_empty(),
            "minute {m}"
        );
    }
    assert_eq!(fired(&r.tick(at(2026, 11, 3, 8, 5))), ["iv"]);
    let rows = r.rows("iv");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(rows[0].0.starts_with("2026-11-03T08:05"), "{rows:?}");
    // The next one counts from that slot, not from the watermark.
    r.finish_all();
    for m in 6..10 {
        assert!(
            fired(&r.tick(at(2026, 11, 3, 8, m))).is_empty(),
            "minute {m}"
        );
    }
    assert_eq!(fired(&r.tick(at(2026, 11, 3, 8, 10))), ["iv"]);
    assert_eq!(r.rows("iv").len(), 2);
}

#[test]
fn an_agent_whose_file_cannot_be_read_is_planned_from_its_index_row() {
    let mut r = Rig::new();
    r.jobs("mel", &json!([daily("daily", "30 8 * * *")]));
    r.tick(at(2026, 11, 3, 8, 0));
    std::fs::write(r.p.agents.join("mel").join("jobs.json"), "{ not json").unwrap();
    let rep = r.tick(at(2026, 11, 3, 8, 30));
    assert!(
        rep["problems"].to_string().contains("mel"),
        "the problem is said: {rep}"
    );
    assert_eq!(fired(&rep), ["daily"], "the index decides");
    assert_eq!(r.outcomes("daily"), ["fired"]);
    assert_eq!(r.wm(), Some(at(2026, 11, 3, 8, 30)));
}

#[test]
fn one_failing_job_does_not_stop_the_others_or_move_the_watermark() {
    let mut r = Rig::new();
    r.jobs(
        "mel",
        &json!([daily("a", "30 8 * * *"), daily("b", "30 8 * * *")]),
    );
    let before = at(2026, 11, 3, 8, 29);
    r.set_wm(before);
    let seen = Cell::new(0);
    let rep = tick(&mut r.core, &r.p, &r.ctx, at(2026, 11, 3, 8, 30), &|s| {
        if s == Step::Enqueue {
            seen.set(seen.get() + 1);
            if seen.get() == 1 {
                return Err(Error::Io("injected".into()));
            }
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(fired(&rep), ["b"], "{rep}");
    assert!(rep["problems"].to_string().contains("injected"));
    assert_eq!(r.outcomes("a"), Vec::<String>::new(), "a left no half row");
    assert_eq!(r.wm(), Some(before), "the failed slot is not passed");
    // The next tick fires a and b is decided already.
    assert_eq!(fired(&r.tick(at(2026, 11, 3, 8, 30))), ["a"]);
    assert_eq!(r.wm(), Some(at(2026, 11, 3, 8, 30)));
}

#[test]
fn the_daemon_path_ignores_a_callers_now() {
    let mut r = Rig::new();
    r.jobs("mel", &json!([daily("daily", "30 8 * * *")]));
    let far = at(2099, 1, 1, 0, 0);
    super::fire::call_write(&mut r.core, "schedule.tick", &json!({"now": far})).unwrap();
    assert!(
        r.wm().is_none_or(|w| w < far),
        "a future `now` did not move the watermark"
    );
}

#[test]
fn schedule_jobs_next_is_the_one_the_fire_uses() {
    use super::{Rule as _, TriggerRule};
    let ny = json!({"id": "j", "cron": "30 8 * * *", "tz": "America/New_York", "task": "t"});
    let now = at(2026, 11, 3, 0, 0);
    let a = TriggerRule.next(&ny, now).unwrap();
    let t = super::sync::trigger_of(&ny).unwrap();
    assert_eq!(
        a,
        t.next_after(now).and_then(|n| {
            use chrono::TimeZone as _;
            chrono::Local
                .timestamp_millis_opt(n)
                .single()
                .map(|d| d.format("%Y-%m-%dT%H:%M").to_string())
        })
    );
    // A trigger object is read too (it was "bad cron: no cron" before).
    let iv = every("iv", 300);
    assert!(TriggerRule.next(&iv, now).unwrap().is_some());
}

// ---- M4-4a: the follow-ups of the fire review

#[test]
fn a_pause_written_after_the_settle_is_seen_by_the_fire_even_when_the_hash_is_the_indexs() {
    let mut r = Rig::new();
    r.jobs("mel", &json!([daily("daily", "30 8 * * *")]));
    r.tick(at(2026, 11, 3, 8, 0));
    // The tick's read of the file, already indexed (the settle applied it).
    let old = index(&r.p, Some("mel"));
    let a = &old.agents[0];
    // Then the pause lands in the file, before the fire's transaction.
    r.jobs("mel", &json!([paused_daily()]));
    let read = Read {
        agent: &a.agent,
        hash: &a.hash,
        jobs: &a.jobs,
        paths: Some(&r.p),
    };
    let slot = super::slot_of_ms(at(2026, 11, 3, 8, 30)).unwrap();
    let seen = claims::revoke_epoch(&r.core, "kai").unwrap();
    let out = fire(
        &mut r.core,
        &Fire {
            read: &read,
            job: "daily",
            slot: Some(&slot),
            missed: None,
            now: at(2026, 11, 3, 8, 30),
            mac: &r.mac,
            person: "kai",
            revoke_seen: seen,
            advance_to: None,
            ctx: &r.ctx,
            after: &ok,
        },
    )
    .unwrap();
    assert_eq!(out, Outcome::Stale, "not fired over the pause");
    assert_eq!(r.n("SELECT count(*) FROM turns"), 0);
    assert_eq!(r.n("SELECT count(*) FROM sched_fires"), 0);
    // The next tick reads the pause and decides on it.
    r.tick(at(2026, 11, 3, 8, 30));
    assert_eq!(r.outcomes("daily"), ["skipped:paused"]);
}

#[test]
fn an_unfired_intervals_first_fire_does_not_move_when_another_job_in_the_file_changes() {
    let mut r = Rig::new();
    r.jobs(
        "mel",
        &json!([every("iv", 300), daily("other", "0 3 * * *")]),
    );
    r.tick(at(2026, 11, 3, 8, 0));
    let first = r.n("SELECT first_seen FROM sched_jobs WHERE job = 'iv'");
    assert_eq!(first, at(2026, 11, 3, 8, 0));
    // Edit, then pause, the other job: the file is re-indexed each time.
    r.jobs(
        "mel",
        &json!([every("iv", 300), daily("other", "0 4 * * *")]),
    );
    r.tick(at(2026, 11, 3, 8, 2));
    r.jobs("mel", &json!([every("iv", 300), paused_daily()]));
    r.tick(at(2026, 11, 3, 8, 3));
    assert_eq!(
        r.n("SELECT first_seen FROM sched_jobs WHERE job = 'iv'"),
        first,
        "an update leaves first_seen alone"
    );
    assert!(fired(&r.tick(at(2026, 11, 3, 8, 4))).is_empty());
    assert_eq!(fired(&r.tick(at(2026, 11, 3, 8, 5))), ["iv"]);
}

#[test]
fn a_job_that_fails_three_ticks_running_gets_an_error_row_and_the_watermark_moves() {
    let mut r = Rig::new();
    r.jobs(
        "mel",
        &json!([daily("a", "30 8 * * *"), daily("b", "30 8 * * *")]),
    );
    let before = at(2026, 11, 3, 8, 29);
    r.set_wm(before);
    // a fails at its enqueue every time; b (the second enqueue of the first tick) goes through.
    let seen = Cell::new(0);
    let run = |r: &mut Rig, now: i64| {
        tick(&mut r.core, &r.p, &r.ctx, now, &|s| {
            if s == Step::Enqueue {
                seen.set(seen.get() + 1);
                if seen.get() != 2 {
                    return Err(Error::Io("injected".into()));
                }
            }
            Ok(())
        })
        .unwrap()
    };
    assert_eq!(fired(&run(&mut r, at(2026, 11, 3, 8, 30))), ["b"]);
    run(&mut r, at(2026, 11, 3, 8, 31));
    assert_eq!(r.wm(), Some(before), "held for the first two ticks");
    assert!(r.outcomes("a").is_empty());
    let rep = run(&mut r, at(2026, 11, 3, 8, 32));
    let rows = r.rows("a");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(rows[0].1.starts_with("skipped:error: "), "{rows:?}");
    assert!(rows[0].1.contains("injected"), "{rows:?}");
    assert_eq!(
        rep["watermark"],
        json!(at(2026, 11, 3, 8, 32)),
        "the tick that gave up moves the watermark: a's slot has its row"
    );
    // b is not planned again, and a's old slot stays decided.
    let rep = run(&mut r, at(2026, 11, 3, 8, 33));
    assert_eq!(r.wm(), Some(at(2026, 11, 3, 8, 33)), "{rep}");
    assert_eq!(r.outcomes("b"), ["fired"]);
    // Fixed: the old slot does not come back as missed, and the next day fires again.
    assert_eq!(
        r.n("SELECT count(*) FROM sched_fires WHERE outcome LIKE 'skipped:missed%'"),
        0
    );
    r.finish_all();
    let rep = r.tick(at(2026, 11, 4, 8, 30));
    assert_eq!(fired(&rep), ["a", "b"], "{rep}");
}

#[test]
fn a_failure_that_stops_before_the_limit_starts_the_count_again() {
    let mut r = Rig::new();
    r.jobs("mel", &json!([daily("a", "30 8 * * *")]));
    r.set_wm(at(2026, 11, 3, 8, 29));
    let go = |r: &mut Rig, now: i64, yes: bool| {
        tick(&mut r.core, &r.p, &r.ctx, now, &|s| {
            if yes && s == Step::Enqueue {
                return Err(Error::Io("injected".into()));
            }
            Ok(())
        })
        .unwrap()
    };
    go(&mut r, at(2026, 11, 3, 8, 30), true);
    go(&mut r, at(2026, 11, 3, 8, 31), true);
    assert_eq!(r.n("SELECT fail_count FROM sched_jobs WHERE job = 'a'"), 2);
    // It goes through, so the run of failures is over.
    assert_eq!(fired(&go(&mut r, at(2026, 11, 3, 8, 32), false)), ["a"]);
    assert_eq!(r.n("SELECT fail_count FROM sched_jobs WHERE job = 'a'"), 0);
    assert_eq!(r.outcomes("a"), ["fired"]);
}

fn minute(id: &str) -> Value {
    daily(id, "* * * * *")
}

#[test]
fn a_minute_job_that_fails_every_tick_stops_holding_the_watermark() {
    let mut r = Rig::new();
    r.jobs("mel", &json!([minute("bad"), minute("good")]));
    let start = at(2026, 11, 3, 8, 29);
    r.set_wm(start);
    // `bad` is the first enqueue of a tick and always fails; `good` is the second and goes through. Its turn is
    // finished first, so its skip rule does not turn the next tick's enqueue into a skip.
    let seen = Cell::new(0);
    let run = |r: &mut Rig, now: i64| {
        r.finish_all();
        tick(&mut r.core, &r.p, &r.ctx, now, &|s| {
            if s == Step::Enqueue {
                seen.set(seen.get() + 1);
                if seen.get() % 2 == 1 {
                    return Err(Error::Io("injected".into()));
                }
            }
            Ok(())
        })
        .unwrap()
    };
    // The slot changes every tick (plan fires the latest due one); the count is the job's, not the slot's.
    run(&mut r, at(2026, 11, 3, 8, 30));
    run(&mut r, at(2026, 11, 3, 8, 31));
    assert_eq!(r.wm(), Some(start), "held for the first two ticks");
    assert!(
        r.rows("bad").is_empty(),
        "nothing recorded for bad yet: {:?}",
        r.rows("bad")
    );
    let rep = run(&mut r, at(2026, 11, 3, 8, 32));
    assert_eq!(r.wm(), Some(at(2026, 11, 3, 8, 32)), "{rep}");
    let rep = run(&mut r, at(2026, 11, 3, 8, 33));
    assert_eq!(r.wm(), Some(at(2026, 11, 3, 8, 33)), "{rep}");
    // Every slot the watermark passed holds a row for bad: the missed span the failed fire rolled back, and the error.
    let rows = r.rows("bad");
    assert!(
        rows.iter()
            .any(|(s, o)| s == "2026-11-03T08:33:00Z" && o.starts_with("skipped:error: ")),
        "{rows:?}"
    );
    assert!(
        rows.iter()
            .any(|(s, o)| s == "2026-11-03T08:32:00Z" && o.starts_with("skipped:error: ")),
        "{rows:?}"
    );
    assert!(
        rows.iter().any(|(_, o)| o.starts_with("skipped:missed")),
        "the missed span of the third tick: {rows:?}"
    );
    // The healthy job fired on every tick, and no turn was made for bad.
    assert_eq!(
        r.outcomes("good").iter().filter(|o| *o == "fired").count(),
        4
    );
    assert_eq!(
        r.n("SELECT count(*) FROM turns WHERE conversation LIKE '%bad%'"),
        0
    );
}

#[test]
fn restarts_inside_one_minute_count_as_one_failing_tick() {
    let mut r = Rig::new();
    r.jobs("mel", &json!([daily("a", "30 8 * * *")]));
    r.set_wm(at(2026, 11, 3, 8, 29));
    let go = |r: &mut Rig, now: i64| {
        tick(&mut r.core, &r.p, &r.ctx, now, &|s| {
            if s == Step::Enqueue {
                return Err(Error::Io("injected".into()));
            }
            Ok(())
        })
        .unwrap()
    };
    for _ in 0..5 {
        go(&mut r, at(2026, 11, 3, 8, 30));
    }
    assert_eq!(r.n("SELECT fail_count FROM sched_jobs WHERE job = 'a'"), 1);
    assert!(r.rows("a").is_empty());
    assert_eq!(r.wm(), Some(at(2026, 11, 3, 8, 29)));
}

/// Every enqueue of every tick fails.
fn all_fail(r: &mut Rig, minutes: std::ops::Range<u32>) {
    for m in minutes {
        tick(&mut r.core, &r.p, &r.ctx, at(2026, 11, 3, 8, m), &|s| {
            if s == Step::Enqueue {
                return Err(Error::Io("injected".into()));
            }
            Ok(())
        })
        .unwrap();
    }
}

#[test]
fn a_tick_where_every_due_job_fails_is_counted_with_a_longer_limit() {
    let mut r = Rig::new();
    r.jobs(
        "mel",
        &json!([daily("a", "30 8 * * *"), daily("b", "30 8 * * *")]),
    );
    r.set_wm(at(2026, 11, 3, 8, 29));
    // Everything fails: it may be the core (a keychain that is locked), so nobody is given up on at the usual limit ...
    all_fail(&mut r, 30..36);
    assert_eq!(
        r.n("SELECT min(fail_count) FROM sched_jobs"),
        6,
        "each failing tick is counted for each job"
    );
    assert!(r.rows("a").is_empty() && r.rows("b").is_empty());
    assert_eq!(r.wm(), Some(at(2026, 11, 3, 8, 29)));
    // ... so the core recovers and both fire for the slot they never lost.
    let rep = r.tick(at(2026, 11, 3, 8, 36));
    assert_eq!(fired(&rep).len(), 2, "{rep}");
    assert_eq!(r.n("SELECT sum(fail_count) FROM sched_jobs"), 0);
}

#[test]
fn jobs_that_all_fail_for_good_are_given_up_on_at_the_longer_limit() {
    let mut r = Rig::new();
    r.jobs(
        "mel",
        &json!([daily("a", "30 8 * * *"), daily("b", "30 8 * * *")]),
    );
    let start = at(2026, 11, 3, 8, 29);
    r.set_wm(start);
    all_fail(&mut r, 30..39);
    assert_eq!(r.wm(), Some(start), "nine ticks: still held");
    assert!(r.rows("a").is_empty() && r.rows("b").is_empty());
    all_fail(&mut r, 39..40);
    // The tenth: each job gets its skipped:error row and the watermark is free to move.
    assert_eq!(r.wm(), Some(at(2026, 11, 3, 8, 39)));
    for id in ["a", "b"] {
        assert!(
            r.rows(id)
                .iter()
                .any(|(_, o)| o.starts_with("skipped:error: ")),
            "{id}: {:?}",
            r.rows(id)
        );
    }
}

fn put_fire(r: &Rig, job: &str, slot: &str, at_ms: i64, turn: Option<&str>) {
    r.core
        .conn()
        .execute(
            "INSERT INTO sched_fires (agent, job, slot, outcome, turn_id, at) VALUES ('mel', ?1, ?2, 'fired', ?3, ?4)",
            rusqlite::params![job, slot, turn, at_ms],
        )
        .unwrap();
}

#[test]
fn old_fire_rows_are_pruned_but_deliveries_and_the_interval_anchor_stay() {
    use super::fire::{DELIVERIES_KEEP_MS, FIRES_KEEP_MS, prune_fires};
    let mut r = Rig::new();
    let now = at(2026, 11, 20, 8, 0);
    let old = now - FIRES_KEEP_MS - 1;
    let older = now - DELIVERIES_KEEP_MS - 1;
    put_fire(&r, "iv", "2026-10-01T08:00:00Z", old, None);
    put_fire(&r, "iv", "2026-10-01T09:00:00Z", old, None); // the latest timed slot of iv: the anchor
    put_fire(&r, "cr", "2026-10-01T08:00:00Z", old, None);
    put_fire(&r, "cr", "2026-10-02T08:00:00Z", old, None);
    put_fire(&r, "cr", "manual:m1", old, None);
    put_fire(&r, "cr", "event:dold", old, None); // a delivery younger than the delivery limit
    put_fire(&r, "cr", "event:dolder", older, None);
    put_fire(&r, "cr", "event:n1-abc", old, None); // a delivery with no id: no dedupe to keep
    put_fire(&r, "cr", "2026-10-03T08:00:00Z", now - 1000, None); // recent
    let n = prune_fires(&mut r.core, now).unwrap();
    let left = |job: &str| -> Vec<String> {
        let mut st = r
            .core
            .conn()
            .prepare("SELECT slot FROM sched_fires WHERE job = ?1 ORDER BY slot")
            .unwrap();
        st.query_map([job], |x| x.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert_eq!(left("iv"), ["2026-10-01T09:00:00Z"]);
    assert_eq!(
        left("cr"),
        ["2026-10-03T08:00:00Z", "event:dold"],
        "the latest slot is the recent one; the delivery younger than 60 days stays"
    );
    assert_eq!(n, 6, "{n}");
}

#[test]
fn a_fire_row_with_an_unfinished_turn_is_not_pruned() {
    use super::fire::{FIRES_KEEP_MS, prune_fires};
    let mut r = Rig::new();
    r.jobs("mel", &json!([daily("a", "30 8 * * *")]));
    r.set_wm(at(2026, 11, 3, 8, 29));
    let rep = r.tick(at(2026, 11, 3, 8, 30));
    assert_eq!(fired(&rep), ["a"], "{rep}");
    // The turn is queued: pausing the job must still find it through the row.
    let later = at(2026, 11, 3, 8, 30) + FIRES_KEEP_MS + 1000;
    // Another, later slot, so the first is not the job's latest.
    put_fire(&r, "a", "2026-11-04T08:30:00Z", later, None);
    assert_eq!(prune_fires(&mut r.core, later).unwrap(), 0);
    assert_eq!(r.n("SELECT count(*) FROM sched_fires WHERE job = 'a'"), 2);
    r.finish_all();
    assert_eq!(prune_fires(&mut r.core, later).unwrap(), 1);
}

#[test]
fn a_job_that_names_another_mac_is_not_fired_here_and_one_that_names_this_mac_is() {
    let mut r = Rig::new();
    let mine = r.mac.clone();
    let mut theirs = daily("theirs", "30 8 * * *");
    theirs["mac"] = json!("0123456789abcdef0123456789abcdef");
    let mut ours = daily("ours", "30 8 * * *");
    ours["mac"] = json!(mine);
    r.jobs("mel", &json!([theirs, ours, daily("anyone", "30 8 * * *")]));
    r.set_wm(at(2026, 11, 3, 8, 29));
    let rep = r.tick(at(2026, 11, 3, 8, 30));
    assert_eq!(fired(&rep), ["ours", "anyone"], "{rep}");
    assert!(
        r.rows("theirs").is_empty(),
        "nothing is recorded for a job of another Mac"
    );
    assert_eq!(r.wm(), Some(at(2026, 11, 3, 8, 30)));
}

#[test]
fn a_job_row_without_first_seen_falls_back_to_the_watermark() {
    let mut r = Rig::new();
    r.jobs("mel", &json!([every("iv", 300)]));
    r.tick(at(2026, 11, 3, 8, 0));
    let conn = r.core.conn();
    for set in ["NULL", "0"] {
        conn.execute(&format!("UPDATE sched_jobs SET first_seen = {set}"), [])
            .unwrap();
        assert_eq!(super::sync::first_seen(conn, "mel", "iv").unwrap(), None);
    }
    // A row written by something that does not know the column.
    conn.execute(
        "INSERT INTO sched_jobs (agent, job, def, paused) VALUES ('mel', 'raw', '{}', 0)",
        [],
    )
    .unwrap();
    assert_eq!(super::sync::first_seen(conn, "mel", "raw").unwrap(), None);
}

#[test]
fn run_now_refuses_a_job_of_another_mac_and_writes_nothing_while_mine_and_unnamed_fire() {
    let mut r = Rig::new();
    let mine = r.mac.clone();
    let mut theirs = daily("theirs", "30 8 * * *");
    theirs["mac"] = json!("0123456789abcdef0123456789abcdef");
    let mut ours = daily("ours", "30 8 * * *");
    ours["mac"] = json!(mine);
    r.jobs("mel", &json!([theirs, ours, daily("anyone", "30 8 * * *")]));
    let now = at(2026, 11, 3, 12, 0);
    for _ in 0..2 {
        let e = run_now(&mut r.core, &r.p, &r.ctx, "mel", "theirs", "k1", now).unwrap_err();
        assert!(e.to_string().contains("is not on this Mac"), "{e}");
    }
    assert_eq!(r.n("SELECT count(*) FROM sched_fires"), 0);
    assert_eq!(r.n("SELECT count(*) FROM turns"), 0);
    assert_eq!(r.wm(), None);
    assert_eq!(r.run_now("mel", "ours", "k1", now)["outcome"], "fired");
    assert_eq!(r.run_now("mel", "anyone", "k1", now)["outcome"], "fired");
}
