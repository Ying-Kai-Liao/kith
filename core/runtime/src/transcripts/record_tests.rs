//! `session.record` on a real core.db: validation never stricter than the hook (MUST 4), newer wins with its tiebreak, the
//! file always from the row (MUST 2), the refusals kept apart (MUST 5), the compact rule, and the embedded call that never
//! waits.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use kith_kernel::threads::Ctx;
use serde_json::{Map, Value, json};

use super::project::{self, file_of, read_file};
use super::record::{self, Key, Took, validate};
use crate::cutover::state::{self, Phase, Switch};
use crate::db::{Core, MIGRATIONS};
use crate::layout::Layout;
use crate::start_with;
use crate::tests::{machine, paths};

pub(super) const SID: &str = "11111111-1111-4111-8111-111111111111";

fn no_env(_: &str) -> Option<String> {
    None
}

pub(super) fn ctx(m: &Path) -> Ctx<'static> {
    Ctx {
        state: Some(m.to_path_buf()),
        machine: Some(m.to_path_buf()),
        home: m.to_path_buf(),
        env: &no_env,
    }
}

pub(super) fn dir(m: &Path) -> PathBuf {
    m.join("live/terminal")
}

/// A core opened with its folders; `switched` also writes the switch file and the marker `kith migrate step` would.
pub(super) fn core(m: &Path, switched: bool) -> Core {
    let mut core = start_with(&Layout::new(m), MIGRATIONS)
        .expect("start")
        .with_folders(&paths(m));
    if switched {
        switch(m, &mut core);
    }
    core
}

pub(super) fn switch(m: &Path, core: &mut Core) {
    state::write(
        &Layout::new(m),
        "transcripts",
        &Switch {
            phase: Phase::Switched,
            snapshot: None,
            at: 5,
        },
    )
    .unwrap();
    crate::cutover::marker::write(core, "transcripts", 5, "d", None).unwrap();
}

/// What `kith-sessions record` writes, at `at` ns.
pub(super) fn rec(sid: &str, at: i64, extra: &Value) -> Value {
    let mut v = json!({
        "v": 1, "sessionId": sid, "agent": "coder", "cwd": "/work", "transcriptPath": null, "pid": null,
        "project": null, "source": "startup", "resumedFrom": null, "startedAt": "2026-11-17T01:02:03Z",
        "account": null, "kind": "terminal", "label": null, "procStart": null, "recordedAt": at,
    });
    v.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    v
}

pub(super) fn ask(core: &mut Core, r: &Value) -> Value {
    record::call(core, &json!({ "record": r })).expect("session.record")
}

pub(super) fn row_of(core: &Core, sid: &str) -> Map<String, Value> {
    record::row(core.conn(), sid).unwrap().expect("a row").0
}

pub(super) fn file(m: &Path, sid: &str) -> Map<String, Value> {
    read_file(&file_of(&dir(m), sid)).expect("a file").0
}

