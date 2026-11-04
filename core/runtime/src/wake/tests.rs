//! The wake rules over core.db, in a test core with a fake world, and the import's safety rules. The golden replay of
//! `kernel/tests/difftest` is `parity_tests.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::call::{call_with, tick};
use super::model::State;
use super::outside::{Check, Civil, Outside, PrView, Transcripts};
use super::queue;
use super::{load, read};
use crate::db::Core;
use crate::error::Error;
use crate::layout::Layout;
use crate::tests::{machine, open_all};

/// 2026-09-13T12:26:40Z.
pub(super) const T0: f64 = 1_790_000_000.0;

/// A world where the test says what runs, what `gh` answers and what the clock reads.
pub(super) struct Fake {
    pub now: Cell<f64>,
    /// Process id -> its start time; a pid not here is not running.
    pub procs: RefCell<BTreeMap<i64, String>>,
    pub view: RefCell<Option<PrView>>,
    pub checks: RefCell<Option<Vec<Check>>>,
    pub state: PathBuf,
    pub seq: Cell<u64>,
}

impl Fake {
    pub(super) fn new(state: &Path) -> Self {
        Self {
            now: Cell::new(T0),
            procs: RefCell::new(BTreeMap::from([(1, "Mon Jan 1 00:00:00 2026".to_owned())])),
            view: RefCell::new(None),
            checks: RefCell::new(None),
            state: state.to_path_buf(),
            seq: Cell::new(0),
        }
    }
}

impl Outside for Fake {
    fn now(&self) -> f64 {
        self.now.get()
    }
    fn new_id(&self) -> String {
        self.seq.set(self.seq.get() + 1);
        format!("{:012x}", 0xabc0_0000_0000_u64 + self.seq.get())
    }
    fn alive(&self, pid: i64, started: Option<&str>) -> bool {
        self.procs
            .borrow()
            .get(&pid)
            .is_some_and(|s| started.is_none_or(|w| w == s))
    }
    fn proc_start(&self, pid: i64) -> Option<String> {
        self.procs.borrow().get(&pid).cloned()
    }
    fn writer_live(&self, sid: &str) -> bool {
        ["claim", "takeover.claim"].iter().any(|ext| {
            let p = self
                .state
                .join("live/terminal")
                .join(format!("{sid}.{ext}"));
            std::fs::read(p)
                .ok()
                .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
                .and_then(|c| c.get("pid").and_then(Value::as_i64))
                .is_some_and(|pid| self.alive(pid, None))
        })
    }
    fn pr_view(&self, _: &str, _: i64) -> Option<PrView> {
        self.view.borrow().clone()
    }
    fn pr_checks(&self, _: &str, _: i64) -> Option<Vec<Check>> {
        self.checks.borrow().clone()
    }
    /// The test machine keeps UTC.
    fn local_epoch(&self, at: Civil) -> Option<f64> {
        #[allow(clippy::cast_precision_loss)]
        Some(
            (super::model::days_from_civil(at.year, at.month, at.day) * 86_400
                + at.hour * 3600
                + at.minute * 60
                + at.second) as f64,
        )
    }
    fn ci_poll(&self) -> f64 {
        0.0
    }
}

/// A transcript that holds exactly these markers.
pub(super) struct Seen(pub Vec<(String, String)>);

impl Transcripts for Seen {
    fn has(&self, sid: &str, marker: &str) -> bool {
        self.0.iter().any(|(s, m)| s == sid && m == marker)
    }
}

pub(super) fn core() -> (tempfile::TempDir, Layout, Core) {
    let (t, m) = machine();
    let l = Layout::new(&m);
    let c = open_all(&l);
    (t, l, c)
}

fn row(core: &mut Core, fake: &Fake, method: &str, params: &Value) -> Result<Value, Error> {
    crate::call::check(kith_api::Principal::Owner, method, params)?;
    call_with(core, method, params, fake, &Seen(Vec::new()))
}

fn enqueue(core: &mut Core, fake: &Fake, session: &str, id: &str, door: &str) -> String {
    let a = row(
        core,
        fake,
        "wake.enqueue",
        &json!({"session": session, "id": id, "agent": "coder", "door": door, "text": ["hi"]}),
    )
    .unwrap();
    a["result"].as_str().unwrap().to_owned()
}

fn state(core: &mut Core) -> State {
    read(core, T0, Clone::clone).unwrap()
}

