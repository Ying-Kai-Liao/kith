//! The write verbs of `kith schedule` (`schedule.put`, `.drop`, `.setPaused`): the file they leave, the index they settle,
//! what a pause or a removal cancels, what they refuse, and who may call them.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use kith_kernel::paths::Paths;
use kith_kernel::secrets::{Ctx, Sandbox, ServeDir};
use serde_json::{Value, json};
use tempfile::TempDir;

use super::edit::{call, serves};
use super::fire::run_now;
use super::mac_id;
use crate::authority::call::call_with;
use crate::conversations::claims;
use crate::db::Core;
use crate::error::Error;
use crate::layout::Layout;
use crate::tests::{machine, open_all, paths};

const NOW: i64 = 1_800_000_000_000;

struct Rig {
    _t: TempDir,
    p: Paths,
    core: Core,
    ctx: Ctx,
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
        mac_id(&m).unwrap();
        let mut r = Self {
            _t: t,
            p,
            core,
            ctx,
        };
        call_with(
            &mut r.core,
            &r.ctx,
            "people.addPerson",
            &json!({"id": "kai", "owner": true}),
        )
        .unwrap();
        r
    }

    fn file(&self, agent: &str) -> std::path::PathBuf {
        self.p.agents.join(agent).join("jobs.json")
    }

    fn text(&self, agent: &str) -> String {
        std::fs::read_to_string(self.file(agent)).unwrap_or_default()
    }

    fn put(&mut self, params: &Value) -> Value {
        call(&mut self.core, "schedule.put", params, NOW).unwrap()
    }

    fn err(&mut self, method: &str, params: &Value) -> String {
        call(&mut self.core, method, params, NOW)
            .unwrap_err()
            .to_string()
    }

    fn n(&self, sql: &str) -> i64 {
        self.core.conn().query_row(sql, [], |r| r.get(0)).unwrap()
    }
}

#[test]
fn put_writes_the_file_as_kith_sched_does_and_answers_the_row() {
    let mut r = Rig::new();
    let out = r.put(&json!({
        "agent": "mel", "cron": "0 8 * * *", "task": "Send the digest!",
        "model": "sonnet", "effort": "low", "person": "kai"
    }));
    assert_eq!(out["ok"], true);
    assert_eq!(out["replaced"], false);
    assert_eq!(out["job"]["id"], "send-the-digest");
    assert_eq!(out["job"]["person"], "kai");
    assert_eq!(out["job"]["paused"], false);
    assert!(out["job"]["next"].is_string(), "{out}");
    // Two-space indent, a newline at the end, keys in the order kith-sched writes them.
    assert_eq!(
        r.text("mel"),
        "[\n  {\n    \"id\": \"send-the-digest\",\n    \"cron\": \"0 8 * * *\",\n    \"task\": \"Send the digest!\",\n    \"person\": \"kai\",\n    \"model\": \"sonnet\",\n    \"effort\": \"low\"\n  }\n]\n"
    );
    // The index is up to the file at once.
    assert_eq!(
        r.n("SELECT count(*) FROM sched_jobs WHERE job = 'send-the-digest'"),
        1
    );
}

