//! Running `git` (and `cp`), as `subprocess.run(..., capture_output=True, timeout=...)` does, and the part of `kith-pkg` that
//! commits a good version into the agent's own repo.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::py::{Kind, PyErr, R, repr, strip};
use super::{Ctx, name_of};

/// What a finished program printed.
pub struct Ran {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

fn drain(mut r: impl std::io::Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = vec![];
        let _ = r.read_to_end(&mut buf);
        buf
    })
}

/// Runs `cmd` with its output captured. A program that does not start is an `OSError`; one that runs past `limit` is killed
/// and is `subprocess.TimeoutExpired`.
pub fn run(mut cmd: Command, limit: Duration) -> R<Ran> {
    let shown = {
        let mut words = vec![cmd.get_program().to_string_lossy().into_owned()];
        words.extend(cmd.get_args().map(|a| a.to_string_lossy().into_owned()));
        format!(
            "[{}]",
            words.iter().map(|w| repr(w)).collect::<Vec<_>>().join(", ")
        )
    };
    let program = cmd.get_program().to_string_lossy().into_owned();
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| super::py::os_err(&e, Path::new(&program)))?;
    let out = child.stdout.take().map(drain);
    let err = child.stderr.take().map(drain);
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if started.elapsed() >= limit => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(PyErr::new(
                    Kind::Timeout,
                    format!(
                        "Command '{shown}' timed out after {} seconds",
                        limit.as_secs()
                    ),
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(e) => return Err(super::py::os_err(&e, Path::new("git"))),
        }
    };
    let text = |h: Option<std::thread::JoinHandle<Vec<u8>>>| {
        h.and_then(|h| h.join().ok())
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default()
    };
    Ok(Ran {
        code: status.code().unwrap_or(-1),
        stdout: text(out),
        stderr: text(err),
    })
}

/// `git(home, *args)`: `git -C home args...` without a prompt for credentials, two minutes at most.
pub fn git(home: &Path, args: &[&str]) -> R<Ran> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(home)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0");
    run(cmd, Duration::from_secs(120))
}

/// `owned(home)`: the agent has a history of its own: it lives in the agents folder and is inside a git repo.
fn owned(ctx: &Ctx, home: &Path) -> R<bool> {
    let inside = home.parent() == Some(ctx.agents.as_path())
        || (name_of(home) == "role"
            && home.parent().and_then(Path::parent) == Some(ctx.agents.as_path()));
    Ok(inside && strip(&git(home, &["rev-parse", "--is-inside-work-tree"])?.stdout) == "true")
}

/// `commit(home, msg)`: the package's pending edits (this folder only, for an agent inside a bigger repo). Never fails a run.
pub fn commit(ctx: &Ctx, home: &Path, msg: &str) -> bool {
    let go = || -> R<bool> {
        if !owned(ctx, home)? {
            return Ok(false);
        }
        git(home, &["add", "-A", "--", "."])?;
        if git(home, &["diff", "--cached", "--quiet", "--", "."])?.code == 0 {
            return Ok(false);
        }
        Ok(git(home, &["commit", "-q", "-m", msg, "--", "."])?.code == 0)
    };
    // `except (OSError, subprocess.TimeoutExpired): return False`
    go().unwrap_or(false)
}
