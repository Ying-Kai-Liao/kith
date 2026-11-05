//! Running a program for the MCP tools: `subprocess.run(..., capture_output=True, text=True, timeout=...)`, and the one Python
//! helper the bug report and the doors still lean on.

use super::{Env, scrub};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// What a finished program printed.
pub struct Ran {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Why a program did not finish.
pub enum Failed {
    /// `OSError`: the text is `e.strerror` (`No such file or directory`).
    Os(String),
    Timeout,
}

/// Python's text mode: universal newlines, and a byte that is no UTF-8 stays visible.
fn text(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw)
        .replace("\r\n", "\n")
        .replace('\r', "\n")
}

/// `e.strerror` of an `io::Error`.
fn strerror(e: &std::io::Error) -> String {
    let s = e.to_string();
    match s.rfind(" (os error ") {
        Some(i) => s[..i].to_owned(),
        None => s,
    }
}

/// Reads a stream to its end on a thread of its own, so the caller can wait for it with a deadline.
fn drain<R: Read + Send + 'static>(s: Option<R>) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut s) = s {
            let _ = s.read_to_end(&mut buf);
        }
        let _ = tx.send(buf);
    });
    rx
}

/// Runs `cmd` with exactly the environment `env` holds, `stdin` on its standard input (nothing when `None`), and both output
/// streams captured. Everything, the output pipes after the child exits included, is bounded by `limit`: the child is killed when
/// it outlives it, and a program that handed its pipes to a grandchild gives `Timeout` rather than a hang.
pub fn run(cmd: Command, env: &Env, stdin: Option<&str>, limit: Duration) -> Result<Ran, Failed> {
    let mut cmd = cmd;
    cmd.env_clear()
        .envs(env.vars())
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| Failed::Os(strerror(&e)))?;
    let deadline = Instant::now() + limit;
    if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
        // a thread, so a child that never reads cannot hold the server; a child that exits early closes the pipe
        let bytes = text.as_bytes().to_vec();
        std::thread::spawn(move || {
            let _ = pipe.write_all(&bytes);
        });
    }
    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());
    let pid = child.id();
    let (tx, waited) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait());
    });
    let status = match waited.recv_timeout(limit) {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return Err(Failed::Os(strerror(&e))),
        Err(_) => {
            // the waiter thread reaps the child, so the pid is still ours
            crate::threads::os::signal(i64::from(pid), crate::threads::os::SIGKILL);
            return Err(Failed::Timeout);
        }
    };
    let rest = |rx: &mpsc::Receiver<Vec<u8>>| {
        rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| Failed::Timeout)
    };
    let stdout = rest(&out)?;
    let stderr = rest(&err)?;
    Ok(Ran {
        code: status.code().unwrap_or(-1),
        stdout: text(&stdout),
        stderr: text(&stderr),
    })
}

/// The interpreter of the Python helpers, by absolute path, as the launch picks it for the read fence hook
/// (`launch::settings::python3_abs`): the first executable `python3` in an absolute directory of the PATH the run has after
/// `guest_scrub` (so none in the guest's folders), else `/usr/bin/python3`. A bare `python3` would be searched on the run's own
/// PATH, which a guest can point at a program of their own.
pub(super) fn interpreter(env: &Env, kernel: &Path) -> String {
    use std::os::unix::fs::PermissionsExt as _;
    if !env.python.is_empty() {
        return env.python.clone();
    }
    let path = match env.get("KITH_PERSON") {
        Some(person) => scrub::guest_scrub(env, kernel, person).0,
        None => env.clone(),
    };
    path.get("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(p).collect::<Vec<_>>())
        .filter(|d| d.is_absolute())
        .map(|d| d.join("python3"))
        .find(|c| {
            std::fs::metadata(c).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
        .map_or_else(
            || "/usr/bin/python3".to_owned(),
            |c| c.to_string_lossy().into_owned(),
        )
}

/// Runs `python3 -I -c SCRIPT KERNEL/lib` in this environment, `request` on its stdin (argv shows in `ps` and is size-limited),
/// and reads the JSON it printed. `None` when it fails.
pub fn python(
    env: &Env,
    kernel: &Path,
    script: &str,
    request: &str,
    limit: Duration,
) -> Option<serde_json::Value> {
    let mut cmd = Command::new(interpreter(env, kernel));
    cmd.arg("-I").arg("-c").arg(script).arg(kernel.join("lib"));
    let ran = run(cmd, env, Some(request), limit).ok()?;
    if ran.code != 0 {
        return None;
    }
    serde_json::from_str(ran.stdout.trim()).ok()
}
