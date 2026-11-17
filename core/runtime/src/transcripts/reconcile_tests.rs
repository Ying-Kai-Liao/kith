//! The rows and the files kept the same (MUST 1): the spool drain, the minute's reconcile (an older hook's file, a crash
//! between commit and rename, a missing file), the prune, the import and the rollback's export.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::{Value, json};

use super::project::{self, file_of, write_atomic};
use super::reconcile::{drain_once, export_rows, import_files, reconcile_once};
use super::record::{self, INBOX, validate};
use super::record_tests::{SID, ask, core, ctx, dir, file, rec, row_of};
use crate::tests::machine;

const OTHER: &str = "22222222-2222-4222-8222-222222222222";

fn count(core: &crate::db::Core, table: &str) -> i64 {
    core.conn()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

fn spool(m: &std::path::Path, name: &str, v: &Value) {
    let inbox = dir(m).join(INBOX);
    std::fs::create_dir_all(&inbox).unwrap();
    std::fs::write(inbox.join(name), v.to_string()).unwrap();
}

#[test]
fn the_drain_takes_two_entries_of_one_session_newest_wins_and_empties_the_inbox() {
    let (_t, m) = machine();
    let mut core = core(&m, true);
    // The newer one sorts first by name, so the order key decides, not the name.
    spool(
        &m,
        &format!("{SID}.100.7.json"),
        &rec(SID, 100, &json!({"cwd": "/new"})),
    );
    spool(
        &m,
        &format!("{SID}.20.9.json"),
        &rec(SID, 20, &json!({"cwd": "/old"})),
    );
    // A hook still writing its entry (a temp name) and a bad one.
    spool(&m, &format!(".{SID}.30.1.json.tmp"), &json!({}));
    spool(&m, "junk.1.1.json", &json!({"v": 2}));
    let pass = drain_once(&mut core, &ctx(&m), &dir(&m)).unwrap();
    assert_eq!((pass.taken, pass.drained), (1, 3), "{pass:?}");
    assert_eq!(pass.notes.len(), 1, "the invalid entry is a note");
    assert_eq!(row_of(&core, SID)["cwd"], "/new");
    assert_eq!(file(&m, SID), row_of(&core, SID));
    let left: Vec<String> = std::fs::read_dir(dir(&m).join(INBOX))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        left,
        vec![format!(".{SID}.30.1.json.tmp")],
        "only the temp file stays"
    );
    // A spool entry never shows up as a registry record.
    assert!(!dir(&m).join(format!("{SID}.100.7.json")).exists());
}

#[test]
fn a_crash_between_commit_and_rename_is_fixed_by_the_reconcile() {
    let (_t, m) = machine();
    let mut core = core(&m, true);
    let c = ctx(&m);
    ask(&mut core, &rec(SID, 10, &json!({"cwd": "/a"})));
    // The commit of a newer record, and no file step after it (the process died).
    let b = validate(Some(&rec(SID, 20, &json!({"cwd": "/b"}))))
        .unwrap()
        .1;
    record::commit(&mut core, &c, SID, b, 0).unwrap();
    // A row whose file was never written at all.
    let o = validate(Some(&rec(OTHER, 20, &json!({})))).unwrap().1;
    record::commit(&mut core, &c, OTHER, o, 0).unwrap();
    assert_eq!(file(&m, SID)["cwd"], "/a");
    assert!(!file_of(&dir(&m), OTHER).exists());
    let pass = reconcile_once(&mut core, &c, &dir(&m)).unwrap();
    assert_eq!((pass.taken, pass.written), (0, 2), "{pass:?}");
    assert_eq!(file(&m, SID), row_of(&core, SID));
    assert_eq!(file(&m, OTHER), row_of(&core, OTHER));
    // Settled: a second pass does nothing.
    let again = reconcile_once(&mut core, &c, &dir(&m)).unwrap();
    assert_eq!((again.taken, again.written), (0, 0));
}

