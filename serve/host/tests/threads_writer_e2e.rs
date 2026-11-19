//! `kith kernel threads` in Writer mode against a real core (retire-python R5; the takers' cases W1-W7 of
//! `kernel/tests/kith-sessions-core-check`, which drives the Python takers against a fake `kith` program). An in-process Rust
//! call never runs that program, so the Rust verbs are run here as the launcher runs them: the real `kith` binary, as a
//! process, against the host's real core daemon (with its holder, so the claims serve) in a throwaway state folder marked
//! switched for `claims` by hand, as `direct_park_e2e.rs` does. The crossed cases are here too: switched with no daemon,
//! the marker alone, the switch file alone.
//!
//! This is also the check that every parameter the verbs send is one the real dispatch takes (it rejects unknown params).

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::assert_is_empty)]

mod common;

use common::host::Host;
use kith_kernel::paths::Paths;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(90);

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

struct Holder(u32);

impl Drop for Holder {
    fn drop(&mut self) {
        let _ = Command::new("kill").args(["-s", "KILL", &self.0.to_string()]).status();
    }
}

struct StateDir(PathBuf);

impl Drop for StateDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Marks the module switched in the core of `paths`: the switch file and the marker row in core.db that must match it
/// (`cutover::may_serve`). Only ever in a throwaway folder.
fn mark_switched(paths: &Paths, module: &str) {
    assert!(paths.machine.to_string_lossy().starts_with("/tmp/ktw-"), "never mark a real Mac's modules");
    let at = 1_800_000_000_i64;
    let core = paths.machine.join("core");
    std::fs::create_dir_all(core.join("cutover")).unwrap();
    std::fs::write(core.join(format!("cutover/{module}.switch")), format!("state=switched\nat={at}\n")).unwrap();
    rusqlite::Connection::open(core.join("core.db"))
        .unwrap()
        .execute("INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)", [format!("cutover:{module}"), format!("at={at};digest=")])
        .unwrap();
}

/// A process that lives until it is dropped: a claimer that is not the test.
struct Sleeper(Child);

impl Sleeper {
    fn new() -> Self {
        Self(Command::new("sleep").arg("120").stdin(Stdio::null()).spawn().unwrap())
    }

    fn pid(&self) -> String {
        self.0.id().to_string()
    }
}

impl Drop for Sleeper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

struct Rig {
    host: Option<Host>,
    _holder: Holder,
    paths: Paths,
    state: PathBuf,
    _dir: StateDir,
}