fn count(core: &Core, table: &str) -> i64 {
    core.conn()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

#[test]
fn validation_keeps_what_the_hook_writes_and_refuses_only_the_four_things() {
    // pid null, transcriptPath null or empty, extra fields: kept (MUST 4).
    for extra in [
        json!({}),
        json!({"transcriptPath": ""}),
        json!({"pid": 4242, "launcher": 7, "launcherStart": "e1", "whatever": [1]}),
    ] {
        assert!(validate(Some(&rec(SID, 1, &extra))).is_ok(), "{extra}");
    }
    let mut person = rec(SID, 1, &json!({}));
    person["person"] = json!("sam");
    for (bad, what) in [
        (rec("../etc", 1, &json!({})), "bad sid"),
        (rec("", 1, &json!({})), "empty sid"),
        (rec(SID, 1, &json!({"v": 2})), "v2"),
        (person, "person"),
        (json!("text"), "not an object"),
    ] {
        assert!(validate(Some(&bad)).is_err(), "{what}");
    }
    assert!(validate(None).is_err());

    let (_t, m) = machine();
    let mut core = core(&m, true);
    let out = ask(&mut core, &rec("no/slash", 1, &json!({})));
    assert_eq!(out["refused"], "invalid", "{out}");
    let out = ask(&mut core, &rec(SID, 1, &json!({})));
    assert_eq!(out, json!({"outcome": "recorded"}));
    assert_eq!(count(&core, "session_records"), 1);
}

#[test]
fn a_record_writes_the_row_the_engine_session_the_conversation_and_the_file() {
    let (_t, m) = machine();
    let mut core = core(&m, true);
    let r = rec(
        SID,
        10,
        &json!({"account": "claude:work", "transcriptPath": "/x/projects/p/s.jsonl"}),
    );
    assert_eq!(ask(&mut core, &r), json!({"outcome": "recorded"}));
    assert_eq!(Value::Object(row_of(&core, SID)), r);
    assert_eq!(
        Value::Object(file(&m, SID)),
        r,
        "the file is the same object"
    );
    let es = super::rows::get(core.conn(), SID, "claude:work")
        .unwrap()
        .expect("engine session");
    assert_eq!((es.engine.as_str(), es.cwd.as_str()), ("claude", "/work"));
    assert_eq!(count(&core, "conversations"), 1);
    // Python's spacing: `json.dump` writes `{"v": 1, ...}`.
    let bytes = std::fs::read_to_string(file_of(&dir(&m), SID)).unwrap();
    assert!(bytes.starts_with("{\"v\": 1, \"sessionId\": "), "{bytes}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&file_of(&dir(&m), SID)), 0o600);
        assert_eq!(mode(&dir(&m)), 0o700);
    }
    // No temp file is left beside it.
    let left: Vec<_> = std::fs::read_dir(dir(&m))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with('.'))
        .collect();
    assert!(left.is_empty());
}

#[test]
fn the_newer_record_wins_and_ties_go_by_proc_start_then_arrival() {
    let (_t, m) = machine();
    let mut core = core(&m, true);
    let new = rec(SID, 20, &json!({"cwd": "/new"}));
    let old = rec(SID, 10, &json!({"cwd": "/old"}));
    assert_eq!(ask(&mut core, &new)["outcome"], "recorded");
    assert_eq!(ask(&mut core, &old)["outcome"], "older");
    assert_eq!(row_of(&core, SID)["cwd"], "/new");
    assert_eq!(file(&m, SID)["cwd"], "/new");

    // The same recordedAt: the later procStart wins, whatever the order.
    let by_e200 = rec(SID, 30, &json!({"procStart": "e200", "cwd": "/a"}));
    let by_e100 = rec(SID, 30, &json!({"procStart": "e100", "cwd": "/b"}));
    assert_eq!(ask(&mut core, &by_e200)["outcome"], "recorded");
    assert_eq!(ask(&mut core, &by_e100)["outcome"], "older");
    assert_eq!(row_of(&core, SID)["cwd"], "/a");
    // No procStart sorts first.
    let no_start = rec(SID, 30, &json!({"cwd": "/c"}));
    assert_eq!(ask(&mut core, &no_start)["outcome"], "older");

    // A full tie: the last to arrive wins.
    let tie = rec(SID, 30, &json!({"procStart": "e200", "cwd": "/d"}));
    assert_eq!(ask(&mut core, &tie)["outcome"], "recorded");
    assert_eq!(row_of(&core, SID)["cwd"], "/d");
    assert_eq!(file(&m, SID)["cwd"], "/d");

    assert!(
        Key {
            at: 1,
            proc_start: String::new()
        } < Key {
            at: 1,
            proc_start: "a".into()
        },
        "the empty procStart sorts first"
    );
}