#[test]
fn an_older_hooks_file_with_no_recorded_at_is_taken_into_the_row_by_its_mtime() {
    let (_t, m) = machine();
    let mut core = core(&m, true);
    let c = ctx(&m);
    // The row is from long ago (recordedAt 1 ns after the epoch).
    ask(&mut core, &rec(SID, 1, &json!({"cwd": "/core"})));
    // A session started before the upgrade runs the old hook: it writes the file itself, with no recordedAt.
    let mut old = rec(SID, 0, &json!({"cwd": "/old-hook", "source": "resume"}));
    old.as_object_mut().unwrap().remove("recordedAt");
    write_atomic(&file_of(&dir(&m), SID), &old).unwrap();
    // And a file with no row at all (the old hook's first record).
    let mut fresh = rec(OTHER, 0, &json!({}));
    fresh.as_object_mut().unwrap().remove("recordedAt");
    write_atomic(&file_of(&dir(&m), OTHER), &fresh).unwrap();
    // Not records: left alone, said once.
    std::fs::write(dir(&m).join("x.json"), "not json").unwrap();
    std::fs::write(dir(&m).join(format!("{SID}.claim")), "{}").unwrap();
    let pass = reconcile_once(&mut core, &c, &dir(&m)).unwrap();
    assert_eq!(pass.taken, 2, "{pass:?}");
    assert_eq!(row_of(&core, SID)["cwd"], "/old-hook");
    assert_eq!(Value::Object(row_of(&core, OTHER)), fresh);
    assert_eq!(file(&m, SID), row_of(&core, SID));
    // Its key is the file's st_mtime_ns: a new record (recordedAt now) still wins over it.
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    )
    .unwrap();
    assert_eq!(
        ask(
            &mut core,
            &rec(SID, now + 1_000_000_000, &json!({"cwd": "/new"}))
        )["outcome"],
        "recorded"
    );
    assert_eq!(file(&m, SID)["cwd"], "/new");
}

#[test]
fn the_prune_takes_old_dead_records_and_keeps_the_engine_sessions() {
    let (_t, m) = machine();
    let mut core = core(&m, true);
    let day = 86_400 * 1_000_000_000_i64;
    let now = 100 * day;
    ask(&mut core, &rec(SID, now - 8 * day, &json!({"pid": null})));
    ask(&mut core, &rec(OTHER, now - 8 * day, &json!({"pid": 4242})));
    let young = "33333333-3333-4333-8333-333333333333";
    ask(&mut core, &rec(young, now - day, &json!({})));
    let gone = super::prune::prune_once(&mut core, &dir(&m), now, &|pid| pid == 4242).unwrap();
    assert_eq!(
        gone,
        vec![SID.to_owned()],
        "the running one and the young one stay"
    );
    assert!(!file_of(&dir(&m), SID).exists());
    assert!(record::row(core.conn(), SID).unwrap().is_none());
    assert!(file_of(&dir(&m), OTHER).exists());
    assert_eq!(count(&core, "engine_sessions"), 3, "engine sessions stay");
    assert_eq!(count(&core, "conversations"), 3);
    // conversation.transcript still answers for the pruned session.
    assert!(core.transcript(&json!({"id": SID})).is_ok());
    // A pruned session's file is not brought back by the reconcile.
    reconcile_once(&mut core, &ctx(&m), &dir(&m)).unwrap();
    assert!(!file_of(&dir(&m), SID).exists());
}

#[test]
fn the_import_takes_files_into_rows_and_the_export_writes_only_newer_rows() {
    let (_t, m) = machine();
    let mut core = core(&m, false);
    let cx = ctx(&m);
    let first = rec(SID, 10, &json!({"cwd": "/a"}));
    write_atomic(&file_of(&dir(&m), SID), &first).unwrap();
    let (n, notes) = import_files(core.conn(), &cx, &dir(&m)).unwrap();
    assert_eq!((n, notes.len()), (1, 0));
    assert_eq!(Value::Object(row_of(&core, SID)), first, "the full JSON");
    // Files win before the switch, even an older one.
    let older = rec(SID, 5, &json!({"cwd": "/older"}));
    write_atomic(&file_of(&dir(&m), SID), &older).unwrap();
    assert_eq!(import_files(core.conn(), &cx, &dir(&m)).unwrap().0, 1);
    assert_eq!(row_of(&core, SID)["cwd"], "/older");
    assert_eq!(
        import_files(core.conn(), &cx, &dir(&m)).unwrap().0,
        0,
        "idempotent"
    );

    // The rollback's export: a row newer than its file is written, an older one is not, a missing file is.
    let newer = validate(Some(&rec(SID, 50, &json!({"cwd": "/row"}))))
        .unwrap()
        .1;
    record::commit(&mut core, &cx, SID, newer, 0).unwrap();
    let other = validate(Some(&rec(OTHER, 1, &json!({})))).unwrap().1;
    record::commit(&mut core, &cx, OTHER, other, 0).unwrap();
    assert_eq!(export_rows(&core, &dir(&m)).unwrap(), 2);
    assert_eq!(file(&m, SID)["cwd"], "/row");
    let ahead = rec(OTHER, 99, &json!({"cwd": "/hook"}));
    write_atomic(&file_of(&dir(&m), OTHER), &ahead).unwrap();
    assert_eq!(
        export_rows(&core, &dir(&m)).unwrap(),
        0,
        "the newer file stays"
    );
    assert_eq!(file(&m, OTHER)["cwd"], "/hook");
    assert!(project::read_file(&file_of(&dir(&m), OTHER)).is_some());
}
