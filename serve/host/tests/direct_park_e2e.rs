//! A turn behind `kith <agent> --direct`, on a real host in Writer mode (M3 cutover P11, design 1.3 and 1.6).
//!
//! Real processes throughout: the host (`kith-serve`) with its own core daemon, a fake ACP agent (`fixtures/writer_agent.py`) under
//! the resident `kith-stub`, and a one-shot `kith-stub` that stands for a `--direct` run (it takes the conversation's writer lock in
//! `<machine>/core/locks`, exactly as `kernel/bin/kith` starts claude). The Mac is marked switched for the `claims` module by hand
//! (the step refuses until P9, and these tests never touch a real Mac's cutover state): the switch file and the marker row in the
//! temp core.db that the gate reads. Writer mode is the host's own gate answer, nothing is scripted.
//!
//! What it shows:
//! - an app send behind a one-shot that holds the lock parks with its waiting notice, and runs once the one-shot is gone;
//! - a `--direct` run on an idle warm conversation: the core refuses its external claim `waiting` and stops the idle process
//!   (P7), grants it once the process group is gone, the app send then waits for the terminal, and the next turn after the run
//!   starts a process that loads the session again.
//!
//! Every wait has a deadline and says what it was waiting for; a failure prints the core's claims, turns and warm processes.
//!
//! Gaps this found: the claims' reconcile mark (the holder's hello writes it now; nothing here marks it) and the reaping of a
//! killed run's claim (the daemon's tick settles it now) are fixed; a lock-only run on a warm main-lane conversation that the next
//! turn does not reload after is still open (the ignored test, P5 on the main lane: step 7's sync decides by thread). What it does not show: the revoked
//! person's idle process. The owner's `sweep_claims` is reached only through `revokePerson`, which needs kith-auth and a person
//! record this temp host has none of.

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::host::Host;
use kith_kernel::paths::Paths;
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// The agent fixture names its session after its folder (`sess-<folder>`); a bot's folder is `workspace`.
const SESSION: &str = "sess-workspace";
/// How long a step may take. A parked turn asks again after 2 s, then 4 s, and so on up to 30 s.
const WAIT: Duration = Duration::from_secs(90);

/// The stub binary: built beside the test binaries, as the in-crate Writer tests do.
fn stub_bin() -> &'static PathBuf {
    static BUILT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BUILT.get_or_init(|| {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core/stub/Cargo.toml");
        let built = Command::new(cargo).args(["build", "--manifest-path"]).arg(&manifest).status().unwrap();
        assert!(built.success(), "kith-stub did not build");
        // The test binary is in target/<profile>/deps, the stub in target/<profile>.
        let stub = std::env::current_exe().unwrap().parent().unwrap().parent().unwrap().join("kith-stub");
        assert!(stub.is_file(), "no {}", stub.display());
        stub
    })
}

/// The built holder program, next to `kith`: the core starts one and the claims wait for its hello (the reconcile).
fn holder_bin() -> PathBuf {
    let beside = Path::new(env!("CARGO_BIN_EXE_kith")).parent().unwrap().join("kith-holder");
    if !beside.is_file() {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
        let built =
            Command::new(cargo).args(["build", "--manifest-path", manifest, "-p", "kith-holder"]).status().unwrap();
        assert!(built.success() && beside.is_file(), "kith-holder is not built");
    }
    beside
}

/// The holder the core started detached: it outlives the host, so the test ends it (the field after the host, so it is last).
struct Holder(u32);

impl Drop for Holder {
    fn drop(&mut self) {
        let _ = Command::new("kill").args(["-s", "KILL", &self.0.to_string()]).status();
    }
}

/// The state folder of a rig, removed when it goes (the field after the host, so the host is stopped first).
struct StateDir(PathBuf);

impl Drop for StateDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A host in Writer mode and what a `--direct` run needs beside it.
struct Rig {
    host: Host,
    _holder: Holder,
    paths: Paths,
    bot: String,
    /// The one-shot stubs standing in for `--direct` runs.
    direct: Vec<Child>,
    _state: StateDir,
}