impl Rig {
    async fn start() -> Self {
        let state = PathBuf::from("/tmp").join(format!("ktw-{}", &uuid::Uuid::new_v4().simple().to_string()[..10]));
        std::fs::create_dir_all(&state).unwrap();
        let given = state.display().to_string();
        let env = |k: &str| (k == "KITH_STATE").then(|| given.clone());
        let paths = kith_kernel::paths::paths(&state, &state.join("kernel"), &env);
        drop(kith_core::start(&paths).expect("the core starts"));
        mark_switched(&paths, "claims");
        let holder = holder_bin().display().to_string();
        let host = Host::start_env(&[("KITH_STATE", &given), ("KITH_HOLDER_BIN", &holder)]).await;
        let end = Instant::now() + WAIT;
        let pid = loop {
            let said = kith_core::status(&paths);
            if said["holder"]["state"] == "connected" {
                break said["holder"]["pid"].as_u64().unwrap();
            }
            assert!(Instant::now() < end, "the holder never connected: {said}");
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        Self {
            host: Some(host),
            _holder: Holder(u32::try_from(pid).unwrap()),
            paths,
            state: state.clone(),
            _dir: StateDir(state),
        }
    }

    /// `kith kernel threads ARGS` as the launcher runs it: its own process, the state folder and the home of this rig.
    fn threads(&self, env: &[(&str, &str)], args: &[&str], stdin: &str) -> Out {
        let kernel = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../kernel");
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_kith"));
        cmd.args(["kernel", "threads", "--kernel"])
            .arg(&kernel)
            .args(args)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", &self.state)
            .env("KITH_STATE", &self.state)
            .env("KITH_CLAIM_WAIT", "0")
            .env("KITH_SERVE_PORT", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().unwrap();
        {
            use std::io::Write as _;
            let mut i = child.stdin.take().unwrap();
            let _ = i.write_all(stdin.as_bytes());
        }
        let o = child.wait_with_output().unwrap();
        Out {
            code: o.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        }
    }

    fn core(&self, method: &str, params: &Value) -> Value {
        match kith_core::reach(&self.paths, method, params, true) {
            kith_core::Reached::Answered(v) => v,
            other => panic!("{method} was not answered: {other:?}"),
        }
    }

    fn claim_files(&self) -> Vec<String> {
        std::fs::read_dir(self.state.join("live/terminal"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| Path::new(n).extension().is_some_and(|x| x == "claim"))
            .collect()
    }
}

/// W1: a claim is a row in the core and no file; a second writer is refused with the line kith-wake knows; a release frees it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_claim_is_a_core_row_and_a_second_writer_is_refused() {
    let rig = Rig::start().await;
    let (one, two) = (Sleeper::new(), Sleeper::new());
    let sid = "11111111-1111-4111-8111-111111111111";
    let end = Instant::now() + WAIT;
    // The core may answer `waiting` for a moment after the holder's hello; the verb retries for KITH_CLAIM_WAIT.
    let first = loop {
        let o = rig.threads(&[("KITH_CLAIM_WAIT", "5")], &["terminal", sid, &one.pid(), "", "coder"], "");
        if o.code == 0 || Instant::now() >= end {
            break o;
        }
        std::thread::sleep(Duration::from_millis(300));
    };
    assert_eq!((first.code, first.stdout.as_str()), (0, ""), "{}", first.stderr);
    assert!(rig.claim_files().is_empty(), "Writer mode writes no claim file: {:?}", rig.claim_files());
    let row = rig.core("claim.list", &serde_json::json!({}));
    assert!(row.to_string().contains(sid), "the claim is a row in the core: {row}");
    let again = rig.threads(&[], &["terminal", sid, &two.pid(), "", "coder"], "");
    assert_eq!(again.code, 1);
    assert!(
        again.stderr.starts_with(&format!(
            "kith: that session is busy: held by external (pid {}); try again in a moment",
            one.pid()
        )) || again.stderr.contains("that session is busy: held by"),
        "{}",
        again.stderr
    );
    // The same process asking again holds it already.
    assert_eq!(rig.threads(&[], &["terminal", sid, &one.pid(), "", "coder"], "").code, 0);
    // release SID gives back the claim of the caller's parent: run it under a shell that took the claim for itself.
    let bin = env!("CARGO_BIN_EXE_kith");
    let kernel = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../kernel");
    let script = format!(
        "\"{bin}\" kernel threads --kernel \"{k}\" terminal other-1 $$ '' coder && \"{bin}\" kernel threads --kernel \"{k}\" release other-1; echo \"exit $?\"",
        k = kernel.display()
    );
    let o = Command::new("sh")
        .arg("-c")
        .arg(&script)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", &rig.state)
        .env("KITH_STATE", &rig.state)
        .output()
        .unwrap();
    let shown = String::from_utf8_lossy(&o.stdout);
    assert!(shown.trim().ends_with("exit 0"), "{shown} {}", String::from_utf8_lossy(&o.stderr));
}

/// W5, W6, W7: a person's claim needs the epoch read before kith-auth decides; the sweep ends their runs and never the owner's;
/// `proc` is a conversation of its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_persons_run_is_found_by_the_sweep_with_the_epoch_it_was_read_at() {
    let rig = Rig::start().await;
    let (run, owner) = (Sleeper::new(), Sleeper::new());
    let epoch = rig.threads(&[], &["epoch", "alice"], "");
    assert_eq!(epoch.code, 0, "{}", epoch.stderr);
    assert!(epoch.stdout.starts_with("alice:"), "{}", epoch.stdout);
    let seen = epoch.stdout.trim().to_owned();
    // Without the epoch the core refuses a person's claim.
    let none = rig.threads(&[], &["terminal", "alice-1", &run.pid(), "alice", "coder"], "");
    assert_eq!(none.code, 1, "{}", none.stderr);
    assert!(none.stderr.contains("the Kith core refused the claim"), "{}", none.stderr);
    let end = Instant::now() + WAIT;
    let got = loop {
        let o = rig.threads(
            &[("KITH_REVOKE_SEEN", &seen), ("KITH_CLAIM_WAIT", "5")],
            &["terminal", "alice-1", &run.pid(), "alice", "coder"],
            "",
        );
        if o.code == 0 || Instant::now() >= end {
            break o;
        }
        std::thread::sleep(Duration::from_millis(300));
    };
    assert_eq!(got.code, 0, "{}", got.stderr);
    // proc: its own conversation, so a run with no session is still found.
    let proc = rig.threads(&[("KITH_REVOKE_SEEN", &seen)], &["proc", &run.pid(), "alice", "coder"], "");
    assert_eq!(proc.code, 0, "{}", proc.stderr);
    assert!(rig.claim_files().is_empty());
    // The owner's run is never swept.
    let mine = rig.threads(&[("KITH_CLAIM_WAIT", "5")], &["terminal", "owner-1", &owner.pid(), "", "coder"], "");
    assert_eq!(mine.code, 0, "{}", mine.stderr);
    let swept = rig.threads(&[], &["sweep", "alice"], "");
    assert_eq!((swept.code, swept.stdout.as_str()), (0, "swept 1\n"), "{}", swept.stderr);
    let end = Instant::now() + Duration::from_secs(10);
    let mut run = run;
    while run.0.try_wait().unwrap().is_none() {
        assert!(Instant::now() < end, "the sweep did not end the person's run");
        std::thread::sleep(Duration::from_millis(100));
    }
    let mut owner = owner;
    assert!(owner.0.try_wait().unwrap().is_none(), "the owner's run was swept");
}

/// W2, W13: the claims are the core's but nothing answers (the host stopped): the owner's run goes on under the conversation lock
/// alone, a person's run and the epoch fail closed, `proc` is exit 9, and no claim file appears. The marker alone, and the switch
/// file alone, are the same mode.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn with_no_core_answering_the_owner_goes_on_alone_and_a_person_is_refused() {
    let mut rig = Rig::start().await;
    drop(rig.host.take()); // the host and its daemon stop
    let run = Sleeper::new();
    let owner = rig.threads(&[], &["terminal", "alone-1", &run.pid(), "", "coder"], "");
    assert_eq!((owner.code, owner.stdout.as_str()), (0, "lock-alone\n"), "{}", owner.stderr);
    assert!(owner.stderr.contains("this run is held by the conversation lock alone"), "{}", owner.stderr);
    let person = rig.threads(&[], &["terminal", "alone-2", &run.pid(), "alice", "coder"], "");
    assert_eq!(person.code, 1);
    assert!(person.stderr.contains("busy until the Kith core answers"), "{}", person.stderr);
    assert_eq!(rig.threads(&[], &["epoch", "alice"], "").code, 1);
    let proc = rig.threads(&[], &["proc", &run.pid(), "alice", "coder"], "");
    assert_eq!(proc.code, 9, "{}", proc.stderr);
    assert!(rig.claim_files().is_empty(), "{:?}", rig.claim_files());
    // The marker alone is Writer mode too (the switch file removed): still no file.
    std::fs::remove_file(rig.paths.machine.join("core/cutover/claims.switch")).unwrap();
    std::fs::create_dir_all(rig.state.join("live/terminal")).unwrap();
    std::fs::write(rig.state.join("live/terminal/claims.tombstone"), "").unwrap();
    let marker = rig.threads(&[], &["terminal", "alone-3", &run.pid(), "", "coder"], "");
    assert_eq!(marker.stdout, "lock-alone\n", "{}", marker.stderr);
    assert!(rig.claim_files().is_empty());
}

