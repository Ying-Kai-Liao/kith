//! The process that will be claude, parked until the claim names it, on Windows (W12-4). The Unix one (`proc.rs`) forks: the child
//! waits on a pipe, and the exec of claude happens in that same process, so the pid the claim names IS claude's (or the stub's).
//! Windows has no fork, so the parked process is a spawn of this program (`kith kernel lifecycle parked ...`, [`parked_main`]):
//!
//! - it waits on its standard input for the byte `1` and exits unrun when the pipe closes first (the compaction was refused, or
//!   the sweep died: the same `go` the fork reads);
//! - on `1` it starts the program (`argv`, in `cwd`, standard streams on `NUL`), says `ok` or `err <why>` on its standard error
//!   and waits for it. The claim names this process, which lives exactly as long as the program does (the part the stub plays on
//!   a Mac), so a sweep that dies while claude writes leaves the claim held by a live process;
//! - it ends with the program's exit code.
//!
//! The functions and their meaning are those of `proc.rs`, so `sweep.rs` is one text for both. A "descriptor" is a small number
//! standing for a pipe end this process holds (the table below), so the sweep's `i32`s need no change.
//!
//! What differs: the conversation lock (`kith-stub --status-fd`) is not taken here, because the stub's status pipe is Unix only
//! ([`lock_stub`] says so); a timeout ends the whole tree below the parked process (there is no process group); there is no
//! exec failure text of Python's shape, the OS's own message stands in.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::windows::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crate::paths::Env;

/// How long a compaction may run before claude is killed.
pub const COMPACT_TIMEOUT: Duration = Duration::from_secs(900);

/// The stub's status descriptor on a Mac. Named so `sweep.rs` reads the same; nothing is passed on Windows ([`pipe`]).
pub const STATUS_FD: i32 = 9;

/// `CREATE_NO_WINDOW`: a sweep that runs from a hidden tick has no console, and a console program would open one.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// One parked process at a time in this process, as on a Mac (the tests, and a second sweep in the same process, wait their turn).
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// Held for the whole life of a parked process, from before [`parked`] to after [`reap`] and the closes.
pub fn exclusive() -> MutexGuard<'static, ()> {
    ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The pipe ends this process holds for a parked process, by the number [`Parked`] hands out.
enum End {
    Go(ChildStdin),
    Err(BufReader<ChildStderr>),
}

static NEXT_END: AtomicI32 = AtomicI32::new(100);
static ENDS: Mutex<Option<HashMap<i32, End>>> = Mutex::new(None);
static CHILDREN: Mutex<Option<HashMap<i32, Child>>> = Mutex::new(None);

fn ends() -> MutexGuard<'static, Option<HashMap<i32, End>>> {
    ENDS.lock().unwrap_or_else(PoisonError::into_inner)
}

fn children() -> MutexGuard<'static, Option<HashMap<i32, Child>>> {
    CHILDREN.lock().unwrap_or_else(PoisonError::into_inner)
}

fn put_end(end: End) -> i32 {
    let id = NEXT_END.fetch_add(1, Ordering::Relaxed);
    ends().get_or_insert_with(HashMap::new).insert(id, end);
    id
}

/// A parked process: write `1` to `go` to start it, close `go` to make it exit unrun; what it says about starting the program is read
/// from `err`.
pub struct Parked {
    pub pid: i32,
    pub go: i32,
    pub err: i32,
}

impl Parked {
    /// What the parked process wrote to `err` when the program would not start (the OS's text); empty when it said nothing.
    pub fn why(&self, said: &[u8]) -> String {
        String::from_utf8_lossy(said).into_owned()
    }
}

/// The program that is parked: this one, with the verb that waits for `go`. The tests put their own test binary here.
fn launcher() -> Vec<OsString> {
    #[cfg(test)]
    if let Some(l) = TEST_LAUNCHER
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
    {
        return l;
    }
    let me = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("kith"));
    vec![
        me.into_os_string(),
        "kernel".into(),
        "lifecycle".into(),
        "parked".into(),
    ]
}

