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
fn take_offers_a_session_a_live_writer_holds_and_back_held_pauses_it() {
    // As the Python's take: the host asks the terminal's claim next and answers `back --held` (resume.rs). The hold is also
    // a parked `terminal_live` turn in the conversations lane (q217; see the turn tests below), not a silent skip.
    let (t, _l, mut c) = core();
    let f = Fake::new(&t.path().join("state"));
    std::fs::create_dir_all(t.path().join("state/live/terminal")).unwrap();
    std::fs::write(
        t.path().join("state/live/terminal/s1.claim"),
        r#"{"pid": 1}"#,
    )
    .unwrap();
    enqueue(&mut c, &f, "s1", "a", "host");
    let turn = row(&mut c, &f, "wake.take", &json!({"holder": 1})).unwrap();
    assert_eq!(
        turn["turn"]["ids"],
        json!(["a"]),
        "offered despite the live writer"
    );
    let run = turn["turn"]["run"].as_str().unwrap().to_owned();
    let back = row(&mut c, &f, "wake.back", &json!({"run": run, "held": true})).unwrap();
    assert_eq!(back["done"], true);
    let st = state(&mut c);
    assert_eq!(st.items["s1"].requests.len(), 1, "nothing was lost");
    assert_eq!(st.items["s1"].tries, 0, "a held writer is not a failed try");
    assert!(
        st.items["s1"].next > f.now.get(),
        "paused, so the next take does not offer it at once"
    );
    assert!(row(&mut c, &f, "wake.take", &json!({"holder": 1})).unwrap()["turn"].is_null());
}

