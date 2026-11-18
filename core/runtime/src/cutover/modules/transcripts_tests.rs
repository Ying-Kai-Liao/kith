#![allow(clippy::unwrap_used)]
//! The `transcripts` step on a temp machine: a real core.db, registry files as `kith-sessions record` writes them, a spool as
//! the hook leaves it. Each preflight refusal alone, the step, its crash points (the tombstone in place, a refused re-read),
//! the second step after a rollback, the same answer. The rollback is in `transcripts_back_tests.rs`. Nothing here touches the
//! machine's own Kith folder.

use std::path::PathBuf;

use rusqlite::Connection;
use serde_json::{Value, json};

use super::transcripts::Transcripts;
use super::transcripts_preflight::CAPS;
use crate::cutover::same_answer::may_serve;
use crate::cutover::state::{self, Phase, Switch};
use crate::cutover::{Env, Module, Opts, registry, rollback, step, tombstone};
use crate::db::Core;
use crate::tests::machine;
use crate::transcripts::record;

pub(super) struct Rig {
    pub(super) dir: tempfile::TempDir,
    pub(super) env: Env,
    pub(super) host: PathBuf,
}

pub(super) fn rig() -> Rig {
    let (t, m) = machine();
    let mut paths = crate::tests::paths(&m);
    paths.state = m.join("state");
    paths.home = m.join("home");
    let mut env = Env::with_paths(&paths);
    let host = t.path().join("host");
    std::fs::create_dir_all(&host).unwrap();
    env.serve_db = Some(host.join("serve.db"));
    std::fs::create_dir_all(env.state.as_ref().unwrap().join("live/terminal")).unwrap();
    CAPS.with(|c| *c.borrow_mut() = Some(Ok("registry-core\n".into())));
    // core.db exists and is migrated (a Mac that has run the core), the lifetime lock let go again; claims already switched.
    drop(crate::tests::open_all(&env.layout));
    state::write(
        &env.layout,
        "claims",
        &Switch {
            phase: Phase::Switched,
            snapshot: None,
            at: 1,
        },
    )
    .unwrap();
    Rig { dir: t, env, host }
}

pub(super) fn opts() -> Opts {
    Opts::default()
}

pub(super) fn dry() -> Opts {
    Opts {
        dry_run: true,
        ..Opts::default()
    }
}

/// What `kith-sessions record` writes, at `at` ns (`recordedAt`).
pub(super) fn rec(sid: &str, at: i64, cwd: &str) -> Value {
    json!({
        "v": 1, "sessionId": sid, "agent": "coder", "cwd": cwd, "transcriptPath": null, "pid": null,
        "project": null, "source": "startup", "resumedFrom": null, "startedAt": "2026-11-17T01:02:03Z",
        "account": null, "kind": "terminal", "label": null, "procStart": null, "recordedAt": at,
    })
}

impl Rig {
    pub(super) fn terminal(&self) -> PathBuf {
        self.env.state.as_ref().unwrap().join("live/terminal")
    }

    pub(super) fn tombstone(&self) -> PathBuf {
        self.terminal().join("registry.tombstone")
    }

    pub(super) fn write(&self, sid: &str, body: &Value) {
        std::fs::write(
            self.terminal().join(format!("{sid}.json")),
            body.to_string(),
        )
        .unwrap();
    }

