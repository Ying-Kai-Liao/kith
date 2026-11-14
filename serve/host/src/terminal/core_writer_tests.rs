//! Writer mode (`core_writer.rs`) against a scripted core and a REAL `kith-stub --resident`: the rows of the answer table of
//! design 1.3, the busy park and its unpark, a stub that dies, a redeem the core refuses, `LockStuck` and a lost redeem reply.
//! The rules of the rows themselves are tested on a real core in `core/runtime/src/hostclaim_tests.rs` and `basis_tests.rs`.

use super::*;
use crate::terminal::core_claims::tests::{Fake, register};
use kith_acp::{Acp, Ctl, Incoming, spawn_resident};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

const LIVE: [&str; 3] = ["claimed", "running", "ending"];

#[derive(Clone, Debug)]
struct C {
    id: i64,
    conv: String,
    state: String,
    kind: String,
    pid: Option<u64>,
    started: Option<String>,
    instance: Option<String>,
    turn: Option<i64>,
}

#[derive(Default)]
#[allow(clippy::struct_excessive_bools, reason = "one switch per scripted answer")]
struct World {
    claims: Vec<C>,
    /// (id, state, parked reason)
    turns: Vec<(i64, String, Option<String>)>,
    log: Vec<String>,
    procs: Vec<u64>,
    reports: Vec<(String, Value)>,
    account: String,
    down: bool,
    revoke_enqueue: bool,
    /// `hostclaim.turn` answers this refusal as data.
    turn_refusal: Option<&'static str>,
    exhausted: bool,
    /// The redeem is applied but its reply is lost, this many times.
    redeem_lost: u32,
    /// The redeem is refused as ended.
    redeem_refused: bool,
    /// What `cutover.mayServe` answers for the claims module.
    switched: Option<bool>,
    /// The gate answers `ok: false` with a warning (stepping, rolling back, a marker mismatch), or refuses the call.
    gate_warning: bool,
    gate_refused: bool,
    /// (conversation, key, turn): an enqueue with a key already seen answers the same turn.
    keys: Vec<(String, String, i64)>,
    /// The enqueue is applied but its reply is lost, this many times.
    enqueue_lost: u32,
}

fn data(v: Value) -> Answer {
    Answer::Done(v)
}

fn refused(kind: &str) -> Answer {
    data(json!({"refused": kind, "why": kind}))
}

impl World {
    fn live(&self, conv: &str) -> Option<usize> {
        self.claims.iter().position(|c| c.conv == conv && LIVE.contains(&c.state.as_str()))
    }

    fn turn(&mut self, id: i64) -> Option<&mut (i64, String, Option<String>)> {
        self.turns.iter_mut().find(|t| t.0 == id)
    }

