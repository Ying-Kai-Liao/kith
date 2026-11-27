#![allow(clippy::unwrap_used)]
#![allow(
    clippy::items_after_statements,
    clippy::unnecessary_wraps,
    clippy::unused_self,
    clippy::needless_pass_by_value,
    reason = "a test rig: its listers have the production lister's shape, its helpers sit with their one test"
)]
//! The `wake` step (WK6) on temp homes only: a real core.db and real-shaped queue files. U1 the gate, U2 the preflight, U3 the
//! crash points, U7 the step's own locks against the import's, and a step that goes round once. The rollback is in
//! `wake_back_tests.rs`, which shares this rig. Nothing here touches the machine's own Kith folder.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::Duration;

use rusqlite::Connection;
use serde_json::{Value, json};
use tempfile::TempDir;

use super::wake::{Crash, Facts, MESSAGE, Wake, differences, refusal};
use super::wake_preflight::{self as pre, Lister};
use crate::cutover::same_answer::may_serve;
use crate::cutover::state::{self, Phase, Switch};
use crate::cutover::{Env, Module, Opts, rollback, step, tombstone};
use crate::tests::{machine, open_all};
use crate::wake::{file_key, load};

pub(super) fn opts() -> Opts {
    Opts::default()
}

fn dry() -> Opts {
    Opts {
        dry_run: true,
        ..Opts::default()
    }
}

/// A child that outlives the test body and is killed when dropped: a turn names a pid that is alive.
pub(super) struct Proc(pub Child);

impl Proc {
    pub(super) fn new() -> Self {
        Self(Command::new("sleep").arg("120").spawn().unwrap())
    }

    pub(super) fn pid(&self) -> i64 {
        i64::from(self.0.id())
    }

    pub(super) fn stamp(&self) -> String {
        match kith_kernel::procstart::probe(self.0.id()) {
            kith_kernel::procstart::Probe::Alive(Some(s)) => s,
            other => panic!("{other:?}"),
        }
    }

    pub(super) fn kill(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        self.kill();
    }
}

pub(super) struct Rig {
    pub dir: TempDir,
    pub env: Env,
    pub host: PathBuf,
    pub state: PathBuf,
    pub machine: PathBuf,
}

fn idle() -> Result<Vec<(u32, Vec<String>)>, String> {
    Ok(Vec::new())
}

pub(super) fn rig() -> Rig {
    let (t, m) = machine();
    let mut paths = crate::tests::paths(&m);
    paths.state = m.join("state");
    paths.home = m.join("home");
    let mut env = Env::with_paths(&paths);
    // The host's data folder is the tmp folder's, so `host.lock` is this test's to hold.
    let host = t.path().join("host");
    std::fs::create_dir_all(&host).unwrap();
    env.serve_db = Some(host.join("serve.db"));
    let state = paths.state.clone();
    for d in ["queue", "waits"] {
        std::fs::create_dir_all(state.join("sessions").join(d)).unwrap();
    }
    // core.db exists and is migrated (a Mac that has run the core), and the lock is let go again.
    drop(open_all(&env.layout));
    for dep in ["claims", "transcripts"] {
        state::write(
            &env.layout,
            dep,
            &Switch {
                phase: Phase::Switched,
                snapshot: None,
                at: 1,
            },
        )
        .unwrap();
    }
    Rig {
        dir: t,
        env,
        host,
        state,
        machine: m,
    }
}

impl Rig {
    pub(super) fn queue(&self) -> PathBuf {
        self.state.join("sessions/queue")
    }

    pub(super) fn waits(&self) -> PathBuf {
        self.state.join("sessions/waits")
    }

    pub(super) fn tomb(&self) -> PathBuf {
        self.queue().join(".tombstone")
    }

    /// A wake that passes the gate and sees no process.
    pub(super) fn wake(&self) -> Wake {
        Wake::new().open().processes(idle)
    }

