//! Running a program for the MCP tools: `subprocess.run(..., capture_output=True, text=True, timeout=...)`, and the one Python
//! helper the bug report and the doors still lean on.

use super::Env;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
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

fn drain<R: Read + Send + 'static>(s: Option<R>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut s) = s {
            let _ = s.read_to_end(&mut buf);
        }
        buf
    })
}

/// Runs `cmd` with exactly the environment `env` holds, no stdin, and both output streams captured. The child is killed when
/// it outlives `limit`.
pub fn run(mut cmd: Command, env: &Env, limit: Duration) -> Result<Ran, Failed> {
    cmd.env_clear()
        .envs(env.vars())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| Failed::Os(strerror(&e)))?;
    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if start.elapsed() < limit => std::thread::sleep(Duration::from_millis(5)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                // a program that handed its pipes to a grandchild would hold the readers open
                return Err(Failed::Timeout);
            }
            Err(e) => return Err(Failed::Os(strerror(&e))),
        }
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    Ok(Ran {
        code: status.code().unwrap_or(-1),
        stdout: text(&stdout),
        stderr: text(&stderr),
    })
}

/// Runs `python3 -I -c SCRIPT KERNEL/lib ARG` in this environment and reads the JSON it printed. `None` when it fails.
pub fn python(env: &Env, kernel: &Path, script: &str, arg: &str, limit: Duration) -> Option<serde_json::Value> {
    let mut cmd = Command::new(env.python());
    cmd.arg("-I")
        .arg("-c")
        .arg(script)
        .arg(kernel.join("lib"))
        .arg(arg);
    let ran = run(cmd, env, limit).ok()?;
    if ran.code != 0 {
        return None;
    }
    serde_json::from_str(ran.stdout.trim()).ok()
}
