//! `kith-serve serve --supervise` for real: the supervisor starts the host as its child, passes signals on, and holds
//! nothing the host needs. (The supervisor's finer points, with a fake child, are unit tests in `supervise.rs`.)

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[allow(dead_code)]
mod common;

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// A supervised host on disposable data; the host child is killed on drop even when the supervisor is gone.
struct Supervised {
    supervisor: Child,
    host_pid: String,
    home: PathBuf,
}

impl Drop for Supervised {
    fn drop(&mut self) {
        let _ = Command::new("kill").args(["-9", &self.host_pid]).output();
        let _ = self.supervisor.kill();
        let _ = self.supervisor.wait();
        let _ = std::fs::remove_dir_all(&self.home);
        common::forget_home(&self.home);
    }
}

fn children_of(pid: u32) -> Vec<String> {
    let out = Command::new("pgrep").args(["-P", &pid.to_string()]).output().expect("pgrep");
    String::from_utf8_lossy(&out.stdout).split_whitespace().map(str::to_owned).collect()
}

fn pgid_of(pid: &str) -> String {
    let out = Command::new("ps").args(["-o", "pgid=", "-p", pid]).output().expect("ps");
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn alive(pid: &str) -> bool {
    Command::new("kill").args(["-0", pid]).output().is_ok_and(|o| o.status.success())
}

fn wait_gone(pid: &str, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while alive(pid) {
        assert!(Instant::now() < deadline, "{what} (pid {pid}) is still running");
        std::thread::sleep(Duration::from_millis(50));
    }
}

async fn start() -> Supervised {
    let home = std::env::temp_dir().join(format!("kith-supervise-e2e-{}", uuid::Uuid::new_v4()));
    let agents = common::agents_env(&home);
    let vars: Vec<(&str, &str)> = agents.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let (supervisor, _base) = common::serve_with(&home, &["--supervise"], &vars).await;
    let children = children_of(supervisor.id());
    assert_eq!(children.len(), 1, "one supervisor, one host: {children:?}");
    Supervised { host_pid: children[0].clone(), supervisor, home }
}

#[tokio::test]
async fn the_host_runs_as_the_supervisors_only_child_and_in_its_group_and_holds_the_lock() {
    let s = start().await;
    assert_eq!(pgid_of(&s.host_pid), pgid_of(&s.supervisor.id().to_string()), "same process group");
    // The host, not the supervisor, took host.lock: had the supervisor held it, the host would not have started. A
    // second host for this data folder is refused while the pair runs.
    let again = Command::new(env!("CARGO_BIN_EXE_kith-serve"))
        .args(["serve", "--bind", "127.0.0.1", "--port", "0"])
        .env("KITH_SERVE_HOME", &s.home)
        .env("KITH_SERVE_CLOUD", "off")
        .env("KITH_KERNEL_BIN", "")
        .output()
        .unwrap();
    assert!(!again.status.success(), "a second host got host.lock");
    assert!(
        String::from_utf8_lossy(&again.stderr).contains("already running"),
        "{}",
        String::from_utf8_lossy(&again.stderr)
    );
    // One supervisor never starts a second host.
    assert_eq!(children_of(s.supervisor.id()), std::slice::from_ref(&s.host_pid));
}

#[tokio::test]
async fn term_reaches_the_host_and_both_end() {
    let mut s = start().await;
    let started = Instant::now();
    let _ = Command::new("kill").arg(s.supervisor.id().to_string()).output();
    let status = loop {
        if let Some(status) = s.supervisor.try_wait().unwrap() {
            break status;
        }
        assert!(started.elapsed() < Duration::from_secs(30), "the supervisor did not end");
        std::thread::sleep(Duration::from_millis(50));
    };
    // The host shut down on the forwarded TERM (a SIGKILL after the grace would give 137).
    assert_eq!(status.code(), Some(0), "{status:?}");
    wait_gone(&s.host_pid, "the host");
    // The next instance gets the lock at once.
    assert!(
        fs2::FileExt::try_lock_exclusive(
            &std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(s.home.join("host.lock"))
                .unwrap()
        )
        .is_ok()
    );
}

#[tokio::test]
async fn a_host_that_dies_ends_the_supervisor_with_128_plus_the_signal_and_nothing_restarts_it() {
    let mut s = start().await;
    let _ = Command::new("kill").args(["-9", &s.host_pid]).output();
    let started = Instant::now();
    let status = loop {
        if let Some(status) = s.supervisor.try_wait().unwrap() {
            break status;
        }
        assert!(started.elapsed() < Duration::from_secs(15), "the supervisor outlived its host");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(status.code(), Some(137));
    // Restarting is launchd's KeepAlive: no second host appears from the supervisor.
    std::thread::sleep(Duration::from_millis(500));
    assert!(!alive(&s.host_pid));
}