#[test]
fn the_rows_are_in_the_table_and_the_tables_in_the_schema() {
    for m in [
        "wake.enqueue",
        "wake.withdraw",
        "wake.withdrawPerson",
        "wake.take",
        "wake.done",
        "wake.back",
        "wake.queueList",
        "wake.queueStatus",
        "wake.wait",
    ] {
        let row = kith_api::method(m).unwrap_or_else(|| panic!("{m}: no row"));
        assert_eq!(row.served, kith_api::Served::Core, "{m}");
        assert_eq!(row.stability, kith_api::Stability::Experimental, "{m}");
        assert_eq!(row.principals, &[kith_api::Principal::Owner], "{m}");
        assert!(super::serves(m), "{m}");
    }
    let (_t, _l, c) = core();
    for t in ["wake_sessions", "wake_items", "wake_answered", "waiters"] {
        let n: i64 = c
            .conn()
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [t],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "{t}");
    }
}

#[test]
fn enqueue_merges_dedupes_and_survives_a_reopen() {
    let (t, l, mut c) = core();
    let f = Fake::new(&t.path().join("state"));
    assert_eq!(enqueue(&mut c, &f, "s1", "a", "run"), "queued");
    assert_eq!(enqueue(&mut c, &f, "s1", "b", "run"), "merged");
    assert_eq!(enqueue(&mut c, &f, "s1", "a", "run"), "duplicate");
    // A host-door request makes a run-door item a host-door one.
    assert_eq!(enqueue(&mut c, &f, "s1", "c", "host"), "merged");
    let st = state(&mut c);
    assert_eq!(st.items["s1"].door, "host");
    assert_eq!(st.items["s1"].requests.len(), 3);
    // Rule 13: withdrawn, then the same id again, is dropped.
    row(
        &mut c,
        &f,
        "wake.withdraw",
        &json!({"session": "s1", "id": "b"}),
    )
    .unwrap();
    assert_eq!(enqueue(&mut c, &f, "s1", "b", "run"), "duplicate");
    drop(c);
    let mut c = open_all(&l);
    let st = state(&mut c);
    assert_eq!(
        st.items["s1"]
            .requests
            .iter()
            .map(|r| r.id.as_str())
            .collect::<Vec<_>>(),
        ["a", "c"]
    );
    assert_eq!(st.answered["b"].how, "withdrawn");
}

#[test]
fn a_request_leaves_the_queue_only_when_its_turn_ends() {
    let (t, _l, mut c) = core();
    let f = Fake::new(&t.path().join("state"));
    enqueue(&mut c, &f, "s1", "a", "host");
    enqueue(&mut c, &f, "s1", "b", "host");
    let took = row(&mut c, &f, "wake.take", &json!({"holder": 1})).unwrap();
    let run = took["turn"]["run"].as_str().unwrap().to_owned();
    assert_eq!(took["turn"]["ids"], json!(["a", "b"]));
    // W2: taken, not gone; a request that comes in meanwhile merges into the next turn.
    let status = row(&mut c, &f, "wake.queueStatus", &json!({"id": "a"})).unwrap();
    assert_eq!(status["state"], "running");
    assert_eq!(enqueue(&mut c, &f, "s1", "c", "host"), "merged");
    assert!(
        row(&mut c, &f, "wake.take", &json!({"holder": 1})).unwrap()["turn"].is_null(),
        "the session has a turn in flight"
    );
    let done = row(&mut c, &f, "wake.done", &json!({"run": run})).unwrap();
    assert_eq!(done["done"], true);
    let st = state(&mut c);
    assert_eq!(st.items["s1"].requests.len(), 1);
    assert_eq!(st.answered["a"].how, "answered");
    assert_eq!(st.answered["a"].run.as_deref(), Some(run.as_str()));
}

#[test]
fn a_session_a_live_writer_holds_is_not_offered() {
    let (t, _l, mut c) = core();
    let f = Fake::new(&t.path().join("state"));
    std::fs::create_dir_all(t.path().join("state/live/terminal")).unwrap();
    std::fs::write(
        t.path().join("state/live/terminal/s1.claim"),
        r#"{"pid": 1}"#,
    )
    .unwrap();
    enqueue(&mut c, &f, "s1", "a", "host");
    assert!(
        row(&mut c, &f, "wake.take", &json!({"holder": 1})).unwrap()["turn"].is_null(),
        "W3: a terminal holds the session"
    );
    // The terminal lets go: now it is offered, and nothing was lost.
    std::fs::remove_file(t.path().join("state/live/terminal/s1.claim")).unwrap();
    assert_eq!(
        row(&mut c, &f, "wake.take", &json!({"holder": 1})).unwrap()["turn"]["ids"],
        json!(["a"])
    );
}

