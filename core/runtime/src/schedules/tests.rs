//! The schedule store (M4-2): the mac id, the index over jobs.json files, the read rows through `Core::call`, the
//! import of `kith-sched`'s state and its parity with what `kith-sched ls --json` prints.
//!
//! TODO(parity): when M4-0 lands `kernel/tests/difftest/golden/sched.json`, replay its `ls` and `json` cases here
//! against a core after the import and compare as `parity_with_kith_sched_json_ls` does against the fixture below.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use kith_kernel::paths::Paths;
use rusqlite::Connection;
use serde_json::{Value, json};

use super::{NoRule, Rule, RuleError, mac_id};
use crate::import::schedules::run as import;
use crate::layout::Layout;
use crate::tests::{machine, open_all, paths};

struct Fixed;

impl Rule for Fixed {
    fn next(&self, job: &Value, _now: i64) -> Result<Option<String>, RuleError> {
        match job.get("cron").and_then(Value::as_str).unwrap_or("") {
            "0 8 * * *" => Ok(Some("2026-11-05T08:00".into())),
            "*/5 * * * *" => Ok(Some("2026-11-04T12:05".into())),
            other => Err(RuleError::Bad(format!(
                "cron needs 5 fields, got {other:?}"
            ))),
        }
    }
}

fn write(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// A machine folder with the agents `mel` (two jobs), `ola` (a malformed jobs.json), `kit` (no jobs.json) and a folder
/// that is no agent.
fn world() -> (tempfile::TempDir, std::path::PathBuf, Paths) {
    let (t, m) = machine();
    let mut p = paths(&m);
    p.agents = m.join("agents");
    p.state = m.join("state");
    for a in ["mel", "ola", "kit"] {
        write(&p.agents.join(a).join("role/agent.md"), "# agent\n");
    }
    std::fs::create_dir_all(p.agents.join("notes")).unwrap();
    write(
        &p.agents.join("mel/jobs.json"),
        &json!([
            {"id": "digest", "cron": "0 8 * * *", "task": "send the digest", "person": "kai"},
            {"id": "poll", "cron": "*/5 * * * *", "task": "poll", "paused": true},
            {"id": "norun", "cron": "*/5 * * * *", "task": "never ran"},
            {"id": "weird", "cron": "bad", "task": "x"},
        ])
        .to_string(),
    );
    write(&p.agents.join("ola/jobs.json"), "{not json");
    (t, m, p)
}

fn core_in(m: &Path, p: &Paths) -> crate::Core {
    open_all(&Layout::new(m)).with_folders(p)
}

fn state_json(m: &Path, text: &str) {
    write(&m.join("sched/state.json"), text);
}

fn owner() -> kith_api::Principal {
    kith_api::Principal::Owner
}

const STATE: &str = r#"{
  "tick": "2026-11-04T11:59",
  "pruned": 1.0,
  "last": {
    "mel/digest": {"at": "2026-11-04T08:00", "pid": 4242, "log": "/logs/digest.log", "why": "due 08:00"},
    "mel/gone": {"at": "2026-11-03T08:00", "pid": 1, "log": "/logs/gone.log", "why": "due 08:00"},
    "bad-key": {"at": "x"},
    "mel/noat": {"pid": 3}
  }
}"#;

