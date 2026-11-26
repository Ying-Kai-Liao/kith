//! The kernel tools a guest run reaches on the run socket (M2(c)): `kith recall`, `kith-memory search` and `project-id`,
//! run as the identity the run's token names. The host did this for host.sock (`runauth::run_kernel`); the core does it
//! now, so a guest's token is minted, checked and answered in one place.
//!
//! Who asks is the token's, never the request's: the variables that name a caller are scrubbed from the environment the
//! tool inherits and set again from the identity. The tool runs in a process group of its own (`kith recall` is a chain of
//! processes) that is killed whole when the budget runs out. The caller runs this outside the core lock and checks the
//! run again after it, so a revoke that lands meanwhile drops the output (`daemon.rs`, the run handler).

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::runs::RunIdentity;

/// Output past this is cut off.
const MAX_OUTPUT: usize = 256 * 1024;
const BUDGET: Duration = Duration::from_secs(30);

/// The names a run's caller is known by. Cleared before the tool starts, set again from the token.
pub(crate) const SCRUB: &[&str] = &[
    "KITH_AGENT",
    "HQ_AGENT",
    "KITH_PERSON",
    "HQ_PERSON",
    "KITH_ROOM",
    "HQ_ROOM",
    "KITH_ROOM_MEMBERS",
    "KITH_ROOM_OWNER_ONLY",
    "KITH_PROJECT",
    "HQ_PROJECT",
    "KITH_VIA",
    "HQ_VIA",
    "KITH_RUN_TOKEN",
    "KITH_RUN_SOCKET",
    "KITH_CORE_RUN_SOCKET",
    "KITH_ASK_TOKEN",
    "KITH_ASK_SOCKET",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tool {
    Recall,
    MemorySearch,
    ProjectId,
}

impl Tool {
    /// The tool a run-socket row runs, `None` for a row the core answers itself.
    pub(crate) fn of_row(row: &str, args: &[String]) -> Option<Tool> {
        match row {
            "run.recall" => Some(Tool::Recall),
            "run.memorySearch" => Some(Tool::MemorySearch),
            // `project-id` with no arguments is answered from the token (`Core::call_run`).
            "run.projectId" if !args.is_empty() => Some(Tool::ProjectId),
            _ => None,
        }
    }
}

type Finder = fn() -> Option<PathBuf>;
static FINDER: OnceLock<Finder> = OnceLock::new();
#[cfg(test)]
pub(crate) static TEST_KERNEL: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

/// A test's kernel (`None`: no kernel), for as long as the guard lives. One test at a time holds it.
#[cfg(all(test, unix))]
pub(crate) fn use_kernel(bin: Option<PathBuf>) -> impl Drop {
    static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());
    struct Guard(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);
    impl Drop for Guard {
        fn drop(&mut self) {
            *TEST_KERNEL
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        }
    }
    let held = ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *TEST_KERNEL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = bin;
    Guard(held)
}

/// Tells the core where the kernel's `kith` entry point is, as the host finds it (the installed CLI, `KITH_KERNEL_BIN`).
/// Asked again on every call: a kernel upgraded while the daemon runs is the one used. Never `<home>/kernel`: on a Mac
/// where that is a checkout, a guest would run whatever branch is checked out. Without it only the variables below are
/// read, so a standalone daemon that was given neither serves no recall (fail closed).
pub fn set_kernel_finder(find: fn() -> Option<PathBuf>) {
    let _ = FINDER.set(find);
}