#[test]
fn an_older_record_still_writes_the_file_from_the_row() {
    let (_t, m) = machine();
    let mut core = core(&m, true);
    ask(&mut core, &rec(SID, 20, &json!({"cwd": "/new"})));
    // A slow Files-mode hook overwrote the newer file with an older record.
    let stale = rec(SID, 5, &json!({"cwd": "/stale"}));
    project::write_atomic(&file_of(&dir(&m), SID), &stale).unwrap();
    assert_eq!(
        ask(&mut core, &rec(SID, 10, &json!({"cwd": "/old"})))["outcome"],
        "older"
    );
    assert_eq!(file(&m, SID), row_of(&core, SID), "file == row");
    assert_eq!(file(&m, SID)["cwd"], "/new");
}

#[test]
fn a_compact_keeps_the_rows_started_at() {
    let (_t, m) = machine();
    let mut core = core(&m, true);
    ask(
        &mut core,
        &rec(SID, 1, &json!({"startedAt": "2026-01-01T00:00:00Z"})),
    );
    let compact = rec(
        SID,
        2,
        &json!({"source": "compact", "startedAt": "2026-11-17T09:00:00Z"}),
    );
    assert_eq!(ask(&mut core, &compact)["outcome"], "recorded");
    assert_eq!(row_of(&core, SID)["startedAt"], "2026-01-01T00:00:00Z");
    assert_eq!(file(&m, SID)["startedAt"], "2026-01-01T00:00:00Z");
    // A resume is a new start.
    ask(
        &mut core,
        &rec(
            SID,
            3,
            &json!({"source": "resume", "startedAt": "2026-11-17T10:00:00Z"}),
        ),
    );
    assert_eq!(row_of(&core, SID)["startedAt"], "2026-11-17T10:00:00Z");
}

#[test]
fn not_serving_until_switched_and_nothing_is_written() {
    let (_t, m) = machine();
    let mut core = core(&m, false);
    let out = ask(&mut core, &rec(SID, 1, &json!({})));
    assert_eq!(out["refused"], "not-serving", "{out}");
    // A bad record is still not-serving, not invalid: the switch is asked first, so a spool entry is never dropped
    // for a core that would not have looked at it.
    assert_eq!(
        ask(&mut core, &rec("bad/sid", 1, &json!({})))["refused"],
        "not-serving"
    );
    assert_eq!(count(&core, "session_records"), 0);
    assert!(!dir(&m).exists(), "the core touched no registry file");
    // A switch file without its marker in core.db (forged, or a step part-way through): still not serving.
    state::write(
        &Layout::new(&m),
        "transcripts",
        &Switch {
            phase: Phase::Switched,
            snapshot: None,
            at: 5,
        },
    )
    .unwrap();
    assert_eq!(
        ask(&mut core, &rec(SID, 1, &json!({})))["refused"],
        "not-serving"
    );
    // Stepping: not serving either.
    state::write(
        &Layout::new(&m),
        "transcripts",
        &Switch {
            phase: Phase::Stepping,
            snapshot: None,
            at: 5,
        },
    )
    .unwrap();
    crate::cutover::marker::write(&mut core, "transcripts", 5, "d", None).unwrap();
    assert_eq!(
        ask(&mut core, &rec(SID, 1, &json!({})))["refused"],
        "not-serving"
    );
    switch(&m, &mut core);
    assert_eq!(
        ask(&mut core, &rec(SID, 1, &json!({})))["outcome"],
        "recorded"
    );
}

#[test]
fn two_threads_recording_one_session_in_opposite_orders_leave_file_equal_to_row() {
    let (_t, m) = machine();
    let core = Arc::new(Mutex::new(core(&m, true)));
    let run = |order: Vec<i64>| {
        let core = core.clone();
        std::thread::spawn(move || {
            for at in order {
                let r = rec(SID, at, &json!({"cwd": format!("/c{at}")}));
                let mut c = core.lock().unwrap();
                ask(&mut c, &r);
            }
        })
    };
    let up: Vec<i64> = (1..=40).collect();
    let down: Vec<i64> = (1..=40).rev().map(|n| n + 1000).collect();
    let (a, b) = (run(up), run(down));
    a.join().unwrap();
    b.join().unwrap();
    let c = core.lock().unwrap();
    assert_eq!(row_of(&c, SID)["recordedAt"], 1040);
    assert_eq!(file(&m, SID), row_of(&c, SID));
}