#[test]
fn a_failed_try_waits_a_doubling_pause_and_the_fifth_ends_failed() {
    let mut st = State::default();
    let f = Fake::new(Path::new("/nonexistent"));
    let new = queue::New {
        session: "s1",
        id: "a",
        agent: "coder",
        text: "hi",
        person: None,
        room: None,
        door: "host",
        why: "",
    };
    queue::enqueue(&mut st, T0, &new).unwrap();
    let none = Seen(Vec::new());
    for (try_, pause) in [(1, 5.0), (2, 10.0), (3, 20.0), (4, 40.0)] {
        let turn = queue::take(&mut st, &f, &none, T0 + 1e6, 7, None, None).unwrap();
        assert!(turn["run"].is_string(), "try {try_}");
        assert!(queue::give_back(
            &mut st,
            T0,
            turn["run"].as_str().unwrap(),
            false
        ));
        assert!(
            (st.items["s1"].next - (T0 + pause)).abs() < 1e-6,
            "try {try_}"
        );
        assert_eq!(st.items["s1"].tries, try_);
    }
    let turn = queue::take(&mut st, &f, &none, T0 + 1e6, 7, None, None).unwrap();
    queue::give_back(&mut st, T0, turn["run"].as_str().unwrap(), false);
    assert!(st.items.is_empty(), "the fifth failed try ends the request");
    assert_eq!(queue::status(&st, "a").0, "failed");
    // A held writer is not a failed try.
    queue::enqueue(&mut st, T0, &queue::New { id: "b", ..new }).unwrap();
    let turn = queue::take(&mut st, &f, &none, T0 + 1e6, 7, None, None).unwrap();
    queue::give_back(&mut st, T0, turn["run"].as_str().unwrap(), true);
    assert_eq!(st.items["s1"].tries, 0);
    assert!((st.items["s1"].next - (T0 + queue::HELD_PAUSE)).abs() < 1e-6);
}

#[test]
fn a_dead_turn_is_answered_when_its_marker_was_seen_and_owed_again_when_not() {
    let (t, _l, mut c) = core();
    let f = Fake::new(&t.path().join("state"));
    enqueue(&mut c, &f, "sid-1", "a", "host");
    enqueue(&mut c, &f, "sid-2", "b", "host");
    // Holder 99 is not running, so both turns are recoverable.
    row(
        &mut c,
        &f,
        "wake.take",
        &json!({"holder": 99, "session": "sid-1"}),
    )
    .unwrap();
    // This take first recovers the turn above (its marker was not seen: owed again, after a pause), then starts its own.
    let two = row(
        &mut c,
        &f,
        "wake.take",
        &json!({"holder": 99, "session": "sid-2"}),
    )
    .unwrap();
    let marker = format!("[kith wake {}]", two["turn"]["run"].as_str().unwrap());
    tick(&mut c, &f, &Seen(vec![("sid-2".into(), marker)])).unwrap();
    let st = state(&mut c);
    assert_eq!(st.answered["b"].how, "recovered");
    assert!(st.items["sid-1"].turn.is_none(), "unseen: owed again");
    assert_eq!(st.items["sid-1"].tries, 1);
    assert!(st.items["sid-1"].requests[0].run.is_none());
}

#[test]
fn a_pid_waiter_is_matched_by_pid_and_start_time() {
    let (t, _l, mut c) = core();
    let f = Fake::new(&t.path().join("state"));
    f.procs
        .borrow_mut()
        .insert(5, "Mon Jan 1 00:00:05 2026".into());
    let w = row(
        &mut c,
        &f,
        "wake.wait",
        &json!({"on": "pid:5", "session": "s1", "agent": "coder"}),
    )
    .unwrap();
    assert_eq!(w["waiter"]["procStart"], "Mon Jan 1 00:00:05 2026");
    assert_eq!(
        tick(&mut c, &f, &Seen(Vec::new())).unwrap(),
        0,
        "still running"
    );
    // The pid is reused by another process: the waiter's process is gone.
    f.procs
        .borrow_mut()
        .insert(5, "Tue Jan 2 00:00:05 2026".into());
    assert_eq!(tick(&mut c, &f, &Seen(Vec::new())).unwrap(), 1);
    let st = state(&mut c);
    assert!(st.waiters.is_empty());
    assert_eq!(
        st.items["s1"].requests[0].text,
        "the process you waited for has exited"
    );
}

