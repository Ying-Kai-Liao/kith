//! The daemon front on real processes: `kith kernel core-daemon` (the daemon `kith serve` starts inside itself), the real
//! `kith` CLI asking it, and a real `kith-holder` it starts detached. The in-process cases (token, hello, lock, fake
//! holders) are in `core/runtime/src/daemon_tests.rs`.
#![cfg(unix)]

use std::io::{BufRead as _, BufReader, Read as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

/// A short folder: a Unix socket path holds about a hundred bytes.
fn sandbox(tag: &str) -> PathBuf {
    let d = PathBuf::from("/tmp").join(format!("kdc-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("kith")).expect("sandbox");
    d.canonicalize().expect("canonical")
}

fn kith(home: &Path, extra_env: &[(&str, &str)]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_kith"));
    c.env_clear()
        .env("HOME", home)
        .env("KITH_HOME", home.join("kith"))
        .env("PATH", "/usr/bin:/bin")
        // No holder unless a test asks for one: a stray one would outlive it.
        .env("KITH_HOLDER_BIN", home.join("no-holder"));
    for (k, v) in extra_env {
        c.env(k, v);
    }
    c
}

fn machine(home: &Path) -> PathBuf {
    let out = kith(home, &[]).args(["paths", "--json"]).output().expect("paths");
    let v: Value = serde_json::from_slice(&out.stdout).expect("json");
    PathBuf::from(v["KITH_MACHINE"].as_str().expect("machine"))
}

#[derive(Debug)]
struct Daemon {
    child: Child,
    home: PathBuf,
}

impl Daemon {
    /// Starts the daemon and waits for its `ready` line; `Err` is what it printed instead.
    fn start(home: &Path, env: &[(&str, &str)]) -> Result<Daemon, String> {
        let mut child = kith(home, env)
            .args(["kernel", "core-daemon"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn");
        let mut line = String::new();
        BufReader::new(child.stdout.as_mut().expect("stdout")).read_line(&mut line).expect("ready line");
        if line.trim() != "ready" {
            let mut err = String::new();
            let _ = child.stderr.take().map(|mut e| e.read_to_string(&mut err));
            let _ = child.wait();
            return Err(format!("{line:?} {err}"));
        }
        Ok(Daemon { child, home: home.to_owned() })
    }

    /// Closes its stdin, the way it is stopped; returns the number of calls it served.
    fn stop(mut self) -> u64 {
        drop(self.child.stdin.take());
        let mut out = String::new();
        self.child.stdout.take().expect("stdout").read_to_string(&mut out).expect("out");
        assert!(self.child.wait().expect("wait").success(), "the daemon ends cleanly");
        out.lines().find_map(|l| l.strip_prefix("served=")).expect("served= line").parse().expect("number")
    }

    fn status(&self) -> Value {
        status(&self.home)
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn status(home: &Path) -> Value {
    let out = kith(home, &[]).args(["kernel", "core-status", "--json"]).output().expect("status");
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)))
}

fn until(what: &str, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(40);
    while Instant::now() < end {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("gave up waiting for {what}");
}

/// The built holder program, next to `kith`: built on demand when only the host's tests were built.
fn holder_bin() -> PathBuf {
    let beside = Path::new(env!("CARGO_BIN_EXE_kith")).parent().expect("dir").join("kith-holder");
    if !beside.is_file() {
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let built = Command::new(cargo)
            .args(["build", "--manifest-path", concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"), "-p", "kith-holder"])
            .status()
            .expect("cargo build");
        assert!(built.success() && beside.is_file(), "kith-holder is not built");
    }
    beside
}

fn pid_alive(pid: u64) -> bool {
    Command::new("kill").args(["-s", "0", &pid.to_string()]).output().is_ok_and(|o| o.status.success())
}

#[test]
fn the_cli_goes_to_a_real_daemon_with_its_own_environment_and_falls_back_when_there_is_none() {
    let home = sandbox("cli");
    let m = machine(&home);
    std::fs::create_dir_all(&m).expect("machine");

    // No daemon: embedded or bare, exactly as before.
    let alone = kith(&home, &[]).args(["paths", "--json"]).output().expect("paths");
    assert!(alone.status.success());

    let daemon = Daemon::start(&home, &[]).expect("daemon");
    // The daemon's environment is not the CLI's: the CLI here has another KITH_HOME and gets its own answer back.
    let theirs = kith(&home, &[("KITH_HOME", "/caller/view/kith")]).args(["paths", "--json"]).output().expect("paths");
    assert!(theirs.status.success(), "{}", String::from_utf8_lossy(&theirs.stderr));
    let seen: Value = serde_json::from_slice(&theirs.stdout).expect("json");
    assert_eq!(seen["KITH_HOME"], "/caller/view/kith");
    assert_eq!(seen["KITH_MACHINE"], m.to_string_lossy().as_ref());
    // The text and shell forms come out of the same answer.
    for form in ["--sh", ""] {
        let args: Vec<&str> = ["paths", form].into_iter().filter(|a| !a.is_empty()).collect();
        let via = kith(&home, &[]).args(&args).output().expect("form");
        assert!(via.status.success());
        assert!(String::from_utf8_lossy(&via.stdout).contains("KITH_MACHINE"));
    }
    let version = kith(&home, &[]).args(["kernel", "version"]).output().expect("version");
    assert!(String::from_utf8_lossy(&version.stdout).starts_with("{\"abi\":"));
    // A verb the daemon does not answer yet still runs, in the CLI itself.
    assert!(kith(&home, &[]).args(["kernel", "engines", "--json"]).output().expect("engines").status.success());

    // Owner-only: the folder, the socket and the token.
    let mode = |p: &Path| std::fs::metadata(p).expect("exists").permissions().mode() & 0o777;
    assert_eq!(mode(&m.join("core")), 0o700);
    assert_eq!(mode(&m.join("core/owner.sock")), 0o600);
    assert_eq!(mode(&m.join("core/owner.token")), 0o600);

    // paths (json), paths (--sh), paths: through the socket. `kernel version` is the router's per-binary question and is
    // never the daemon's to answer; engines is not served; the first `paths` before it was asked ahead of a daemon.
    assert_eq!(daemon.stop(), 3);
    let after = kith(&home, &[]).args(["paths", "--json"]).output().expect("paths");
    assert_eq!(after.stdout, alone.stdout, "a stopped daemon leaves the CLI as it was");
    assert!(!m.join("core/owner.sock").exists());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_restarted_daemon_is_a_newer_generation_and_waits_for_a_running_command() {
    let home = sandbox("gen");
    let m = machine(&home);
    std::fs::create_dir_all(&m).expect("machine");

    let first = Daemon::start(&home, &[]).expect("first");
    let g1 = first.status()["daemon"]["generation"].as_i64().expect("generation");
    // A second daemon is refused at once (one lock), with the recovery named.
    let second = Daemon::start(&home, &[]).expect_err("a second daemon");
    assert!(second.contains("holds the lock"), "{second}");
    first.stop();
    let again = Daemon::start(&home, &[]).expect("again");
    assert!(again.status()["daemon"]["generation"].as_i64().expect("generation") > g1);
    again.stop();

    // An embedded command holds the lock while the daemon starts: it waits instead of failing.
    let p = kith_kernel::paths::paths(&home, &home.join("kith/kernel"), &|k: &str| match k {
        "KITH_HOME" => Some(home.join("kith").to_string_lossy().into_owned()),
        _ => None,
    });
    let held = match kith_core::open_embedded(&p).expect("open") {
        kith_core::Embedded::Open(c) => c,
        kith_core::Embedded::DaemonRunning { .. } => panic!("free"),
    };
    let started = std::thread::spawn({
        let home = home.clone();
        move || Daemon::start(&home, &[]).map(Daemon::stop)
    });
    std::thread::sleep(Duration::from_millis(600));
    assert!(!started.is_finished(), "the daemon is waiting for the command");
    drop(held);
    started.join().expect("join").expect("it took over once the command was done");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn no_holder_program_and_a_holder_killed_mid_run_stop_neither_the_daemon_nor_the_cli() {
    let home = sandbox("holder");
    let m = machine(&home);
    std::fs::create_dir_all(&m).expect("machine");

    // The program is not there at all.
    let daemon = Daemon::start(&home, &[]).expect("daemon without a holder");
    until("the holder to be reported absent", || daemon.status()["holder"]["state"] == "absent");
    assert!(kith(&home, &[]).args(["paths", "--json"]).output().expect("paths").status.success());
    daemon.stop();

    // A real holder: started detached by the daemon, said hello to, its instance recorded.
    let holder = holder_bin();
    let env = [("KITH_HOLDER_BIN", holder.to_str().expect("utf-8"))];
    let daemon = Daemon::start(&home, &env).expect("daemon");
    until("the holder to be connected", || daemon.status()["holder"]["state"] == "connected");
    let first = daemon.status()["holder"].clone();
    let (instance, pid) =
        (first["instance"].as_str().expect("instance").to_owned(), first["pid"].as_u64().expect("pid"));
    assert!(pid_alive(pid));
    let startup = Instant::now();
    assert!(kith(&home, &[]).args(["paths", "--json"]).output().expect("paths").status.success());
    assert!(startup.elapsed() < Duration::from_secs(5));

    // The holder crashes: the daemon and the CLI go on, and the daemon starts another and records the new instance.
    assert!(Command::new("kill").args(["-s", "KILL", &pid.to_string()]).status().expect("kill").success());
    assert!(kith(&home, &[]).args(["paths", "--json"]).output().expect("paths").status.success());
    until("a new holder instance", || {
        let h = daemon.status()["holder"].clone();
        h["state"] == "connected" && h["instance"].as_str().is_some_and(|i| i != instance)
    });
    let second = daemon.status()["holder"].clone();
    let new_pid = second["pid"].as_u64().expect("pid");
    assert_ne!(new_pid, pid);
    assert_eq!(second["tied_to_starter"], false);

    // Stopping the daemon leaves the holder to idle out; end it here so the test leaves nothing behind.
    daemon.stop();
    let _ = Command::new("kill").args(["-s", "KILL", &new_pid.to_string()]).status();
    let _ = std::fs::remove_dir_all(&home);
}
