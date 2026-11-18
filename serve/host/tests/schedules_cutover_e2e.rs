//! The schedules cutover on real processes (M4-5 package F). Each case runs a real `kith serve` (and the core daemon it starts
//! inside itself, and a real `kith-holder`), the real `kith kernel cutover` verbs, and the real `kith-sched` (python) from a
//! copy of the kernel whose `bin/kith` is a stub. Throwaway homes in /tmp, `env_clear` for every process, no launchd, no
//! launchctl: the two plists are files the gate reads.
//!
//! - (a) a slot due while the host is down fires exactly once when the host starts, for a routine and a cron job
//! - (b) a host start during the step is refused, and one after it runs as the core's
//! - (c) a rollback while a detached run goes on leaves `kith-sched` skipping that job
//! - (d) condition 6: a `kith-sched tick` that started before the step holds it up
//!
//! Not stubbed: the step and rollback, both locks, the host's tick and runner, `kith-sched` (tick and run-turn), the core's
//! claims (`claim.attach` reaches the real `kith`), the holder. Stubbed: the engine of a cron job (`<kernel>/bin/kith`: it
//! logs a line and holds while the file `hold` exists), the agent of a routine (a scripted ACP agent, as `hq_e2e`), `hq`.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[allow(dead_code)]
mod common;

use chrono::{Timelike as _, Utc};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const KITH: &str = env!("CARGO_BIN_EXE_kith");
/// The gate's marker line of a `kith-sched` that takes `sched/tick.lock`.
const MARKER: &str = "KITH_SCHED_TICK_LOCK = 1";

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

