//! The wake cutover on real processes (retire-python P2, WK6 brief section 4). Each case runs a real `kith serve` (and the core
//! daemon it starts inside itself, and a real `kith-holder`), the real `kith kernel cutover` verbs (claims, transcripts, wake),
//! and the real `kith-wake` and `kith-ask` (python) from a copy of the kernel; the launch is the built `kith`, through the
//! copy's `bin/kith` (the run door's `hq::bin()`). Throwaway homes in /tmp, `env_clear` for every process, no launchd, no
//! launchctl: the plist is a file the gate reads. Only the engine is a stand-in: a `claude` script that records each start
//! (argv and environment), appends the prompt to the session's transcript (where the core looks for the `[kith wake RUN]`
//! marker) and holds while the file `hold` exists.
//!
//! - E1 an owed wake runs once across the step
//! - E2 a run cut off by a host restart is ended by `end_gone` and answered by its marker, and starts once
//! - E3 a host-door wake (a reply card's) is delivered once, with neither turn variable reaching the engine
//! - E4 an unread ask answer arrives as one wake within two ticks
//! - E5 a rollback while a wake runs hands the run to `kith-wake` with its process
//! - E6 a refused step resumes: the files written meanwhile are imported
//!
//! Not stubbed: the step and rollback, the three locks, the host's clock and run door, the core's claims, turns and `end_gone`,
//! the holder, `kith-wake`, `kith-ask`, `bin/kith` and the Rust launch. Not covered here: the card RPC that makes a reply card (E3
//! enqueues the same host-door request the card's answer does).
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[allow(dead_code)]
mod common;

use serde_json::{Value, json};
use std::cell::RefCell;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const KITH: &str = env!("CARGO_BIN_EXE_kith");
const SID: &str = "7d1c5a40-2b9e-4c61-8f3a-0a1b2c3d4e51";

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
        std::thread::sleep(Duration::from_millis(200));
    }
    panic!("gave up waiting for {what}");
}

/// A host, as a process: killed when the test ends any way.
struct Reap(Child);

impl Drop for Reap {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Host {
    child: Reap,
}

/// The engine. Probes answer as Claude's do. A run records its start at once (argv, then the environment), puts its prompt in the
/// session's transcript, and holds while `$HOLD` exists.
const ENGINE: &str = r#"#!/bin/sh
case "$1 $2" in
  "--version "*) echo "2.1.309 (Claude Code)"; exit 0 ;;
  "plugin list") echo '[]'; exit 0 ;;
  "auth status") echo '{"loggedIn":true}'; exit 0 ;;
esac
sid=""; prompt=""; prev=""
for a in "$@"; do
  case "$prev" in --resume|-r) sid="$a" ;; esac
  case "$a" in "[kith wake "*) prompt="$a" ;; esac
  prev="$a"
done
case "$prompt" in "[kith wake "*) ;; *) exit 0 ;; esac
{
  echo "argv=$*"
  env | sort
} > "$REC/start.$$"
mkdir -p "$HOME/.claude/projects/p1"
printf '%s\n' "$prompt" | tr '\n' ' ' >> "$HOME/.claude/projects/p1/$sid.jsonl"
echo >> "$HOME/.claude/projects/p1/$sid.jsonl"
while [ -e "$HOLD" ]; do sleep 0.2; done
echo "ok"
exit 0
"#;

/// Logs every python started through `PATH` that runs `kith-wake`, then runs the real one: what the host (and the test) started.
const PY_SHIM: &str =
    "#!/bin/sh\ncase \"$*\" in *kith-wake*) echo \"$*\" >> \"$WAKELOG\" ;; esac\nexec /usr/bin/python3 \"$@\"\n";

struct Lab {
    dir: PathBuf,
    home: PathBuf,
    kith_home: PathBuf,
    serve: PathBuf,
    kernel: PathBuf,
    machine: PathBuf,
    state: PathBuf,
    /// Pids the test started through the host and must not leave behind.
    left: RefCell<Vec<u32>>,
}

fn executable(p: &Path) {
    let mut perm = std::fs::metadata(p).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
    std::fs::set_permissions(p, perm).unwrap();
}