    pub(super) fn spool(&self, sid: &str, body: &Value) {
        let inbox = self.terminal().join("inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        let at = body["recordedAt"].as_i64().unwrap();
        std::fs::write(
            inbox.join(format!("{sid}.{at}.4242.json")),
            body.to_string(),
        )
        .unwrap();
    }

    pub(super) fn inbox_len(&self) -> usize {
        std::fs::read_dir(self.terminal().join("inbox"))
            .map_or(0, |d| d.flatten().filter(|e| e.path().is_file()).count())
    }

    pub(super) fn file(&self, sid: &str) -> Option<Value> {
        std::fs::read(self.terminal().join(format!("{sid}.json")))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
    }

    pub(super) fn db(&self) -> Connection {
        Connection::open(&self.env.layout.db).unwrap()
    }

    pub(super) fn row(&self, sid: &str) -> Option<Value> {
        record::row(&self.db(), sid)
            .unwrap()
            .map(|(m, _)| Value::Object(m))
    }

    pub(super) fn phase(&self) -> Option<Phase> {
        state::read(&self.env.layout, "transcripts")
            .unwrap()
            .map(|s| s.phase)
    }

    pub(super) fn running_host(&self) -> std::fs::File {
        use fs2::FileExt as _;
        let f = std::fs::File::create(self.host.join("host.lock")).unwrap();
        f.lock_exclusive().unwrap();
        f
    }

    pub(super) fn serves(&self) -> bool {
        may_serve(&Transcripts, &self.env, &self.db(), false).ok
    }

    pub(super) fn core(&self) -> Core {
        crate::start_with(&self.env.layout, crate::db::MIGRATIONS)
            .unwrap()
            .with_folders(self.env.paths.as_ref().unwrap())
    }

    pub(super) fn step(&self) -> crate::error::Result<Vec<String>> {
        step(&registry(), &self.env, "transcripts", opts())
    }

    pub(super) fn rollback(&self) -> crate::error::Result<Vec<String>> {
        rollback(&registry(), &self.env, "transcripts", opts())
    }

    pub(super) fn nothing_written(&self) {
        assert_eq!(self.phase(), None);
        assert!(!self.tombstone().exists());
        assert_eq!(self.row("s-one"), None);
    }
}

#[test]
fn each_preflight_refusal_stands_alone_and_writes_nothing() {
    let r = rig();
    r.write("s-one", &rec("s-one", 10, "/work"));
    // The host is running.
    {
        let _host = r.running_host();
        let e = r.step().unwrap_err().to_string();
        assert!(
            e.contains("host is stopped") || e.contains("host is running"),
            "{e}"
        );
        r.nothing_written();
    }
    // The installed kith-sessions lacks Writer mode, or is not there.
    CAPS.with(|c| *c.borrow_mut() = Some(Ok("something-else\n".into())));
    let e = r.step().unwrap_err().to_string();
    assert!(e.contains("registry-core"), "{e}");
    r.nothing_written();
    CAPS.with(|c| *c.borrow_mut() = Some(Err("kith-sessions is not installed".into())));
    let e = r.step().unwrap_err().to_string();
    assert!(e.contains("not installed"), "{e}");
    r.nothing_written();
    CAPS.with(|c| *c.borrow_mut() = Some(Ok("registry-core\n".into())));
    // Claims not switched.
    state::clear(&r.env.layout, "claims").unwrap();
    let e = r.step().unwrap_err().to_string();
    assert!(e.contains("needs claims switched"), "{e}");
    r.nothing_written();
    assert!(r.file("s-one").is_some(), "the registry was only read");
}

#[test]
fn a_dry_run_counts_and_changes_nothing() {
    let r = rig();
    r.write("s-one", &rec("s-one", 10, "/work"));
    let out = step(&registry(), &r.env, "transcripts", dry()).unwrap();
    assert!(out.iter().any(|l| l.contains("would import 1")), "{out:?}");
    assert!(out.iter().any(|l| l.contains("inbox")), "{out:?}");
    r.nothing_written();
    CAPS.with(|c| *c.borrow_mut() = Some(Err("kith-sessions is not installed".into())));
    let e = step(&registry(), &r.env, "transcripts", dry())
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("not ready") && e.contains("kith-sessions"),
        "{e}"
    );
}

#[test]
fn a_step_imports_puts_the_tombstone_and_switches() {
    let r = rig();
    r.write("s-one", &rec("s-one", 10, "/work"));
    // An older hook's file: no recordedAt, so its row stores the file's mtime (A6).
    let mut old = rec("s-two", 0, "/other");
    old.as_object_mut().unwrap().remove("recordedAt");
    r.write("s-two", &old);
    let out = r.step().unwrap();
    assert!(out.iter().any(|l| l.contains("same answer")), "{out:?}");
    assert_eq!(r.phase(), Some(Phase::Switched));
    assert!(tombstone::is_tombstone(&r.tombstone()));
    assert!(r.serves());
    assert_eq!(r.row("s-one").unwrap()["cwd"], "/work");
    assert!(r.row("s-two").unwrap()["recordedAt"].as_i64().unwrap() > 0);
    // The files are still there, untouched by the step.
    assert_eq!(r.file("s-one").unwrap()["cwd"], "/work");
    // A repeated step changes nothing.
    let again = r.step().unwrap();
    assert!(
        again.iter().any(|l| l.contains("already switched")),
        "{again:?}"
    );
}

#[test]
fn spool_entries_are_drained_by_the_reread_not_refused() {
    let r = rig();
    r.write("s-one", &rec("s-one", 10, "/work"));
    // A hook that ran while an earlier step stopped with the tombstone in place, and one older than the file.
    r.spool("s-one", &rec("s-one", 20, "/resumed"));
    r.spool("s-two", &rec("s-two", 5, "/new"));
    r.step().unwrap();
    assert_eq!(r.inbox_len(), 0);
    assert_eq!(r.row("s-one").unwrap()["cwd"], "/resumed");
    assert_eq!(r.file("s-one").unwrap()["cwd"], "/resumed");
    assert_eq!(r.row("s-two").unwrap()["cwd"], "/new");
}