#[test]
fn the_mac_id_is_made_private_once_and_stays() {
    let (_t, m) = machine();
    let a = mac_id(&m).unwrap();
    assert_eq!(a.len(), 32);
    assert_eq!(mac_id(&m).unwrap(), a);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(m.join("mac-id"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
    // An empty file (a crash of something else) is repaired; a file holding something else is not.
    std::fs::write(m.join("mac-id"), "").unwrap();
    let b = mac_id(&m).unwrap();
    assert_eq!(b.len(), 32);
    assert_eq!(mac_id(&m).unwrap(), b);
    std::fs::write(m.join("mac-id"), "not an id").unwrap();
    assert!(
        mac_id(&m).is_err(),
        "a file that is no id is refused, not replaced"
    );
    assert_eq!(
        std::fs::read_to_string(m.join("mac-id")).unwrap(),
        "not an id"
    );
}

#[test]
fn the_index_reads_each_agents_file_and_reports_a_bad_one_per_agent() {
    let (_t, _m, p) = world();
    let idx = super::index(&p, None);
    assert!(idx.mounted);
    let names: Vec<_> = idx.agents.iter().map(|a| a.agent.as_str()).collect();
    assert_eq!(names, ["kit", "mel", "ola"], "notes has no agent.md");
    assert_eq!(idx.agents[1].jobs.len(), 4);
    assert!(idx.agents[0].jobs.is_empty() && idx.agents[0].problem.is_none());
    assert!(
        idx.agents[2]
            .problem
            .as_deref()
            .unwrap()
            .contains("jobs.json")
    );
    // Reading never wrote a jobs.json or made a lock.
    assert!(!p.agents.join("kit/jobs.json").exists());
    assert!(!p.machine.join("sched").exists());
}

#[test]
fn list_joins_definitions_with_run_state_and_names_what_is_off() {
    let (_t, m, p) = world();
    state_json(&m, STATE);
    // A real import first: the rows come from the state file.
    let rep = import(&Layout::new(&m), &m, false).unwrap();
    assert!(rep.changed, "{:?}", rep.lines);
    let core = core_in(&m, &p);
    let out = core.call(owner(), "schedule.jobs", &json!({})).unwrap();
    let jobs = out["jobs"].as_array().unwrap();
    assert_eq!(jobs.len(), 4);
    let digest = &jobs[0];
    assert_eq!(digest["id"], "digest");
    assert_eq!(digest["person"], "kai");
    assert_eq!(digest["paused"], false);
    assert_eq!(digest["last"]["at"], "2026-11-04T08:00");
    assert_eq!(digest["last"]["log"], "/logs/digest.log");
    let poll = jobs.iter().find(|j| j["id"] == "poll").unwrap();
    assert_eq!(poll["paused"], true);
    assert_eq!(poll["next"], Value::Null);
    let norun = jobs.iter().find(|j| j["id"] == "norun").unwrap();
    assert_eq!(
        norun["last"],
        Value::Null,
        "a job with no run state still lists"
    );
    // Run state for a job since removed is said, not dropped.
    let orphans = out["orphans"].as_array().unwrap();
    assert_eq!(orphans.len(), 1);
    assert_eq!(orphans[0]["id"], "gone");
    // ola's file is a problem of ola, not of the listing.
    let problems = out["problems"].as_array().unwrap();
    assert_eq!(problems.len(), 1);
    assert_eq!(problems[0]["agent"], "ola");
    assert_eq!(out["ticks"][0]["tick"], "2026-11-04T11:59");
    assert_eq!(out["mounted"], true);
    // The kernel's trigger is wired (M4-3): `0 8 * * *` is next at 08:00 local, by the clock this call reads.
    assert!(
        digest["next"]
            .as_str()
            .is_some_and(|n| n.ends_with("T08:00"))
    );
    assert!(digest.get("note").is_none() && digest.get("error").is_none());
    // One agent only: the other agents' runs are not orphans of this listing.
    let one = core
        .call(owner(), "schedule.jobs", &json!({"agent": "kit"}))
        .unwrap();
    assert_eq!(one["jobs"].as_array().unwrap().len(), 0);
    assert_eq!(one["orphans"].as_array().unwrap().len(), 0);
}

#[test]
fn next_and_show_use_the_rule_and_the_run_history() {
    let (_t, m, p) = world();
    state_json(&m, STATE);
    import(&Layout::new(&m), &m, false).unwrap();
    let core = core_in(&m, &p);
    let conn = core.conn();
    conn.execute(
        "INSERT OR REPLACE INTO sched_fires (agent, job, slot, outcome, turn_id, at) VALUES ('mel', 'digest', '2026-11-04T08:00:00Z', 'skipped:paused', NULL, 9999999999999)",
        [],
    )
    .unwrap();
    let ctx = |method: &str, params: Value| {
        super::call_with(&core, method, &params, &Fixed, 1_000).unwrap()
    };
    let next = ctx("schedule.next", json!({}));
    let ids: Vec<_> = next["next"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    // Paused poll is out; soonest first; the unparseable one last.
    assert_eq!(ids, ["norun", "digest", "weird"]);
    assert_eq!(next["next"][0]["next"], "2026-11-04T12:05");
    assert!(
        next["next"][2]["error"]
            .as_str()
            .unwrap()
            .contains("bad cron")
    );
    let show = ctx("schedule.show", json!({"agent": "mel", "id": "digest"}));
    assert_eq!(show["job"]["next"], "2026-11-05T08:00");
    assert_eq!(show["definition"]["task"], "send the digest");
    assert_eq!(show["runs"].as_array().unwrap().len(), 1);
    assert_eq!(show["fires"][0]["slot"], "2026-11-04T08:00:00Z");
    assert_eq!(show["fires"][0]["outcome"], "skipped:paused");
    assert_eq!(show["fires"][0]["turn"], Value::Null);
    // Refusals: an unknown job, an unknown agent, an agent whose file is unreadable.
    for (a, j) in [("mel", "nope"), ("zed", "digest")] {
        assert!(
            super::call_with(
                &core,
                "schedule.show",
                &json!({"agent": a, "id": j}),
                &Fixed,
                0
            )
            .is_err()
        );
    }
    assert!(
        super::call_with(
            &core,
            "schedule.show",
            &json!({"agent": "ola", "id": "x"}),
            &Fixed,
            0
        )
        .is_err()
    );
    // NoRule is what the core wires until M4-1 merges.
    assert_eq!(
        NoRule.next(&json!({"cron": "0 8 * * *"}), 0),
        Err(RuleError::NotWired)
    );
}

#[test]
fn an_unmounted_agents_folder_is_a_status_not_a_failure() {
    let (_t, m, mut p) = world();
    p.agents = m.join("not-mounted");
    let core = core_in(&m, &p);
    let out = core.call(owner(), "schedule.jobs", &json!({})).unwrap();
    assert_eq!(out["mounted"], false);
    assert_eq!(out["note"], "agent folder not mounted");
    assert_eq!(out["jobs"].as_array().unwrap().len(), 0);
    assert_eq!(out["orphans"].as_array().unwrap().len(), 0);
    let nxt = core.call(owner(), "schedule.next", &json!({})).unwrap();
    assert_eq!(nxt["note"], "agent folder not mounted");
    assert!(
        core.call(
            owner(),
            "schedule.show",
            &json!({"agent": "mel", "id": "digest"})
        )
        .is_err()
    );
}

#[test]
fn an_unknown_agent_is_refused_as_kith_sched_refuses_it() {
    let (_t, m, p) = world();
    let core = core_in(&m, &p);
    for method in ["schedule.jobs", "schedule.next"] {
        let e = core
            .call(owner(), method, &json!({"agent": "zed"}))
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("no agent \"zed\"") && e.contains("kit mel ola"),
            "{method}: {e}"
        );
    }
}

#[test]
fn the_write_rows_are_not_served_by_the_core() {
    let (_t, m, p) = world();
    let core = core_in(&m, &p);
    for (method, params) in [
        (
            "schedule.add",
            json!({"agent": "mel", "spec": ["0 8 * * *", "x"]}),
        ),
        ("schedule.pause", json!({"agent": "mel", "id": "digest"})),
        ("schedule.resume", json!({"agent": "mel", "id": "digest"})),
        ("schedule.remove", json!({"agent": "mel", "id": "digest"})),
        ("schedule.run", json!({"agent": "mel", "id": "digest"})),
    ] {
        match core.call(owner(), method, &params) {
            Err(crate::Error::NotServed(_)) => {}
            other => panic!("{method}: {other:?}"),
        }
    }
    let before = std::fs::read_to_string(p.agents.join("mel/jobs.json")).unwrap();
    assert!(before.contains("digest"));
}

fn count(c: &Connection, table: &str) -> i64 {
    c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

#[test]
fn the_import_is_idempotent_read_only_and_has_a_dry_run() {
    let (_t, m, _p) = world();
    state_json(&m, STATE);
    let l = Layout::new(&m);
    let before = std::fs::read(m.join("sched/state.json")).unwrap();
    // A dry run on a machine with no core.db opens nothing for writing and makes no mac id.
    let dry = import(&l, &m, true).unwrap();
    assert!(dry.changed && dry.dry_run && dry.backup.is_none());
    assert!(!l.db.exists() && !m.join("mac-id").exists());
    // First real run: two runs (the entries with no `at` and no `/` are skipped and said so) and the tick.
    let first = import(&l, &m, false).unwrap();
    assert!(first.changed);
    assert!(first.lines.iter().any(|x| x.contains("bad-key")));
    assert!(first.lines.iter().any(|x| x.contains("mel/noat")));
    let core = open_all(&l);
    assert_eq!(count(core.conn(), "sched_runs"), 2);
    assert_eq!(count(core.conn(), "sched_tick"), 1);
    // Every imported run is keyed on a UTC slot, linked to a fire that has no turn yet.
    assert_eq!(count(core.conn(), "sched_fires"), 2);
    let slot: String = core
        .conn()
        .query_row(
            "SELECT slot FROM sched_runs WHERE job = 'digest'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(Some(slot.clone()), super::slot_of_local("2026-11-04T08:00"));
    let (outcome, turn): (String, Option<String>) = core
        .conn()
        .query_row(
            "SELECT outcome, turn_id FROM sched_fires WHERE slot = ?1 AND job = 'digest'",
            [&slot],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((outcome.as_str(), turn), ("fired", None));
    assert_eq!(count(core.conn(), "import_log"), 1);
    drop(core);
    // Second run, and a dry run after it: nothing.
    let backups = std::fs::read_dir(&l.snapshots).unwrap().count();
    let second = import(&l, &m, false).unwrap();
    assert!(
        !second.changed && second.backup.is_none(),
        "{:?}",
        second.lines
    );
    assert!(!import(&l, &m, true).unwrap().changed);
    assert_eq!(
        std::fs::read_dir(&l.snapshots).unwrap().count(),
        backups,
        "no new backup"
    );
    let core = open_all(&l);
    assert_eq!(count(core.conn(), "sched_runs"), 2);
    assert_eq!(count(core.conn(), "import_log"), 1);
    drop(core);
    // The source was only read.
    assert_eq!(std::fs::read(m.join("sched/state.json")).unwrap(), before);
    // A newer tick and a new run later: only those go in; the old run is not duplicated.
    state_json(
        &m,
        r#"{"tick":"2026-11-04T12:01","last":{"mel/digest":{"at":"2026-11-04T12:00","pid":9,"log":"/l","why":"due"}}}"#,
    );
    assert!(import(&l, &m, false).unwrap().changed);
    let core = open_all(&l);
    assert_eq!(count(core.conn(), "sched_runs"), 3);
    let tick: String = core
        .conn()
        .query_row("SELECT tick FROM sched_tick", [], |r| r.get(0))
        .unwrap();
    assert_eq!(tick, "2026-11-04T12:01");
}

#[test]
fn the_import_reads_old_files_and_refuses_unreadable_ones() {
    let (_t, m, _p) = world();
    let l = Layout::new(&m);
    // No file, and an old file with no `last` and no `tick`.
    assert!(!import(&l, &m, false).unwrap().changed);
    state_json(&m, "{}");
    assert!(!import(&l, &m, false).unwrap().changed);
    // An old entry without pid, log or why.
    state_json(&m, r#"{"last":{"mel/digest":{"at":"2026-11-04T08:00"}}}"#);
    assert!(import(&l, &m, false).unwrap().changed);
    for bad in ["not json", "[1]"] {
        state_json(&m, bad);
        assert!(import(&l, &m, false).is_err(), "{bad}");
    }
    let core = open_all(&l);
    assert_eq!(count(core.conn(), "sched_runs"), 1);
}

/// What `kith-sched ls --json` prints for the jobs of the world above after the state file of `STATE`: the job rows
/// (`next` and `error` left out: the cron rule is M4-1's, so they are compared when it is wired).
#[test]
fn matches_the_fixture_of_kith_sched_json_ls_until_the_golden_lands() {
    let (_t, m, p) = world();
    state_json(&m, STATE);
    import(&Layout::new(&m), &m, false).unwrap();
    let core = core_in(&m, &p);
    let got = core
        .call(owner(), "schedule.jobs", &json!({"agent": "mel"}))
        .unwrap();
    let project = |j: &Value| {
        let mut o = j.clone();
        let obj = o.as_object_mut().unwrap();
        obj.remove("next");
        obj.remove("error");
        obj.remove("note");
        if let Some(l) = obj.get_mut("last").and_then(Value::as_object_mut) {
            // kith-sched's `last` is these five keys; the core adds state, outcome and mac.
            l.retain(|k, _| ["at", "why", "pid", "log", "running"].contains(&k.as_str()));
        }
        o
    };
    let got: Vec<Value> = got["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(project)
        .collect();
    // pid 4242 is not a process on the test machine; kith-sched says running only for a live pid.
    let want = json!([
        {"agent": "mel", "id": "digest", "cron": "0 8 * * *", "task": "send the digest", "paused": false, "person": "kai",
         "last": {"at": "2026-11-04T08:00", "why": "due 08:00", "pid": 4242, "log": "/logs/digest.log", "running": false}},
        {"agent": "mel", "id": "poll", "cron": "*/5 * * * *", "task": "poll", "paused": true, "last": null},
        {"agent": "mel", "id": "norun", "cron": "*/5 * * * *", "task": "never ran", "paused": false, "last": null},
        {"agent": "mel", "id": "weird", "cron": "bad", "task": "x", "paused": false, "last": null}
    ]);
    assert_eq!(Value::Array(got), want);
}

#[test]
fn two_creators_end_with_one_id_and_nobody_sees_an_empty_file() {
    for _ in 0..20 {
        let (_t, m) = machine();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let ids: Vec<String> = (0..8)
            .map(|_| {
                let (m, b) = (m.clone(), barrier.clone());
                std::thread::spawn(move || {
                    b.wait();
                    mac_id(&m).unwrap()
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect();
        assert!(ids.iter().all(|i| *i == ids[0] && i.len() == 32), "{ids:?}");
        assert_eq!(std::fs::read_to_string(m.join("mac-id")).unwrap(), ids[0]);
        // No temp file is left behind.
        let left: Vec<_> = std::fs::read_dir(&m)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(left.is_empty());
    }
}

#[test]
fn slots_are_utc_instants_or_manual_and_states_are_checked_in_code() {
    use super::{slot_of_local, valid_fire_outcome, valid_run_state, valid_slot};
    let slot = slot_of_local("2026-11-04T08:00").unwrap();
    assert!(valid_slot(&slot) && slot.ends_with(":00Z"), "{slot}");
    assert_eq!(slot_of_local("not a time"), None);
    for good in ["2026-11-04T08:00:00Z", "manual:abc-1"] {
        assert!(valid_slot(good), "{good}");
    }
    for bad in [
        "2026-11-04T08:00",
        "2026-11-04T08:00:30Z",
        "manual:",
        "manual:a b",
        "",
    ] {
        assert!(!valid_slot(bad), "{bad}");
    }
    assert!(valid_fire_outcome("fired") && valid_fire_outcome("skipped:paused"));
    assert!(!valid_fire_outcome("skipped:") && !valid_fire_outcome("fire"));
    assert!(valid_run_state("started") && !valid_run_state("weird"));
}