fn turns_of(c: &Core, conv: &str) -> Vec<(String, Option<String>, String)> {
    let mut st = c
        .conn()
        .prepare(
            "SELECT state, parked_reason, source FROM turns WHERE conversation = ?1 ORDER BY id",
        )
        .unwrap();
    st.query_map([conv], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn terminal_claim(c: &Core, conv: &str) -> i64 {
    c.conn()
        .execute(
            "INSERT OR IGNORE INTO conversations (id, agent, engine, created_at, updated_at) VALUES (?1, 'coder', 'claude', 1, 1)",
            [conv],
        )
        .unwrap();
    c.conn()
        .execute(
            "INSERT INTO claims (claim_id, conversation, generation, kind, state, person, pid, created_at, claimed_at, running_at)
             VALUES (7, ?1, 1, 'terminal', 'running', 'owner', 1, 1, 1, 1)",
            [conv],
        )
        .unwrap();
    7
}

#[test]
fn take_queues_the_runs_turn_in_the_conversations_lane_once() {
    let (t, _l, mut c) = core();
    let f = Fake::new(&t.path().join("state"));
    enqueue(&mut c, &f, "s1", "a", "host");
    let took = row(&mut c, &f, "wake.take", &json!({"holder": 1})).unwrap();
    let run = took["turn"]["run"].as_str().unwrap().to_owned();
    assert_eq!(
        turns_of(&c, "s1"),
        [("queued".to_owned(), None, "wake".to_owned())]
    );
    let prompt: String = c
        .conn()
        .query_row(
            "SELECT prompt FROM turns WHERE conversation = 's1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        prompt.starts_with(&format!("[kith wake {run}]")),
        "{prompt}"
    );
    // The link names the turn; it is what closing goes through, not the prompt (a changed prompt closes all the same).
    let linked: i64 = c
        .conn()
        .query_row(
            "SELECT turn_id FROM wake_turns WHERE run = ?1",
            [&run],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(linked, 1);
    c.conn()
        .execute("UPDATE turns SET prompt = 'edited'", [])
        .unwrap();
    // Done: the turn the run carried is done with it.
    row(&mut c, &f, "wake.done", &json!({"run": run})).unwrap();
    assert_eq!(turns_of(&c, "s1")[0].0, "done");
    let links: i64 = c
        .conn()
        .query_row("SELECT count(*) FROM wake_turns", [], |r| r.get(0))
        .unwrap();
    assert_eq!(links, 0, "the link goes with the run");
}

#[test]
fn a_turn_that_looks_like_a_runs_note_is_not_closed_with_the_run() {
    // A person typing the marker into a turn of the same conversation must not get it closed by a wake's end.
    let (t, _l, mut c) = core();
    let f = Fake::new(&t.path().join("state"));
    enqueue(&mut c, &f, "s1", "a", "host");
    let took = row(&mut c, &f, "wake.take", &json!({"holder": 1})).unwrap();
    let run = took["turn"]["run"].as_str().unwrap().to_owned();
    c.conn()
        .execute(
            "INSERT INTO turns (conversation, person, source, prompt, state, queued_at)
             VALUES ('s1', 'owner', 'app', ?1, 'queued', 1)",
            [format!("[kith wake {run}] typed by hand")],
        )
        .unwrap();
    row(&mut c, &f, "wake.done", &json!({"run": run})).unwrap();
    let states: Vec<String> = turns_of(&c, "s1").into_iter().map(|t| t.0).collect();
    assert_eq!(states, ["done", "queued"]);
}

#[test]
fn a_session_a_terminal_holds_gets_a_parked_turn_that_waits_for_the_terminal_to_end() {
    let (t, _l, mut c) = core();
    let f = Fake::new(&t.path().join("state"));
    let claim = terminal_claim(&c, "s1");
    enqueue(&mut c, &f, "s1", "a", "host");
    let took = row(&mut c, &f, "wake.take", &json!({"holder": 1})).unwrap();
    assert_eq!(
        took["turn"]["ids"],
        json!(["a"]),
        "the answer is the Python's"
    );
    assert_eq!(
        turns_of(&c, "s1"),
        [(
            "parked".to_owned(),
            Some("terminal_live".to_owned()),
            "wake".to_owned()
        )]
    );
    // While the terminal lives, the level-triggered wake leaves it parked ...
    let woke = crate::conversations::claims::unpark_ready(&mut c, "s1", true, true, 5).unwrap();
    assert_eq!(woke.len(), 0);
    // ... and queues it once the terminal's claim has ended.
    c.conn()
        .execute(
            "UPDATE claims SET state = 'ended', ended_at = 2 WHERE claim_id = ?1",
            [claim],
        )
        .unwrap();
    let woke = crate::conversations::claims::unpark_ready(&mut c, "s1", true, false, 5).unwrap();
    assert_eq!(woke.len(), 1);
    assert_eq!(turns_of(&c, "s1")[0].0, "queued");
}

#[test]
fn a_claim_file_writer_parks_the_turn_too_and_a_given_back_run_cancels_its_turn() {
    // Until the switch the claim files are the writers (the Python's writer_live).
    let (t, _l, mut c) = core();
    let f = Fake::new(&t.path().join("state"));
    std::fs::create_dir_all(t.path().join("state/live/terminal")).unwrap();
    std::fs::write(
        t.path().join("state/live/terminal/s1.claim"),
        r#"{"pid": 1}"#,
    )
    .unwrap();
    enqueue(&mut c, &f, "s1", "a", "host");
    let took = row(&mut c, &f, "wake.take", &json!({"holder": 1})).unwrap();
    let run = took["turn"]["run"].as_str().unwrap().to_owned();
    assert_eq!(turns_of(&c, "s1")[0].1.as_deref(), Some("terminal_live"));
    row(&mut c, &f, "wake.back", &json!({"run": run, "held": true})).unwrap();
    assert_eq!(
        turns_of(&c, "s1")[0].0,
        "cancelled",
        "the next take makes a new run and a new turn"
    );
}

fn answered_rows(c: &Core) -> i64 {
    c.conn()
        .query_row("SELECT count(*) FROM wake_answered", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn an_answered_id_is_remembered_for_seven_days() {
    let (t, _l, mut c) = core();
    let f = Fake::new(&t.path().join("state"));
    assert_eq!(enqueue(&mut c, &f, "s1", "a", "run"), "queued");
    row(
        &mut c,
        &f,
        "wake.withdraw",
        &json!({"session": "s1", "id": "a"}),
    )
    .unwrap();
    assert_eq!(answered_rows(&c), 1);
    // Six days on, the id is still a duplicate.
    f.now.set(T0 + 6.0 * 86400.0);
    assert_eq!(enqueue(&mut c, &f, "s1", "a", "run"), "duplicate");
    assert_eq!(answered_rows(&c), 1);
    // Eight days on, it is forgotten (the write that accepts it prunes the row) and queues as new.
    f.now.set(T0 + 8.0 * 86400.0);
    assert_eq!(enqueue(&mut c, &f, "s1", "a", "run"), "queued");
    assert_eq!(answered_rows(&c), 0);
}

#[test]
fn withdraw_with_an_empty_id_withdraws_nothing_but_still_unwaits() {
    // The Python's `--id ''` is falsy: the waiters go, and no request matches the empty id.
    let (t, _l, mut c) = core();
    let f = Fake::new(&t.path().join("state"));
    enqueue(&mut c, &f, "s1", "a", "run");
    row(
        &mut c,
        &f,
        "wake.wait",
        &json!({"on": "pid:1", "session": "s1", "agent": "coder"}),
    )
    .unwrap();
    assert_eq!(state(&mut c).waiters.len(), 1);
    let n = row(
        &mut c,
        &f,
        "wake.withdraw",
        &json!({"session": "s1", "id": ""}),
    )
    .unwrap();
    assert_eq!(n["count"], 1, "only the waiter");
    let st = state(&mut c);
    assert!(st.waiters.is_empty());
    assert_eq!(st.items["s1"].requests.len(), 1, "the request stays");
}

#[test]
fn an_evaluation_outside_the_lock_starts_over_when_the_rows_moved() {
    use super::write_after;
    let (t, l, mut c) = core();
    let f = Fake::new(&t.path().join("state"));
    enqueue(&mut c, &f, "s1", "a", "run");
    let mut seen = Vec::new();
    write_after(&mut c, T0, |st| {
        seen.push(st.items["s1"].tries);
        if seen.len() == 1 {
            // Another writer gets in while this one evaluates (no lock is held then).
            let other = rusqlite::Connection::open(&l.db).unwrap();
            other
                .execute(
                    "UPDATE wake_sessions SET tries = 3 WHERE session = 's1'",
                    [],
                )
                .unwrap();
        }
        st.items.get_mut("s1").unwrap().next = 99.0;
    })
    .unwrap();
    assert_eq!(seen, [0, 3], "the second look saw the other writer's rows");
    let st = state(&mut c);
    assert_eq!((st.items["s1"].tries, st.items["s1"].next), (3, 99.0));
}

/// The real process table: a pid is matched by its start time as well (a reused pid has another one).
#[cfg(unix)]
#[test]
fn a_pid_is_matched_by_pid_and_start_time_on_the_real_system() {
    use super::outside::System;
    let t = tempfile::tempdir().unwrap();
    let sys = System {
        state: t.path().to_path_buf(),
        gh: None,
    };
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let pid = i64::from(child.id());
    let start = sys.proc_start(pid).expect("the system gives a start time");
    assert!(sys.alive(pid, Some(&start)), "alive with its true start");
    assert!(sys.alive(pid, None), "no start recorded: the pid alone");
    // Same form, another start: the pid was reused. Anything else cannot be told apart: alive, never dead.
    let (other, foreign) = if cfg!(target_os = "linux") {
        ("bGONE:1", "e1")
    } else {
        ("e1", "bGONE:1")
    };
    for (recorded, want) in [
        (start.as_str(), true),
        (other, false),
        (foreign, true),
        ("Mon Jan 1 00:00:00 1990", true),
        ("", true),
        ("  ", true),
    ] {
        assert_eq!(
            sys.alive(pid, Some(recorded)),
            want,
            "recorded {recorded:?}"
        );
    }
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(!sys.alive(pid, Some(&start)), "exited");
    assert!(
        !sys.alive(pid, Some("Mon Jan 1 00:00:00 1990")),
        "exited, old-form record: gone is gone"
    );
    assert!(!sys.alive(pid, None), "exited, no start");
    // A terminal's claim file names a pid and the start it had; a stale start is not a live writer.
    let live = t.path().join("live/terminal");
    std::fs::create_dir_all(&live).unwrap();
    let me = i64::from(std::process::id());
    let mine = sys.proc_start(me).unwrap();
    std::fs::write(
        live.join("s1.claim"),
        format!(r#"{{"pid": {me}, "procStart": "{mine}"}}"#),
    )
    .unwrap();
    assert!(sys.writer_live("s1"));
    std::fs::write(
        live.join("s1.claim"),
        format!(r#"{{"pid": {me}, "procStart": "{other}"}}"#),
    )
    .unwrap();
    assert!(!sys.writer_live("s1"), "a stale procStart is a reused pid");
    std::fs::write(
        live.join("s1.claim"),
        format!(r#"{{"pid": {me}, "procStart": "Mon Jan 1 00:00:00 1990"}}"#),
    )
    .unwrap();
    assert!(
        sys.writer_live("s1"),
        "a Python claim's lstart text is an unknown start: alive"
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

/// The Python `kith-wake take` has no conversations lane, so it starts a session whatever agent core.db has for it. Here a
/// session that is another agent's conversation gets no turn: its requests end as `failed` in the same take, which goes
/// on to the next owed session (one bad item must not stop every unnamed take). Intended difference. `wake.enqueue`
/// refuses such a session at the start, so only an item queued before the conversation was written gets here.
#[test]
fn a_take_for_another_agents_conversation_fails_that_item_and_goes_on() {
    let (t, _l, mut c) = core();
    let f = Fake::new(&t.path().join("state"));
    enqueue(&mut c, &f, "s2", "a", "host");
    enqueue(&mut c, &f, "s1", "b", "host");
    c.conn()
        .execute(
            "INSERT INTO conversations (id, agent, engine, created_at, updated_at) VALUES ('s2', 'other', 'claude', 1, 1)",
            [],
        )
        .unwrap();
    let took = row(&mut c, &f, "wake.take", &json!({"holder": 1})).unwrap();
    assert_eq!(took["turn"]["session"], "s1");
    assert_eq!(turns_of(&c, "s1").len(), 1);
    assert_eq!(turns_of(&c, "s2").len(), 0);
    let st = state(&mut c);
    assert!(!st.items.contains_key("s2"));
    assert_eq!(queue::status(&st, "a").0, "failed");
}

#[test]
fn enqueue_refuses_a_session_core_db_gives_to_another_agent() {
    let (t, _l, mut c) = core();
    let f = Fake::new(&t.path().join("state"));
    c.conn()
        .execute(
            "INSERT INTO conversations (id, agent, engine, created_at, updated_at) VALUES ('s2', 'other', 'claude', 1, 1)",
            [],
        )
        .unwrap();
    let err = row(
        &mut c,
        &f,
        "wake.enqueue",
        &json!({"session": "s2", "id": "a", "agent": "coder", "door": "host", "text": ["hi"]}),
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("belongs to another agent"),
        "{err}"
    );
    assert!(!state(&mut c).items.contains_key("s2"));
}