#[test]
fn commits_and_file_steps_interleaved_the_wrong_way_still_end_with_the_row() {
    // MUST 2's race made by hand: A (older) and B (newer) commit, then B's file step runs before A's.
    let (_t, m) = machine();
    let mut core = core(&m, true);
    let c = ctx(&m);
    let a = validate(Some(&rec(SID, 10, &json!({"cwd": "/a"}))))
        .unwrap()
        .1;
    let b = validate(Some(&rec(SID, 20, &json!({"cwd": "/b"}))))
        .unwrap()
        .1;
    assert_eq!(
        record::commit(&mut core, &c, SID, a, 0).unwrap(),
        Took::Recorded
    );
    assert_eq!(
        record::commit(&mut core, &c, SID, b, 0).unwrap(),
        Took::Recorded
    );
    project::settle(&mut core, &c, &dir(&m), SID).unwrap();
    project::settle(&mut core, &c, &dir(&m), SID).unwrap();
    assert_eq!(file(&m, SID)["cwd"], "/b");
    assert_eq!(file(&m, SID), row_of(&core, SID));
}

#[test]
fn the_embedded_call_answers_with_no_daemon_and_never_waits_for_a_held_lock() {
    use crate::{EmbeddedCall, call_transcripts_embedded};
    let (_t, m) = machine();
    let p = paths(&m);
    let params = json!({"record": rec(SID, 1, &json!({}))});
    {
        let _c = core(&m, true);
        let started = std::time::Instant::now();
        let busy = call_transcripts_embedded(&p, "session.record", &params).unwrap();
        assert!(matches!(busy, EmbeddedCall::Busy), "{busy:?}");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "it did not wait"
        );
    }
    let EmbeddedCall::Answered(out) =
        call_transcripts_embedded(&p, "session.record", &params).unwrap()
    else {
        panic!("not answered");
    };
    assert_eq!(out, json!({"outcome": "recorded"}));
    assert_eq!(file(&m, SID)["sessionId"], SID);
    assert!(call_transcripts_embedded(&p, "claim.list", &json!({})).is_err());
}

#[test]
fn the_embedded_call_finds_no_core_on_a_fresh_install() {
    use crate::{EmbeddedCall, call_transcripts_embedded};
    let (_t, m) = machine();
    let out =
        call_transcripts_embedded(&paths(&m), "session.record", &json!({"record": {}})).unwrap();
    assert!(matches!(out, EmbeddedCall::NoCore(_)), "{out:?}");
}

/// Contract A10: every case under `kernel/tests/fixtures/registry-order/` (the files `kith_registry` runs too) has a strict
/// winner by `(recordedAt else the file mtime else -1, procStart or "", arrival)`.
#[test]
fn the_shared_order_fixtures_pick_their_winner_by_key() {
    let root =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../kernel/tests/fixtures/registry-order");
    let mut n = 0;
    for e in std::fs::read_dir(&root).unwrap().flatten() {
        let path = e.path();
        if path.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let case: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let key = |side: &str| {
            let rec = case[side].as_object().unwrap();
            let mtime = case["mtime"][side].as_i64().unwrap_or(-1);
            (
                Key::of(rec, mtime),
                case["arrival"][side].as_i64().unwrap_or(0),
            )
        };
        assert_ne!(key("a"), key("b"), "{path:?}: no strict winner");
        let winner = if key("a") > key("b") { "a" } else { "b" };
        assert_eq!(winner, case["winner"].as_str().unwrap(), "{path:?}");
        n += 1;
    }
    assert!(n >= 14, "only {n} order fixtures");
}
