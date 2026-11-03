// Adapted from herdr-core b63ecbd (crates/herdr-core/src/client.rs, starting a holder that nothing answers).
// Copyright herdrdev and the herdr-core contributors, Apache-2.0. See NOTICE.
//
//! Starting a holder when none answers: in its own session on Unix, out of its parent's job on Windows
//! (CREATE_BREAKAWAY_FROM_JOB), so it outlives the daemon that started it. When the starter's job forbids breakaway
//! the holder stays in it and says so in its welcome (`tied_to_starter`).

use crate::platform;
use std::io::{self, BufRead, Read};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

/// Reads the secret a holder (or `start`) is given: one line on stdin. It never travels in the environment or the
/// command line, which any process of this user can read for as long as the holder lives (`ps -E`,
/// /proc/PID/environ); a pipe is gone once it is read.
pub fn read_secret(mut input: impl io::BufRead) -> io::Result<String> {
    let mut line = String::new();
    // A secret is short; a peer that never ends its line cannot make the holder buffer without bound.
    input.by_ref().take(4096).read_line(&mut line)?;
    let secret = line.trim_end_matches(['\r', '\n']);
    if secret.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "no secret on stdin",
        ));
    }
    Ok(secret.to_string())
}

/// Makes sure a holder answers in `dir`, starting `exe serve` detached if none does. A holder that already runs
/// keeps its own secret: the caller finds out at hello.
pub fn ensure(exe: &Path, dir: &Path, secret: &str, idle: Duration) -> io::Result<()> {
    if secret.contains(['\r', '\n']) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the holder secret cannot hold a line break",
        ));
    }
    platform::private_dir(dir)?;
    let endpoint = platform::endpoint(dir);
    if platform::connect(&endpoint).is_ok() {
        return Ok(());
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("holder.log"))?;
    let mut cmd = Command::new(exe);
    cmd.arg("serve")
        .arg("--dir")
        .arg(dir)
        .arg("--idle-secs")
        .arg(idle.as_secs().to_string());
    // The secret goes down an inherited stdin pipe, never the environment.
    platform::spawn_detached(cmd, log, format!("{secret}\n").as_bytes())?;
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if platform::connect(&endpoint).is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "the holder did not start (see holder.log in its folder)",
    ))
}