#[test]
fn waiters_are_refused_for_a_person_and_for_a_host_session() {
    let (t, _l, mut c) = core();
    let f = Fake::new(&t.path().join("state"));
    let ask = |extra: Value| {
        let mut p = json!({"on": "at:+1h", "session": "s1", "agent": "coder"});
        p.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        p
    };
    let e = row(&mut c, &f, "wake.wait", &ask(json!({"person": "p1"}))).unwrap_err();
    assert!(
        e.to_string()
            .contains("a guest's session can't set waiters"),
        "{e}"
    );
    let e = row(&mut c, &f, "wake.wait", &ask(json!({"via": "serve"}))).unwrap_err();
    assert!(
        e.to_string()
            .contains("a serve session wakes through the host"),
        "{e}"
    );
    row(&mut c, &f, "wake.wait", &ask(json!({"via": "ask"}))).unwrap();
    assert_eq!(state(&mut c).waiters.len(), 1);
}

#[test]
fn a_persons_session_needs_the_host_door() {
    let (t, _l, mut c) = core();
    let f = Fake::new(&t.path().join("state"));
    let e = row(
        &mut c,
        &f,
        "wake.enqueue",
        &json!({"session": "s1", "id": "a", "agent": "coder", "person": "p1", "text": ["x"]}),
    )
    .unwrap_err();
    assert!(e.to_string().contains("--door host"), "{e}");
    assert!(load(c.conn()).unwrap().items.is_empty());
}

// --- the import ------------------------------------------------------------------------------------------------------

#[cfg(unix)]
mod import {
    use super::*;
    use crate::import::wake::{read_files, run_in};

