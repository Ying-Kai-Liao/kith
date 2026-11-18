#![allow(clippy::unwrap_used)]
//! Rollback of the transcripts step: `export_back` writes a row to its file only where the row is newer (A3), drains the spool,
//! removes the tombstone last; the order fixtures of both sides decide the winner in both directions; and one run end to end:
//! step, `session.record`, rollback, files.

use std::path::Path;

use serde_json::{Value, json};

use super::transcripts::Transcripts;
use super::transcripts_tests::{opts, rec, rig};
use crate::cutover::state::Phase;
use crate::cutover::{Module, tombstone};
use crate::transcripts::reconcile::{export_rows, import_files};
use crate::transcripts::record::{self, Key};

fn call(r: &super::transcripts_tests::Rig, body: &Value) -> Value {
    let mut core = r.core();
    record::call(&mut core, &json!({ "record": body })).unwrap()
}

#[test]
fn step_record_rollback_and_the_files_are_the_rows() {
    let r = rig();
    r.write("s-one", &rec("s-one", 10, "/work"));
    r.step().unwrap();
    // The core answers once switched, and writes the file from its row.
    assert_eq!(
        call(&r, &rec("s-one", 20, "/resumed"))["outcome"],
        "recorded"
    );
    assert_eq!(call(&r, &rec("s-new", 30, "/new"))["outcome"], "recorded");
    assert_eq!(call(&r, &rec("s-one", 15, "/older"))["outcome"], "older");
    assert_eq!(r.file("s-one").unwrap()["cwd"], "/resumed");
    // A hook spooled just before the rollback and was not answered.
    r.spool("s-late", &rec("s-late", 40, "/late"));
    // The rollback.
    let out = r.rollback().unwrap();
    assert!(out.iter().any(|l| l.contains("rolled back")), "{out:?}");
    assert_eq!(r.phase(), None);
    assert!(!r.tombstone().exists());
    assert_eq!(r.inbox_len(), 0);
    for (sid, cwd) in [
        ("s-one", "/resumed"),
        ("s-new", "/new"),
        ("s-late", "/late"),
    ] {
        assert_eq!(r.file(sid).unwrap()["cwd"], cwd, "{sid}");
        // Every file the core wrote carries recordedAt (A6).
        assert!(r.file(sid).unwrap()["recordedAt"].is_i64());
    }
    // The core no longer answers.
    assert_eq!(call(&r, &rec("s-new", 50, "/x"))["refused"], "not-serving");
}

#[test]
fn export_back_writes_a_row_only_over_an_older_or_missing_file() {
    let r = rig();
    r.write("s-keep", &rec("s-keep", 10, "/a"));
    r.write("s-new", &rec("s-new", 10, "/a"));
    r.write("s-gone", &rec("s-gone", 10, "/a"));
    r.write("s-pruned", &rec("s-pruned", 10, "/a"));
    r.step().unwrap();
    // Writer mode: rows move on. Then a Files-mode hook of a later run wrote s-keep's file after the switch (a late hook).
    for s in ["s-new", "s-gone", "s-keep", "s-pruned"] {
        call(&r, &rec(s, 20, "/row"));
    }
    std::fs::remove_file(r.terminal().join("s-gone.json")).unwrap();
    r.write("s-keep", &rec("s-keep", 30, "/file-is-newer"));
    // s-pruned: its row is deleted (the core's prune), file stays.
    r.db()
        .execute("DELETE FROM session_records WHERE sid = 's-pruned'", [])
        .unwrap();
    std::fs::write(
        r.terminal().join("s-pruned.json"),
        rec("s-pruned", 5, "/stays").to_string(),
    )
    .unwrap();
    r.rollback().unwrap();
    assert_eq!(r.file("s-new").unwrap()["cwd"], "/row");
    assert_eq!(
        r.file("s-gone").unwrap()["cwd"],
        "/row",
        "a missing file is written"
    );
    assert_eq!(
        r.file("s-keep").unwrap()["cwd"],
        "/file-is-newer",
        "a newer file is never replaced by an older row"
    );
    assert_eq!(r.file("s-pruned").unwrap()["cwd"], "/stays");
}