    /// One queue file, as `kith-wake` writes it. `reqs`: (id, run it is carried by).
    pub(super) fn item(
        &self,
        session: &str,
        door: &str,
        reqs: &[(&str, Option<&str>)],
        turn: Option<Value>,
    ) {
        let requests: Vec<Value> = reqs
            .iter()
            .map(|(id, run)| json!({"id": id, "why": "w", "text": "hello", "at": "2026-11-26T10:00:00Z", "run": run}))
            .collect();
        let v = json!({"v": 1, "session": session, "agent": "mel", "person": null, "room": null, "door": door,
            "requests": requests, "turn": turn, "tries": 0, "next": 0});
        std::fs::write(
            self.queue().join(format!("{}.json", file_key(session))),
            v.to_string(),
        )
        .unwrap();
    }

    pub(super) fn answered(&self, id: &str, how: &str) {
        let at = self.queue().join("answered.json");
        let mut all: Value = std::fs::read(&at)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_else(|| json!({}));
        all[id] = json!({"how": how, "t": 4_102_444_800_i64, "run": null});
        std::fs::write(at, all.to_string()).unwrap();
    }

    pub(super) fn waiter(&self, id: &str, session: &str) -> Value {
        let v = json!({"v": 1, "id": id, "on": "at:+1h", "session": session, "agent": "mel", "why": "", "note": "",
            "created": 1, "checked": 0, "seen": null, "until": 4_102_444_800_i64, "kind": "at", "at": 4_102_444_800_i64});
        std::fs::write(self.waits().join(format!("{id}.json")), v.to_string()).unwrap();
        v
    }

    pub(super) fn db(&self) -> Connection {
        Connection::open(&self.env.layout.db).unwrap()
    }

    /// The host holding `host.lock`, as a running host does.
    pub(super) fn running_host(&self) -> std::fs::File {
        use fs2::FileExt as _;
        let f = std::fs::File::create(self.host.join("host.lock")).unwrap();
        f.lock_exclusive().unwrap();
        f
    }

    pub(super) fn phase(&self) -> Option<Phase> {
        state::read(&self.env.layout, "wake")
            .unwrap()
            .map(|s| s.phase)
    }

    pub(super) fn step(&self, w: Wake, o: Opts) -> crate::error::Result<Vec<String>> {
        step(&[Box::new(w) as Box<dyn Module>], &self.env, "wake", o)
    }

    pub(super) fn rollback(&self, w: Wake, o: Opts) -> crate::error::Result<Vec<String>> {
        rollback(&[Box::new(w) as Box<dyn Module>], &self.env, "wake", o)
    }

    /// Nothing was written: no record, no tombstone, no marker.
    fn untouched(&self) {
        assert_eq!(self.phase(), None, "no switch record");
        assert!(!self.tomb().exists(), "no tombstone");
        assert!(crate::cutover::marker::read(&self.db(), "wake").is_none());
    }

    /// What the files say, read the way the import reads them.
    pub(super) fn files(&self) -> crate::wake::State {
        crate::import::wake::read_files(&self.state).unwrap().0
    }

    pub(super) fn rows(&self) -> crate::wake::State {
        load(&self.db()).unwrap()
    }

    pub(super) fn serves(&self) -> bool {
        may_serve(&Wake::new(), &self.env, &self.db(), false).ok
    }
}

pub(super) fn err(r: crate::error::Result<Vec<String>>) -> String {
    r.unwrap_err().to_string()
}

/// Two sessions owed, one answered id and one waiter: what a Mac with a busy queue has.
pub(super) fn busy(r: &Rig) {
    r.item("s1", "run", &[("card:1", None), ("card:2", None)], None);
    r.item("s2", "host", &[("ci:1", None)], None);
    r.answered("old:1", "answered");
    r.waiter("w1", "s1");
}

// --- U1: the gate, each condition on its own --------------------------------------------------------------------------------

fn passing() -> Facts {
    Facts {
        runs_wake_turns: true,
        launchd: Ok(()),
        holder: Ok(()),
        host: None,
    }
}