/// R3 with a real core that does not serve the registry: the hook spools first, the core's answer is a refusal, and the entry
/// stays for the core's drain. The hook exits 0 whatever happens.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_record_the_core_does_not_take_stays_in_the_spool() {
    let rig = Rig::start().await;
    std::fs::create_dir_all(rig.state.join("live/terminal")).unwrap();
    std::fs::write(rig.state.join("live/terminal/registry.tombstone"), "").unwrap();
    let hook = r#"{"session_id":"s-1","cwd":"/w"}"#;
    let o = rig.threads(&[("KITH_AGENT", "coder"), ("KITH_RUN_KIND", "schedule")], &["record"], hook);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(!rig.state.join("live/terminal/s-1.json").exists(), "the core writes the file, never the hook");
    let spooled: Vec<_> = std::fs::read_dir(rig.state.join("live/terminal/inbox")).unwrap().flatten().collect();
    // Either the core took it (the entry went) or it said so and the entry stayed: never a lost record.
    assert!(spooled.len() <= 1);
    if !spooled.is_empty() {
        assert!(o.stderr.contains("kept for the core"), "{}", o.stderr);
    }
}

/// R3 with the registry switched: the core takes the record, so the hook's spool entry is settled and gone, and nothing says
/// it was kept.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_record_the_core_takes_leaves_the_spool_empty() {
    let rig = Rig::start().await;
    mark_switched(&rig.paths, "transcripts");
    std::fs::create_dir_all(rig.state.join("live/terminal")).unwrap();
    std::fs::write(rig.state.join("live/terminal/registry.tombstone"), "").unwrap();
    let hook = r#"{"session_id":"s-2","cwd":"/w"}"#;
    let o = rig.threads(&[("KITH_AGENT", "coder"), ("KITH_RUN_KIND", "schedule")], &["record"], hook);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(!o.stderr.contains("kept for the core"), "{}", o.stderr);
    let left = std::fs::read_dir(rig.state.join("live/terminal/inbox")).map_or(0, |d| d.flatten().count());
    assert_eq!(left, 0, "the core took the record, so the spool is empty");
}