impl Drop for Rig {
    fn drop(&mut self) {
        // The one-shots first, then the fields: the host (its warm processes are its children), the holder, the folder.
        self.kill_direct();
    }
}

/// Marks the `claims` module switched in the core of `paths`: the switch file and the marker row in core.db that must match it
/// (`cutover::may_serve`; the step itself refuses until P9). Only ever in a throwaway folder.
fn mark_switched(paths: &Paths) {
    assert!(
        paths.machine.to_string_lossy().starts_with("/tmp/kdp-"),
        "never mark a real Mac's modules: {}",
        paths.machine.display()
    );
    let at = 1_800_000_000_i64;
    let core = paths.machine.join("core");
    std::fs::create_dir_all(core.join("cutover")).unwrap();
    std::fs::write(core.join("cutover/claims.switch"), format!("state=switched\nat={at}\n")).unwrap();
    rusqlite::Connection::open(core.join("core.db"))
        .unwrap()
        .execute("INSERT OR REPLACE INTO meta (key, value) VALUES ('cutover:claims', ?1)", [format!("at={at};digest=")])
        .unwrap();
}

/// Ends `child` within a few seconds (it was asked to stop already), or kills it.
fn reap(mut child: Child) {
    let end = Instant::now() + Duration::from_secs(10);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= end {
            let _ = child.kill();
            let _ = child.wait();
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 only asks whether the group is there.
    unsafe { libc::killpg(i32::try_from(pid).unwrap(), 0) == 0 }
}

impl Rig {
    async fn start() -> Self {
        // A short folder: the core's owner socket lives under it.
        let state = PathBuf::from("/tmp").join(format!("kdp-{}", &uuid::Uuid::new_v4().simple().to_string()[..10]));
        std::fs::create_dir_all(&state).unwrap();
        let given = state.display().to_string();
        let env = |k: &str| (k == "KITH_STATE").then(|| given.clone());
        let paths = kith_kernel::paths::paths(&state, &state.join("kernel"), &env);
        // The core's database, with the `claims` module switched.
        drop(kith_core::start(&paths).expect("the core starts"));
        mark_switched(&paths);
        let stub = stub_bin().display().to_string();
        let holder = holder_bin().display().to_string();
        let host =
            Host::start_env(&[("KITH_STATE", &given), ("KITH_STUB_BIN", &stub), ("KITH_HOLDER_BIN", &holder)]).await;
        // The claims wait for the core's reconcile, which the holder's hello makes.
        let end = Instant::now() + WAIT;
        let pid = loop {
            let said = kith_core::status(&paths);
            if said["holder"]["state"] == "connected" {
                break said["holder"]["pid"].as_u64().unwrap();
            }
            assert!(Instant::now() < end, "the holder never connected: {said}");
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        let bot = host.fixture_bot("direct-park", "writer_agent.py", "").await;
        Self {
            host,
            _holder: Holder(u32::try_from(pid).unwrap()),
            paths,
            bot,
            direct: Vec::new(),
            _state: StateDir(state),
        }
    }

    fn workspace(&self) -> PathBuf {
        self.host.home.join("bots").join(&self.bot).join("workspace")
    }

    /// The lines the fixture logged in its folder (`prompts.jsonl`, `loads.jsonl`).
    fn log(&self, file: &str) -> Vec<String> {
        std::fs::read_to_string(self.workspace().join(file))
            .map(|t| t.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    }

    /// What the bot's status line says now.
    async fn activity(&self) -> String {
        let roster = self.host.roster().await;
        let bot = roster.iter().find(|b| b["id"] == self.bot.as_str());
        bot.map(|b| b["activity"].as_str().unwrap_or_default().to_owned()).unwrap_or_default()
    }

    /// Starts a one-shot stub that holds the session's writer lock, the way `kith <agent> --direct` starts claude.
    async fn hold(&mut self) -> u32 {
        let locks = self.paths.machine.join("core").join("locks");
        std::fs::create_dir_all(&locks).unwrap();
        let end = Instant::now() + WAIT;
        loop {
            let mut child = Command::new(stub_bin())
                .arg("--lock-dir")
                .arg(&locks)
                .args(["--key", SESSION, "--", "cat"])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            // Busy ends it at once (exit 75); alive after a moment, it holds the lock.
            tokio::time::sleep(Duration::from_millis(300)).await;
            if child.try_wait().unwrap().is_none() {
                let pid = child.id();
                self.direct.push(child);
                return pid;
            }
            assert!(Instant::now() < end, "the one-shot stub never took the lock of {SESSION}");
        }
    }

    /// The `--direct` run ends: its stdin closes, `cat` exits, the stub and its lock go.
    fn end_direct(&mut self) {
        for mut c in self.direct.drain(..) {
            drop(c.stdin.take());
            reap(c);
        }
    }

    /// The `--direct` run is killed outright: nothing releases its claim, and the stub's lock goes with it.
    fn kill_direct(&mut self) {
        for mut c in self.direct.drain(..) {
            let _ = c.kill();
            reap(c);
        }
    }

    /// Asks for the run's external claim until the core grants it (it says `waiting` while an idle process is in the way): the
    /// answers on the way, the last one the grant.
    async fn claim_when_free(&self, claim: &Value) -> Vec<Value> {
        let end = Instant::now() + WAIT;
        let mut said = Vec::new();
        loop {
            said.push(self.core("claim.external", claim));
            let last = &said[said.len() - 1];
            if last.get("claimId").is_some() {
                return said;
            }
            assert_eq!(last["refused"], "waiting", "only the idle process is in the way: {last}");
            assert!(Instant::now() < end, "timed out after {WAIT:?} waiting for the idle kill; {}", self.dump());
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }

    /// Sends a message without waiting for the answer.
    async fn send(&self, text: &str) {
        let nonce = uuid::Uuid::new_v4().to_string();
        self.host.call("send", json!({"botId": self.bot, "text": text, "clientNonce": nonce})).await;
    }

    /// How many finished answers the chat holds.
    async fn answers(&self) -> usize {
        let history = self.host.call("history", json!({"botId": self.bot})).await;
        history["entries"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["kind"] == "agent" && e["data"]["final"] == true)
            .count()
    }

    async fn wait_activity(&self, needle: &str) {
        let end = Instant::now() + WAIT;
        loop {
            let now = self.activity().await;
            if now.contains(needle) {
                return;
            }
            assert!(
                Instant::now() < end,
                "timed out after {WAIT:?} waiting for the notice {needle:?}; activity {now:?}"
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    /// Waits for the chat to hold `n` finished answers.
    async fn wait_answers(&self, n: usize) {
        let end = Instant::now() + WAIT;
        while self.answers().await < n {
            assert!(
                Instant::now() < end,
                "timed out after {WAIT:?} waiting for answer {n}; activity {:?}{}",
                self.activity().await,
                self.dump()
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    /// The core's claims, turns and warm processes, for a failure message.
    fn dump(&self) -> String {
        let db = rusqlite::Connection::open(self.paths.machine.join("core/core.db")).unwrap();
        let mut out = String::new();
        for (table, cols) in [
            ("claims", "claim_id, kind, state, pid, conversation"),
            ("turns", "id, state, conversation, parked_reason"),
            ("warm_procs", "pid, conversation, kill_at"),
            ("meta", "key, value"),
        ] {
            let Ok(mut q) = db.prepare(&format!("SELECT {cols} FROM {table}")) else {
                let _ = write!(out, "\n{table}: unreadable");
                continue;
            };
            let rows: Vec<String> = q
                .query_map([], |r| {
                    (0..r.as_ref().column_count())
                        .map(|i| r.get::<_, rusqlite::types::Value>(i).map(|v| format!("{v:?}")))
                        .collect::<Result<Vec<_>, _>>()
                        .map(|c| c.join(","))
                })
                .map(|m| m.flatten().collect())
                .unwrap_or_default();
            let _ = write!(out, "\n{table}: {rows:?}");
        }
        if let Ok(rd) = std::fs::read_dir(&self.paths.logs) {
            for f in rd.flatten() {
                let text = std::fs::read_to_string(f.path()).unwrap_or_default();
                let tail: Vec<&str> = text.lines().rev().take(15).collect();
                let _ = write!(out, "\nlog {}: {tail:?}", f.path().display());
            }
        }
        out
    }

    /// The core's turns, as `(state, parked_reason)`, oldest first.
    fn turns(&self) -> Vec<(String, Option<String>)> {
        let db = rusqlite::Connection::open(self.paths.machine.join("core/core.db")).unwrap();
        let mut q = db.prepare("SELECT state, parked_reason FROM turns ORDER BY id").unwrap();
        q.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().flatten().collect()
    }

    /// How many claim rows the conversation has, and how many of them are live.
    fn claim_rows(&self) -> (usize, usize) {
        let db = rusqlite::Connection::open(self.paths.machine.join("core/core.db")).unwrap();
        let count =
            |sql: &str| db.query_row(sql, [], |r| r.get::<_, i64>(0)).map(|n| usize::try_from(n).unwrap()).unwrap();
        (
            count("SELECT count(*) FROM claims"),
            count("SELECT count(*) FROM claims WHERE state IN ('claimed', 'running', 'ending')"),
        )
    }

    /// Waits for `what` to hold, polling `f` (sync: it reads the core's database), with the deadline and the core's state in the message.
    async fn until(&self, what: &str, f: impl Fn(&Self) -> bool) {
        let end = Instant::now() + WAIT;
        while !f(self) {
            assert!(Instant::now() < end, "timed out after {WAIT:?} waiting for {what}; {}", self.dump());
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// The pids of the processes the core lists as warm.
    fn warm_pids(&self) -> Vec<u32> {
        let db = rusqlite::Connection::open(self.paths.machine.join("core/core.db")).unwrap();
        let mut q = db.prepare("SELECT DISTINCT pid FROM warm_procs").unwrap();
        q.query_map([], |r| r.get::<_, u32>(0)).unwrap().flatten().collect()
    }

    /// A row of the core, as the owner.
    fn core(&self, method: &str, params: &Value) -> Value {
        match kith_core::reach(&self.paths, method, params, false) {
            kith_core::Reached::Answered(v) => v,
            other => panic!("{method} was not answered: {other:?}"),
        }
    }
}

/// An app send behind a `--direct` run that holds the lock: the turn parks on the lock (`lock_busy`, with the waiting notice) and does
/// not run, however often it asks again; it runs after the run is gone.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_app_send_parks_behind_a_direct_run_and_runs_after_it() {
    let mut rig = Rig::start().await;
    rig.hold().await;
    rig.send("behind direct").await;
    rig.wait_activity("waiting for the agent's session").await;
    // The notice alone also reads for a reconcile wait: the core's own row says why the turn waits.
    rig.until("the turn parked on the lock", |r| {
        r.turns().first().is_some_and(|t| t.0 == "parked" && t.1.as_deref() == Some("lock_busy"))
    })
    .await;
    // After at least one more try (each try is a claim row that ended busy), still nothing prompted.
    rig.until("a second try behind the lock", |r| r.claim_rows().0 >= 2).await;
    assert!(rig.log("prompts.jsonl").is_empty(), "nothing is prompted behind the lock: {}", rig.dump());
    rig.end_direct();
    rig.wait_answers(1).await;
    assert_eq!(rig.log("prompts.jsonl").len(), 1, "the one send ran once");
}

/// `kith <agent> --direct` on a conversation whose warm process is idle, with a core answering (P7, design 1.6): the run's external
/// claim is refused `waiting` while the core stops the idle process, and is granted once its process group is gone. The app send that
/// follows waits for the terminal, and after the run the next turn runs on a new process that loads the session again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_direct_run_on_an_idle_warm_conversation_waits_for_the_idle_kill_and_the_next_turn_reloads() {
    let mut rig = Rig::start().await;
    rig.send("warm up").await;
    rig.wait_answers(1).await;
    // The host releases the lock before it reports the turn done: the run starts once nothing of the turn is live.
    rig.until("the first turn's claim to end", |r| r.claim_rows().1 == 0).await;
    let warm = rig.warm_pids();
    assert_eq!(warm.len(), 1, "one warm process after the first turn: {}", rig.dump());
    assert!(rig.log("loads.jsonl").is_empty(), "the first process made the session, it loaded nothing");

    // The idle process holds no lock, so the one-shot takes it. Its claim is the next row: refused while the process is there.
    let run = rig.hold().await;
    let started = kith_kernel::procstart::start_stamp(run).expect("the one-shot's start stamp");
    let claim = json!({"conversation": SESSION, "pid": run, "started": started});
    let said = rig.claim_when_free(&claim).await;
    assert_eq!(said[0]["refused"], "waiting", "an idle warm process is in the way: {said:?}");
    assert!(warm.iter().all(|p| !alive(*p)), "the claim was granted while the warm process still ran: {said:?}");

    // An app send while the run holds the conversation: the terminal is the writer, so it waits.
    rig.send("while direct").await;
    rig.wait_activity("waiting for your terminal session").await;
    assert_eq!(rig.log("prompts.jsonl").len(), 1, "nothing is prompted while the run holds the conversation");

    // The run ends and releases its claim (`kith-sessions release`), and the send runs on a process of its own.
    rig.end_direct();
    let released = rig.core("claim.release", &json!({"pid": run, "started": started, "conversation": SESSION}));
    assert_eq!(released["ended"].as_array().map(Vec::len), Some(1), "the run's claim ended: {released}");
    rig.wait_answers(2).await;
    assert_eq!(rig.log("prompts.jsonl").len(), 2);
    assert_eq!(rig.log("loads.jsonl").len(), 1, "the new process loaded the session again");
    let now = rig.warm_pids();
    assert!(now.iter().all(|p| !warm.contains(p)), "a new process: was {warm:?}, now {now:?}");
}

/// A `--direct` run that holds only the lock (an owner run no core answered for, so no external claim) on an idle warm conversation:
/// the run wrote the session, so the next turn must load it again before it prompts (P5, design 1.3 step 7).
///
/// IGNORED, and it fails today (gap 3): step 7's sync decides by thread, `sync_for_turn_blocking` is `Unchanged` for a lane without
/// one (the bot's main lane), and nothing else tells the warm process that someone else wrote its session. Un-ignore it when the
/// stub or the host records the last writer of a key.
#[ignore = "a lock-only --direct run on a warm main-lane conversation does not make the next turn reload: host gap, see the PR"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lock_only_direct_run_on_an_idle_warm_conversation_means_the_next_turn_reloads() {
    let mut rig = Rig::start().await;
    rig.send("warm up").await;
    rig.wait_answers(1).await;
    rig.until("the first turn's claim to end", |r| r.claim_rows().1 == 0).await;
    let warm = rig.warm_pids();
    rig.hold().await;
    rig.end_direct();
    rig.send("after direct").await;
    rig.wait_answers(2).await;
    let reloaded = rig.log("loads.jsonl").len() == 1 || rig.warm_pids().iter().all(|p| !warm.contains(p));
    assert!(reloaded, "the second turn prompted the process that never saw the run's writes: {}", rig.dump());
}

/// A `--direct` run that is killed (nothing releases its claim) must not hold the conversation for ever: its process is seen gone and
/// the parked app send runs.
///
/// The daemon's kill-resend tick settles dead claims (the core ended a dead external claim only inside a later `claim.*` row or
/// `hostclaim.settle`, which a parked turn calls neither of).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_direct_run_that_is_killed_frees_the_conversation_for_the_parked_send() {
    let mut rig = Rig::start().await;
    rig.send("warm up").await;
    rig.wait_answers(1).await;
    rig.until("the first turn's claim to end", |r| r.claim_rows().1 == 0).await;
    let run = rig.hold().await;
    let started = kith_kernel::procstart::start_stamp(run).expect("the one-shot's start stamp");
    rig.claim_when_free(&json!({"conversation": SESSION, "pid": run, "started": started})).await;
    rig.send("while direct").await;
    rig.wait_activity("waiting for your terminal session").await;
    rig.kill_direct();
    rig.wait_answers(2).await;
}