#[test]
fn rollback_refuses_while_the_host_runs_and_changes_nothing() {
    let r = rig();
    r.write("s-one", &rec("s-one", 10, "/work"));
    r.step().unwrap();
    {
        let _host = r.running_host();
        let e = r.rollback().unwrap_err().to_string();
        assert!(
            e.contains("host is running") && e.contains("--force"),
            "{e}"
        );
    }
    assert_eq!(r.phase(), Some(Phase::Switched));
    assert!(tombstone::is_tombstone(&r.tombstone()));
    assert!(r.serves());
}

#[test]
fn a_rollback_that_died_after_the_export_finishes_when_run_again() {
    let r = rig();
    r.write("s-one", &rec("s-one", 10, "/work"));
    r.step().unwrap();
    call(&r, &rec("s-one", 20, "/row"));
    // Died after export_back removed the tombstone, before the record was cleared.
    let mut core = r.core();
    Transcripts.export_back(&r.env, &mut core).unwrap();
    drop(core);
    assert!(!r.tombstone().exists());
    let out = r.rollback().unwrap();
    assert!(out.iter().any(|l| l.contains("rolled back")), "{out:?}");
    assert_eq!(r.phase(), None);
    assert_eq!(r.file("s-one").unwrap()["cwd"], "/row");
}

fn order_cases() -> Vec<(String, Value)> {
    let root =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../kernel/tests/fixtures/registry-order");
    let mut out = Vec::new();
    for e in std::fs::read_dir(root).unwrap().flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "json") {
            let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
            out.push((p.display().to_string(), v));
        }
    }
    out
}

/// The shared order fixtures decide the winner of a file and a row the same way going in (import) and out (`export_back`):
/// the cases whose records carry an integer `recordedAt`, with a strict key winner (the others need a file mtime or an
/// arrival order that only the hooks have).
#[test]
fn the_order_fixtures_decide_import_and_export_the_same_way() {
    let r = rig();
    let ctx = super::transcripts::ctx(&r.env);
    let dir = r.terminal();
    let mut ran = 0;
    for (name, case) in order_cases() {
        let (a, b) = (
            case["a"].as_object().unwrap(),
            case["b"].as_object().unwrap(),
        );
        let stamped =
            |m: &serde_json::Map<String, Value>| m.get("recordedAt").is_some_and(Value::is_i64);
        if !(stamped(a) && stamped(b)) || Key::of(a, -1) == Key::of(b, -1) {
            continue;
        }
        let Some(sid) = a
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            continue;
        };
        let newer = if Key::of(a, -1) > Key::of(b, -1) {
            a
        } else {
            b
        };
        let write_file = |m: &serde_json::Map<String, Value>| {
            std::fs::write(
                dir.join(format!("{sid}.json")),
                Value::Object(m.clone()).to_string(),
            )
            .unwrap();
        };
        // Import: the row holds a, the file b; the newer of the two ends in the row.
        let mut core = r.core();
        core.conn()
            .execute("DELETE FROM session_records", [])
            .unwrap();
        record::commit(&mut core, &ctx, &sid, a.clone(), -1).unwrap();
        write_file(b);
        {
            let tx = core.claim_tx().unwrap();
            import_files(&tx, &ctx, &dir).unwrap();
            tx.commit().unwrap();
        }
        assert_eq!(
            record::row(core.conn(), &sid).unwrap().unwrap().0["agent"],
            newer["agent"],
            "import {name}"
        );
        // Export: the row holds a, the file b; the newer of the two ends in the file.
        core.conn()
            .execute("DELETE FROM session_records", [])
            .unwrap();
        record::commit(&mut core, &ctx, &sid, a.clone(), -1).unwrap();
        write_file(b);
        export_rows(&core, &dir).unwrap();
        let got: Value =
            serde_json::from_slice(&std::fs::read(dir.join(format!("{sid}.json"))).unwrap())
                .unwrap();
        assert_eq!(got["agent"], newer["agent"], "export {name}");
        ran += 1;
    }
    assert!(ran >= 4, "only {ran} fixtures were usable");
}

#[test]
fn a_step_cancelled_after_a_rollback_cycle_works_the_same() {
    // Rollback, a second step that is refused (host running), and a third that goes through.
    let r = rig();
    r.write("s-one", &rec("s-one", 10, "/work"));
    r.step().unwrap();
    r.rollback().unwrap();
    {
        let _host = r.running_host();
        assert!(r.step().is_err());
    }
    r.write("s-one", &rec("s-one", 70, "/again"));
    r.step().unwrap();
    assert_eq!(r.row("s-one").unwrap()["cwd"], "/again");
    let _ = opts();
}