fn kernel_bin() -> Option<PathBuf> {
    #[cfg(test)]
    {
        TEST_KERNEL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    #[cfg(not(test))]
    {
        match FINDER.get() {
            Some(find) => find(),
            None => ["KITH_KERNEL_BIN", "HQ_BIN"]
                .iter()
                .find_map(std::env::var_os)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
        }
    }
}

/// The program to run for `tool` beside the `kith` entry point (`kith recall` is a subcommand of it, `kith-memory` is a
/// program of the same `bin/`). `project-id` is not run at all: [`project_info`] answers it in this process.
fn program(bin: &Path, tool: Tool) -> Option<(PathBuf, Option<&'static str>)> {
    match tool {
        Tool::Recall => Some((bin.to_path_buf(), Some("recall"))),
        Tool::MemorySearch => {
            let real = bin.canonicalize().unwrap_or_else(|_| bin.to_path_buf());
            Some(real.parent()?.join("kith-memory"))
                .filter(|p| p.is_file())
                .map(|p| (p, None))
        }
        Tool::ProjectId => None,
    }
}

/// `project-id --info ID` for a guest (the only form besides the bare one, which the core answers from the token): the
/// kernel's own `project::run` in this process, on the registry of the kernel the finder returned, the way
/// `kith kernel project-id --kernel K` would answer it. Not a guest run (`guest: None`): this is the core's side of the
/// socket. Never the sh stub at `bin/project-id`, which would have to find a kith program of its own.
#[cfg(unix)]
pub(crate) fn project_info(
    bin: &Path,
    home: PathBuf,
    env: kith_kernel::paths::Env<'_>,
    args: &[String],
) -> Value {
    let real = bin.canonicalize().unwrap_or_else(|_| bin.to_path_buf());
    let kernel = real
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let state = kith_kernel::paths::paths(&home, &kernel, env).state;
    let ctx = kith_kernel::project::Ctx {
        state,
        home,
        cwd: std::env::current_dir().unwrap_or_default(),
        guest: None,
    };
    let out = kith_kernel::project::run(&ctx, args);
    json!({"stdout": out.stdout, "stderr": out.stderr, "code": out.code})
}

fn both(command: &mut Command, name: &str, value: &str) {
    command.env(format!("KITH_{name}"), value);
    command.env(format!("HQ_{name}"), value);
}

/// Runs `tool` with `args` as `who`: `{stdout, stderr, code}`.
pub(crate) fn run(tool: Tool, who: &RunIdentity, args: &[String]) -> Result<Value> {
    let unavailable = || Error::Refused("kith isn't available on this computer".into());
    let bin = kernel_bin().ok_or_else(unavailable)?;
    if tool == Tool::ProjectId {
        // The kernel's project module is unix only, like the guest runs it answers.
        #[cfg(unix)]
        {
            let home = kith_kernel::paths::process_home();
            return Ok(project_info(
                &bin,
                home,
                &kith_kernel::paths::process_env,
                args,
            ));
        }
        #[cfg(not(unix))]
        return Err(Error::Refused("that isn't available here".into()));
    }
    let (path, sub) =
        program(&bin, tool).ok_or_else(|| Error::Refused("that isn't available here".into()))?;
    let mut command = Command::new(path);
    command.args(sub).args(args);
    for name in SCRUB {
        command.env_remove(name);
    }
    both(&mut command, "AGENT", &who.agent);
    both(&mut command, "PERSON", &who.person);
    both(&mut command, "VIA", "serve");
    if let Some(room) = &who.room {
        both(&mut command, "ROOM", room);
    }
    if !who.members.is_empty() {
        command.env("KITH_ROOM_MEMBERS", who.members.join(","));
    }
    // The project's shared files are part of what a guest's memory search may read (kith-memory narrows to this one).
    if let (Tool::MemorySearch, Some(project)) = (tool, &who.project) {
        both(&mut command, "PROJECT", project);
    }
    #[cfg(windows)]
    {
        run_in_job(&command)
    }
    #[cfg(unix)]
    {
        run_group(command)
    }
}

/// Collects what a tool wrote on one pipe over a channel, so the wait for it can have an end of its own: a descendant
/// that left the tool's group (or job) may hold a pipe open after the tool is gone, and the call must not outlive its
/// budget for that.
fn reader(pipe: Option<Box<dyn std::io::Read + Send>>) -> std::sync::mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut out = Vec::new();
        if let Some(p) = pipe {
            // Output past the cap is left unread: a child blocked on its full pipe is cut off by the budget.
            let _ = p.take(MAX_OUTPUT as u64).read_to_end(&mut out);
        }
        let _ = tx.send(out);
    });
    rx
}

/// The tool is gone and anything it left behind with it, so the readers end at the pipes' close.
fn collect(
    code: i32,
    started: Instant,
    out: &std::sync::mpsc::Receiver<Vec<u8>>,
    err: &std::sync::mpsc::Receiver<Vec<u8>>,
) -> Result<Value> {
    let left = BUDGET.saturating_sub(started.elapsed());
    let Ok(stdout) = out.recv_timeout(left) else {
        return Err(Error::Refused("timed out".into()));
    };
    // What is left is worked out again: the stdout wait has used some of it.
    let left = BUDGET.saturating_sub(started.elapsed());
    let Ok(stderr) = err.recv_timeout(left) else {
        return Err(Error::Refused("timed out".into()));
    };
    Ok(json!({
        "stdout": String::from_utf8_lossy(&stdout),
        "stderr": String::from_utf8_lossy(&stderr),
        "code": code,
    }))
}

