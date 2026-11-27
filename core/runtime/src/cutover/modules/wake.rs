//! The `wake` module (retire-python S4, WK6): the wake queue (`$KITH_STATE/sessions/queue/*.json`, `answered.json` and
//! `sessions/waits/*.json`, which `kernel/bin/kith-wake` writes) moves into core.db's `wake_*` and `waiters` tables, and the
//! queue becomes the core's: `kith-wake` and its callers ask the core (`wake.*`), and the host's clock ticks it (`wake.tick`).
//! Stepped by `kith migrate step wake` with the host stopped. The design is `designs/2026-11-21-wake-s4` and the WK6 brief.
//!
//! The step, in order (each crash point is a unit test; a rerun resumes):
//! 1. `step_refusal`: this build runs wake turns ([`RUNS_WAKE_TURNS`]), launchd's `dev.kith.serve` runs this build and has a
//!    `kith-holder`, and the host is stopped. The step holds the core's lifetime lock, so there is no daemon to ask: like the
//!    schedules gate, this binary answers for itself.
//! 2. dependencies (`claims`, `transcripts`) are switched; the core's lifetime lock.
//! 3. `source_lock`: `host.lock`, then `waits/.lock` and `queue/.lock` exclusively, in `kith-wake`'s order, to the end.
//! 4. `preflight` (`wake_preflight.rs`): refuses with nothing written.
//! 5. snapshot and the `stepping` record.
//! 6. `import`: the files are read without asking for the locks again (they are this step's own, see `import::wake`).
//! 7. `same_answer`: the rows are exactly the files, answered ids and waiters included.
//! 8. `before_tombstone`: the host is still stopped, then `sessions/queue/.tombstone` is put. From here `kith-wake` is in Core
//!    mode and the core still refuses (`wake::call`), so a caller fails closed and keeps its work. It is put here and not
//!    after the record: the tombstone is a marker, not a source file it replaces, and the framework refuses a `switched`
//!    record whose tombstone is missing, so a crash between the two would need a person.
//! 9. `reread`: the files are read again with the locks held; a difference refuses and the module stays `stepping`.
//! 10. the marker and the `switched` record (the framework's).
//!
//! A `kith-wake` that read Files mode and then waited on a lock this step held re-reads the mode inside the lock and hands
//! over to the core (`kernel/bin/kith-wake` `locked`), so it writes no file after the tombstone.
//!
//! Rollback (`wake_back.rs`): refuses while the host runs; runs adopt, `end_gone` and one settle pass; takes the three locks;
//! refuses what no file can hold (`wake::rollback::refusals`); then writes the files back and puts the tombstone away last.

#[cfg(test)]
use std::cell::Cell;
use std::fs::{File, OpenOptions};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use fs2::FileExt as _;
use rusqlite::{Connection, OpenFlags};

use super::schedules_gate as gate;
use super::schedules_switch::{HostHold, hold_host_for};
use super::wake_back;
use super::wake_preflight::{self as pre, Lister};
use crate::cutover::authority_preflight::Preflight;
use crate::cutover::{Env, Module, Opts, SourceLock, tombstone};
use crate::db::Core;
use crate::error::{Error, Result};
use crate::import::wake::{read_files, read_files_held, run, run_in_held};
use crate::wake::{State, load};

/// Whether the core runs the turns the wake queue makes (a run-door wake and a host-door wake become a turn that starts, runs
/// and ends). `step_refusal` refuses while this is false: stepping tombstones the queue, so `kith-wake` stands down and the
/// core becomes the only runner, and a build that cannot run a kind of wake would strand it. It is also what `kith kernel
/// version` lists as `wake-runner`. Each line names its PR and the test that proves it; it turns true only in the PR that ticks
/// the last one, and goes back to false if one stops being true.
///
/// - [x] WK1-WK5 and WK4a/b/c, A2, S1, S2 and C3L are merged (the wake rows, `wake.tick`, the settle rule R4, the run door,
///   the channel door, the launch's attach and `end_gone`).
/// - [ ] The lifecycle sweep runs after a tick (blocker B1: PR #1245; `wake_clock.rs` starts it through `kernel::launch_program()`,
///   its proof is that PR's production-resolver row). After the switch `kith-wake drain` does nothing, so nothing else runs it.
/// - [ ] A non-Claude agent's run-door wake runs (blocker B2: PR #1250; the `engine_gap` cancel in `wake_clock.rs` goes, the
///   launch of S1/A2 runs the turn; its proof is #1250's real-launch rows, not `wake_clock_command_tests.rs`). Python's `deliver`
///   never delivered a non-Claude wake, so this is no regression, but until then every such request is cancelled and, after five
///   tries, ends `failed`.
/// - [ ] This PR's end-to-end rows pass in CI (`serve/host/tests/wake_cutover_e2e.rs`, E1 to E6, the "direct lock, schedules
///   cutover" host-checks row).
pub(crate) const RUNS_WAKE_TURNS: bool = false;

