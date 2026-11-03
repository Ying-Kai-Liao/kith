// Adapted from herdr-core b63ecbd (crates/herdr-core/src/client.rs, starting a holder that nothing answers).
// Copyright herdrdev and the herdr-core contributors, Apache-2.0. See NOTICE.
//
//! Starting a holder when none answers: in its own session on Unix, out of its parent's job on Windows
//! (CREATE_BREAKAWAY_FROM_JOB), so it outlives the daemon that started it.

use crate::platform;
use std::io;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

/// The environment variable the secret travels in. It is read once at start and removed.
pub const SECRET_VAR: &str = "KITH_HOLDER_SECRET";

/// Makes sure a holder answers in `dir`, starting `exe serve` detached if none does. A holder that already runs
/// keeps its own secret: the caller finds out at hello.
pub fn ensure(exe: &Path, dir: &Path, secret: &str, idle: Duration) -> io::Result<()> {
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
    cmd.env(SECRET_VAR, secret);
    platform::spawn_detached(cmd, log)?;
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
