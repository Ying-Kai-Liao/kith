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
