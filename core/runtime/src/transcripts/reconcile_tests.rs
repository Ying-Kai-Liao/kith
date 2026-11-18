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
    // The row stores the file's st_mtime_ns as its recordedAt (A6), and the file is written again with it.
    let stored = row_of(&core, OTHER);
    assert!(stored["recordedAt"].as_i64().unwrap() > 0);
    let mut stamped = fresh.as_object().unwrap().clone();
    stamped.insert("recordedAt".into(), stored["recordedAt"].clone());
    assert_eq!(stored, stamped);
    assert_eq!(file(&m, OTHER)["recordedAt"], stored["recordedAt"]);
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
    // The session's lock file stays (A8): a flock lives on the file, so it is never deleted.
    assert!(project::lock_of(&dir(&m), SID).exists());
}

#[test]
fn the_prune_deletes_only_the_row_it_judged() {
    let (_t, m) = machine();
    let mut core = core(&m, true);
    let now = 100 * 86_400 * 1_000_000_000_i64;
    let day = 86_400 * 1_000_000_000_i64;
    ask(
        &mut core,
        &rec(SID, now - 8 * day, &json!({"procStart": "e1"})),
    );
    // Same recordedAt, a later procStart: another key, so a prune that judged the first must not delete it.
    let cx = ctx(&m);
    let judged = validate(Some(&rec(SID, now - 8 * day, &json!({"procStart": "e1"}))))
        .unwrap()
        .1;
    assert_eq!(record::row(core.conn(), SID).unwrap().unwrap().0, judged);
    record::commit(
        &mut core,
        &cx,
        SID,
        validate(Some(&rec(SID, now - 8 * day, &json!({"procStart": "e2"}))))
            .unwrap()
            .1,
        0,
    )
    .unwrap();
    let n = core
        .conn()
        .execute(
            "DELETE FROM session_records WHERE sid = ?1 AND recorded_at = ?2 AND COALESCE(proc_start, '') = ?3",
            rusqlite::params![SID, now - 8 * day, "e1"],
        )
        .unwrap();
    assert_eq!(
        n, 0,
        "the compare-and-delete misses a row whose key changed"
    );
    assert!(record::row(core.conn(), SID).unwrap().is_some());
}

#[test]
fn a_settle_that_read_an_older_row_never_writes_it_over_a_row_committed_in_between() {
    // Contract A7: the settle reads row 3 and file 5; row 6 commits in between; the settle must leave row 6 and file 6.
    let (_t, m) = machine();
    let mut core = core(&m, true);
    let cx = ctx(&m);
    ask(&mut core, &rec(SID, 3, &json!({"cwd": "/row3"})));
    write_atomic(
        &file_of(&dir(&m), SID),
        &rec(SID, 5, &json!({"cwd": "/file5"})),
    )
    .unwrap();
    let six = validate(Some(&rec(SID, 6, &json!({"cwd": "/row6"}))))
        .unwrap()
        .1;
    let done = project::settle_with(&mut core, &cx, &dir(&m), SID, &mut |core| {
        assert_eq!(row_of(core, SID)["cwd"], "/row3");
        record::commit(core, &cx, SID, six.clone(), 0).map(|_| ())
    })
    .unwrap();
    assert!(!done.taken, "file 5 is older than row 6");
    assert_eq!(row_of(&core, SID)["cwd"], "/row6");
    assert_eq!(file(&m, SID)["cwd"], "/row6");
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
    // The one order key (A6): an older file does not replace its row, a newer one does.
    let older = rec(SID, 5, &json!({"cwd": "/older"}));
    write_atomic(&file_of(&dir(&m), SID), &older).unwrap();
    assert_eq!(import_files(core.conn(), &cx, &dir(&m)).unwrap().0, 0);
    assert_eq!(row_of(&core, SID)["cwd"], "/a");
    let newer_file = rec(SID, 15, &json!({"cwd": "/newer"}));
    write_atomic(&file_of(&dir(&m), SID), &newer_file).unwrap();
    assert_eq!(import_files(core.conn(), &cx, &dir(&m)).unwrap().0, 1);
    assert_eq!(row_of(&core, SID)["cwd"], "/newer");
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

/// Contract A9: every shared fixture under `kernel/tests/fixtures/registry-records/` (the same files `kith_registry`'s tests
/// run): a `valid/` file is taken as the record of the session its name names, an `invalid/` one is refused.
#[test]
fn the_shared_registry_fixtures_are_taken_or_refused_as_their_folder_says() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../kernel/tests/fixtures/registry-records");
    for (kind, taken) in [("valid", true), ("invalid", false)] {
        let mut names: Vec<_> = std::fs::read_dir(root.join(kind))
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect();
        names.sort();
        assert!(!names.is_empty(), "no {kind} fixtures");
        for path in names {
            let t = tempfile::tempdir().unwrap();
            let name = path.file_name().unwrap();
            std::fs::copy(&path, t.path().join(name)).unwrap();
            let mut notes = Vec::new();
            let got = super::reconcile::files(t.path(), &mut notes);
            assert_eq!(got.len() == 1, taken, "{kind}/{name:?}: {notes:?}");
        }
    }
}

/// A killed hook leaves its spool temp file; once the Mac is switched the Python sweep no longer runs, so the drain deletes
/// dot-names older than 300 s and leaves a younger one (a write in progress).
#[test]
fn the_drain_sweeps_old_spool_temp_files_and_keeps_a_young_one() {
    let (_t, m) = machine();
    let mut core = core(&m, true);
    spool(&m, &format!(".{SID}.30.1.json.tmp"), &json!({}));
    spool(&m, &format!(".{SID}.31.2.json.tmp"), &json!({}));
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(301);
    std::fs::File::options()
        .write(true)
        .open(dir(&m).join(INBOX).join(format!(".{SID}.30.1.json.tmp")))
        .unwrap()
        .set_modified(old)
        .unwrap();
    drain_once(&mut core, &ctx(&m), &dir(&m)).unwrap();
    let left: Vec<String> = std::fs::read_dir(dir(&m).join(INBOX))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(left, vec![format!(".{SID}.31.2.json.tmp")]);
}
