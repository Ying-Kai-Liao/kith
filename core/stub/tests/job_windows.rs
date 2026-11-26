//! `kith-stub` on Windows holds its engine in a kill-on-close job (docs/plans/windows-owner.md, W12 rules H1, H2, H5, H7): a
//! stub terminated hard takes its engine and the engine's child with it, and a stub whose engine has ended waits for what the
//! engine left running.
#![cfg(windows)]
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const STUB: &str = env!("CARGO_BIN_EXE_kith-stub");
const ROLE: &str = "KITH_STUB_JOB_ROLE";
const DIR: &str = "KITH_STUB_JOB_DIR";

/// Not a test of its own: the entry point of the engine and its child, which these tests start by running this binary again.
/// Without a role it does nothing.
#[test]
fn engine_role() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    let dir = PathBuf::from(std::env::var(DIR).unwrap());
    match role.as_str() {
        "engine" | "engine-exits" => {
            std::fs::write(dir.join("engine"), std::process::id().to_string()).unwrap();
            #[allow(clippy::zombie_processes)]
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "engine_role", "--nocapture"])
                .env(ROLE, "leaf")
                .spawn()
                .unwrap();
            if role == "engine" {
                let _ = child.wait();
            }
            // "engine-exits": this process ends at once and leaves its child running.
        }
        _ => {
            std::fs::write(dir.join("leaf"), std::process::id().to_string()).unwrap();
            std::thread::sleep(Duration::from_secs(8));
        }
    }
}

fn pid_in(dir: &Path, name: &str) -> u32 {
    let t = Instant::now();
    loop {
        if let Ok(s) = std::fs::read_to_string(dir.join(name))
            && let Ok(pid) = s.trim().parse()
        {
            return pid;
        }
        assert!(
            t.elapsed() < Duration::from_secs(60),
            "{name} never started"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn gone(pid: u32) -> bool {
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(20) {
        if kith_procstart::probe(pid) == kith_procstart::Probe::Gone {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn stub(dir: &Path, role: &str) -> std::process::Child {
    let locks = dir.join("locks");
    Command::new(STUB)
        .arg("--lock-dir")
        .arg(&locks)
        .args(["--key", "job", "--"])
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", "engine_role", "--nocapture"])
        .env(ROLE, role)
        .env(DIR, dir)
        .spawn()
        .unwrap()
}

#[test]
fn a_stub_that_is_terminated_takes_its_engine_and_the_engines_child_with_it() {
    let dir = std::env::temp_dir().join(format!("kith-stub-job-kill-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut stub = stub(&dir, "engine");
    let (engine, leaf) = (pid_in(&dir, "engine"), pid_in(&dir, "leaf"));
    assert_ne!(kith_procstart::probe(engine), kith_procstart::Probe::Gone);
    assert_ne!(kith_procstart::probe(leaf), kith_procstart::Probe::Gone);
    // A hard kill: nothing in the stub runs again, so only the job's close can end the rest.
    stub.kill().unwrap();
    stub.wait().unwrap();
    assert!(gone(engine), "the engine outlived its stub");
    assert!(gone(leaf), "the engine's child outlived its stub");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_stub_outlives_what_its_engine_left_running() {
    let dir = std::env::temp_dir().join(format!("kith-stub-job-wait-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let started = Instant::now();
    let status = stub(&dir, "engine-exits").wait().unwrap();
    assert_eq!(status.code(), Some(0));
    let leaf = pid_in(&dir, "leaf");
    // The stub ended only after the leaf had.
    assert!(gone(leaf));
    assert!(
        started.elapsed() >= Duration::from_secs(7),
        "the stub ended while the engine's child was still running"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