/// The build's answer. A debug build also reads `KITH_WAKE_GATE_OPEN=1`, so the end-to-end test can step a build whose gate is
/// still shut (its other conditions are checked for real); a release build never reads it.
fn runs_wake_turns() -> bool {
    gate_open(
        cfg!(debug_assertions),
        std::env::var("KITH_WAKE_GATE_OPEN").ok().as_deref(),
    )
}

/// [`runs_wake_turns`] with its two inputs given, so a test can check the release shape.
pub(super) fn gate_open(debug_build: bool, env: Option<&str>) -> bool {
    RUNS_WAKE_TURNS || (debug_build && env == Some("1"))
}

/// How long the step waits for a `kith-wake` that holds a lock: the waiters' lock is held while a CI waiter looks at `gh`
/// (20 s a call), the queue's only for one change.
const LOCK_WAIT: Duration = Duration::from_secs(60);

const STEP_RUNNING: &str = "not stepped: the host is running and reads the wake switch only when it starts; stop the host, step wake, start the host. `step wake --dry-run` works while it runs";
const ROLLBACK_RUNNING: &str = "the host is running and drives the wake queue's clock; stop the host first (then roll back, then start it); --force does not override this";

/// Said by `step`, `step --dry-run` and `check`, exactly.
pub(super) const MESSAGE: &str =
    "not stepped: this build can't run the turns the wake queue makes; kith-wake keeps your wakes.";

/// What the gate looked at; each field refuses alone.
pub(super) struct Facts {
    pub runs_wake_turns: bool,
    /// `Err` with what launchd runs next to this binary.
    pub launchd: std::result::Result<(), String>,
    /// `Err` with why launchd's host has no `kith-holder`.
    pub holder: std::result::Result<(), String>,
    /// `Some` when the host is running (or its lock cannot be checked).
    pub host: Option<String>,
}

/// `MESSAGE`, then which conditions failed.
pub(super) fn refusal(f: &Facts) -> Option<String> {
    let mut failed = Vec::new();
    if !f.runs_wake_turns {
        failed.push("1: the core does not run every kind of wake turn yet".to_owned());
    }
    if let Err(why) = &f.launchd {
        failed.push(format!("2: {why}"));
    }
    if let Err(why) = &f.holder {
        failed.push(format!("3: {why}"));
    }
    if let Some(why) = &f.host {
        failed.push(format!("4: {why}"));
    }
    (!failed.is_empty()).then(|| format!("{MESSAGE} Failed: {}.", failed.join("; ")))
}

/// Where a test of the step makes it fail, once: the three crash points between the record and the switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code, reason = "named by the step's tests"))]
pub(super) enum Crash {
    /// Inside the import: the record says `stepping`, nothing is tombstoned.
    Import,
    /// Before the tombstone is put.
    BeforeTombstone,
    /// After it: `stepping`, the tombstone in place, no marker.
    AfterTombstone,
}

pub(crate) struct Wake {
    /// Whether this step or rollback holds the queue's locks (so the files are read without asking for them again).
    locked: Arc<AtomicBool>,
    list: Lister,
    #[cfg(test)]
    open: bool,
    #[cfg(test)]
    crash: Cell<Option<Crash>>,
    #[cfg(test)]
    lock_wait: Duration,
}

impl Wake {
    pub(crate) fn new() -> Self {
        Self {
            locked: Arc::new(AtomicBool::new(false)),
            list: pre::own_processes,
            #[cfg(test)]
            open: false,
            #[cfg(test)]
            crash: Cell::new(None),
            #[cfg(test)]
            lock_wait: LOCK_WAIT,
        }
    }