/// What the tests start instead of `kith kernel lifecycle parked`: the test binary, running the one test that calls [`parked_main`].
#[cfg(test)]
pub(super) static TEST_LAUNCHER: Mutex<Option<Vec<OsString>>> = Mutex::new(None);

/// Starts the process that will be claude: it starts `argv` (in `cwd`, with `env`, the standard streams on `NUL`) when `1` is written
/// to `go`, and exits without running anything when `go` closes unwritten (also when this process dies). Its pid is the one the
/// claim names. `group` and `keep` are the Unix process group and the stub's status descriptor: neither exists here.
pub fn parked(
    argv: &[OsString],
    cwd: &Path,
    env: &[(OsString, OsString)],
    _group: bool,
    keep: Option<i32>,
) -> std::io::Result<Parked> {
    if keep.is_some() {
        return Err(std::io::Error::other("no status descriptor on Windows"));
    }
    let launch = launcher();
    let mut cmd = Command::new(&launch[0]);
    cmd.args(&launch[1..])
        .arg("--cwd")
        .arg(cwd)
        .arg("--")
        .args(argv)
        .env_clear()
        .envs(env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW);
    let mut child = cmd.spawn()?;
    let (Some(stdin), Some(stderr)) = (child.stdin.take(), child.stderr.take()) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(std::io::Error::other("the parked process has no pipes"));
    };
    let pid = i32::try_from(child.id()).unwrap_or(-1);
    children()
        .get_or_insert_with(HashMap::new)
        .insert(pid, child);
    Ok(Parked {
        pid,
        go: put_end(End::Go(stdin)),
        err: put_end(End::Err(BufReader::new(stderr))),
    })
}