#[test]
fn put_with_an_id_that_exists_replaces_it_and_moves_it_last() {
    let mut r = Rig::new();
    r.put(&json!({"agent": "mel", "id": "a", "cron": "0 8 * * *", "task": "one"}));
    r.put(&json!({"agent": "mel", "id": "b", "cron": "0 9 * * *", "task": "two"}));
    let out = r.put(&json!({"agent": "mel", "id": "a", "cron": "0 10 * * *", "task": "three"}));
    assert_eq!(out["replaced"], true);
    let list: Value = serde_json::from_str(&r.text("mel")).unwrap();
    let ids: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|j| j["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["b", "a"]);
    assert_eq!(list[1]["cron"], "0 10 * * *");
}

#[test]
fn the_slug_is_kith_scheds() {
    assert_eq!(
        super::edit::slug("  Daily -- Review (08:00) "),
        "daily-review-08-00"
    );
    assert_eq!(super::edit::slug("!!!"), "job");
    assert_eq!(
        super::edit::slug("abcdefghijklmnopqrstuvwxyz0123"),
        "abcdefghijklmnopqrstuvwx"
    );
}

#[test]
fn a_bad_cron_effort_or_agent_is_refused_and_nothing_is_written() {
    let mut r = Rig::new();
    let e = r.err(
        "schedule.put",
        &json!({"agent": "mel", "cron": "nope", "task": "x"}),
    );
    assert!(e.contains("schedule:"), "{e}");
    let e = r.err(
        "schedule.put",
        &json!({"agent": "mel", "cron": "0 8 * * *", "task": "x", "effort": "huge"}),
    );
    assert!(
        e.contains("effort must be one of low, medium, high, xhigh, max"),
        "{e}"
    );
    let e = r.err(
        "schedule.put",
        &json!({"agent": "zed", "cron": "0 8 * * *", "task": "x"}),
    );
    assert!(e.contains("no agent 'zed'") && e.contains("mel ops"), "{e}");
    assert!(!r.file("mel").exists());
}

#[test]
fn a_file_that_cannot_be_read_is_never_overwritten() {
    let mut r = Rig::new();
    std::fs::write(r.file("mel"), "{not json").unwrap();
    for (m, p) in [
        (
            "schedule.put",
            json!({"agent": "mel", "cron": "0 8 * * *", "task": "x"}),
        ),
        ("schedule.drop", json!({"agent": "mel", "id": "x"})),
        (
            "schedule.setPaused",
            json!({"agent": "mel", "id": "x", "paused": true}),
        ),
    ] {
        let e = r.err(m, &p);
        assert!(e.contains("nothing was changed"), "{m}: {e}");
    }
    assert_eq!(r.text("mel"), "{not json");
}

#[test]
fn pause_and_resume_report_whether_they_changed_anything_and_keep_the_other_fields() {
    let mut r = Rig::new();
    r.put(
        &json!({"agent": "mel", "id": "a", "cron": "0 8 * * *", "task": "one", "model": "sonnet"}),
    );
    let on = |r: &mut Rig, paused: bool| {
        call(
            &mut r.core,
            "schedule.setPaused",
            &json!({"agent": "mel", "id": "a", "paused": paused}),
            NOW,
        )
        .unwrap()
    };
    assert_eq!(on(&mut r, true)["changed"], true);
    assert_eq!(on(&mut r, true)["changed"], false);
    assert_eq!(r.n("SELECT paused FROM sched_jobs WHERE job = 'a'"), 1);
    let list: Value = serde_json::from_str(&r.text("mel")).unwrap();
    assert_eq!(list[0]["paused"], true);
    assert_eq!(list[0]["model"], "sonnet");
    assert_eq!(on(&mut r, false)["changed"], true);
    assert_eq!(on(&mut r, false)["changed"], false);
    assert_eq!(r.n("SELECT paused FROM sched_jobs WHERE job = 'a'"), 0);
    let list: Value = serde_json::from_str(&r.text("mel")).unwrap();
    assert!(list[0].get("paused").is_none(), "{list}");
}

#[test]
fn a_pause_and_a_removal_cancel_what_the_job_has_waiting_now() {
    let mut r = Rig::new();
    for id in ["a", "b"] {
        r.put(&json!({"agent": "mel", "id": id, "cron": "0 8 * * *", "task": id}));
    }
    let fired = |r: &mut Rig, job: &str, key: &str| {
        let v = run_now(&mut r.core, &r.p, &r.ctx, "mel", job, key, NOW).unwrap();
        assert_eq!(v["outcome"], "fired", "{v}");
        v["turn"]
            .as_i64()
            .or_else(|| v["turn"].as_str().and_then(|s| s.parse().ok()))
            .unwrap()
    };
    let (ta, tb) = (fired(&mut r, "a", "k1"), fired(&mut r, "b", "k2"));
    let state = |r: &Rig, id: i64| -> String {
        r.core
            .conn()
            .query_row("SELECT state FROM turns WHERE id = ?1", [id], |x| x.get(0))
            .unwrap()
    };
    assert_ne!(state(&r, ta), "cancelled");
    call(
        &mut r.core,
        "schedule.setPaused",
        &json!({"agent": "mel", "id": "a", "paused": true}),
        NOW,
    )
    .unwrap();
    assert_eq!(state(&r, ta), "cancelled");
    assert_ne!(state(&r, tb), "cancelled");
    let out = call(
        &mut r.core,
        "schedule.drop",
        &json!({"agent": "mel", "id": "b"}),
        NOW,
    )
    .unwrap();
    assert_eq!(out["changed"], true);
    assert_eq!(state(&r, tb), "cancelled");
    assert_eq!(r.n("SELECT count(*) FROM sched_jobs WHERE job = 'b'"), 0);
    let list: Value = serde_json::from_str(&r.text("mel")).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
}

#[test]
fn drop_and_pause_name_the_jobs_there_are_when_one_is_not() {
    let mut r = Rig::new();
    r.put(&json!({"agent": "mel", "id": "a", "cron": "0 8 * * *", "task": "one"}));
    for (m, p) in [
        ("schedule.drop", json!({"agent": "mel", "id": "zz"})),
        (
            "schedule.setPaused",
            json!({"agent": "mel", "id": "zz", "paused": true}),
        ),
    ] {
        let e = r.err(m, &p);
        assert!(e.contains("mel has no job 'zz' (have: a)"), "{m}: {e}");
    }
    let e = r.err("schedule.drop", &json!({"agent": "ops", "id": "zz"}));
    assert!(e.contains("ops has no job 'zz' (have: none)"), "{e}");
}

#[test]
fn only_the_owner_may_call_them_and_the_daemon_serves_them() {
    for m in [
        "schedule.put",
        "schedule.drop",
        "schedule.setPaused",
        "schedule.runs",
    ] {
        let row = kith_api::method(m).unwrap_or_else(|| panic!("{m} has no row"));
        assert_eq!(row.served, kith_api::Served::Core, "{m}");
        assert_eq!(row.stability, kith_api::Stability::Experimental, "{m}");
        for who in [
            kith_api::Principal::Member,
            kith_api::Principal::Bot,
            kith_api::Principal::RunToken,
            kith_api::Principal::Host,
        ] {
            assert!(!row.allows(who), "{m} is open to {}", who.name());
        }
        assert!(row.allows(kith_api::Principal::Owner), "{m}");
    }
    for m in ["schedule.put", "schedule.drop", "schedule.setPaused"] {
        assert!(
            serves(m) && super::serves_write(m) && !super::serves(m),
            "{m}"
        );
    }
    let r = Rig::new();
    let e = r
        .core
        .call(
            kith_api::Principal::Member,
            "schedule.put",
            &json!({"agent": "mel", "cron": "0 8 * * *", "task": "x"}),
        )
        .unwrap_err();
    assert!(matches!(e, Error::NotAllowed(_)), "{e}");
}

#[test]
fn runs_are_the_newest_fire_with_a_turn_and_fall_back_to_the_imported_run() {
    use crate::schedules::{TriggerRule, call_with as read};
    let mut r = Rig::new();
    for id in ["a", "b", "c"] {
        r.put(&json!({"agent": "mel", "id": id, "cron": "0 8 * * *", "task": id}));
    }
    // `b` has only what the import carried over from kith-sched.
    r.core
        .conn()
        .execute(
            "INSERT INTO sched_runs (agent, job, mac, slot, at, state, why, pid, log)
             VALUES ('mel', 'b', 'old', '2026-11-01T08:00:00Z', '2026-11-01T08:00', 'done', 'due 08:00', 7, '/l/b.log')",
            [],
        )
        .unwrap();
    let fired = run_now(&mut r.core, &r.p, &r.ctx, "mel", "a", "k1", NOW).unwrap();
    assert_eq!(fired["outcome"], "fired", "{fired}");
    let runs = |r: &Rig, p: Value| -> Vec<Value> {
        read(&r.core, "schedule.runs", &p, &TriggerRule, NOW).unwrap()["runs"]
            .as_array()
            .unwrap()
            .clone()
    };
    let all = runs(&r, json!({}));
    assert_eq!(all.len(), 2, "{all:?}");
    let ran = all.iter().find(|x| x["id"] == "a").unwrap();
    assert_eq!(ran["why"], "run by hand");
    assert_eq!(ran["state"], "queued");
    assert_eq!(ran["running"], false);
    let imported = all.iter().find(|x| x["id"] == "b").unwrap();
    assert_eq!(imported["log"], "/l/b.log");
    // A turn that ended shows how it went.
    r.core
        .conn()
        .execute("UPDATE turns SET state = 'failed'", [])
        .unwrap();
    assert_eq!(runs(&r, json!({"id": "a"}))[0]["state"], "failed");
    assert_eq!(runs(&r, json!({"agent": "ops"})).len(), 0);
    let err = read(
        &r.core,
        "schedule.runs",
        &json!({"agent": "zed"}),
        &TriggerRule,
        NOW,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("no agent 'zed'"), "{err}");
}

#[test]
fn the_jobs_lock_is_free_again_when_a_write_is_answered() {
    use fs2::FileExt as _;
    let mut r = Rig::new();
    r.put(&json!({"agent": "mel", "id": "a", "cron": "0 8 * * *", "task": "a"}));
    let f = std::fs::File::open(r.p.machine.join("sched/mel.jobs.lock")).unwrap();
    f.try_lock_exclusive()
        .expect("the write let the jobs lock go before it answered");
}

#[test]
fn a_job_the_routine_import_made_is_not_replaced_by_put() {
    let mut r = Rig::new();
    std::fs::write(
        r.file("mel"),
        json!([{"id": "daily", "cron": "0 8 * * *", "task": "t", "routine": {"id": "daily", "bot": "b"}}])
            .to_string(),
    )
    .unwrap();
    let before = r.text("mel");
    let e = r.err(
        "schedule.put",
        &json!({"agent": "mel", "id": "daily", "cron": "0 9 * * *", "task": "x"}),
    );
    assert!(e.contains("imported from a routine"), "{e}");
    assert_eq!(r.text("mel"), before);
}