#[test]
fn each_condition_of_the_gate_refuses_on_its_own() {
    assert!(refusal(&passing()).is_none());
    let cases: [(u8, Facts); 4] = [
        (
            1,
            Facts {
                runs_wake_turns: false,
                ..passing()
            },
        ),
        (
            2,
            Facts {
                launchd: Err("launchd runs /old/kith".into()),
                ..passing()
            },
        ),
        (
            3,
            Facts {
                holder: Err("there is no kith-holder".into()),
                ..passing()
            },
        ),
        (
            4,
            Facts {
                host: Some("the host is running".into()),
                ..passing()
            },
        ),
    ];
    for (n, f) in cases {
        let said = refusal(&f).unwrap();
        assert!(said.starts_with(MESSAGE), "{said}");
        assert!(said.contains(&format!("Failed: {n}: ")), "{n}: {said}");
        for other in (1..=4).filter(|o| *o != n) {
            assert!(!said.contains(&format!("; {other}: ")), "{n} alone: {said}");
        }
    }
    // All of them at once name every line.
    let all = refusal(&Facts {
        runs_wake_turns: false,
        launchd: Err("a".into()),
        holder: Err("b".into()),
        host: Some("c".into()),
    })
    .unwrap();
    for n in 1..=4 {
        assert!(all.contains(&format!("{n}: ")), "{all}");
    }
}

#[test]
fn the_shut_gate_refuses_a_step_but_not_a_dry_run() {
    let r = rig();
    busy(&r);
    // The build's own constant (false until the lines of its checklist are all true) and this test home's missing plist.
    let w = Wake::new().processes(idle);
    assert!(
        w.step_refusal(&r.env)
            .is_some_and(|l| l.starts_with(MESSAGE))
    );
    let e = err(r.step(Wake::new().processes(idle), opts()));
    assert!(e.contains(MESSAGE), "{e}");
    r.untouched();
    let said = r.step(Wake::new().processes(idle), dry()).unwrap();
    assert!(
        said.iter()
            .any(|l| l.contains("dry run: wake is not switched; nothing was changed")),
        "{said:?}"
    );
    assert!(said.iter().any(|l| l.contains(MESSAGE)), "{said:?}");
    r.untouched();
}

/// `KITH_WAKE_GATE_OPEN=1` is read by a debug build only (the e2e steps a build whose gate is shut); a release-shaped check
/// ignores it, and the constant stays the one switch a real Mac depends on.
#[allow(
    clippy::assertions_on_constants,
    reason = "the constant is what this pins"
)]
#[test]
fn the_gate_env_opens_a_debug_build_only_and_never_a_release_one() {
    use super::wake::{RUNS_WAKE_TURNS, gate_open};
    assert!(
        !RUNS_WAKE_TURNS,
        "the gate stays shut until the checklist is ticked"
    );
    assert!(gate_open(true, Some("1")));
    assert!(!gate_open(true, None));
    assert!(!gate_open(true, Some("0")));
    assert!(!gate_open(false, Some("1")), "a release build ignores it");
    assert!(!gate_open(false, None));
}

/// `KITH_WAKE_SWITCHED_FOR_TEST=1` says "switched" to a debug build's core only (a difftest daemon's), never a release one's.
#[test]
fn the_switched_env_of_a_difftest_daemon_is_read_by_a_debug_build_only() {
    use crate::wake::forced_for_test;
    assert!(forced_for_test(true, Some("1")));
    assert!(!forced_for_test(true, None));
    assert!(!forced_for_test(true, Some("0")));
    assert!(!forced_for_test(false, Some("1")));
}

#[test]
fn a_running_host_refuses_the_step_and_nothing_is_written() {
    let r = rig();
    busy(&r);
    let _host = r.running_host();
    let e = err(r.step(r.wake(), opts()));
    assert!(e.contains("the host is running"), "{e}");
    r.untouched();
    let looked = err(r.step(r.wake(), dry()));
    assert!(
        looked.contains("dry run: not ready") && looked.contains("the host is running"),
        "{looked}"
    );
    r.untouched();
}

// --- U2: the preflight, each item on its own -------------------------------------------------------------------------------

fn items_of(r: &Rig, list: Lister) -> Vec<Vec<String>> {
    pre::preflight(&r.env, &r.state, "RUN", list, false)
        .unwrap()
        .items
        .into_iter()
        .map(|i| i.ids)
        .collect()
}