    /// Tests: skip the build gate (conditions 1 to 3), so the step tests can step.
    #[cfg(test)]
    pub(super) fn open(mut self) -> Self {
        self.open = true;
        self
    }

    /// Tests: a fixed process table in place of the real one.
    #[cfg(test)]
    pub(super) fn processes(mut self, list: Lister) -> Self {
        self.list = list;
        self
    }

    /// Tests: fail once at `at`.
    #[cfg(test)]
    pub(super) fn crash_at(self, at: Crash) -> Self {
        self.crash.set(Some(at));
        self
    }

    /// Tests: how long the step waits for a lock.
    #[cfg(test)]
    pub(super) fn waiting(mut self, wait: Duration) -> Self {
        self.lock_wait = wait;
        self
    }

    fn crash(&self, at: Crash) -> Result<()> {
        #[cfg(test)]
        {
            if self.crash.get() == Some(at) {
                self.crash.set(None);
                return Err(Error::Refused(format!("injected crash at {at:?}")));
            }
        }
        let _ = at;
        Ok(())
    }

    #[cfg(test)]
    fn wait(&self) -> Duration {
        self.lock_wait
    }

    #[cfg(not(test))]
    #[allow(clippy::unused_self, reason = "the test build's wait is the rig's")]
    fn wait(&self) -> Duration {
        LOCK_WAIT
    }

    fn facts(&self, env: &Env) -> Facts {
        let host = pre::host_line(env, STEP_RUNNING);
        #[cfg(test)]
        {
            if self.open {
                return Facts {
                    runs_wake_turns: true,
                    launchd: Ok(()),
                    holder: Ok(()),
                    host,
                };
            }
        }
        let home = gate::user_home(env);
        Facts {
            runs_wake_turns: runs_wake_turns(),
            launchd: gate::launchd_runs_this_build(&home),
            holder: gate::launchd_has_holder(&home),
            host,
        }
    }

    /// The files as the step sees them: without asking for the locks it holds itself.
    fn read(&self, state: &Path) -> Result<(State, Vec<String>, String)> {
        if self.locked.load(Ordering::SeqCst) {
            read_files_held(state)
        } else {
            read_files(state)
        }
    }

    /// `host.lock`, then the waiters' and the queue's lock, in `kith-wake`'s order.
    fn hold(&self, env: &Env, running: &str) -> Result<Held> {
        let state = state(env)?;
        let host = hold_host_for("wake", env, running)?;
        let wait = self.wait();
        let waits = lock_dir(&waits_dir(state), wait)?;
        let queue = lock_dir(&queue_dir(state), wait)?;
        self.locked.store(true, Ordering::SeqCst);
        Ok(Held {
            flag: self.locked.clone(),
            _host: host,
            _waits: waits,
            _queue: queue,
        })
    }
}

/// What a step or rollback holds; the flag says so while it does.
struct Held {
    flag: Arc<AtomicBool>,
    _host: HostHold,
    _waits: File,
    _queue: File,
}

impl Drop for Held {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::SeqCst);
    }
}

pub(super) fn state(env: &Env) -> Result<&Path> {
    env.state.as_deref().ok_or_else(|| {
        Error::Refused("cutover: wake: needs $KITH_STATE (the CLI passes it)".into())
    })
}

pub(super) fn queue_dir(state: &Path) -> PathBuf {
    state.join("sessions").join("queue")
}

pub(super) fn waits_dir(state: &Path) -> PathBuf {
    state.join("sessions").join("waits")
}

/// `sessions/queue/.tombstone`: the marker `kernel/lib/kith_wake_core.py` reads by existence.
pub(super) fn tombstone_of(state: &Path) -> PathBuf {
    queue_dir(state).join(".tombstone")
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// `dir/.lock`, exclusive, waiting up to `wait` for a `kith-wake` that holds it; the folder is made private when missing.
fn lock_dir(dir: &Path, wait: Duration) -> Result<File> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| Error::Io(format!("cutover: wake: {}: {e}", dir.display())))?;
    let path = dir.join(".lock");
    let f = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| Error::Refused(format!("cutover: wake: {}: {e}", path.display())))?;
    let until = Instant::now() + wait;
    loop {
        match f.try_lock_exclusive() {
            Ok(()) => return Ok(f),
            Err(e) if e.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
                if Instant::now() >= until {
                    return Err(Error::Refused(format!(
                        "cutover: wake: {} is held by a running kith-wake for more than {}s; nothing was changed, try again",
                        path.display(),
                        wait.as_secs()
                    )));
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => {
                return Err(Error::Io(format!("cutover: wake: {}: {e}", path.display())));
            }
        }
    }
}