#[test]
fn a_rerun_after_a_crash_with_the_tombstone_in_place_goes_through() {
    let r = rig();
    r.write("s-one", &rec("s-one", 10, "/work"));
    // The step died after before_tombstone put the tombstone: stepping, hooks in Writer mode, one spooled.
    state::write(
        &r.env.layout,
        "transcripts",
        &Switch {
            phase: Phase::Stepping,
            snapshot: None,
            at: 3,
        },
    )
    .unwrap();
    tombstone::ensure(&r.tombstone(), "transcripts", 3).unwrap();
    r.spool("s-three", &rec("s-three", 30, "/late"));
    let out = r.step().unwrap();
    assert!(out.iter().any(|l| l.contains("switched")), "{out:?}");
    assert!(r.serves());
    assert_eq!(r.row("s-three").unwrap()["cwd"], "/late");
    assert_eq!(r.inbox_len(), 0);
}

#[test]
fn a_cancelled_step_takes_the_tombstone_away_and_keeps_the_spool() {
    let r = rig();
    r.write("s-one", &rec("s-one", 10, "/work"));
    state::write(
        &r.env.layout,
        "transcripts",
        &Switch {
            phase: Phase::Stepping,
            snapshot: None,
            at: 3,
        },
    )
    .unwrap();
    tombstone::ensure(&r.tombstone(), "transcripts", 3).unwrap();
    r.spool("s-one", &rec("s-one", 20, "/spooled"));
    let out = r.rollback().unwrap();
    assert!(out.iter().any(|l| l.contains("cancelled")), "{out:?}");
    assert!(!r.tombstone().exists());
    assert_eq!(r.phase(), None);
    // Nothing is lost: the files are as they were and the entry waits for a Files-mode drain.
    assert_eq!(r.file("s-one").unwrap()["cwd"], "/work");
    assert_eq!(r.inbox_len(), 1);
}

#[test]
fn a_refused_reread_removes_the_tombstone_and_leaves_the_module_stepping() {
    let r = rig();
    r.write("s-one", &rec("s-one", 10, "/work"));
    r.write("s-two", &rec("s-two", 11, "/two"));
    let mut core = r.core();
    Transcripts.import(&r.env, Some(&mut core), opts()).unwrap();
    tombstone::ensure(&r.tombstone(), "transcripts", 3).unwrap();
    // The core's row for s-one is newer than its file (not a state Python can make), and s-two's file is gone.
    let ctx = super::transcripts::ctx(&r.env);
    record::commit(
        &mut core,
        &ctx,
        "s-one",
        rec("s-one", 99, "/newer").as_object().unwrap().clone(),
        -1,
    )
    .unwrap();
    std::fs::remove_file(r.terminal().join("s-two.json")).unwrap();
    // s-two's file is gone, so the re-read drops its row; s-one's newer row is what it refuses on.
    let e = Transcripts
        .reread(&r.env, &mut core)
        .unwrap_err()
        .to_string();
    assert!(e.contains("NOT switched") && e.contains("s-one"), "{e}");
    assert!(e.contains("tombstone is removed"), "{e}");
    assert!(!r.tombstone().exists());
    drop(core);
    assert_eq!(r.row("s-two"), None, "the row of a pruned file is dropped");
}

#[test]
fn a_reread_that_fails_takes_the_tombstone_away_too() {
    use fs2::FileExt as _;
    let r = rig();
    r.write("s-one", &rec("s-one", 10, "/work"));
    let mut core = r.core();
    Transcripts.import(&r.env, Some(&mut core), opts()).unwrap();
    tombstone::ensure(&r.tombstone(), "transcripts", 3).unwrap();
    // A spool entry for a session whose lock another writer holds: the drain gives up after its wait.
    r.spool("s-one", &rec("s-one", 20, "/spooled"));
    let locks = r.terminal().join("locks");
    std::fs::create_dir_all(&locks).unwrap();
    let held = std::fs::File::create(locks.join("s-one.lock")).unwrap();
    held.lock_exclusive().unwrap();
    let e = Transcripts
        .reread(&r.env, &mut core)
        .unwrap_err()
        .to_string();
    assert!(e.contains("held by another writer"), "{e}");
    assert!(!r.tombstone().exists(), "hooks write files again");
    assert_eq!(r.inbox_len(), 1, "the entry waits");
}