#[test]
fn each_preflight_item_refuses_on_its_own_and_names_what_it_saw() {
    // 1: the host.
    let r = rig();
    let host = r.running_host();
    let items = items_of(&r, idle);
    assert_eq!(items[0], ["RUN"]);
    assert!(items[1..].iter().all(Vec::is_empty), "{items:?}");
    drop(host);

    // 3: a `kith-wake deliver` is running (and is not this process).
    fn one() -> Result<Vec<(u32, Vec<String>)>, String> {
        Ok(vec![
            (
                4242,
                vec![
                    "/usr/bin/python3".into(),
                    "/k/bin/kith-wake".into(),
                    "deliver".into(),
                    "sess-d".into(),
                ],
            ),
            (
                4243,
                vec![
                    "/usr/bin/python3".into(),
                    "/k/bin/kith-wake".into(),
                    "ls".into(),
                ],
            ),
        ])
    }
    let items = items_of(&r, one);
    assert_eq!(items[2], ["pid 4242 delivering sess-d"]);
    assert!(items[0].is_empty() && items[1].is_empty() && items[3].is_empty());

    // 3: a process table that cannot be read says so, and refuses.
    fn blind() -> Result<Vec<(u32, Vec<String>)>, String> {
        Err("cannot see processes: the process list does not hold this process".into())
    }
    let items = items_of(&r, blind);
    assert_eq!(items[2].len(), 1);
    assert!(items[2][0].contains("cannot see processes"), "{items:?}");
    // The real lister lists this process with its own arguments (#1046), so it is never blind here.
    let me = std::process::id();
    assert!(
        pre::own_processes()
            .unwrap()
            .iter()
            .any(|(p, a)| *p == me && !a.is_empty())
    );

    // 4: a turn in the files whose holder runs refuses and names the session; one whose holder is gone does not.
    let mut kid = Proc::new();
    r.item(
        "s-live",
        "host",
        &[("a:1", Some("run1"))],
        Some(json!({"run": "run1", "pid": kid.pid(), "procStart": kid.stamp(), "at": "t", "sid": null})),
    );
    let items = items_of(&r, idle);
    assert_eq!(items[3].len(), 1, "{items:?}");
    assert!(
        items[3][0].starts_with("s-live (turn run1 held by pid"),
        "{items:?}"
    );
    kid.kill();
    let items = items_of(&r, idle);
    assert!(items.iter().all(Vec::is_empty), "{items:?}");
}

#[test]
fn a_live_holder_stops_the_step_before_it_writes_anything() {
    let r = rig();
    let kid = Proc::new();
    r.item(
        "s-live",
        "host",
        &[("a:1", Some("run1"))],
        Some(json!({"run": "run1", "pid": kid.pid(), "procStart": kid.stamp(), "at": "t", "sid": null})),
    );
    for o in [dry(), opts()] {
        let e = err(r.step(r.wake(), o));
        assert!(e.contains("preflight") && e.contains("s-live"), "{e}");
    }
    r.untouched();
}

#[test]
fn the_preflight_refuses_nothing_an_earlier_run_of_the_step_left() {
    let r = rig();
    busy(&r);
    // The tombstone, a `stepping` record and the held turn of a holder that is gone.
    r.item(
        "s3",
        "run",
        &[("x:1", Some("run9"))],
        Some(json!({"run": "run9", "pid": 1_999_999, "procStart": "x", "at": "t", "sid": "s3"})),
    );
    tombstone::ensure(&r.tomb(), "wake", 1).unwrap();
    state::write(
        &r.env.layout,
        "wake",
        &Switch {
            phase: Phase::Stepping,
            snapshot: None,
            at: 1,
        },
    )
    .unwrap();
    let items = items_of(&r, idle);
    assert!(items.iter().all(Vec::is_empty), "{items:?}");
}

// --- a step, round once ----------------------------------------------------------------------------------------------------