    const ITEM: &str = r#"{"v": 1, "session": "s1", "agent": "coder", "person": null, "room": "r1", "door": "host",
        "requests": [{"id": "a", "why": "card", "text": "hi", "at": "2026-01-01T00:00:00Z", "run": "aaaaaaaaaaaa"},
                     {"id": "b", "why": "", "text": "later", "at": "2026-01-01T00:00:01Z", "run": null}],
        "turn": {"run": "aaaaaaaaaaaa", "pid": 4242, "procStart": "Mon Jan  1 00:00:00 2026", "at": "2026-01-01T00:00:00Z", "sid": "s1"},
        "tries": 2, "next": 1790000123.5}"#;
    const WAIT: &str = r#"{"v": 1, "id": "wwwwwwwwwwww", "on": "ci:o/r#5", "session": "s1", "agent": "coder", "why": "",
        "note": "n", "created": 1.0, "checked": 2.0, "seen": "0123", "until": 4000000000.0, "kind": "ci", "repo": "o/r", "pr": 5}"#;

    fn files(dir: &Path, entries: &[(&str, &str)]) {
        for (rel, text) in entries {
            let p = dir.join("sessions").join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
    }

    fn full(dir: &Path) {
        files(
            dir,
            &[
                ("queue/e8bc163c82eee18733288c7d4ac636db.json", ITEM),
                (
                    "queue/answered.json",
                    r#"{"x": {"how": "failed", "t": 1790000000.0, "run": "bbbbbbbbbbbb"}}"#,
                ),
                ("waits/wwwwwwwwwwww.json", WAIT),
            ],
        );
    }

    fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(d).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    out.push((p.clone(), std::fs::read(&p).unwrap()));
                }
            }
        }
        out.sort();
        out
    }

    #[test]
    fn the_files_come_in_whole_and_a_second_run_writes_nothing() {
        let (t, _l, mut c) = core();
        let state_dir = t.path().join("state");
        full(&state_dir);
        let before = snapshot(&state_dir);
        let rep = run_in(&mut c, &state_dir, false).unwrap();
        assert!(rep.changed, "{:?}", rep.lines);
        let st = state(&mut c);
        let it = &st.items["s1"];
        assert_eq!(
            (it.tries, it.next, it.room.as_deref()),
            (2, 1_790_000_123.5, Some("r1"))
        );
        let turn = it.turn.as_ref().unwrap();
        assert_eq!(
            (turn.pid, turn.proc_start.as_deref()),
            (Some(4242), Some("Mon Jan  1 00:00:00 2026"))
        );
        assert_eq!(it.requests[0].run.as_deref(), Some("aaaaaaaaaaaa"));
        assert_eq!(it.requests[1].run, None);
        assert_eq!(st.waiters["wwwwwwwwwwww"].seen.as_deref(), Some("0123"));
        assert_eq!(st.answered["x"].how, "failed");
        let log = |c: &Core| -> i64 {
            c.conn()
                .query_row(
                    "SELECT count(*) FROM import_log WHERE step = 'wake'",
                    [],
                    |r| r.get(0),
                )
                .unwrap()
        };
        assert_eq!(log(&c), 1);
        let again = run_in(&mut c, &state_dir, false).unwrap();
        assert!(!again.changed, "{:?}", again.lines);
        assert_eq!(log(&c), 1, "no second log row");
        assert_eq!(snapshot(&state_dir), before, "the files are only read");
    }

    #[test]
    fn a_dry_run_writes_nothing() {
        let (t, _l, mut c) = core();
        let state_dir = t.path().join("state");
        full(&state_dir);
        let rep = run_in(&mut c, &state_dir, true).unwrap();
        assert!(rep.changed);
        assert!(load(c.conn()).unwrap().items.is_empty());
        assert!(load(c.conn()).unwrap().waiters.is_empty());
    }

    #[test]
    fn rows_that_differ_are_replaced_by_the_files() {
        let (t, _l, mut c) = core();
        let state_dir = t.path().join("state");
        full(&state_dir);
        run_in(&mut c, &state_dir, false).unwrap();
        let f = Fake::new(&state_dir);
        enqueue(&mut c, &f, "other", "z", "run");
        let rep = run_in(&mut c, &state_dir, false).unwrap();
        assert!(rep.changed);
        let st = state(&mut c);
        assert!(!st.items.contains_key("other"));
    }

    #[test]
    fn what_it_cannot_read_is_skipped_and_what_it_does_not_know_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        files(
            dir.path(),
            &[
                ("queue/a.json", "{nope"),
                ("queue/b.json", "{}"),
                ("queue/answered.json", "[]"),
                ("waits/c.json", "x"),
                ("queue/ok.json", ITEM),
            ],
        );
        let (st, skipped, _) = read_files(dir.path()).unwrap();
        assert_eq!(st.items.len(), 1);
        assert_eq!(skipped.len(), 3, "{skipped:?}");

        for (name, text, why) in [
            ("v2", ITEM.replace(r#""v": 1"#, r#""v": 2"#), "version 2"),
            ("door", ITEM.replace(r#""host""#, r#""carrier""#), "door"),
        ] {
            let d = tempfile::tempdir().unwrap();
            files(d.path(), &[("queue/x.json", &text)]);
            let e = read_files(d.path()).unwrap_err();
            assert!(e.to_string().contains(why), "{name}: {e}");
            assert!(e.to_string().contains("queue/x.json"), "{name}: {e}");
        }
        let d = tempfile::tempdir().unwrap();
        files(d.path(), &[("queue/x.json", ITEM), ("queue/y.json", ITEM)]);
        assert!(
            read_files(d.path())
                .unwrap_err()
                .to_string()
                .contains("another file")
        );
        let d = tempfile::tempdir().unwrap();
        files(
            d.path(),
            &[
                ("queue/x.json", ITEM),
                ("queue/y.json", &ITEM.replace("\"s1\"", "\"s2\"")),
            ],
        );
        assert!(
            read_files(d.path())
                .unwrap_err()
                .to_string()
                .contains("queued twice")
        );
        let d = tempfile::tempdir().unwrap();
        files(
            d.path(),
            &[("waits/w.json", &WAIT.replace(r#""v": 1"#, r#""v": 3"#))],
        );
        assert!(
            read_files(d.path())
                .unwrap_err()
                .to_string()
                .contains("waits/w.json")
        );
    }

    #[test]
    fn a_file_without_v_is_version_one() {
        let d = tempfile::tempdir().unwrap();
        files(
            d.path(),
            &[("queue/x.json", &ITEM.replace(r#""v": 1, "#, ""))],
        );
        let (st, ..) = read_files(d.path()).unwrap();
        assert_eq!(st.items["s1"].format, 1);
    }
}