    fn answer(&mut self, method: &str, p: &Value) -> Answer {
        self.log.push(method.to_owned());
        if self.down {
            return Answer::Down { why: "the core is down".into(), lost: false };
        }
        let conv = p["conversation"].as_str().unwrap_or_default().to_owned();
        let id = p["claimId"].as_i64().unwrap_or_default();
        match method {
            "cutover.mayServe" => {
                if self.gate_refused {
                    return Answer::Refused("the gate is not answering".into());
                }
                if self.gate_warning {
                    return data(json!({"ok": false, "warning": "the module is stepping"}));
                }
                data(json!({"ok": self.switched.unwrap_or(false)}))
            }
            "hostclaim.epoch" => data(json!({"epoch": 0})),
            "hostclaim.enqueue" => {
                if self.revoke_enqueue {
                    return refused("revoked");
                }
                let key = p["key"].as_str().expect("every enqueue carries a key").to_owned();
                let seen = self.keys.iter().find(|k| k.0 == conv && k.1 == key).map(|k| k.2);
                let n = seen.unwrap_or_else(|| {
                    let n = i64::try_from(self.turns.len()).unwrap() + 1;
                    self.turns.push((n, "queued".into(), None));
                    self.keys.push((conv.clone(), key, n));
                    n
                });
                if self.enqueue_lost > 0 {
                    self.enqueue_lost -= 1;
                    return Answer::Down { why: "the reply was lost".into(), lost: true };
                }
                data(json!({"turn": n}))
            }
            "hostclaim.turn" => {
                let turn = p["turn"].as_i64().unwrap();
                if let Some(kind) = self.turn_refusal {
                    return refused(kind);
                }
                if self.exhausted {
                    return data(json!({"status": "exhausted", "text": "no account left"}));
                }
                if self.live(&conv).is_some() {
                    return refused("held");
                }
                let n = i64::try_from(self.claims.len()).unwrap() + 1;
                self.claims.push(C {
                    id: n,
                    conv,
                    state: "claimed".into(),
                    kind: "turn".into(),
                    pid: None,
                    started: None,
                    instance: p["holderInstance"].as_str().map(str::to_owned),
                    turn: Some(turn),
                });
                if let Some(t) = self.turn(turn) {
                    t.1 = "claimed".into();
                }
                data(
                    json!({"status": "claimed", "claimId": n, "generation": 1, "account": self.account, "fresh": false}),
                )
            }
            "hostclaim.redeem" => {
                if self.redeem_refused {
                    return data(json!({"refused": "ended"}));
                }
                let (pid, started) = (p["pid"].as_u64(), p["started"].as_str().map(str::to_owned));
                let Some(c) = self.claims.iter_mut().find(|c| c.id == id) else {
                    return data(json!({"refused": "unknown"}));
                };
                let answer = if c.state == "claimed" {
                    (c.state, c.pid, c.started, c.instance) =
                        ("running".into(), pid, started, p["holderInstance"].as_str().map(str::to_owned));
                    let turn = c.turn;
                    if let Some(t) = turn.and_then(|t| self.turn(t)) {
                        t.1 = "running".into();
                    }
                    json!({"running": true, "again": false})
                } else if c.state == "running" && c.pid == pid && c.started == started {
                    json!({"running": true, "again": true})
                } else {
                    json!({"refused": c.state})
                };
                if self.redeem_lost > 0 && answer["running"] == true {
                    self.redeem_lost -= 1;
                    return Answer::Down { why: "the reply was lost".into(), lost: true };
                }
                data(answer)
            }
            "hostclaim.report" => {
                let body: Value = serde_json::from_str(p["body"].as_str().unwrap()).unwrap();
                let kind = p["kind"].as_str().unwrap().to_owned();
                self.reports.push((kind.clone(), body.clone()));
                let pid = body["pid"].as_u64();
                let hit = self
                    .claims
                    .iter_mut()
                    .find(|c| c.id == id && LIVE.contains(&c.state.as_str()) && (c.pid.is_none() || c.pid == pid));
                let Some(c) = hit else { return data(json!({"stored": true, "applied": null})) };
                c.state = "ended".into();
                let turn = c.turn;
                if let Some(t) = turn.and_then(|t| self.turn(t)) {
                    (t.1, t.2) = match kind.as_str() {
                        "done" => ("done".into(), None),
                        "busy" => ("parked".into(), Some("lock_busy".into())),
                        _ => ("queued".into(), None),
                    };
                }
                data(json!({"stored": true, "applied": kind}))
            }
            "hostclaim.stop" => {
                let requeue = p["requeue"] == true;
                let Some(c) = self.claims.iter_mut().find(|c| c.id == id && LIVE.contains(&c.state.as_str())) else {
                    return data(json!({"kill": null}));
                };
                if c.state == "claimed" && c.pid.is_none() {
                    c.state = "ended".into();
                    let turn = c.turn;
                    if let Some(t) = turn.and_then(|t| self.turn(t)) {
                        t.1 = if requeue { "queued".into() } else { "cancelled".into() };
                    }
                } else {
                    c.state = "ending".into();
                }
                data(json!({"kill": null}))
            }
            "hostclaim.settle" => {
                let mut ended = Vec::new();
                for c in &mut self.claims {
                    let gone = c.pid.and_then(|p| u32::try_from(p).ok()).is_some_and(|p| !pid_runs(p));
                    if gone && c.state == "ending" {
                        c.state = "ended".into();
                        ended.push(c.id);
                    }
                }
                data(json!({"ended": ended}))
            }
            "hostclaim.show" => {
                let c = if p["claimId"].is_i64() {
                    self.claims.iter().find(|c| c.id == id)
                } else {
                    self.live(&conv).map(|i| &self.claims[i])
                };
                data(json!({"claim": c.map(|c| json!({"claimId": c.id, "conversation": c.conv, "state": c.state,
                    "kind": c.kind, "person": "owner", "pid": c.pid, "started": c.started, "holderInstance": c.instance, "turn": c.turn}))}))
            }
            "hostclaim.cancel" => {
                let t = p["turn"].as_i64().unwrap();
                let mut done = false;
                if let Some(x) = self.turn(t).filter(|x| ["queued", "parked", "claimed"].contains(&x.1.as_str())) {
                    (x.1, x.2, done) = ("cancelled".into(), None, true);
                }
                self.claims
                    .iter_mut()
                    .filter(|c| c.turn == Some(t) && c.state == "claimed")
                    .for_each(|c| c.state = "ended".into());
                data(json!({"cancelled": done}))
            }
            "hostclaim.park" => {
                let t = p["turn"].as_i64().unwrap();
                let reason = p["reason"].as_str().unwrap().to_owned();
                match self.turn(t).filter(|x| x.1 == "queued") {
                    Some(x) => {
                        (x.1, x.2) = ("parked".into(), Some(reason));
                        data(json!({"parked": true}))
                    }
                    None => data(json!({"parked": false})),
                }
            }
            "hostclaim.unpark" => {
                let held = self.live(&conv).is_some();
                let (free, term) = (p["lockFree"] == true, p["terminalLive"] == true);
                let mut woken = Vec::new();
                if !held {
                    for t in self.turns.iter_mut().filter(|t| t.1 == "parked") {
                        let ready = match t.2.as_deref() {
                            Some("lock_busy") => free,
                            Some("terminal_live") => !term,
                            _ => false,
                        };
                        if ready {
                            (t.1, t.2) = ("queued".into(), None);
                            woken.push(t.0);
                        }
                    }
                }
                data(json!({"turns": woken}))
            }
            "hostclaim.proc" => {
                self.procs.push(p["pid"].as_u64().unwrap());
                data(json!({"registered": true}))
            }
            "hostclaim.procGone" => {
                let pid = p["pid"].as_u64().unwrap();
                let had = self.procs.contains(&pid);
                self.procs.retain(|x| *x != pid);
                data(json!({"gone": had}))
            }
            "hostclaim.adopt" => {
                let me = p["instance"].as_str().unwrap().to_owned();
                let (mut ended, mut kill) = (Vec::new(), Vec::new());
                for c in &mut self.claims {
                    let other = c.instance.as_deref().is_some_and(|i| i != me && i.starts_with("kith-serve-host"));
                    if !other || !LIVE.contains(&c.state.as_str()) {
                        continue;
                    }
                    if c.state == "claimed" && c.pid.is_none() {
                        c.state = "ended".into();
                        ended.push(c.id);
                    } else {
                        c.state = "ending".into();
                        kill.push(json!({"claimId": c.id, "pid": c.pid, "started": c.started}));
                    }
                }
                data(json!({"ended": ended, "kill": kill}))
            }
            other => Answer::Refused(format!("the scripted core has no {other}")),
        }
    }
}

struct Rig {
    dir: PathBuf,
    folders: Paths,
    world: Arc<Mutex<World>>,
    writer: CoreWriter,
}