/// The built holder next to `kith`: built on demand when only the host's tests were built.
fn ensure_holder() {
    let beside = Path::new(KITH).parent().unwrap().join("kith-holder");
    if !beside.is_file() {
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let built = Command::new(cargo)
            .args(["build", "--manifest-path", concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"), "-p", "kith-holder"])
            .status()
            .unwrap();
        assert!(built.success() && beside.is_file(), "kith-holder is not built");
    }
}

fn until(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("gave up waiting for {what}");
}

fn pid_alive(pid: u32) -> bool {
    !matches!(kith_kernel::procstart::probe(pid), kith_kernel::procstart::Probe::Gone)
}

/// A host or a step, as a process: killed when the test ends any way.
struct Reap(Child);

impl Drop for Reap {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Lab {
    dir: PathBuf,
    home: PathBuf,
    kith_home: PathBuf,
    serve: PathBuf,
    kernel: PathBuf,
    machine: PathBuf,
    /// The stub `hq`'s folder, which is also the working folder of the scripted agent.
    hq: PathBuf,
    /// Pids the test started through the host and must not leave behind.
    left: RefCell<Vec<u32>>,
}

struct Host {
    child: Reap,
    base: String,
    token: String,
}

impl Lab {
    /// A home with a kernel copy, the two plists, two agents and the owner. `stand_in`: the text of a `bin/kith-sched` in
    /// place of the real file (case d).
    fn new(tag: &str, stand_in: Option<&str>) -> Lab {
        ensure_holder();
        let dir = PathBuf::from("/tmp").join(format!("kse-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        let (home, kernel) = (dir.join("home"), dir.join("k"));
        let kith_home = home.join("kith");
        let machine = home.join(".config/kith");
        let hq = dir.join("hq");
        for d in [&kernel.join("bin"), &machine.join("cli"), &hq, &kith_home.join("state/live/terminal")] {
            std::fs::create_dir_all(d).unwrap();
        }
        for agent in ["mel", "house"] {
            let role = kith_home.join("agents").join(agent).join("role");
            std::fs::create_dir_all(&role).unwrap();
            std::fs::write(role.join("agent.md"), format!("---\nname: {agent}\ndescription: test\n---\nbody\n"))
                .unwrap();
        }
        let real = repo().join("kernel");
        std::os::unix::fs::symlink(real.join("lib"), kernel.join("lib")).unwrap();
        // `kith-sched` stays the real, unmodified file: its helpers find the lib beside the copy.
        for b in ["kith-sched", "kith-auth"] {
            std::fs::copy(real.join("bin").join(b), kernel.join("bin").join(b)).unwrap();
        }
        if let Some(text) = stand_in {
            std::fs::write(kernel.join("bin/kith-sched"), text).unwrap();
        }
        for b in ["kith-ask", "kith-wake"] {
            std::fs::write(kernel.join("bin").join(b), "pass\n").unwrap();
        }
        // The engine of a cron job: one line per run (its agent and pid), held while `hold` exists.
        let stub = "#!/bin/sh\nk=$(dirname \"$0\")/..\necho \"$1 $$\" >> \"$k/runs.log\"\nwhile [ -e \"$k/hold\" ]; do sleep 0.2; done\n";
        std::fs::write(kernel.join("bin/kith"), stub).unwrap();
        for b in ["kith", "kith-sched"] {
            let f = kernel.join("bin").join(b);
            let mut perm = std::fs::metadata(&f).unwrap().permissions();
            std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
            std::fs::set_permissions(&f, perm).unwrap();
        }
        std::os::unix::fs::symlink(&kernel, machine.join("cli/current")).unwrap();
        let plists = home.join("Library/LaunchAgents");
        std::fs::create_dir_all(&plists).unwrap();
        let words = |w: &[&str]| {
            let s = w.join("</string><string>");
            let s = format!("<string>{s}</string>");
            format!("<plist><dict><key>ProgramArguments</key><array>{s}</array></dict></plist>")
        };
        std::fs::write(plists.join("dev.kith.serve.plist"), words(&[KITH, "serve", "--port", "1"])).unwrap();
        let sched = machine.join("cli/current/bin/kith-sched");
        std::fs::write(plists.join("dev.kith.sched.plist"), words(&[sched.to_str().unwrap(), "tick"])).unwrap();
        // The owner is a real people id, not the word `owner` (a job with no person runs as them).
        std::fs::write(machine.join("auth.json"), "{\"people\": {\"kai\": {\"owner\": true}}}\n").unwrap();
        Lab { serve: dir.join("serve"), dir, home, kith_home, kernel, machine, hq, left: RefCell::new(Vec::new()) }
    }

    fn env(&self) -> Vec<(String, String)> {
        let p = |x: &Path| x.to_string_lossy().into_owned();
        let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let acp = format!("{} claude", common::python_agent(&fixtures.join("instructions_agent.py")));
        [
            ("HOME", p(&self.home)),
            ("KITH_HOME", p(&self.kith_home)),
            ("KITH_SERVE_HOME", p(&self.serve)),
            ("KITH_KERNEL", p(&self.kernel)),
            ("KITH_BIN", KITH.to_owned()),
            ("PATH", "/usr/bin:/bin".to_owned()),
            ("TZ", "UTC".to_owned()),
            ("PYTHONDONTWRITEBYTECODE", "1".to_owned()),
            ("KITH_SERVE_CLOUD", "off".to_owned()),
            ("KITH_KERNEL_BIN", p(&fixtures.join("stub_hq.py"))),
            ("STUB_HQ_HOME", p(&self.hq)),
            ("KITH_SERVE_HQ_ACP_COMMAND", acp),
            // Never this Mac's master key item.
            ("KITH_SECRETS_KEYSTORE_ACCOUNT", format!("test-{}", self.dir.display().to_string().replace('/', "-"))),
            ("CLAUDE_CONFIG_DIR", p(&self.dir.join("claude-config"))),
            ("KITH_SERVE_MAX_RUNNING_AGENTS", "64".to_owned()),
            ("KITH_SERVE_MIN_FREE_MEMORY_PERCENT", "0".to_owned()),
            ("KITH_SERVE_MAX_SWAP_USED_GB", "0".to_owned()),
            ("RUST_LOG", "kith=info".to_owned()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect()
    }

    fn cmd(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut c = Command::new(program);
        c.env_clear().envs(self.env());
        c
    }

    fn kith(&self, args: &[&str]) -> Command {
        let mut c = self.cmd(KITH);
        c.args(args);
        c
    }

    fn cutover(&self, verb: &str, module: &str) -> Output {
        self.kith(&["kernel", "cutover", verb, module]).output().unwrap()
    }

    fn step_ok(&self, module: &str) {
        let out = self.cutover("step", module);
        assert!(out.status.success(), "step {module}: {}", text(&out));
    }

    /// The core's calls for what the schedules module needs first, on a home with no host yet.
    fn step_before_schedules(&self) {
        self.step_ok("claims");
        self.step_ok("authority");
    }

    fn paths(&self) -> kith_kernel::paths::Paths {
        let kith_home = self.kith_home.to_string_lossy().into_owned();
        kith_kernel::paths::paths(&self.home, &self.kernel, &|k: &str| match k {
            "KITH_HOME" => Some(kith_home.clone()),
            _ => None,
        })
    }

    /// An owner call of the running daemon.
    /// `Some(answer)` once the daemon took the call. A call that may have run (the connection ended before the answer) is
    /// `Some(null)`: it is never repeated. `None`: nothing ran (no daemon yet, a refusal), so asking again is safe.
    fn owner(&self, method: &str, params: &Value) -> Option<Value> {
        match kith_core::reach(&self.paths(), method, params, false) {
            kith_core::Reached::Answered(v) => Some(v),
            kith_core::Reached::Lost(_) => Some(Value::Null),
            _ => None,
        }
    }

    fn sched(&self, now: Option<&str>, args: &[&str]) -> Output {
        let mut c = self.cmd("python3");
        c.arg(self.kernel.join("bin/kith-sched")).args(args);
        if let Some(now) = now {
            c.env("KITH_SCHED_NOW", now).env("KITH_SCHED_LOCK_WAIT", "5");
        }
        c.output().unwrap()
    }

    fn runs_log(&self) -> Vec<(String, u32)> {
        let text = std::fs::read_to_string(self.kernel.join("runs.log")).unwrap_or_default();
        text.lines()
            .filter_map(|l| {
                let (agent, pid) = l.split_once(' ')?;
                Some((agent.to_owned(), pid.parse().ok()?))
            })
            .collect()
    }

    fn state_json(&self) -> Value {
        serde_json::from_slice(&std::fs::read(self.machine.join("sched/state.json")).unwrap()).unwrap()
    }

    fn db(&self) -> rusqlite::Connection {
        let db = rusqlite::Connection::open_with_flags(
            self.machine.join("core/core.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        db.busy_timeout(Duration::from_secs(5)).unwrap();
        db
    }

    /// (agent, job, slot, outcome) of every fire row.
    fn fires(&self) -> Vec<(String, String, String, String)> {
        let db = self.db();
        let mut st = db.prepare("SELECT agent, job, slot, outcome FROM sched_fires ORDER BY agent, job, slot").unwrap();
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap().map(Result::unwrap).collect()
    }

    /// (agent's conversation, state) of every scheduled turn.
    fn turns(&self) -> Vec<(String, String)> {
        let db = self.db();
        let mut st = db.prepare("SELECT conversation, state FROM turns WHERE source = 'schedule' ORDER BY id").unwrap();
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(Result::unwrap).collect()
    }

    fn prompts(&self) -> usize {
        let text = std::fs::read_to_string(self.hq.join("requests.jsonl")).unwrap_or_default();
        text.lines().filter(|l| serde_json::from_str::<Value>(l).is_ok_and(|r| r["method"] == "session/prompt")).count()
    }

    fn host_log(&self) -> String {
        std::fs::read_to_string(self.dir.join("host.log")).unwrap_or_default()
    }

    fn spawn_host(&self, port: u16) -> Child {
        let log = std::fs::OpenOptions::new().create(true).append(true).open(self.dir.join("host.log")).unwrap();
        self.kith(&["serve", "--bind", "127.0.0.1", "--port", &port.to_string()])
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap()
    }

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
    }

    async fn start_host(&self) -> Host {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            // A port picked here is only free for a moment: a host that finds it taken exits, and this tries another.
            let port = Self::free_port();
            let mut child = Reap(self.spawn_host(port));
            let base = format!("http://127.0.0.1:{port}");
            while child.0.try_wait().unwrap().is_none() && Instant::now() < deadline {
                let token = std::fs::read_to_string(self.serve.join("token")).unwrap_or_default().trim().to_owned();
                if !token.is_empty() {
                    let probe = reqwest::Client::new()
                        .post(format!("{base}/api/probe-whose-host"))
                        .bearer_auth(&token)
                        .send()
                        .await;
                    if probe.is_ok_and(|r| r.status() != reqwest::StatusCode::UNAUTHORIZED) {
                        return Host { child, base, token };
                    }
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            assert!(Instant::now() < deadline, "host didn't start:\n{}", self.host_log());
        }
    }

    /// Stops a host the way launchd does (SIGTERM). Its detached holder is noted first, to end it with the test.
    fn stop_host(&self, mut host: Host) {
        let status = self.kith(&["kernel", "core-status", "--json"]).output().unwrap();
        if let Some(pid) =
            serde_json::from_slice::<Value>(&status.stdout).ok().and_then(|v| v["holder"]["pid"].as_u64())
        {
            self.left.borrow_mut().push(u32::try_from(pid).unwrap());
        }
        common::stop(&mut host.child.0);
    }

    /// Waits for the host to say the core owns the schedules.
    fn wait_core_gate(&self) {
        until("the host to forward to the core", 30, || self.host_log().contains("the core runs the schedules"));
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        // What a cron run left (held by `hold`), and the holder: only pids this test started.
        let mut pids: Vec<u32> = self.left.borrow().clone();
        pids.extend(self.runs_log().into_iter().map(|(_, p)| p));
        for pid in pids {
            let _ = Command::new("kill").arg(pid.to_string()).stderr(Stdio::null()).status();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn text(out: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

/// The slot `time` names, as the core writes it, and as `kith-sched` is told its clock (UTC: the lab's TZ).
fn slot_of(time: chrono::DateTime<Utc>) -> (String, String) {
    (time.format("%Y-%m-%dT%H:%M:00Z").to_string(), time.format("%Y-%m-%dT%H:%M").to_string())
}

/// Waits until the second of the minute is within `from..=to`, so the work that follows has the rest of the minute.
fn wait_for_second(from: u32, to: u32) {
    loop {
        let s = Utc::now().second();
        if (from..=to).contains(&s) {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

async fn post(host: &Host, method: &str, body: Value) -> Value {
    let res = reqwest::Client::new()
        .post(format!("{}/api/{method}", host.base))
        .bearer_auth(&host.token)
        .json(&body)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .unwrap();
    let ok = res.status().is_success();
    let body: Value = res.json().await.unwrap();
    assert!(ok, "{method}: {body}");
    body
}

/// The id of the routine saved for the hq agent `house`, with a cron trigger at `minute` of every hour.
async fn seed_routine(host: &Host, minute: u32) -> String {
    let deadline = Instant::now() + Duration::from_secs(20);
    let bot = loop {
        let snap = common::snapshot(&host.base, &host.token).await;
        if let Some(b) = snap["bots"].as_array().and_then(|bots| bots.iter().find(|b| b["hqAgent"] == "house")) {
            break b["id"].as_str().unwrap().to_owned();
        }
        assert!(Instant::now() < deadline, "house never appeared");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let trigger = json!({"type": "cron", "expression": format!("{minute} * * * *"), "timeZone": "UTC"});
    let saved = post(
        host,
        "saveRoutine",
        json!({"botId": bot, "name": "R", "instruction": "say hi", "triggers": [trigger], "timeoutSeconds": 60}),
    )
    .await;
    saved["routine"]["id"].as_str().unwrap().to_owned()
}

// --- (a) -------------------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_slot_due_while_the_host_is_down_fires_exactly_once_when_the_host_starts() {
    let lab = Lab::new("a", None);
    lab.step_before_schedules();
    // Early in a minute, so the whole setup is done before the next one begins: that edge is the slot.
    wait_for_second(2, 12);
    let edge = Utc::now().with_second(0).unwrap().with_nanosecond(0).unwrap() + chrono::Duration::minutes(1);
    let minute = edge.minute();
    let (slot, kith_sched_now) = slot_of(edge);

    // While the host is up (and the schedules are still its own): a routine with a trigger that is not due yet.
    let host = lab.start_host().await;
    let routine = seed_routine(&host, minute).await;
    lab.stop_host(host);
    // And an hourly cron job of `kith-sched`.
    let jobs = format!("[{{\"id\": \"hourly\", \"cron\": \"{minute} * * * *\", \"task\": \"do it\"}}]\n");
    std::fs::write(lab.kith_home.join("agents/mel/jobs.json"), jobs).unwrap();

    lab.step_ok("schedules");
    assert!(Utc::now() < edge, "the step was not done before the minute edge: a slow machine, run it again");
    // The host stays down past the edge.
    std::thread::sleep((edge + chrono::Duration::seconds(2) - Utc::now()).to_std().unwrap());
    // `kith-sched` (launchd is not running ticks here) stands down at the tombstone and starts nothing.
    let tick = lab.sched(Some(&kith_sched_now), &["tick"]);
    assert!(text(&tick).contains("the schedules moved to the Kith core"), "{}", text(&tick));
    assert!(lab.runs_log().is_empty(), "kith-sched ran a job");

    let host = lab.start_host().await;
    let mine = |rows: &[(String, String, String, String)], agent: &str| {
        rows.iter().filter(|r| r.0 == agent && r.2 == slot && r.3 == "fired").count()
    };
    until("both jobs to fire once and their turns to end", 45, || {
        let rows = lab.fires();
        mine(&rows, "mel") == 1
            && mine(&rows, "house") == 1
            && lab.runs_log().len() == 1
            && lab.prompts() == 1
            && lab.turns().len() == 2
            && lab.turns().iter().all(|t| t.1 == "done")
    });
    let rows = lab.fires();
    assert!(rows.iter().any(|r| r.0 == "house" && r.1 == routine), "{rows:?}");
    assert_eq!(lab.runs_log()[0].0, "mel");

    // A restart of the host adds no row and no run: it ticks again at start and at the next minute.
    lab.stop_host(host);
    let host = lab.start_host().await;
    tokio::time::sleep(Duration::from_secs(6)).await;
    assert_eq!(lab.fires(), rows, "a host restart wrote a fire row");
    assert_eq!((lab.runs_log().len(), lab.prompts()), (1, 1), "a host restart ran a job again");
    lab.stop_host(host);
}

// --- (b) -------------------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_host_start_during_the_step_is_refused_and_one_after_it_runs_as_the_core() {
    let lab = Lab::new("b", None);
    lab.step_before_schedules();
    // The test holds tick.lock (as a tick in flight would) and never touches host.lock.
    std::fs::create_dir_all(lab.machine.join("sched")).unwrap();
    let tick_lock =
        std::fs::OpenOptions::new().create(true).append(true).open(lab.machine.join("sched/tick.lock")).unwrap();
    fs2::FileExt::lock_exclusive(&tick_lock).unwrap();

    let mut step = Reap(
        lab.kith(&["kernel", "cutover", "step", "schedules"])
            .env("KITH_SCHEDULES_TICK_WAIT_MS", "60000")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    // The step says where it is: it holds host.lock and waits for tick.lock.
    let (tx, rx) = std::sync::mpsc::channel();
    let stderr = step.0.stderr.take().unwrap();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let end = Instant::now() + Duration::from_secs(30);
    loop {
        let line =
            rx.recv_timeout(end.saturating_duration_since(Instant::now())).expect("the step never reached tick.lock");
        if line.contains("waiting for") && line.contains("tick.lock") {
            break;
        }
    }

    // A host that starts now cannot take host.lock: it exits non-zero and has written nothing.
    let mut refused = Reap(lab.spawn_host(Lab::free_port()));
    let end = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(s) = refused.0.try_wait().unwrap() {
            break s;
        }
        assert!(Instant::now() < end, "the host did not refuse to start:\n{}", lab.host_log());
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(!status.success());
    assert!(lab.host_log().contains("already running"), "{}", lab.host_log());
    assert!(!lab.serve.join("serve.db").exists(), "the refused host wrote serve.db");
    assert_eq!(lab.fires().len(), 0);

    // The step goes on once tick.lock is free, and nothing was lost in between.
    drop(tick_lock);
    let status = loop {
        if let Some(s) = step.0.try_wait().unwrap() {
            break s;
        }
        assert!(Instant::now() < end + Duration::from_secs(60), "the step never ended");
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(status.success());
    let mut out = String::new();
    std::io::Read::read_to_string(&mut step.0.stdout.take().unwrap(), &mut out).unwrap();
    assert!(out.contains("schedules: switched"), "{out}");

    // A host started after it runs as the core's.
    let host = lab.start_host().await;
    lab.wait_core_gate();
    lab.stop_host(host);
}

// --- (c) -------------------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rollback_while_a_detached_run_goes_on_leaves_kith_sched_skipping_that_job() {
    let lab = Lab::new("c", None);
    lab.step_before_schedules();
    std::fs::write(
        lab.kith_home.join("agents/mel/jobs.json"),
        "[{\"id\": \"hourly\", \"cron\": \"0 * * * *\", \"task\": \"do it\"}]\n",
    )
    .unwrap();
    lab.step_ok("schedules");
    std::fs::write(lab.kernel.join("hold"), "").unwrap();

    // Started now, through the same fire a slot makes.
    let host = lab.start_host().await;
    lab.wait_core_gate();
    // Once: a second run-now would answer busy while the first run is held. Retried only while nothing ran.
    let mut answer = None;
    until("the daemon to take the run-now", 30, || {
        answer = lab.owner("schedule.runNow", &json!({"agent": "mel", "id": "hourly"}));
        answer.is_some()
    });
    let started = Instant::now();
    while lab.runs_log().len() != 1 && started.elapsed() < Duration::from_secs(40) {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(
        lab.runs_log().len(),
        1,
        "the run never started; run-now answered {answer:?}; fires {:?}; turns {:?}; host log:\n{}",
        lab.fires(),
        lab.turns(),
        lab.host_log()
    );
    let pid = lab.runs_log()[0].1;
    let claim_pid: Option<i64> = lab
        .db()
        .query_row("SELECT pid FROM claims WHERE state = 'running' AND kind = 'external'", [], |r| r.get(0))
        .ok();
    assert_eq!(claim_pid, Some(i64::from(pid)), "the run attached its own pid to its claim");

    // Neither the host nor the core daemon is up, and the run goes on.
    lab.stop_host(host);
    let status = lab.kith(&["kernel", "core-status", "--json"]).output().unwrap();
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["daemon"]["running"], false, "{status}");
    assert!(pid_alive(pid), "the run died with the host");

    let back = lab.cutover("rollback", "schedules");
    assert!(back.status.success(), "{}", text(&back));
    let last = lab.state_json()["last"]["mel/hourly"].clone();
    assert_eq!(last["pid"], i64::from(pid), "{last}");
    assert!(last["procStart"].as_str().is_some_and(|s| !s.is_empty()), "{last}");

    // kith-sched ticks at the next hourly slots (inside its 6 h window): the run is still going, so it skips the job.
    let next_hour = Utc::now().with_minute(0).unwrap().with_second(0).unwrap() + chrono::Duration::hours(1);
    let tick = lab.sched(Some(&slot_of(next_hour).1), &["tick"]);
    assert!(text(&tick).contains("still running"), "{}", text(&tick));
    assert_eq!(lab.runs_log().len(), 1, "kith-sched started the job on top of the run");

    // The run ends; the slot after that starts exactly one new run.
    std::fs::remove_file(lab.kernel.join("hold")).unwrap();
    until("the held run to end", 20, || !pid_alive(pid));
    let after = next_hour + chrono::Duration::hours(1);
    let tick = lab.sched(Some(&slot_of(after).1), &["tick"]);
    assert!(text(&tick).contains("started mel/hourly"), "{}", text(&tick));
    until("the new run", 20, || lab.runs_log().len() == 2);
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(lab.runs_log().len(), 2);
}

// --- (d) -------------------------------------------------------------------------------------------------------------------

#[test]
fn a_kith_sched_tick_that_started_before_the_step_holds_it_up_until_it_ends() {
    // A stand-in `kith-sched`: it carries the marker the gate reads, runs as `sh <file> tick` (no exec, so its arguments
    // stay), sleeps, and ends when it is told to.
    let stand_in = format!("#!/bin/sh\nsleep 30 &\nwait $!\nexit 0\n{MARKER}\n");
    let lab = Lab::new("d", Some(&stand_in));
    lab.step_before_schedules();
    let tick = Reap(
        lab.cmd(lab.kernel.join("bin/kith-sched"))
            .arg("tick")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let pid = tick.0.id();
    // Its arguments are readable: it is listed, and the step refuses.
    until("the stand-in to be listed", 10, || {
        kith_kernel::procstart::own_commands()
            .iter()
            .any(|(p, args)| *p == pid && args.iter().any(|a| a.ends_with("bin/kith-sched")))
    });
    let held = lab
        .kith(&["kernel", "cutover", "step", "schedules"])
        .env("KITH_SCHEDULES_TICK_WAIT_MS", "1500")
        .output()
        .unwrap();
    assert!(!held.status.success());
    assert!(text(&held).contains(&format!("6: a kith-sched tick (pid {pid}) is still running")), "{}", text(&held));
    assert!(!text(&held).contains("schedules: switched"), "{}", text(&held));

    // The tick ends during the wait: the step goes through.
    let child_pid = i32::try_from(pid).unwrap();
    let killer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1500));
        // SAFETY: SIGKILL to the one stand-in this test started.
        unsafe { libc::kill(child_pid, libc::SIGKILL) };
    });
    let through = lab
        .kith(&["kernel", "cutover", "step", "schedules"])
        .env("KITH_SCHEDULES_TICK_WAIT_MS", "20000")
        .output()
        .unwrap();
    killer.join().unwrap();
    assert!(through.status.success(), "{}", text(&through));
    assert!(text(&through).contains("schedules: switched"), "{}", text(&through));
    drop(tick);
}