#[test]
fn a_file_newer_than_its_row_is_no_refusal_at_the_reread() {
    let r = rig();
    r.write("s-one", &rec("s-one", 10, "/work"));
    let mut core = r.core();
    Transcripts.import(&r.env, Some(&mut core), opts()).unwrap();
    tombstone::ensure(&r.tombstone(), "transcripts", 3).unwrap();
    // A late hook (or an older kernel's) wrote after the import; the reread takes it in.
    r.write("s-one", &rec("s-one", 50, "/later"));
    Transcripts.reread(&r.env, &mut core).unwrap();
    assert!(r.tombstone().exists());
    drop(core);
    assert_eq!(r.row("s-one").unwrap()["cwd"], "/later");
}

#[test]
fn a_second_step_does_not_write_a_pruned_record_back() {
    let r = rig();
    r.write("s-one", &rec("s-one", 10, "/work"));
    r.write("s-two", &rec("s-two", 11, "/two"));
    r.step().unwrap();
    r.rollback().unwrap();
    assert_eq!(r.phase(), None);
    assert!(r.row("s-two").is_some(), "the rows stay after a rollback");
    // kith-sched prunes s-two's file in Files mode.
    std::fs::remove_file(r.terminal().join("s-two.json")).unwrap();
    let out = r.step().unwrap();
    assert!(
        out.iter().any(|l| l.contains("dropped 1 record row")),
        "{out:?}"
    );
    assert_eq!(r.row("s-two"), None);
    assert!(
        r.file("s-two").is_none(),
        "nothing wrote the pruned record back"
    );
    assert!(r.row("s-one").is_some());
}

#[test]
fn same_answer_names_each_difference() {
    let r = rig();
    r.write("s-one", &rec("s-one", 10, "/work"));
    let mut core = r.core();
    let m = Transcripts;
    assert_ne!(
        m.same_answer(&r.env, core.conn()).unwrap(),
        Vec::<String>::new()
    );
    m.import(&r.env, Some(&mut core), opts()).unwrap();
    assert_eq!(
        m.same_answer(&r.env, core.conn()).unwrap(),
        Vec::<String>::new()
    );
    // A resume rewrote the record (newer), a new session appeared, and a file went.
    r.write("s-one", &rec("s-one", 20, "/elsewhere"));
    r.write("s-new", &rec("s-new", 21, "/w2"));
    let d = m.same_answer(&r.env, core.conn()).unwrap().join("\n");
    assert!(d.contains("s-new"), "{d}");
    assert!(d.contains("working folder"), "{d}");
    assert!(d.contains("newer than the core's record"), "{d}");
    m.import(&r.env, Some(&mut core), opts()).unwrap();
    std::fs::remove_file(r.terminal().join("s-new.json")).unwrap();
    let d = m.same_answer(&r.env, core.conn()).unwrap().join("\n");
    assert!(d.contains("no file"), "{d}");
    // A compact keeps the row's startedAt: that field is not compared.
    m.import(&r.env, Some(&mut core), opts()).unwrap();
    assert_eq!(
        m.same_answer(&r.env, core.conn()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn the_fingerprint_ignores_the_inbox_the_locks_and_the_tombstone() {
    let r = rig();
    r.write("s-one", &rec("s-one", 10, "/work"));
    let core = r.core();
    let before = Transcripts.fingerprint(&r.env, core.conn()).unwrap();
    assert!(before.contains("s-one.json"));
    r.spool("s-one", &rec("s-one", 20, "/x"));
    std::fs::create_dir_all(r.terminal().join("locks")).unwrap();
    tombstone::ensure(&r.tombstone(), "transcripts", 3).unwrap();
    std::fs::write(r.terminal().join(".s-one.1.tmp"), "x").unwrap();
    assert_eq!(
        Transcripts.fingerprint(&r.env, core.conn()).unwrap(),
        before
    );
}

#[test]
fn a_row_newer_than_its_file_is_mirrored_by_the_second_import() {
    // The file was pruned and an older spool entry placed by a Files-mode drain: the row is newer than the file. The files
    // are the truth until the switch, so the import takes the file and the step does not refuse for ever.
    let r = rig();
    r.write("s-one", &rec("s-one", 50, "/work"));
    r.step().unwrap();
    r.rollback().unwrap();
    r.write("s-one", &rec("s-one", 10, "/older-file"));
    let out = step(&registry(), &r.env, "transcripts", dry()).unwrap();
    assert!(out.iter().any(|l| l.contains("would import 1")), "{out:?}");
    r.step().unwrap();
    assert_eq!(r.row("s-one").unwrap()["cwd"], "/older-file");
    assert_eq!(r.row("s-one").unwrap()["recordedAt"], 10);
}