/// core.db for reading (the step holds the lifetime lock; a read-only handle works beside it). `None` when there is none yet
/// or it is not migrated far enough to hold the wake tables.
pub(super) fn read_db(env: &Env) -> Option<Connection> {
    let conn =
        Connection::open_with_flags(&env.layout.db, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN ('wake_sessions', 'wake_turns', 'turns', 'claims')",
        [],
        |r| r.get::<_, i64>(0),
    )
    .is_ok_and(|n| n == 4)
    .then_some(conn)
}

/// The record and the marker a step writes, without its gates: the host's tests start a core with the queue switched.
pub(crate) fn write_switched(
    layout: &crate::layout::Layout,
    core: &mut Core,
    at: i64,
) -> Result<()> {
    crate::cutover::state::write(
        layout,
        "wake",
        &crate::cutover::state::Switch {
            phase: crate::cutover::state::Phase::Switched,
            snapshot: None,
            at,
        },
    )?;
    crate::cutover::marker::write(core, "wake", at, "", None)
}

/// The differences between what the files say and what core.db holds, a line each (empty: the same answer).
pub(super) fn differences(files: &State, rows: &State) -> Vec<String> {
    let mut out = Vec::new();
    fn diff<V: PartialEq>(
        out: &mut Vec<String>,
        what: &str,
        files: &std::collections::BTreeMap<String, V>,
        rows: &std::collections::BTreeMap<String, V>,
    ) {
        for (k, v) in files {
            match rows.get(k) {
                None => out.push(format!("{what} {k} is in the files and not in core.db")),
                Some(r) if r != v => {
                    out.push(format!("{what} {k} differs between the files and core.db"))
                }
                Some(_) => {}
            }
        }
        for k in rows.keys().filter(|k| !files.contains_key(*k)) {
            out.push(format!("{what} {k} is in core.db and not in the files"));
        }
    }
    diff(&mut out, "session", &files.items, &rows.items);
    diff(&mut out, "answered id", &files.answered, &rows.answered);
    diff(&mut out, "waiter", &files.waiters, &rows.waiters);
    out
}