/// Unix: the tool runs in a process group of its own, killed whole when the budget runs out.
#[cfg(unix)]
fn run_group(mut command: Command) -> Result<Value> {
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .map_err(|_| Error::Refused("could not run that".into()))?;
    let out = reader(child.stdout.take().map(|p| Box::new(p) as _));
    let err = reader(child.stderr.take().map(|p| Box::new(p) as _));
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < BUDGET => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                end_group(&mut child);
                return Err(Error::Refused("timed out".into()));
            }
        }
    };
    // The tool is gone; anything it left behind in its group goes too.
    end_group_of(child.id());
    collect(status.code().unwrap_or(1), started, &out, &err)
}

/// Windows: the tool runs in a job this call holds alone and that ends everything in it when the handle closes (W12 rules
/// H1 to H4). The budget, a finished tool and a core that dies all end what it started: through the core's own handle,
/// never by name or pid. Ending an already empty job is fine, so a repeat is harmless.
#[cfg(windows)]
fn run_in_job(command: &Command) -> Result<Value> {
    use kith_winsys::job::{Job, Launch, spawn_in_job};
    use std::os::windows::io::AsHandle as _;

    let refuse = |_: std::io::Error| Error::Refused("could not run that".into());
    // The whole environment the tool gets: ours, less what `command` removed, plus what it set (names are not case
    // sensitive here).
    let mut env: Vec<(String, String)> = std::env::vars_os()
        .map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned()))
        .collect();
    for (k, v) in command.get_envs() {
        let k = k.to_string_lossy().into_owned();
        env.retain(|(n, _)| !n.eq_ignore_ascii_case(&k));
        if let Some(v) = v {
            env.push((k, v.to_string_lossy().into_owned()));
        }
    }
    let args: Vec<String> = command
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let job = Job::create_kill_on_close().map_err(refuse)?;
    let nul = std::fs::File::open("NUL").map_err(refuse)?;
    let (out_r, out_w) = std::io::pipe().map_err(refuse)?;
    let (err_r, err_w) = std::io::pipe().map_err(refuse)?;
    let child = {
        let launch = Launch {
            program: Path::new(command.get_program()),
            args: &args,
            env: &env,
            cwd: None,
            stdio: Some([nul.as_handle(), out_w.as_handle(), err_w.as_handle()]),
        };
        spawn_in_job(&job, &launch).map_err(refuse)?
    };
    // Our write ends close here, so the readers end when the tool's last holder does.
    drop((out_w, err_w, nul));
    let out = reader(Some(Box::new(out_r)));
    let err = reader(Some(Box::new(err_r)));
    let started = Instant::now();
    let code = loop {
        let left = BUDGET.saturating_sub(started.elapsed());
        match child.wait_for(left.min(Duration::from_millis(10))) {
            Ok(Some(code)) => break code,
            Ok(None) if !left.is_zero() => {}
            _ => {
                let _ = job.terminate(1);
                return Err(Error::Refused("timed out".into()));
            }
        }
    };
    // The tool is gone; anything it left behind in the job goes too.
    let _ = job.terminate(1);
    collect(i32::try_from(code).unwrap_or(1), started, &out, &err)
}

#[cfg(unix)]
fn end_group(child: &mut std::process::Child) {
    end_group_of(child.id());
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(unix)]
fn end_group_of(pid: u32) {
    crate::holder_link::signal_group(pid, "KILL");
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    fn cmd(line: &str) -> Command {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        let mut c = Command::new(Path::new(&root).join("System32").join("cmd.exe"));
        c.args(["/c", line]);
        c
    }

    #[test]
    fn a_tool_in_the_job_gives_its_output_and_exit_code() {
        let out = run_in_job(&cmd("echo hi & exit 3")).expect("run");
        assert_eq!(out["stdout"].as_str().map(str::trim), Some("hi"));
        assert_eq!(out["code"], 3);
    }

    #[test]
    fn what_the_tool_left_running_ends_with_the_job() {
        // A detached ping keeps stderr (the pipe) open for a minute: the call returns only because the job was ended.
        let started = Instant::now();
        let out = run_in_job(&cmd("start /b ping -n 60 127.0.0.1 > nul & exit 0")).expect("run");
        assert_eq!(out["code"], 0);
        assert!(started.elapsed() < Duration::from_secs(20));
    }
}
