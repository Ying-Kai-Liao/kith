//! The ask kind of the run door against a real core (the daemon the host tests start), not a stub: the core checks every param of a
//! row strictly and settles by its own rules, so a call the host spells wrong, or an answer it loses, fails here (lesson #1072).
//! The callee is a stand-in `kith` script. It does not attach its claim (the kernel's launch does that, and is tested in the kernel),
//! so what is proved here is the host's half: what it starts, with what, and what it tells the ask when the run ends or is stopped.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::{World, engine_of, take};
use crate::core_front::{Answer, call_host, call_owner};
use crate::core_front::{TestDaemon, serve_for_test};
use crate::terminal::core_writer::CoreWriter;
use kith_kernel::paths::Paths;
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

struct Core {
    home: PathBuf,
    folders: Paths,
    daemon: Option<TestDaemon>,
    writer: Arc<CoreWriter>,
}

impl Core {
    fn start() -> Self {
        // Short, for the core's socket; and with a `.kith` in it, or every home's state is the kernel checkout's.
        let home = PathBuf::from("/tmp").join(format!("kwa-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]));
        std::fs::create_dir_all(home.join(".kith")).expect("home");
        let folders = crate::kernel::folders(&home);
        let daemon = serve_for_test(&folders);
        daemon.reconciled();
        let writer = Arc::new(CoreWriter::with_instance(folders.clone(), "test-host".to_owned()));
        Self { home, folders, daemon: Some(daemon), writer }
    }

    fn call(&self, method: &str, params: &Value) -> Value {
        match call_owner(&self.folders, method, params, false) {
            Answer::Done(v) => v,
            _ => panic!("{method}: the core did not answer"),
        }
    }

    /// A `--bg` ask of `echo`, opened by `boss` in session `sess-1`, as `kith ask --bg` opens it.
    fn open(&self, id: &str, task: &str, extra: &Value) -> Value {
        let mut p = json!({"id": id, "asked": "echo", "mode": "bg", "task": task,
                           "home": self.home, "env": {"KITH_AGENT": "boss", "CLAUDE_CODE_SESSION_ID": "sess-1",
                                                      "KITH_VIA": "terminal", "KITH_STATE": self.folders.state}});
        for (k, v) in extra.as_object().into_iter().flatten() {
            p[k] = v.clone();
        }
        self.call("ask.open", &p)
    }

    fn queued(&self) -> Vec<Value> {
        self.writer.queued_from(&["ask"]).expect("the core lists the ask turns")
    }

    fn state(&self, id: &str) -> String {
        self.call("ask.record", &json!({"id": id}))["state"].as_str().unwrap_or_default().to_owned()
    }

    fn rows(&self, id: &str) -> Vec<Value> {
        self.call("ask.read", &json!({"id": id}))["rows"].as_array().cloned().unwrap_or_default()
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        if let Some(d) = self.daemon.take() {
            d.stop();
        }
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

/// A `kith` that logs the claim, folder and words it was given, and answers as a callee does (JSON on stdout). A task that says
/// SLEEP makes it a process that stays (its pid is written first).
fn stub(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let file = dir.join("kith");
    let script = format!(
        "#!/bin/sh\necho \"$KITH_TURN_CLAIM|$PWD|$KITH_OUTPUT|$KITH_RESUME|$*\" >> {log}\necho $$ > {pid}\n\
         case \"$*\" in *SLEEP*) exec sleep 60;; esac\n\
         echo '{{\"result\":\"hi there\",\"total_cost_usd\":0.25,\"session_id\":\"sess-x\"}}'\n",
        log = dir.join("log").display(),
        pid = dir.join("pid").display()
    );
    std::fs::write(&file, script).unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
    file
}

fn lines(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("log")).unwrap_or_default().lines().map(str::to_owned).collect()
}

fn until(what: &str, f: impl Fn() -> bool) {
    for _ in 0..400 {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("never happened: {what}");
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only asks whether the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

fn pull(core: &Core, dir: &Path, harness: &'static str) {
    let world = World {
        bin: Some(dir.join("kith")),
        kernel: Some(dir.join("kernel")),
        harness: &move |_| Some(harness.to_owned()),
        runtime: None,
    };
    take(&core.writer, &core.queued(), &world);
}

fn scratch() -> PathBuf {
    std::env::temp_dir().join(format!("kith-core-ask-{}", uuid::Uuid::new_v4()))
}

#[test]
fn the_engine_is_the_agents_own_unless_the_turn_names_another() {
    let t = |e: &str| json!({"engine": e});
    assert_eq!(engine_of(&t("claude"), Some("claude")), "claude");
    assert_eq!(engine_of(&t("claude"), Some("codex")), "codex", "the core defaults a conversation to claude");
    assert_eq!(engine_of(&t("qwen"), Some("claude")), "qwen", "-H names it");
    assert_eq!(engine_of(&t("claude"), None), "claude", "an agent whose file cannot be read");
    assert_eq!(engine_of(&json!({}), None), "claude");
}

#[test]
fn a_bg_ask_runs_once_and_the_asker_gets_one_answer_and_one_wake() {
    let core = Core::start();
    let dir = scratch();
    stub(&dir);
    let opened =
        core.open("a1", "do the thing", &json!({"model": "opus", "effort": "high", "folder": dir, "project": "kith"}));
    assert_eq!(core.queued().len(), 1, "{opened}");
    pull(&core, &dir, "claude");
    // The callee ran, as the Claude one-shot, in its folder, with the claim and what was asked.
    until("the ask is answered", || core.state("a1") == "answered");
    let ran = lines(&dir);
    assert_eq!(ran.len(), 1, "{ran:?}");
    let parts: Vec<&str> = ran[0].splitn(5, '|').collect();
    assert!(parts[0].parse::<i64>().is_ok_and(|c| c > 0), "the claim is in KITH_TURN_CLAIM: {ran:?}");
    assert_eq!(std::fs::canonicalize(parts[1]).unwrap(), std::fs::canonicalize(&dir).unwrap());
    assert_eq!(parts[2], "json");
    assert_eq!(parts[4], "echo -m opus -e high -p kith -- do the thing");
    // One answer row, owed to the asker's session once.
    let rows = core.rows("a1");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["text"], "hi there");
    // Pulled again: nothing is left to run, and the same turn is never started twice.
    pull(&core, &dir, "claude");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(lines(&dir).len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_agent_of_another_engine_starts_through_the_kernels_launch_with_the_claim() {
    let core = Core::start();
    let dir = scratch();
    stub(&dir);
    core.open("a2", "go", &json!({"engine": "codex"}));
    pull(&core, &dir, "claude");
    until("the callee ran", || !lines(&dir).is_empty());
    let ran = lines(&dir);
    let tail = ran[0].splitn(5, '|').nth(4).unwrap().to_owned();
    let kernel = dir.join("kernel");
    assert_eq!(
        tail,
        format!("kernel launch echo --exec --kernel {} -H codex -- go", kernel.display()),
        "the claim goes in KITH_TURN_CLAIM, the launch attaches it"
    );
    assert!(ran[0].split('|').next().unwrap().parse::<i64>().is_ok());
    until("the ask is answered", || core.state("a2") == "answered");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_end_mid_run_stops_the_child_and_nothing_reaches_the_asker() {
    let core = Core::start();
    let dir = scratch();
    stub(&dir);
    core.open("a3", "SLEEP", &json!({}));
    pull(&core, &dir, "claude");
    until("the callee is running", || {
        std::fs::read_to_string(dir.join("pid")).is_ok_and(|p| p.trim().parse::<i32>().is_ok())
    });
    let pid: i32 = std::fs::read_to_string(dir.join("pid")).unwrap().trim().parse().unwrap();
    assert!(alive(pid));
    core.call("ask.end", &json!({"roots": ["a3"]}));
    until("the waiter stopped the child", || !alive(pid));
    // The finish that follows leaves the ask as Stop made it, and nothing was owed.
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(core.state("a3"), "stopped");
    assert!(core.rows("a3").is_empty(), "StopWins");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_turn_that_cannot_start_is_cancelled_and_never_runs_unattached() {
    let core = Core::start();
    let dir = scratch();
    stub(&dir);
    core.open("a4", "go", &json!({"folder": "/no/such/folder/for/the/callee"}));
    pull(&core, &dir, "claude");
    until("the turn is gone from the queue", || core.queued().is_empty());
    assert!(lines(&dir).is_empty(), "nothing was started");
    // The tick settles the ask: its turn ended with no answer.
    assert!(matches!(call_host(&core.folders, "wake.tick", &json!({})), Answer::Done(_)));
    assert_eq!(core.state("a4"), "lost");
    let _ = std::fs::remove_dir_all(&dir);
}