impl Module for Wake {
    fn name(&self) -> &'static str {
        "wake"
    }

    /// The runs are turns of the claims lane, and their transcripts are what settles a lost run.
    fn depends_on(&self) -> &'static [&'static str] {
        &["claims", "transcripts"]
    }

    fn tombstones(&self, env: &Env) -> Vec<PathBuf> {
        env.state.iter().map(|s| tombstone_of(s)).collect()
    }

    fn step_refusal(&self, env: &Env) -> Option<String> {
        refusal(&self.facts(env))
    }

    fn dry_run_despite_refusal(&self) -> bool {
        true
    }

    fn rollback_refusal(&self, env: &Env) -> Option<String> {
        // Not a `live` reason: `--force` must not get past it. A running host ticks the queue as Core while the record says
        // rolling-back, and reads its mode at every tick.
        pre::host_line(env, ROLLBACK_RUNNING)
    }

    fn guards_claims(&self) -> bool {
        // A terminal claim elsewhere is no reason to keep the wake queue from going back; the running wake runs are handed over
        // (`wake::rollback`), not refused.
        false
    }

    fn source_lock(&self, env: &Env) -> Result<Option<SourceLock>> {
        Ok(Some(Box::new(self.hold(env, STEP_RUNNING)?)))
    }

    fn preflight(
        &self,
        env: &Env,
        _import: Option<&Result<Vec<String>>>,
    ) -> Result<Option<Preflight>> {
        Ok(Some(pre::preflight(
            env,
            state(env)?,
            STEP_RUNNING,
            self.list,
            self.locked.load(Ordering::SeqCst),
        )?))
    }

    fn import(&self, env: &Env, core: Option<&mut Core>, _opts: Opts) -> Result<Vec<String>> {
        let state = state(env)?;
        let report = match core {
            Some(core) => {
                self.crash(Crash::Import)?;
                run_in_held(core, state)?
            }
            None => run(&env.layout, state, true)?,
        };
        Ok(report.lines)
    }

    fn same_answer(&self, env: &Env, db: &Connection) -> Result<Vec<String>> {
        let (files, _, _) = self.read(state(env)?)?;
        Ok(differences(&files, &load(db)?))
    }

    fn before_tombstone(&self, env: &Env) -> Result<()> {
        let state = state(env)?;
        self.crash(Crash::BeforeTombstone)?;
        // A host started since the preflight would run the queue in Files mode from its own clock.
        if let Some(why) = pre::host_line(env, STEP_RUNNING) {
            return Err(Error::Refused(format!(
                "cutover: wake: {why}; NOT switched"
            )));
        }
        // From here `kith-wake` asks the core, which refuses until the record says switched: a caller fails closed and keeps its work.
        // The tombstone is for recovery: a crash between it and the record leaves `stepping` with Python already standing down, so
        // the rerun (or the rollback) finds a queue nobody writes, instead of two writers.
        tombstone::ensure(&tombstone_of(state), self.name(), now_secs())?;
        self.crash(Crash::AfterTombstone)
    }

    fn reread(&self, env: &Env, core: &mut Core) -> Result<()> {
        let (files, _, _) = self.read(state(env)?)?;
        let left = differences(&files, &load(core.conn())?);
        if left.is_empty() {
            return Ok(());
        }
        Err(Error::Refused(format!(
            "cutover: wake: the queue files changed during the step ({}); NOT switched. Run `kith migrate step wake` again, or `kith rollback wake`",
            left.join(" | ")
        )))
    }

    fn before_rollback(&self, env: &Env, core: &mut Core) -> Result<()> {
        // The host is stopped and we hold the lifetime lock, so nothing can redeem or report: what a stopped or killed host left
        // is settled here as the next host start's `adopt` would (a never-attached `claimed` host claim is requeued), then every
        // claim whose process group is gone is ended, then one R4 settle pass answers the runs whose turn is over. Settle only:
        // no waiter fires, nothing is taken, nothing is unparked.
        let now = crate::holder_link::now_ms();
        crate::conversations::warm::adopt(
            core,
            &format!(
                "{}/rollback-{now}",
                crate::conversations::claims::HOST_INSTANCE
            ),
            now,
        )?;
        crate::conversations::claims::end_gone(core, &crate::gate::System, now)?;
        let folders = env.paths.as_ref().ok_or_else(|| {
            Error::Refused(
                "cutover: wake: the rollback needs the folders (the CLI passes them)".into(),
            )
        })?;
        crate::wake::settle_only(core, folders)?;
        Ok(())
    }

    fn rollback_lock(&self, env: &Env) -> Result<Option<SourceLock>> {
        let held = self.hold(env, ROLLBACK_RUNNING)?;
        // Under the locks and the core's lifetime lock, from a read of core.db; nothing is written yet and `--force` does not
        // get past it.
        if let Some(db) = read_db(env) {
            let why = crate::wake::rollback::refusals(&db)?;
            if !why.is_empty() {
                return Err(Error::Refused(format!(
                    "cutover: wake: not rolling back: {}; end those runs (or wait for them), then roll back; --force does not override this",
                    why.join("; ")
                )));
            }
        }
        Ok(Some(Box::new(held)))
    }

    fn cancel_step(&self, env: &Env) -> Result<Vec<String>> {
        // A step that stopped after the tombstone and before the switch: Python is in Core mode and the core refuses, so
        // without this a caller would fail closed for ever.
        let t = tombstone_of(state(env)?);
        match std::fs::remove_file(&t) {
            Ok(()) => Ok(vec![format!("wake: removed {}", t.display())]),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(Error::Io(format!("cutover: wake: {}: {e}", t.display()))),
        }
    }

    fn export_back(&self, env: &Env, core: &mut Core) -> Result<Vec<String>> {
        wake_back::export(env, core)
    }
}