#[test]
fn a_step_moves_the_queue_into_the_core_and_puts_the_tombstone() {
    let r = rig();
    busy(&r);
    let said = r.step(r.wake(), opts()).unwrap();
    assert!(said.iter().any(|l| l.contains("same answer")), "{said:?}");
    assert_eq!(r.phase(), Some(Phase::Switched));
    assert!(tombstone::is_tombstone(&r.tomb()));
    assert_eq!(tombstone::module_of(&r.tomb()).as_deref(), Some("wake"));
    assert_eq!(
        r.rows(),
        r.files(),
        "the core holds exactly the files' queue"
    );
    assert_eq!(r.rows().items.len(), 2);
    assert_eq!(r.rows().answered.len(), 1);
    assert_eq!(r.rows().waiters.len(), 1);
    assert!(r.serves(), "the core answers for the queue now");
    // Run again: it is switched, nothing more.
    let again = r.step(r.wake(), opts()).unwrap();
    assert!(
        again.iter().any(|l| l.contains("already switched")),
        "{again:?}"
    );
    // The step let go of everything it held.
    assert!(std::fs::File::open(r.queue().join(".lock")).is_ok());
    assert!(
        !r.wake()
            .step_refusal(&r.env)
            .is_some_and(|l| l.contains("4:"))
    );
}

#[test]
fn a_dry_run_reads_the_queue_and_writes_nothing() {
    let r = rig();
    busy(&r);
    let said = r.step(r.wake(), dry()).unwrap();
    assert!(
        said.iter().any(|l| l.contains("preflight: ready")),
        "{said:?}"
    );
    r.untouched();
    assert_eq!(r.rows(), crate::wake::State::default());
}

// --- U7: the step's own locks and the import's -----------------------------------------------------------------------------