/// The parked process's side ([`parked`] starts it with `--cwd CWD -- ARGV...`): waits for `go` on standard input, starts the program,
/// says how that went on standard error (`ok`, or `err` and the OS's text, one line) and waits for the program. Returns its exit code;
/// 127 when it never ran.
pub fn parked_main(args: &[String]) -> i32 {
    let Some(at) = args.iter().position(|a| a == "--") else {
        return 127;
    };
    let cwd = args[..at]
        .iter()
        .position(|a| a == "--cwd")
        .and_then(|i| args.get(i + 1));
    let prog = &args[at + 1..];
    let mut byte = [0u8; 1];
    if prog.is_empty() || !matches!(std::io::stdin().read(&mut byte), Ok(1)) || byte[0] != b'1' {
        return 127;
    }
    let mut cmd = Command::new(&prog[0]);
    cmd.args(&prog[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW);
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    let mut said = std::io::stderr();
    match cmd.spawn() {
        Ok(mut child) => {
            let _ = writeln!(said, "ok");
            let _ = said.flush();
            child.wait().ok().and_then(|s| s.code()).unwrap_or(1)
        }
        Err(e) => {
            let _ = writeln!(said, "err {e}: '{}'", prog[0]);
            let _ = said.flush();
            127
        }
    }
}

/// A pipe: not made on Windows, because the only reader of one is the stub's status descriptor, which [`lock_stub`] does not use.
pub fn pipe() -> std::io::Result<(i32, i32)> {
    Err(std::io::Error::other("no status pipe on Windows"))
}

/// Gives up this process's end of the pipe `fd`.
pub fn close(fd: i32) {
    if let Some(m) = ends().as_mut() {
        m.remove(&fd);
    }
}

/// What the parked process said about starting the program: empty when it started (or said nothing: it died), else the text.
/// Waits for the one line, not for the end of the stream: the parked process keeps its standard error open while the program runs.
pub fn read_to_end(fd: i32) -> Vec<u8> {
    let Some(End::Err(mut r)) = ends().as_mut().and_then(|m| m.remove(&fd)) else {
        return Vec::new();
    };
    let mut line = String::new();
    let _ = r.read_line(&mut line);
    ends()
        .get_or_insert_with(HashMap::new)
        .insert(fd, End::Err(r));
    let line = line.trim_end();
    line.strip_prefix("err ")
        .map(|why| why.as_bytes().to_vec())
        .unwrap_or_default()
}

pub fn write_byte(fd: i32, b: u8) {
    if let Some(End::Go(w)) = ends().as_mut().and_then(|m| m.get_mut(&fd)) {
        let _ = w.write_all(&[b]);
        let _ = w.flush();
    }
}

/// Every process below `root` (children first, `root` last), by the parent each was created with.
fn tree(root: u32) -> Vec<u32> {
    let rows = kith_winsys::procs::table().unwrap_or_default();
    let mut order = vec![root];
    let mut i = 0;
    while i < order.len() {
        let at = order[i];
        i += 1;
        for r in &rows {
            if r.parent == at && r.pid != at && !order.contains(&r.pid) {
                order.push(r.pid);
            }
        }
    }
    order.reverse();
    order
}

/// Waits for `pid`: its exit code, or `None` when it ran past `timeout` (it is then ended with everything below it, and reaped). The
/// claim is given back only once the whole tree has left. `group` is the Unix process group: the tree is ended either way.
pub fn reap(pid: i32, timeout: Option<Duration>, _group: bool) -> Option<i32> {
    let child = children().as_mut().and_then(|m| m.remove(&pid));
    let Some(mut child) = child else {
        // not ours, or reaped already: nothing to wait for, and not a success either
        return Some(-1);
    };
    let end = timeout.map(|t| Instant::now() + t);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status.code().unwrap_or(-1)),
            Ok(None) => {}
            Err(_) => return Some(-1),
        }
        let Some(end) = end else {
            return Some(child.wait().ok().and_then(|s| s.code()).unwrap_or(-1));
        };
        if Instant::now() >= end {
            for p in tree(child.id()) {
                let _ = kith_winsys::procs::terminate(p, 1);
            }
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// An executable file: Windows has no mode bit, so a file with an extension that runs.
fn executable(p: &Path) -> bool {
    p.is_file()
        && p.extension().and_then(OsStr::to_str).is_some_and(|e| {
            matches!(
                e.to_ascii_lowercase().as_str(),
                "exe" | "cmd" | "bat" | "com"
            )
        })
}

/// `shutil.which`: the first executable file named `name` on `PATH` (or `name` itself when it has a directory part), with the
/// extension left off allowed (`claude` finds `claude.exe`, `claude.cmd`).
pub fn which(name: &str, env: Env) -> Option<PathBuf> {
    let with_ext = |base: PathBuf| -> Option<PathBuf> {
        if executable(&base) {
            return Some(base);
        }
        ["exe", "cmd", "bat", "com"].iter().find_map(|e| {
            let mut s = base.clone().into_os_string();
            s.push(".");
            s.push(e);
            let p = PathBuf::from(s);
            executable(&p).then_some(p)
        })
    };
    if name.contains(['/', '\\']) {
        return with_ext(PathBuf::from(name));
    }
    let path = env("PATH").unwrap_or_default();
    std::env::split_paths(&path).find_map(|d| with_ext(d.join(name)))
}

/// The conversation's writer lock, which a Mac takes by starting the run under `kith-stub --status-fd`. The status pipe is Unix only,
/// and without it a busy lock cannot be told from claude's own exit 75, so a Windows compaction runs without the stub and says so
/// (`sweep.rs` prints the second value; `KITH_REQUIRE_STUB=1` makes it a refusal). The writer claim still keeps every other
/// writer that takes claims out. The stub on Windows is W12-5.
pub fn lock_stub(
    _sid: &str,
    _machine: Option<&Path>,
    _kith_bin: Option<&Path>,
    _env: Env,
) -> (Vec<OsString>, Option<String>) {
    (
        Vec::new(),
        Some(
            "the conversation lock (kith-stub's status pipe) is not taken on Windows yet"
                .to_owned(),
        ),
    )
}

/// What kith-stub wrote on its status pipe: nothing is made on Windows ([`pipe`]), so nothing is said.
pub fn stub_word(_fd: i32) -> String {
    String::new()
}