impl Rig {
    fn new() -> Self {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "kith-cw-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        std::fs::create_dir_all(dir.join("state")).unwrap();
        let state = dir.join("state");
        let folders = Paths {
            home: dir.clone(),
            state: state.clone(),
            agents: dir.join("agents"),
            home_real: dir.clone(),
            state_real: state.clone(),
            agents_real: dir.join("agents"),
            machine: dir.join("machine"),
            secrets: dir.join("secrets"),
            logs: dir.join("logs"),
            cache: dir.join("cache"),
            store: dir.join("store"),
        };
        // A switched Mac: the gate says yes, and the switch file is there for when the core does not answer.
        let world = Arc::new(Mutex::new(World { account: "main".into(), switched: Some(true), ..World::default() }));
        let switch = folders.machine.join("core").join("cutover");
        std::fs::create_dir_all(&switch).unwrap();
        std::fs::write(switch.join("claims.switch"), "").unwrap();
        let w = world.clone();
        let fake: Fake = Arc::new(move |m, p| w.lock().unwrap().answer(m, p));
        register(&state, fake);
        let writer = CoreWriter::new(folders.clone());
        Self { dir, folders, world, writer }
    }

    fn w(&self) -> std::sync::MutexGuard<'_, World> {
        self.world.lock().unwrap()
    }

    fn log(&self) -> Vec<String> {
        self.w().log.clone()
    }

    fn turn_state(&self, id: i64) -> (String, Option<String>) {
        let w = self.w();
        let t = w.turns.iter().find(|t| t.0 == id).unwrap();
        (t.1.clone(), t.2.clone())
    }

    fn claim_state(&self, id: i64) -> String {
        self.w().claims.iter().find(|c| c.id == id).unwrap().state.clone()
    }

    fn locks(&self) -> PathBuf {
        self.dir.join("locks")
    }

    /// A warm process under a real resident stub whose engine is `cat` (it exits when the host drops its stdin).
    fn stub(&self, account: &str) -> Real {
        Real::start(&self.locks(), Some(account.to_owned()))
    }

    #[allow(clippy::unused_self, reason = "reads as the rig's")]
    fn turn<'a>(&self, conv: &'a str, id: Option<i64>) -> Turn<'a> {
        Turn {
            // A turn accepted now: its own key (a test that retries one turn reuses the struct).
            key: Box::leak(new_turn_key().into_boxed_str()),
            id,
            conversation: conv,
            agent: "builder",
            engine: "claude",
            source: "app",
            person: "",
            cwd: "/tmp",
            revoke_seen: 0,
            deadline: None,
            room: None,
        }
    }

    /// A one-shot stub (as `kith --direct` runs) holding `key`, until the returned handle is dropped.
    fn direct(&self, key: &str) -> Direct {
        // The probe (`lock_free`) takes the lock for a moment itself, and a one-shot that starts inside that moment finds it busy
        // and exits (75): on a loaded Mac that is the usual case. So a one-shot that has exited is started again.
        for _ in 0..20 {
            let mut child = Command::new(stub_bin())
                .args(["--lock-dir"])
                .arg(self.locks())
                .args(["--key", key, "--", "cat"])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let stdin = child.stdin.take();
            for _ in 0..40 {
                std::thread::sleep(Duration::from_millis(25));
                if child.try_wait().unwrap().is_some() {
                    break;
                }
                if !self.lock_free(key) {
                    return Direct { child, stdin };
                }
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        panic!("the one-shot stub never took the lock");
    }

    /// Whether the lock of `key` can be taken now (a one-shot stub answers 75 when it is held).
    fn lock_free(&self, key: &str) -> bool {
        Command::new(stub_bin())
            .args(["--lock-dir"])
            .arg(self.locks())
            .args(["--key", key, "--", "true"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct Direct {
    child: Child,
    stdin: Option<std::process::ChildStdin>,
}

impl Direct {
    /// The one-shot's engine ends; the stub exits and the lock goes.
    fn end(&mut self) {
        self.stdin.take();
        self.child.wait().unwrap();
    }
}

impl Drop for Direct {
    fn drop(&mut self) {
        self.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn stub_bin() -> PathBuf {
    static BUILT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| {
            let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
            let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../core/stub/Cargo.toml");
            let built = Command::new(cargo).args(["build", "--manifest-path"]).arg(&manifest).status().unwrap();
            assert!(built.success(), "kith-stub did not build");
            // The test binary is in target/<profile>/deps; the stub is in target/<profile>.
            let stub = std::env::current_exe().unwrap().parent().unwrap().parent().unwrap().join("kith-stub");
            assert!(stub.is_file(), "no {}", stub.display());
            stub
        })
        .clone()
}

/// A warm process the way the wiring package will hold one: `spawn_resident`, plus knobs for the failures.
struct Real {
    locks: PathBuf,
    acp: Arc<Acp>,
    ctl: Ctl,
    _rx: tokio::sync::mpsc::UnboundedReceiver<Incoming>,
    account: Option<String>,
    restarts: Vec<Option<String>>,
    /// Every release answers an error (a wedged stub).
    release_fails: bool,
    killed: std::cell::Cell<bool>,
    /// The pid the host reads (a process that is not there: its start time can't be read).
    fake_pid: Option<u32>,
}

impl Real {
    fn start(locks: &std::path::Path, account: Option<String>) -> Self {
        std::fs::create_dir_all(locks).unwrap();
        let (acp, rx, ctl) = spawn_resident(&stub_bin(), locks, "cat", "/tmp", &[]).unwrap();
        Self {
            locks: locks.to_path_buf(),
            acp,
            ctl,
            _rx: rx,
            account,
            restarts: Vec::new(),
            release_fails: false,
            killed: std::cell::Cell::new(false),
            fake_pid: None,
        }
    }

    fn kill_now(&self) {
        let pid = i32::try_from(self.acp.pid().unwrap()).unwrap();
        // SAFETY: a signal to the process group this struct started, and a reap of its leader (a zombie still "runs").
        unsafe {
            libc::killpg(pid, libc::SIGKILL);
            for _ in 0..100 {
                if libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) != 0 {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    /// Waits until the stub is a zombie or gone: its control channel then reads end-of-file.
    #[allow(clippy::unused_self, reason = "reads as the stub's")]
    fn wait_dead(&self) {
        std::thread::sleep(Duration::from_millis(200));
    }
}

impl WarmStub for Real {
    fn pid(&self) -> u32 {
        self.fake_pid.unwrap_or_else(|| self.acp.pid().unwrap())
    }
    fn account(&self) -> Option<String> {
        self.account.clone()
    }
    fn take(&self, key: &str, claim: i64) -> std::io::Result<Take> {
        self.ctl.take(key, claim)
    }
    fn release(&self, key: &str, claim: i64) -> std::io::Result<Release> {
        if self.release_fails {
            return Err(std::io::Error::other("wedged"));
        }
        self.ctl.release(key, claim)
    }
    fn restart(&mut self, account: Option<&str>) -> std::io::Result<()> {
        self.kill_now();
        let account = account.map(str::to_owned).or_else(|| self.account.clone());
        let next = Real::start(&self.locks, account.clone());
        self.restarts.push(account.clone());
        let restarts = std::mem::take(&mut self.restarts);
        *self = Real { restarts, ..next };
        Ok(())
    }
    fn kill_group(&self) {
        self.killed.set(true);
        self.kill_now();
    }
}

fn running(b: Begin) -> Running {
    match b {
        Begin::Running(r) => r,
        other => panic!("expected a running turn, got {other:?}"),
    }
}

// --- the mode switch ---------------------------------------------------------------------------------------------------

#[test]
fn writer_mode_is_off_unless_the_core_says_the_module_is_switched() {
    let yes = Answer::Done(json!({"ok": true}));
    let no = Answer::Done(json!({"ok": false}));
    let warned = Answer::Done(json!({"ok": false, "warning": "marker mismatch"}));
    let down = Answer::Down { why: "no core".into(), lost: false };
    assert_eq!(Mode::decide(&yes, true, false), Mode::Writer);
    assert_eq!(Mode::decide(&no, false, false), Mode::Files, "off by default");
    assert_eq!(Mode::decide(&no, false, true), Mode::Shadow, "the shadow flag alone is the old mode");
    assert_eq!(Mode::decide(&no, true, true), Mode::Shadow, "a clear no is never Writer, even with a switch file");
    // "switched, core down" and "not switched, core up" are two different rigs (the TEST_CORE lesson).
    assert_eq!(Mode::decide(&down, true, false), Mode::Held, "switched and no core: held, so the turn waits");
    assert_eq!(
        Mode::decide(&down, false, false),
        Mode::Files,
        "never switched and no core: files, as on a Mac without one"
    );
    // An unclear answer is never Writer (the rule of `routines/switch.rs`): only `ok: true` is.
    assert_eq!(Mode::decide(&warned, true, false), Mode::Held);
    assert_eq!(Mode::decide(&warned, true, true), Mode::Held, "held beats the shadow flag");
    assert_eq!(Mode::decide(&warned, false, false), Mode::Files);
    let refused = Answer::Refused("no".into());
    assert_eq!(Mode::decide(&refused, true, false), Mode::Held);
    assert_eq!(Mode::decide(&refused, false, false), Mode::Files);
    let odd = Answer::Done(json!({"fine": true}));
    assert_eq!(Mode::decide(&odd, true, false), Mode::Held);
    // No env var turns it on in a shipped build: the flag is Shadow only.
    assert_eq!(Mode::default(), Mode::Files);
}

#[test]
fn the_host_instance_is_per_start_and_a_host_name_to_the_core() {
    let i = host_instance();
    let (prefix, rest) = i.split_once('/').unwrap();
    assert_eq!(prefix, "kith-serve-host");
    assert!(rest.starts_with(&format!("{}-", std::process::id())), "{i}");
    assert_ne!(i, super::super::core_claims::HOST_INSTANCE, "Shadow mode's constant is not this start's");
    let r = Rig::new();
    r.w().switched = Some(true);
    assert_eq!(Mode::select(&r.folders), Mode::Writer);
}

// --- the answer table --------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claimed_takes_the_lock_redeems_on_the_stubs_pid_and_a_normal_end_releases_then_reports_done() {
    let r = Rig::new();
    let mut stub = r.stub("main");
    let run = running(r.writer.begin(&r.turn("s1", None), &mut stub));
    assert_eq!((run.turn, run.claim, run.pid, run.again), (1, 1, stub.pid(), false));
    assert_eq!(run.account.as_deref(), Some("main"));
    assert_eq!(
        r.log(),
        ["cutover.mayServe", "hostclaim.show", "hostclaim.enqueue", "hostclaim.turn", "hostclaim.redeem"],
        "the lock is taken between the claim and the redeem"
    );
    assert_eq!(r.turn_state(1).0, "running");
    assert_eq!(r.w().claims[0].instance.as_deref(), Some(r.writer.instance()));
    assert!(!r.lock_free("s1"), "the stub holds the conversation's lock");
    assert_eq!(r.writer.finish(&run, "s1", &mut stub, End::Done), Finished::Reported);
    assert!(r.lock_free("s1"), "released first");
    assert_eq!((r.claim_state(1).as_str(), r.turn_state(1).0.as_str()), ("ended", "done"));
    assert_eq!(&r.log()[r.log().len() - 1], "hostclaim.report");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_terminal_writer_parks_terminal_live_and_a_kick_runs_it_once_it_is_gone() {
    let r = Rig::new();
    let mut stub = r.stub("main");
    r.w().claims.push(C {
        id: 1,
        conv: "s1".into(),
        state: "running".into(),
        kind: "terminal".into(),
        pid: Some(1),
        started: None,
        instance: None,
        turn: None,
    });
    let b = r.writer.begin(&r.turn("s1", None), &mut stub);
    assert_eq!(b, Begin::Parked { turn: 1, why: Parked::TerminalLive });
    assert_eq!(r.turn_state(1), ("parked".into(), Some("terminal_live".into())));
    assert!(r.lock_free("s1"), "nothing was taken");
    // The terminal is still there: nothing wakes. Then it ends; the kick wakes the turn once, and the second does nothing.
    assert_eq!(r.writer.unpark("s1", true).unwrap(), Vec::<i64>::new());
    r.w().claims[0].state = "ended".into();
    assert_eq!(r.writer.unpark("s1", false).unwrap(), [1]);
    assert_eq!(r.writer.unpark("s1", false).unwrap(), Vec::<i64>::new(), "unpark twice does nothing the second time");
    let run = running(r.writer.begin(&r.turn("s1", Some(1)), &mut stub));
    assert_eq!(run.turn, 1, "the same turn id: enqueued once");
    assert_eq!(r.log().iter().filter(|m| *m == "hostclaim.enqueue").count(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn held_by_another_turn_stays_queued_and_retries() {
    let r = Rig::new();
    let mut stub = r.stub("main");
    let mut other = r.stub("main");
    let first = running(r.writer.begin(&r.turn("s1", None), &mut other));
    let b = r.writer.begin(&r.turn("s1", None), &mut stub);
    assert_eq!(b, Begin::Queued { turn: 2 });
    assert_eq!(r.turn_state(2).0, "queued", "stays queued, not parked");
    // The first turn ends; the same turn id claims on the next tick.
    r.writer.finish(&first, "s1", &mut other, End::Done);
    let run = running(r.writer.begin(&r.turn("s1", Some(2)), &mut stub));
    assert_eq!(run.turn, 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn waiting_retries_after_a_second_and_not_waiting_stopped_and_revoked_drop_the_item() {
    let r = Rig::new();
    let mut stub = r.stub("main");
    r.w().turn_refusal = Some("waiting");
    assert_eq!(r.writer.begin(&r.turn("s1", None), &mut stub), Begin::Retry { turn: 1, after: RECONCILE_RETRY });
    assert_eq!(RECONCILE_RETRY, Duration::from_secs(1));
    r.w().turn_refusal = Some("notWaiting");
    let b = r.writer.begin(&r.turn("s1", Some(1)), &mut stub);
    assert!(matches!(b, Begin::Dropped { turn: Some(1), why: Dropped::NotWaiting, .. }), "{b:?}");
    r.w().turn_refusal = Some("stopped");
    let b = r.writer.begin(&r.turn("s1", Some(1)), &mut stub);
    assert!(matches!(b, Begin::Dropped { why: Dropped::Stopped, .. }), "{b:?}");
    assert_eq!(r.turn_state(1).0, "queued", "stopped: the core cancelled it, the host does not");
    // A person revoked after the host's check: the turn is cancelled (c4-epochcancel), then dropped.
    r.w().turn_refusal = Some("revoked");
    let b = r.writer.begin(&r.turn("s1", Some(1)), &mut stub);
    assert!(matches!(b, Begin::Dropped { why: Dropped::Revoked, .. }), "{b:?}");
    assert_eq!(r.turn_state(1).0, "cancelled");
    assert!(r.lock_free("s1"), "no lock was ever taken for a refused turn");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_revoked_enqueue_drops_the_item_with_nothing_queued() {
    let r = Rig::new();
    let mut stub = r.stub("main");
    r.w().revoke_enqueue = true;
    let b = r.writer.begin(&r.turn("s1", None), &mut stub);
    assert!(matches!(b, Begin::Dropped { turn: None, why: Dropped::Revoked, .. }), "{b:?}");
    assert_eq!(r.w().turns.len(), 0);
    assert_eq!(r.log(), ["cutover.mayServe", "hostclaim.show", "hostclaim.enqueue"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exhausted_cancels_the_turn_and_hands_back_its_text() {
    let r = Rig::new();
    let mut stub = r.stub("main");
    r.w().exhausted = true;
    assert_eq!(r.writer.begin(&r.turn("s1", None), &mut stub), Begin::Exhausted { text: "no account left".into() });
    assert_eq!(r.turn_state(1).0, "cancelled");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_host_follows_the_account_the_core_picked() {
    let r = Rig::new();
    let mut stub = r.stub("main");
    let first = stub.pid();
    r.w().account = "spare".into();
    let run = running(r.writer.begin(&r.turn("s1", None), &mut stub));
    assert_eq!(stub.restarts, [Some("spare".to_owned())], "restarted on the picked account before the take");
    assert_ne!(stub.pid(), first);
    assert_eq!(run.pid, stub.pid(), "the redeem names the new stub");
    assert_eq!(stub.account().as_deref(), Some("spare"));
    // The same account again: no restart.
    r.writer.finish(&run, "s1", &mut stub, End::Done);
    let again = stub.pid();
    running(r.writer.begin(&r.turn("s1", None), &mut stub));
    assert_eq!(stub.pid(), again);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lock_held_by_a_direct_run_parks_the_turn_lock_busy_and_the_unpark_retries_the_take() {
    let r = Rig::new();
    let mut stub = r.stub("main");
    let mut direct = r.direct("s1");
    let b = r.writer.begin(&r.turn("s1", None), &mut stub);
    assert_eq!(b, Begin::Parked { turn: 1, why: Parked::LockBusy });
    assert_eq!(r.claim_state(1), "ended", "the claim ends busy");
    assert_eq!(r.turn_state(1), ("parked".into(), Some("lock_busy".into())));
    assert_eq!(r.w().reports[0].0, "busy");
    assert_eq!(r.w().reports[0].1["pid"], stub.pid());
    // Still held: the unpark wakes it, the retried take is the probe, and it parks again.
    assert_eq!(r.writer.unpark("s1", false).unwrap(), [1]);
    assert_eq!(r.writer.begin(&r.turn("s1", Some(1)), &mut stub), Begin::Parked { turn: 1, why: Parked::LockBusy });
    // The one-shot ends: the next unpark and begin win the lock.
    direct.end();
    assert_eq!(r.writer.unpark("s1", false).unwrap(), [1]);
    let run = running(r.writer.begin(&r.turn("s1", Some(1)), &mut stub));
    assert_eq!(run.turn, 1);
    assert!(!r.lock_free("s1"));
    assert_eq!(r.writer.unpark("s1", false).unwrap(), Vec::<i64>::new());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stub_that_died_is_reported_exit_restarted_and_the_claim_made_again_once() {
    let r = Rig::new();
    let mut stub = r.stub("main");
    let dead = stub.pid();
    stub.kill_now();
    stub.wait_dead();
    let run = running(r.writer.begin(&r.turn("s1", None), &mut stub));
    assert_ne!(run.pid, dead, "the turn runs on the restarted process");
    assert_eq!(r.w().reports[0].0, "exit", "the first claim ended with an exit report");
    assert_eq!(r.w().reports[0].1["pid"], dead);
    assert_eq!(r.claim_state(1), "ended");
    assert_eq!(r.claim_state(2), "running");
    assert_eq!(run.turn, 1, "the same turn, claimed again");
    assert_eq!(r.log().iter().filter(|m| *m == "hostclaim.enqueue").count(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_redeem_the_core_refuses_gives_the_lock_back() {
    let r = Rig::new();
    let mut stub = r.stub("main");
    r.w().redeem_refused = true;
    let b = r.writer.begin(&r.turn("s1", None), &mut stub);
    assert!(matches!(b, Begin::Dropped { why: Dropped::Stopped, .. }), "{b:?}");
    assert!(r.lock_free("s1"), "the lock went back");
    assert_eq!(r.turn_state(1).0, "cancelled");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lock_that_cannot_be_given_back_stops_the_claim_and_kills_the_group() {
    let r = Rig::new();
    let mut stub = r.stub("main");
    stub.release_fails = true;
    r.w().redeem_refused = true;
    let b = r.writer.begin(&r.turn("s1", None), &mut stub);
    assert!(matches!(b, Begin::LockStuck { .. }), "{b:?}");
    assert!(stub.killed.get(), "the process group was killed");
    assert!(r.log().contains(&"hostclaim.stop".to_owned()), "{:?}", r.log());
    stub.wait_dead();
    assert!(r.lock_free("s1"), "the lock went with the stub");
    // At the end of a turn, too.
    let r = Rig::new();
    let mut stub = r.stub("main");
    let run = running(r.writer.begin(&r.turn("s1", None), &mut stub));
    stub.release_fails = true;
    assert!(matches!(r.writer.finish(&run, "s1", &mut stub, End::Done), Finished::LockStuck(_)));
    assert!(stub.killed.get());
    assert_eq!(r.claim_state(1), "ending", "stopped, and the writer until the process is confirmed gone");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lost_redeem_reply_stands_and_a_retry_answers_the_same_thing() {
    let r = Rig::new();
    let mut stub = r.stub("main");
    r.w().redeem_lost = 1;
    let first = running(r.writer.begin(&r.turn("s1", None), &mut stub));
    assert!(first.again, "the claim was running on our pid when show was asked");
    assert!(r.log().contains(&"hostclaim.show".to_owned()));
    assert!(!r.lock_free("s1"), "the lock stays: the claim runs");
    // The same turn begins again (the caller did not see the answer): the same claim, the same stub.
    let second = running(r.writer.begin(&r.turn("s1", Some(first.turn)), &mut stub));
    assert_eq!((second.turn, second.claim, second.pid), (first.turn, first.claim, first.pid));
    assert!(second.again);
    assert_eq!(r.w().claims.len(), 1, "no second claim");
    assert_eq!(r.writer.finish(&second, "s1", &mut stub, End::Done), Finished::Reported);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_core_that_does_not_answer_means_the_turn_waits_and_never_takes_the_lock() {
    let r = Rig::new();
    let mut stub = r.stub("main");
    r.w().down = true;
    // The Mac is switched (its switch file is there) and the core does not answer: held, nothing asked.
    assert_eq!(r.writer.begin(&r.turn("s1", None), &mut stub), Begin::NotWriter { turn: None, mode: Mode::Held });
    assert!(r.lock_free("s1"));
    // The gate answers but the next call does not.
    r.w().down = false;
    let w = r.world.clone();
    let wrapped: Fake = Arc::new(move |m, p| {
        let mut w = w.lock().unwrap();
        if m == "hostclaim.show" && p["conversation"].is_string() {
            w.log.push(m.to_owned());
            return Answer::Down { why: "gone".into(), lost: false };
        }
        w.answer(m, p)
    });
    register(&r.folders.state, wrapped);
    assert!(matches!(r.writer.begin(&r.turn("s1", None), &mut stub), Begin::CoreDown { turn: None, .. }));
    assert!(r.lock_free("s1"));
    let w = r.world.clone();
    register(&r.folders.state, Arc::new(move |m, p| w.lock().unwrap().answer(m, p)));
    // Down in the middle: the claim was made, the redeem got no answer. The lock is kept; the retry finishes the redeem.
    r.w().down = false;
    let w = r.world.clone();
    let wrapped: Fake = Arc::new(move |m, p| {
        let mut w = w.lock().unwrap();
        if m == "hostclaim.redeem" || (m == "hostclaim.show" && p["claimId"].is_i64()) {
            w.log.push(m.to_owned());
            return Answer::Down { why: "gone".into(), lost: false };
        }
        w.answer(m, p)
    });
    register(&r.folders.state, wrapped);
    let b = r.writer.begin(&r.turn("s1", None), &mut stub);
    assert!(matches!(b, Begin::CoreDown { turn: Some(1), .. }), "{b:?}");
    assert!(!r.lock_free("s1"), "the lock is kept while the claim's fate is unknown");
    let w = r.world.clone();
    register(&r.folders.state, Arc::new(move |m, p| w.lock().unwrap().answer(m, p)));
    let run = running(r.writer.begin(&r.turn("s1", Some(1)), &mut stub));
    assert_eq!((run.turn, run.claim), (1, 1), "the unredeemed claim is resumed, not made again");
    assert_eq!(r.w().claims.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_release_with_no_take_is_refused_cleanly_and_a_wedged_channel_stays_closed() {
    let r = Rig::new();
    let stub = r.stub("main");
    assert_eq!(stub.ctl.release("s1", 7).unwrap(), Release::NotHeld);
    assert_eq!(stub.ctl.take("s1", 7).unwrap(), Take::Locked);
    assert_eq!(stub.ctl.take("s1", 7).unwrap(), Take::Locked, "a repeated take of the same claim");
    assert_eq!(stub.ctl.take("s1", 8).unwrap(), Take::Busy, "another claim of the same stub");
    assert_eq!(stub.ctl.release("s1", 7).unwrap(), Release::Released);
    assert_eq!(stub.ctl.release("s1", 7).unwrap(), Release::NotHeld, "a repeated release");
    assert!(stub.ctl.take("bad key", 1).is_err(), "not a key");
    stub.kill_now();
    stub.wait_dead();
    assert!(stub.ctl.take("s1", 9).is_err());
    assert!(!stub.ctl.is_open());
    assert!(stub.ctl.release("s1", 9).is_err(), "closed for good");
}

// --- the end of a turn -------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn running_out_of_usage_reports_exit_with_the_flag_and_the_core_requeues() {
    let r = Rig::new();
    let mut stub = r.stub("main");
    let run = running(r.writer.begin(&r.turn("s1", None), &mut stub));
    assert_eq!(r.writer.finish(&run, "s1", &mut stub, End::OutOfUsage), Finished::Reported);
    assert_eq!(r.w().reports[0].0, "exit");
    assert_eq!(r.w().reports[0].1["out_of_usage"], true);
    assert_eq!(r.turn_state(1).0, "queued");
    assert!(r.lock_free("s1"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_room_level_turn_ends_with_a_stop_a_kill_and_the_process_forgotten() {
    let r = Rig::new();
    let mut stub = r.stub("main");
    let pid = stub.pid();
    r.writer.register_proc(pid, "stamp", "room-1", "builder", Some("s1"), Some(0)).unwrap();
    assert_eq!(r.w().procs, [u64::from(pid)]);
    let run = running(r.writer.begin(&r.turn("s1", None), &mut stub));
    assert_eq!(r.writer.finish(&run, "s1", &mut stub, End::Room), Finished::RoomEnded { gone: true });
    assert!(stub.killed.get());
    assert!(r.w().procs.is_empty(), "forgotten once gone");
    assert_eq!(r.claim_state(1), "ended", "end_gone ended it");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_user_stop_that_ended_in_a_kill_reports_exit_without_releasing() {
    let r = Rig::new();
    let mut stub = r.stub("main");
    let run = running(r.writer.begin(&r.turn("s1", None), &mut stub));
    assert_eq!(r.writer.finish(&run, "s1", &mut stub, End::Killed), Finished::Reported);
    assert_eq!(r.w().reports[0].0, "exit");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn abandoning_a_begun_turn_frees_the_lock_and_cancels_the_claim() {
    let r = Rig::new();
    let mut stub = r.stub("main");
    let run = running(r.writer.begin(&r.turn("s1", None), &mut stub));
    r.writer.abandon(run.turn, run.claim, "s1", &mut stub);
    assert!(r.lock_free("s1"));
    assert_eq!(r.claim_state(1), "ending", "stopped; the process is the stub's, still there");
}

// --- the host restart --------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn adopt_ends_the_claims_of_an_older_host_and_leaves_this_ones() {
    let r = Rig::new();
    let mut stub = r.stub("main");
    let mine = running(r.writer.begin(&r.turn("s1", None), &mut stub));
    let old = |id: i64, conv: &str, state: &str, pid: Option<u64>| C {
        id,
        conv: conv.into(),
        state: state.into(),
        kind: "turn".into(),
        pid,
        started: pid.map(|_| "stamp".into()),
        instance: Some("kith-serve-host/42-old".into()),
        turn: None,
    };
    r.w().claims.extend([old(10, "a", "claimed", None), old(11, "b", "running", Some(99))]);
    let a = r.writer.adopt().unwrap();
    assert_eq!((a.ended.as_slice(), a.kill.as_slice()), (&[10][..], &[11][..]));
    assert_eq!(r.claim_state(10), "ended");
    assert_eq!(r.claim_state(11), "ending", "the writer until its process is confirmed gone");
    assert_eq!(r.claim_state(mine.claim), "running", "this start's own claim is not touched");
    let again = r.writer.adopt().unwrap();
    assert!(again.ended.is_empty(), "repeating changes nothing more");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_begin_ends_no_claim_it_was_not_asked_to_and_another_live_claim_of_ours_is_held() {
    let r = Rig::new();
    let mut stub = r.stub("main");
    let first = running(r.writer.begin(&r.turn("s1", None), &mut stub));
    // A second begin on the conversation without the first having finished (P7 never does this; the rule holds anyway): the
    // running claim is a writer like any other, so the new turn stays queued and nothing of the first is touched.
    let b = r.writer.begin(&r.turn("s1", None), &mut stub);
    assert_eq!(b, Begin::Queued { turn: 2 });
    assert_eq!(r.claim_state(1), "running");
    assert_eq!(r.turn_state(1).0, "running");
    assert!(!r.lock_free("s1"), "its lock is still held");
    assert!(r.w().reports.is_empty(), "no end was reported for it");
    // Naming the first turn resumes it (the same claim); naming another turn does not.
    let again = running(r.writer.begin(&r.turn("s1", Some(first.turn)), &mut stub));
    assert_eq!((again.claim, again.again), (first.claim, true));
    let b = r.writer.begin(&r.turn("s1", Some(2)), &mut stub);
    assert_eq!(b, Begin::Queued { turn: 2 }, "a claim of turn 1 is not resumed by turn 2");
    assert_eq!(r.w().claims.len(), 1);
    // A claim that was never redeemed is no different.
    let r = Rig::new();
    let mut stub = r.stub("main");
    r.w().redeem_lost = 0;
    r.w().claims.push(C {
        id: 1,
        conv: "s1".into(),
        state: "claimed".into(),
        kind: "turn".into(),
        pid: None,
        started: None,
        instance: Some(r.writer.instance().to_owned()),
        turn: Some(7),
    });
    let b = r.writer.begin(&r.turn("s1", Some(9)), &mut stub);
    assert!(matches!(b, Begin::Queued { turn: 9 }), "{b:?}");
    assert_eq!(r.claim_state(1), "claimed", "left alone");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lost_end_report_is_sent_again_by_the_caller_and_a_begin_does_not_end_it() {
    let r = Rig::new();
    let mut stub = r.stub("main");
    let run = running(r.writer.begin(&r.turn("s1", None), &mut stub));
    // The core goes away at the end of the turn: the lock is released, the report is lost.
    r.w().down = true;
    let lost = r.writer.finish(&run, "s1", &mut stub, End::Done);
    assert!(matches!(lost, Finished::ReportLost(_)), "{lost:?}");
    assert!(r.lock_free("s1"), "released first");
    r.w().down = false;
    assert_eq!(r.claim_state(1), "running");
    // The next turn's begin leaves it alone: the conversation stays held for it.
    let b = r.writer.begin(&r.turn("s1", None), &mut stub);
    assert_eq!(b, Begin::Queued { turn: 2 });
    assert_eq!(r.claim_state(1), "running");
    // The slow tick sends the report again: the claim ends, the turn is done, a repeat changes nothing.
    assert_eq!(r.writer.retry_end(&run, End::Done), Finished::Reported);
    assert_eq!((r.claim_state(1).as_str(), r.turn_state(1).0.as_str()), ("ended", "done"));
    assert_eq!(r.writer.retry_end(&run, End::Done), Finished::Reported);
    let next = running(r.writer.begin(&r.turn("s1", Some(2)), &mut stub));
    assert_eq!(next.turn, 2);
    // Still down: the retry says so.
    r.w().down = true;
    assert!(matches!(r.writer.retry_end(&next, End::OutOfUsage), Finished::ReportLost(_)));
}

// --- the gate is read at every begin -----------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unclear_gate_holds_the_turn_with_no_hostclaim_call_and_it_runs_once_the_gate_says_yes() {
    let r = Rig::new();
    let mut stub = r.stub("main");
    let held = Begin::NotWriter { turn: None, mode: Mode::Held };
    // Stepping or rolling back (a warning), with the switch file there.
    r.w().gate_warning = true;
    assert_eq!(r.writer.begin(&r.turn("s1", None), &mut stub), held);
    // The gate question itself refused.
    r.w().gate_warning = false;
    r.w().gate_refused = true;
    assert_eq!(r.writer.begin(&r.turn("s1", None), &mut stub), held);
    // No core at all.
    r.w().gate_refused = false;
    r.w().down = true;
    assert_eq!(r.writer.begin(&r.turn("s1", None), &mut stub), held);
    assert_eq!(r.writer.mode(), Mode::Held);
    assert!(r.log().iter().all(|m| m == "cutover.mayServe"), "nothing but the gate was asked: {:?}", r.log());
    assert!(r.lock_free("s1"));
    // A clear no is not Writer either: the module was switched back.
    r.w().down = false;
    r.w().switched = Some(false);
    let t = r.turn("s1", None);
    assert_eq!(r.writer.begin(&t, &mut stub), Begin::NotWriter { turn: None, mode: Mode::Files });
    // The gate is read again at each begin, not once: it says yes now and the same turn runs.
    r.w().switched = Some(true);
    assert_eq!(r.writer.mode(), Mode::Writer);
    running(r.writer.begin(&t, &mut stub));
}

// --- the stub's start time, the enqueue key -----------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_start_time_that_cannot_be_read_ends_the_claim_and_puts_the_turn_back() {
    let r = Rig::new();
    let mut stub = r.stub("main");
    // A pid that is not there: the take works on the real stub, the start time of the pid the host reads does not.
    stub.fake_pid = Some(4_000_000);
    let b = r.writer.begin(&r.turn("s1", None), &mut stub);
    assert!(matches!(b, Begin::Failed { turn: Some(1), .. }), "{b:?}");
    assert!(r.lock_free("s1"), "the lock went back");
    assert_eq!(r.claim_state(1), "ended", "Failed means ended");
    assert_eq!(r.turn_state(1).0, "queued", "the turn is put back, not cancelled");
    // The next begin of the same turn claims again.
    stub.fake_pid = None;
    let run = running(r.writer.begin(&r.turn("s1", Some(1)), &mut stub));
    assert_eq!((run.turn, run.claim), (1, 2));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_enqueue_whose_reply_was_lost_is_retried_with_its_key_onto_the_same_turn() {
    let r = Rig::new();
    let mut stub = r.stub("main");
    r.w().enqueue_lost = 1;
    let t = r.turn("s1", None);
    // A plain owner turn (empty basis): the row is made, the reply is lost, the caller sees the core down.
    let b = r.writer.begin(&t, &mut stub);
    assert!(matches!(b, Begin::CoreDown { turn: None, .. }), "{b:?}");
    assert_eq!(r.w().turns.len(), 1);
    // The same item begins again with the same key: the same row, not a second one.
    let run = running(r.writer.begin(&t, &mut stub));
    assert_eq!(run.turn, 1);
    assert_eq!(r.w().turns.len(), 1, "one row: the budget is spent once");
    assert_eq!(r.log().iter().filter(|m| *m == "hostclaim.enqueue").count(), 2);
    // Every enqueue carries the key.
    assert_eq!(r.w().keys.len(), 1);
    assert_ne!(new_turn_key(), new_turn_key());
}