#[test]
fn a_step_with_both_lock_files_present_ends() {
    // The import used to ask for shared locks on `waits/.lock` and `queue/.lock`; flock belongs to the open file, so a request on
    // a new descriptor waits for ever behind this process's own exclusive one. A watchdog fails the test instead of hanging it.
    let r = rig();
    busy(&r);
    for d in [r.queue(), r.waits()] {
        std::fs::write(d.join(".lock"), "").unwrap();
    }
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = done.clone();
    std::thread::spawn(move || {
        for _ in 0..300 {
            if flag.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        eprintln!(
            "the step with both .lock files present did not end: the import waits on its own lock"
        );
        std::process::abort();
    });
    let said = r.step(r.wake(), opts());
    done.store(true, std::sync::atomic::Ordering::SeqCst);
    said.unwrap();
    assert_eq!(r.phase(), Some(Phase::Switched));
}

#[test]
fn a_kith_wake_holding_a_lock_holds_the_step_up_and_then_refuses_it_with_nothing_written() {
    use fs2::FileExt as _;
    let r = rig();
    busy(&r);
    let held = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(r.queue().join(".lock"))
        .unwrap();
    held.lock_exclusive().unwrap();
    let e = err(r.step(r.wake().waiting(Duration::from_millis(200)), opts()));
    assert!(e.contains("is held by a running kith-wake"), "{e}");
    r.untouched();
    drop(held);
    r.step(r.wake(), opts()).unwrap();
}

// --- U3: a crash at each point loses nothing ---------------------------------------------------------------------------------

#[test]
fn a_crash_after_the_record_and_before_the_tombstone_leaves_python_the_writer() {
    for at in [Crash::Import, Crash::BeforeTombstone] {
        let r = rig();
        busy(&r);
        let e = err(r.step(r.wake().crash_at(at), opts()));
        assert!(e.contains("injected"), "{e}");
        assert_eq!(r.phase(), Some(Phase::Stepping), "{at:?}");
        assert!(
            !r.tomb().exists(),
            "{at:?}: kith-wake is still in Files mode"
        );
        // Python writes a request meanwhile: the rerun imports again and the rows are the files' (nothing lost).
        r.item("s9", "run", &[("late:1", None)], None);
        r.step(r.wake(), opts()).unwrap();
        assert_eq!(r.phase(), Some(Phase::Switched), "{at:?}");
        assert!(tombstone::is_tombstone(&r.tomb()));
        assert_eq!(r.rows(), r.files(), "{at:?}");
        assert!(
            r.rows().items.contains_key("s9"),
            "{at:?}: the late request is in the core"
        );
        assert!(r.serves());
    }
}

#[test]
fn a_crash_after_the_tombstone_is_finished_by_a_rerun_with_the_import_a_no_op() {
    let r = rig();
    busy(&r);
    let e = err(r.step(r.wake().crash_at(Crash::AfterTombstone), opts()));
    assert!(e.contains("injected"), "{e}");
    assert_eq!(r.phase(), Some(Phase::Stepping));
    assert!(
        tombstone::is_tombstone(&r.tomb()),
        "Python is in Core mode now"
    );
    assert!(
        !r.serves(),
        "stepping is never served from the core: the core still refuses"
    );
    let rows = r.rows();
    // The rerun gets through the preflight (it does not refuse its own tombstone) and finishes.
    let said = r.step(r.wake(), opts()).unwrap();
    assert!(
        said.iter().any(|l| l.contains("nothing to change")),
        "{said:?}"
    );
    assert_eq!(r.phase(), Some(Phase::Switched));
    assert_eq!(r.rows(), rows);
    assert!(r.serves());
}

#[test]
fn a_crash_after_the_marker_and_before_the_record_is_finished_by_a_rerun() {
    let r = rig();
    busy(&r);
    r.step(r.wake(), opts()).unwrap();
    let at = state::read(&r.env.layout, "wake").unwrap().unwrap().at;
    // The record says stepping again, with the marker and the tombstone beside it.
    state::write(
        &r.env.layout,
        "wake",
        &Switch {
            phase: Phase::Stepping,
            snapshot: None,
            at,
        },
    )
    .unwrap();
    assert!(!r.serves());
    r.step(r.wake(), opts()).unwrap();
    assert_eq!(r.phase(), Some(Phase::Switched));
    assert!(r.serves());
    assert_eq!(r.rows(), r.files());
}

#[test]
fn cancelling_a_step_that_got_as_far_as_the_tombstone_puts_python_back_on_the_files() {
    let r = rig();
    busy(&r);
    r.step(r.wake().crash_at(Crash::AfterTombstone), opts())
        .unwrap_err();
    assert!(r.tomb().exists());
    let said = r.rollback(r.wake(), opts()).unwrap();
    assert!(
        said.iter()
            .any(|l| l.contains("an unfinished step was cancelled")),
        "{said:?}"
    );
    r.untouched();
}

#[test]
fn a_step_refused_at_the_same_answer_or_the_reread_stays_stepping_with_no_tombstone_before_it() {
    let r = rig();
    busy(&r);
    let w = r.wake();
    let mut core = open_all(&r.env.layout);
    w.import(&r.env, Some(&mut core), opts()).unwrap();
    // 7: the same answer holds, then a file changes.
    assert_eq!(
        w.same_answer(&r.env, core.conn()).unwrap(),
        Vec::<String>::new()
    );
    r.item("s-new", "run", &[("new:1", None)], None);
    let diff = w.same_answer(&r.env, core.conn()).unwrap();
    assert_eq!(diff.len(), 1, "{diff:?}");
    assert!(
        diff[0].contains("s-new is in the files and not in core.db"),
        "{diff:?}"
    );
    // 9: the reread refuses a file that changed after the import.
    let e = w.reread(&r.env, &mut core).unwrap_err().to_string();
    assert!(
        e.contains("changed during the step") && e.contains("s-new"),
        "{e}"
    );
    assert!(!r.tomb().exists());
}

#[test]
fn the_differences_name_what_is_in_one_side_only() {
    let r = rig();
    busy(&r);
    let files = r.files();
    let mut rows = files.clone();
    rows.waiters.clear();
    rows.answered.clear();
    rows.items.remove("s2");
    let d = differences(&files, &rows);
    assert_eq!(d.len(), 3, "{d:?}");
    assert!(d.iter().any(|l| l.contains("session s2")));
    assert!(d.iter().any(|l| l.contains("answered id old:1")));
    assert!(d.iter().any(|l| l.contains("waiter w1")));
    assert_eq!(differences(&files, &files), Vec::<String>::new());
}

// --- U8, U9, U10 are in `switched.rs`, `sched_run_tests.rs` and `wake_back_tests.rs`. ------------------------------------------

#[allow(dead_code, reason = "the rig of wake_back_tests.rs")]
pub(super) fn path_of(r: &Rig) -> &Path {
    &r.machine
}
