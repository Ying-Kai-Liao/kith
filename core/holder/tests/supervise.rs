//! `kith-holder supervise` against the real binary: the holder answers while it keeps its child running, and starts
//! the child again after it dies. Unix only (the child is `sh`).
#![cfg(unix)]

use kith_holder::client::Client;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const SECRET: &str = "0123456789abcdef0123456789abcdef";
const EXE: &str = env!("CARGO_BIN_EXE_kith-holder");

struct Fixture {
    dir: PathBuf,
    holder: Child,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.holder.kill();
        let _ = self.holder.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn runs(log: &PathBuf) -> usize {
    std::fs::read_to_string(log).map_or(0, |t| t.lines().count())
}

#[test]
fn the_holder_answers_and_starts_its_child_again_when_it_exits() {
    let dir = std::env::temp_dir().join(format!("kh-{}-sup", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (secret, log) = (dir.join("secret"), dir.join("runs"));
    std::fs::write(&secret, SECRET).unwrap();
    let holder = Command::new(EXE)
        .args(["supervise", "--dir"])
        .arg(dir.join("h"))
        .arg("--secret-file")
        .arg(&secret)
        .args(["--backoff-ms", "20:40", "--", "/bin/sh", "-c"])
        // The child dies at once, every time.
        .arg(format!("echo run >> '{}'", log.display()))
        .stdin(Stdio::null())
        .spawn()
        .expect("the holder starts");
    let fixture = Fixture {
        dir: dir.clone(),
        holder,
    };
    let deadline = Instant::now() + Duration::from_secs(20);
    while runs(&log) < 3 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        runs(&log) >= 3,
        "the child was started {} times",
        runs(&log)
    );
    // The holder served the whole time.
    Client::connect(&dir.join("h"), SECRET, 1).expect("the holder answers its hello");
    drop(fixture);
}

fn supervise(dir: &std::path::Path,child_script: &str) -> Child {
    Command::new(EXE)
        .args(["supervise", "--dir"])
        .arg(dir.join("h"))
        .arg("--secret-file")
        .arg(dir.join("secret"))
        .args(["--backoff-ms", "50:200", "--", "/bin/sh", "-c"])
        .arg(child_script)
        .stdin(Stdio::null())
        .spawn()
        .expect("the holder starts")
}

#[test]
fn a_holder_that_finds_the_folder_taken_keeps_its_core_running_and_takes_the_folder_when_it_frees()
{
    let dir = std::env::temp_dir().join(format!("kh-{}-sup2", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("secret"), SECRET).unwrap();
    let log = dir.join("runs");
    // The first supervisor owns the folder (as the core's detached holder does).
    let first = supervise(&dir, "sleep 60");
    let mut fixture = Fixture {
        dir: dir.clone(),
        holder: first,
    };
    let deadline = Instant::now() + Duration::from_secs(20);
    while Client::connect(&dir.join("h"), SECRET, 1).is_err() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    // The second finds it taken: its core still starts and keeps running, and the process does not exit.
    let mut second = supervise(&dir, &format!("echo run >> '{}'; sleep 60", log.display()));
    let deadline = Instant::now() + Duration::from_secs(20);
    while runs(&log) < 1 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(runs(&log), 1, "the core was not started");
    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        second.try_wait().unwrap().is_none(),
        "the second holder exited"
    );
    assert_eq!(runs(&log), 1, "the core was restarted");
    // The first goes away: the second takes the folder and answers.
    let _ = fixture.holder.kill();
    let _ = fixture.holder.wait();
    let deadline = Instant::now() + Duration::from_secs(20);
    while Client::connect(&dir.join("h"), SECRET, 1).is_err() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    Client::connect(&dir.join("h"), SECRET, 1).expect("the second holder took the folder");
    assert!(second.try_wait().unwrap().is_none());
    assert_eq!(runs(&log), 1, "the core was restarted");
    let _ = second.kill();
    let _ = second.wait();
}