impl Lab {
    fn new(tag: &str) -> Lab {
        ensure_holder();
        let dir = PathBuf::from("/tmp").join(format!("kwe-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        let (home, kernel) = (dir.join("home"), dir.join("k"));
        let kith_home = home.join("kith");
        let machine = home.join(".config/kith");
        let state = kith_home.join("state");
        for d in [
            &kernel.join("bin"),
            &machine.join("cli"),
            &state.join("live/terminal"),
            // The old link layout's mark: the host's runs get KITH_STATE=<home>/kith/state, and without it a child reads that as a
            // choice of state folder, which moves its machine folder (and so the core's socket) away from the host's.
            &kith_home.join("kernel"),
            &dir.join("bin"),
            &dir.join("rec"),
            &home.join(".claude"),
        ] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(home.join(".claude/.fake-login"), "").unwrap();
        let role = kith_home.join("agents/house/role");
        std::fs::create_dir_all(&role).unwrap();
        std::fs::write(role.join("agent.md"), "---\nname: house\ndescription: test\n---\nbody\n").unwrap();
        // A copy of the kernel's programs beside the real lib: `kith`, `kith-wake` and `kith-ask` are the repo's own files.
        let real = repo().join("kernel");
        for entry in std::fs::read_dir(real.join("bin")).unwrap().flatten() {
            if entry.path().is_file() {
                std::fs::copy(entry.path(), kernel.join("bin").join(entry.file_name())).unwrap();
            }
        }
        for name in ["lib", "adapters", "agents", "services", "skills"] {
            if real.join(name).exists() {
                std::os::unix::fs::symlink(real.join(name), kernel.join(name)).unwrap();
            }
        }
        std::os::unix::fs::symlink(&kernel, machine.join("cli/current")).unwrap();
        for (name, text) in [("claude", ENGINE), ("python3", PY_SHIM), ("kith-stub", "#!/bin/sh\n")] {
            let f = dir.join("bin").join(name);
            std::fs::write(&f, text).unwrap();
            executable(&f);
        }
        // The sessions a wake resumes were started before: `kith <session>` resumes only one whose transcript it finds.
        let project = home.join(".claude/projects/p1");
        std::fs::create_dir_all(&project).unwrap();
        for sid in [SID, "7d1c5a40-2b9e-4c61-8f3a-0a1b2c3d4e52", "7d1c5a40-2b9e-4c61-8f3a-0a1b2c3d4e53"] {
            std::fs::write(
                project.join(format!("{sid}.jsonl")),
                format!("{}\n", json!({"type": "user", "cwd": home.to_string_lossy(), "message": "earlier"})),
            )
            .unwrap();
        }
        // The copy's bash `kith` finds the program on PATH (the host's launch hands the engine's env, not this test's KITH_BIN).
        // A wrapper, so a failing case can say what the host's launch handed the program.
        std::fs::create_dir_all(dir.join("kcalls")).unwrap();
        let wrapper = dir.join("bin/kith");
        let calls = dir.join("kcalls");
        // A `kernel threads` call (the launch's claim) says what it printed and what the core said at that moment.
        let script = format!(
            "#!/bin/sh\nD=\"{calls}\"\n{{ echo \"args=$*\"; env | sort; }} > \"$D/call.$$\" 2>/dev/null\n\
             case \"$*\" in\n\
               \"kernel threads\"*)\n\
                 \"{KITH}\" \"$@\" 2> \"$D/err.$$\"; rc=$?\n\
                 cat \"$D/err.$$\" >&2\n\
                 if [ $rc -ne 0 ]; then {{ echo \"rc=$rc\"; \"{KITH}\" kernel core-status --json; \"{KITH}\" kernel core-call claim.list --params-stdin < /dev/null; }} > \"$D/live.$$\" 2>&1; fi\n\
                 exit $rc ;;\n\
             esac\n\
             exec \"{KITH}\" \"$@\"\n",
            calls = calls.display()
        );
        std::fs::write(&wrapper, script).unwrap();
        executable(&wrapper);
        let plists = home.join("Library/LaunchAgents");
        std::fs::create_dir_all(&plists).unwrap();
        let words = ["serve", "--port", "1"];
        let items = std::iter::once(KITH).chain(words).fold(String::new(), |mut s, w| {
            s.push_str("<string>");
            s.push_str(w);
            s.push_str("</string>");
            s
        });
        std::fs::write(
            plists.join("dev.kith.serve.plist"),
            format!("<plist><dict><key>ProgramArguments</key><array>{items}</array></dict></plist>"),
        )
        .unwrap();
        std::fs::write(machine.join("auth.json"), "{\"people\": {\"kai\": {\"owner\": true}}}\n").unwrap();
        Lab { serve: dir.join("serve"), dir, home, kith_home, kernel, machine, state, left: RefCell::new(Vec::new()) }
    }

    fn env(&self) -> Vec<(String, String)> {
        let p = |x: &Path| x.to_string_lossy().into_owned();
        [
            ("HOME", p(&self.home)),
            ("KITH_HOME", p(&self.kith_home)),
            ("KITH_SERVE_HOME", p(&self.serve)),
            ("KITH_KERNEL", p(&self.kernel)),
            ("KITH_BIN", KITH.to_owned()),
            // The run door's `hq::bin()`: the copy's bash `kith`, which resumes the session.
            ("KITH_KERNEL_BIN", p(&self.kernel.join("bin/kith"))),
            ("KITH_STUB_BIN", p(&self.dir.join("bin/kith-stub"))),
            ("PATH", format!("{}:/usr/bin:/bin", self.dir.join("bin").display())),
            ("TZ", "UTC".to_owned()),
            ("PYTHONDONTWRITEBYTECODE", "1".to_owned()),
            ("KITH_SERVE_CLOUD", "off".to_owned()),
            ("KITH_WAKE_NO_KICK", "1".to_owned()),
            ("REC", p(&self.dir.join("rec"))),
            ("HOLD", p(&self.dir.join("hold"))),
            ("WAKELOG", p(&self.dir.join("wake.log"))),
            // Never this Mac's master key item.
            ("KITH_SECRETS_KEYSTORE_ACCOUNT", format!("test-{}", self.dir.display().to_string().replace('/', "-"))),
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
        c.env_clear().envs(self.env()).current_dir(&self.home);
        c
    }

    fn kith(&self, args: &[&str]) -> Command {
        let mut c = self.cmd(KITH);
        c.args(args);
        c
    }

    /// `kith kernel cutover VERB MODULE`; the build's gate is shut until its checklist is done, which a debug build lets the test
    /// open (the plist, the holder and the host lock are still checked for real).
    fn cutover(&self, verb: &str, module: &str) -> Output {
        let mut c = self.kith(&["kernel", "cutover", verb, module]);
        c.env("KITH_WAKE_GATE_OPEN", "1");
        c.output().unwrap()
    }

    fn step_ok(&self, module: &str) {
        let out = self.cutover("step", module);
        assert!(out.status.success(), "step {module}: {}", text(&out));
    }

    fn paths(&self) -> kith_kernel::paths::Paths {
        let kith_home = self.kith_home.to_string_lossy().into_owned();
        kith_kernel::paths::paths(&self.home, &self.kernel, &|k: &str| match k {
            "KITH_HOME" => Some(kith_home.clone()),
            _ => None,
        })
    }

    /// An owner call of the running daemon. `None`: nothing ran (no daemon yet, a refusal).
    fn owner(&self, method: &str, params: &Value) -> Option<Value> {
        match kith_core::reach(&self.paths(), method, params, false) {
            kith_core::Reached::Answered(v) => Some(v),
            kith_core::Reached::Lost(_) => Some(Value::Null),
            _ => None,
        }
    }

    /// `python3 kith-wake ARGS` of the copy, as the kernel's own callers run it.
    fn wake(&self, args: &[&str]) -> Output {
        let mut c = self.cmd("python3");
        c.arg(self.kernel.join("bin/kith-wake")).args(args);
        c.output().unwrap()
    }

    fn enqueue(&self, session: &str, id: &str, door: &str) {
        let out = self.wake(&["enqueue", session, "--id", id, "--agent", "house", "--door", door, "--", "hello"]);
        assert!(out.status.success(), "enqueue: {}", text(&out));
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

    fn tombstone(&self) -> PathBuf {
        self.state.join("sessions/queue/.tombstone")
    }

    /// Every file of the queue and waiters folders with its mtime: what a writer after the tombstone would change.
    fn queue_files(&self) -> Vec<(String, std::time::SystemTime)> {
        let mut all = Vec::new();
        for d in ["queue", "waits"] {
            for e in std::fs::read_dir(self.state.join("sessions").join(d)).into_iter().flatten().flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if e.path().is_file() && name != ".lock" {
                    all.push((format!("{d}/{name}"), e.metadata().unwrap().modified().unwrap()));
                }
            }
        }
        all.sort();
        all
    }

    /// The engine starts that carried a wake's note: (file, its text).
    fn starts(&self) -> Vec<String> {
        let mut all: Vec<String> = std::fs::read_dir(self.dir.join("rec"))
            .unwrap()
            .flatten()
            .filter_map(|e| std::fs::read_to_string(e.path()).ok())
            .collect();
        all.sort();
        all
    }

    /// How many lines of the session's transcript carry a wake marker.
    fn markers(&self, sid: &str) -> usize {
        std::fs::read_to_string(self.home.join(".claude/projects/p1").join(format!("{sid}.jsonl")))
            .unwrap_or_default()
            .matches("[kith wake ")
            .count()
    }

    /// What `kith-wake` processes the host or a kernel verb started, as the python shim logged them.
    fn wake_calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("wake.log")).unwrap_or_default().lines().map(str::to_owned).collect()
    }

    fn state_of(&self, id: &str) -> Option<String> {
        let v = self.owner("wake.queueStatus", &json!({"id": id}))?;
        v["state"].as_str().map(str::to_owned)
    }

    /// A failing case prints what the processes said and what the core holds: CI is the only place these run.
    fn say_what_happened(&self) {
        let log = self.host_log();
        let tail: Vec<&str> = log.lines().rev().take(150).collect();
        eprintln!("--- host.log (last {} lines)", tail.len());
        for l in tail.iter().rev() {
            eprintln!("{l}");
        }
        let core_log = std::fs::read_to_string(self.home.join("Library/Logs/kith/core.log")).unwrap_or_default();
        let tail: Vec<&str> = core_log.lines().rev().take(60).collect();
        eprintln!("--- core.log (last {} lines)", tail.len());
        for l in tail.iter().rev() {
            eprintln!("{l}");
        }
        let Ok(db) = rusqlite::Connection::open_with_flags(
            self.machine.join("core/core.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        ) else {
            return;
        };
        let mut calls: Vec<_> = std::fs::read_dir(self.dir.join("kcalls")).into_iter().flatten().flatten().collect();
        calls.sort_by_key(std::fs::DirEntry::file_name);
        for e in std::fs::read_dir(self.dir.join("kcalls")).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with("err.") || name.starts_with("live.") {
                eprintln!("--- {name}\n{}", std::fs::read_to_string(e.path()).unwrap_or_default());
            }
        }
        // Every call's arguments; the whole environment of the first one that is not a probe (a launch).
        let mut shown = false;
        for e in &calls {
            let text = std::fs::read_to_string(e.path()).unwrap_or_default();
            let args = text.lines().next().unwrap_or_default();
            eprintln!("--- a call of the kith program: {} {args}", e.file_name().to_string_lossy());
            if !shown && !args.contains("kernel paths") && !args.contains("memory hook") {
                shown = true;
                for l in text.lines().skip(1) {
                    eprintln!("{l}");
                }
            }
        }
        for table in ["turns", "claims", "wake_sessions", "wake_items", "wake_turns", "wake_answered"] {
            let Ok(mut st) = db.prepare(&format!("SELECT * FROM {table}")) else { continue };
            let names: Vec<String> = st.column_names().iter().map(|s| (*s).to_owned()).collect();
            eprintln!("--- {table}");
            let Ok(rows) = st.query_map([], |r| {
                Ok(names
                    .iter()
                    .enumerate()
                    .map(|(i, n)| format!("{n}={}", r.get_ref(i).map(|v| format!("{v:?}")).unwrap_or_default()))
                    .collect::<Vec<_>>()
                    .join(" "))
            }) else {
                continue;
            };
            for row in rows.flatten() {
                eprintln!("{row}");
            }
        }
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
                        return Host { child };
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

    /// The claims and transcripts steps (the wake step's dependencies), with the host down.
    fn step_dependencies(&self) {
        self.step_ok("claims");
        self.step_ok("transcripts");
    }

    /// The core's live claim on `conversation`: (pid, state), once the run attached.
    fn run_claim(&self, conversation: &str) -> Option<(i64, String)> {
        let listed = self.owner("claim.list", &json!({}))?;
        listed["live"].as_array()?.iter().find_map(|c| {
            (c["conversation"] == conversation && c["kind"] == "external" && c["pid"].is_i64())
                .then(|| (c["pid"].as_i64().unwrap(), c["state"].as_str().unwrap_or_default().to_owned()))
        })
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.say_what_happened();
        }
        // What a run left (held by `hold`), and the holder: only pids this test started.
        let _ = std::fs::remove_file(self.dir.join("hold"));
        for pid in self.left.borrow().iter() {
            let _ = Command::new("kill").arg(pid.to_string()).stderr(Stdio::null()).status();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn text(out: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

/// The env of a recorded start has neither of the host's turn variables.
fn no_turn_vars(start: &str) {
    for gone in ["KITH_TURN_CLAIM", "HQ_TURN_CLAIM", "KITH_TURN_CONVERSATION", "HQ_TURN_CONVERSATION"] {
        assert!(!start.lines().any(|l| l.starts_with(&format!("{gone}="))), "{gone} reached the engine:\n{start}");
    }
}

/// Waits until a request is answered (however it was).
fn answered(lab: &Lab, id: &str, secs: u64) -> String {
    let mut how = String::new();
    until(&format!("{id} to be answered"), secs, || {
        how = lab.state_of(id).unwrap_or_default();
        matches!(how.as_str(), "answered" | "recovered" | "failed" | "withdrawn") || how.starts_with("answered")
    });
    how
}

// --- E1 --------------------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e1_an_owed_wake_runs_once_across_the_step() {
    let lab = Lab::new("e1");
    lab.step_dependencies();
    // Files mode, host down: the request is a file.
    lab.enqueue(SID, "run:1", "run");
    assert!(lab.state.join("sessions/queue").read_dir().unwrap().flatten().any(|e| e.file_name() != ".lock"));
    lab.step_ok("wake");
    assert!(lab.tombstone().is_file(), "kith-wake is in Core mode now");
    let files = lab.queue_files();
    let host = lab.start_host().await;
    let how = answered(&lab, "run:1", 120);
    assert!(how.starts_with("answered"), "{how}");
    assert_eq!(lab.starts().len(), 1, "the engine started once: {:?}", lab.starts());
    no_turn_vars(&lab.starts()[0]);
    assert_eq!(lab.markers(SID), 1, "one marker in the transcript");
    let turns: i64 = lab.db().query_row("SELECT count(*) FROM turns WHERE source = 'wake'", [], |r| r.get(0)).unwrap();
    assert_eq!(turns, 1, "one turn per run");
    assert_eq!(lab.queue_files(), files, "no file of the queue was written after the tombstone");
    let calls = lab.wake_calls();
    assert!(
        calls.iter().all(|c| c.contains(" enqueue ")),
        "the host started no kith-wake process (only the test's own enqueue ran): {calls:?}"
    );
    lab.stop_host(host);
}

// --- E2 --------------------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2_a_run_cut_off_by_a_host_restart_is_ended_by_end_gone_and_answered_by_its_marker() {
    let lab = Lab::new("e2");
    lab.step_dependencies();
    lab.enqueue(SID, "run:2", "run");
    lab.step_ok("wake");
    std::fs::write(lab.dir.join("hold"), "").unwrap();
    let host = lab.start_host().await;
    until("the engine to start", 120, || !lab.starts().is_empty());
    until("the launch's claim to run", 30, || lab.run_claim(SID).is_some_and(|(_, s)| s == "running"));
    let (pid, _) = lab.run_claim(SID).unwrap();
    assert!(pid > 0);
    lab.stop_host(host);
    // The engine goes on without a host: nobody waits on the child, so no report comes.
    std::fs::remove_file(lab.dir.join("hold")).unwrap();
    let host = lab.start_host().await;
    let how = answered(&lab, "run:2", 120);
    assert_eq!(how, "recovered", "the marker is in the transcript: the prompt was delivered");
    assert_eq!(lab.starts().len(), 1, "one engine start in all");
    assert_eq!(lab.markers(SID), 1);
    let (turns, claims): (i64, i64) = {
        let db = lab.db();
        (
            db.query_row("SELECT count(*) FROM turns WHERE source = 'wake'", [], |r| r.get(0)).unwrap(),
            db.query_row(
                "SELECT count(*) FROM claims c JOIN turns t ON t.id = c.turn WHERE t.source = 'wake'",
                [],
                |r| r.get(0),
            )
            .unwrap(),
        )
    };
    assert_eq!((turns, claims), (1, 1), "the turn was never claimed again");
    lab.stop_host(host);
}

// --- E3 --------------------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e3_a_host_door_wake_is_delivered_once_and_the_engine_sees_no_turn_variable() {
    let lab = Lab::new("e3");
    lab.step_dependencies();
    lab.step_ok("wake");
    let host = lab.start_host().await;
    // What a reply card's answer enqueues: a host-door request for the session the ask came from.
    lab.enqueue(SID, "run:3", "host");
    let how = answered(&lab, "run:3", 120);
    assert!(how.starts_with("answered") || how == "recovered", "{how}");
    assert_eq!(lab.starts().len(), 1, "the note arrived once: {:?}", lab.starts());
    no_turn_vars(&lab.starts()[0]);
    assert_eq!(lab.markers(SID), 1);
    lab.stop_host(host);
}

// --- E4 --------------------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e4_an_unread_ask_answer_arrives_as_one_wake_within_two_ticks() {
    let lab = Lab::new("e4");
    lab.step_dependencies();
    lab.step_ok("wake");
    // A terminal session's `kith ask --bg` whose callee answered, and the terminal ended: the box holds an unread answer for an
    // ended session. The host's ask bridge (`kith-ask --sweep`, after each tick) hands it over as a wake.
    let box_dir = lab.machine.join("live/ask");
    std::fs::create_dir_all(&box_dir).unwrap();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64();
    std::fs::write(
        box_dir.join("ask-1.reg"),
        json!({"session": SID, "agent": "house", "state": "open", "at": now, "via": "terminal"}).to_string(),
    )
    .unwrap();
    std::fs::write(
        box_dir.join("ask-1.jsonl"),
        format!("{}\n", json!({"as": "echo", "text": "yes from Kai", "kind": "answer"})),
    )
    .unwrap();
    let host = lab.start_host().await;
    // Within two ticks (5 s each, plus the start's own) the request is listed, then delivered once.
    let mut seen = String::new();
    until("the answer to be a queued wake request", 40, || {
        let listed = lab.owner("wake.queueList", &json!({})).unwrap_or_default();
        seen = listed.to_string();
        seen.contains("ask:") || !lab.starts().is_empty()
    });
    until("the engine to start", 120, || !lab.starts().is_empty());
    until("the run to be over", 120, || lab.run_claim(SID).is_none());
    std::thread::sleep(Duration::from_secs(12));
    assert_eq!(lab.starts().len(), 1, "one wake for the answer: {:?}", lab.starts());
    assert!(lab.starts()[0].contains("yes from Kai"), "{}", lab.starts()[0]);
    assert_eq!(lab.markers(SID), 1, "{seen}");
    lab.stop_host(host);
}

// --- E5 --------------------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e5_a_rollback_while_a_wake_runs_hands_the_run_to_kith_wake_with_its_process() {
    let lab = Lab::new("e5");
    lab.step_dependencies();
    lab.enqueue(SID, "run:5", "run");
    lab.enqueue("7d1c5a40-2b9e-4c61-8f3a-0a1b2c3d4e52", "run:stale", "run");
    lab.step_ok("wake");
    std::fs::write(lab.dir.join("hold"), "").unwrap();
    let host = lab.start_host().await;
    until("the engine to start", 120, || !lab.starts().is_empty());
    until("the launch's claim to run", 30, || lab.run_claim(SID).is_some_and(|(_, s)| s == "running"));
    let (pid, _) = lab.run_claim(SID).unwrap();
    // The core answers the other request after the switch: the file from before it is stale.
    let said = lab.owner("wake.withdraw", &json!({"session": "7d1c5a40-2b9e-4c61-8f3a-0a1b2c3d4e52"})).unwrap();
    assert_eq!(said["count"], 1, "{said}");
    // A rollback with the host running is refused, whatever --force says.
    let refused = lab.cutover("rollback", "wake");
    assert!(!refused.status.success() && text(&refused).contains("the host is running"), "{}", text(&refused));
    lab.stop_host(host);
    let out = lab.cutover("rollback", "wake");
    assert!(out.status.success(), "rollback: {}", text(&out));
    assert!(!lab.tombstone().exists(), "Python reads its files again");
    // The item carries the run's turn with the launch's pid and start stamp.
    let key = {
        use sha2::{Digest as _, Sha256};
        Sha256::digest(SID.as_bytes()).iter().take(16).fold(String::new(), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
    };
    let item: Value =
        serde_json::from_slice(&std::fs::read(lab.state.join("sessions/queue").join(format!("{key}.json"))).unwrap())
            .unwrap();
    assert_eq!(item["turn"]["pid"], pid, "{item}");
    assert!(item["turn"]["procStart"].as_str().is_some_and(|s| !s.is_empty()), "{item}");
    assert_eq!(item["requests"][0]["run"], item["turn"]["run"], "{item}");
    // The stale file of the request the core answered is moved aside, and its answer is in answered.json.
    let aside = std::fs::read_dir(lab.state.join("sessions/queue"))
        .unwrap()
        .flatten()
        .any(|e| e.file_name().to_string_lossy().starts_with(".before-rollback-"));
    assert!(aside, "the pre-switch files were moved aside");
    let answered_json = std::fs::read_to_string(lab.state.join("sessions/queue/answered.json")).unwrap();
    assert!(answered_json.contains("run:stale") && answered_json.contains("withdrawn"), "{answered_json}");
    // Files mode: `kith-wake drain` by hand delivers nothing while the run goes on.
    let drained = lab.wake(&["drain"]);
    assert!(drained.status.success(), "{}", text(&drained));
    assert!(!text(&drained).contains("delivering"), "{}", text(&drained));
    assert_eq!(lab.starts().len(), 1);
    // The host (Files mode) ends the claim once the engine's group is gone; then a drain marks the request recovered.
    let host = lab.start_host().await;
    std::fs::remove_file(lab.dir.join("hold")).unwrap();
    until("the request to be recovered by a drain", 120, || {
        let _ = lab.wake(&["drain"]);
        let out = lab.wake(&["status", "run:5"]);
        text(&out).trim() == "recovered" || text(&out).trim() == "answered"
    });
    assert_eq!(lab.starts().len(), 1, "one engine start in total");
    lab.stop_host(host);
}

// --- E6 --------------------------------------------------------------------------------------------------------------------

#[test]
fn e6_a_refused_step_resumes_and_imports_what_was_written_in_between() {
    let lab = Lab::new("e6");
    lab.step_dependencies();
    lab.enqueue(SID, "run:6", "run");
    // A damaged item (a format this build does not know) refuses the import.
    let damaged = lab.state.join("sessions/queue/damaged.json");
    std::fs::write(
        &damaged,
        json!({"v": 2, "session": "s-bad", "agent": "house", "door": "run", "requests": []}).to_string(),
    )
    .unwrap();
    let refused = lab.cutover("step", "wake");
    assert!(!refused.status.success(), "{}", text(&refused));
    assert!(text(&refused).contains("damaged.json"), "{}", text(&refused));
    assert!(!lab.tombstone().exists(), "there is no tombstone");
    // kith-wake still writes files.
    lab.enqueue("7d1c5a40-2b9e-4c61-8f3a-0a1b2c3d4e53", "late:1", "run");
    std::fs::remove_file(&damaged).unwrap();
    lab.step_ok("wake");
    assert!(lab.tombstone().is_file());
    let db = lab.db();
    for id in ["run:6", "late:1"] {
        let n: i64 = db.query_row("SELECT count(*) FROM wake_items WHERE id = ?1", [id], |r| r.get(0)).unwrap();
        assert_eq!(n, 1, "{id} is in the core");
    }
}
