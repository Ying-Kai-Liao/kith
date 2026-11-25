//! Running a program with a time limit: portable, so the Windows build of the crate has it too.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Runs a command the caller has set up (its environment, say): stdin, stdout and stderr are set
/// here. `None` when it can't start, outlives `limit` (it is killed), or fails.
pub fn run_command_with_timeout(mut cmd: Command, limit: Duration) -> Option<String> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut stdout, &mut s);
        s
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if start.elapsed() < limit => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let out = reader.join().ok()?;
    status.success().then_some(out)
}
