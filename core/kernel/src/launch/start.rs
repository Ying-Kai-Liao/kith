//! The last steps of a run here, in the order of `kernel/bin/kith`: the session id, the conversation lock, the writer claim for
//! this process, the permit token and the guest's grants, and then the engine.
//!
//! Every claim and token names this process's pid and the engine descends from it: on Unix the engine replaces this process
//! (`execve`, the pid is kept); elsewhere it is a child this process waits for and ends with the code of (design 2026-11-21-one-run-path,
//! section 1). Nothing in between starts the engine from another process.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::assemble::{Env, get};
use super::session::{self, Locked};
use crate::permit;
use crate::threads::claim_wait;
use crate::threads::writer::Taker;

/// How the engine starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    /// `execve`: this process becomes the engine (Unix).
    Exec,
    /// A child of this process, waited for (Windows, and the tests, which must go on after it).
    Spawn,
}

impl Engine {
    /// What this platform does.
    pub fn native() -> Self {
        if cfg!(unix) { Self::Exec } else { Self::Spawn }
    }
}

/// A Claude one-shot, ready to claim and start.
pub struct Go<'a> {
    pub agent: &'a str,
    pub kernel: &'a Path,
    /// The run's environment, after the plan and the run state.
    pub env: Env,
    /// Claude's arguments up to the task (the caller's extras included).
    pub args: Vec<String>,
    pub task: String,
    pub owner: bool,
    /// Who the claim names: the person kith-auth resolved ("" for the owner), never anything the run can change.
    pub claim_person: String,
    /// The run's settings file, which a guest's grants are added to once the claim is made.
    pub settings_file: PathBuf,
    /// `{"mcpServers": {"kithpermit": ...}}`, inline.
    pub permit_mcp: String,
    /// The session was resumed under a core that gave no answer: the lock alone holds it.
    pub alone: bool,
    /// Where `kith ask --bg` wants the session id noted ("" for nowhere).
    pub sid_file: String,
    /// The kith program that is running (where the stub sits beside).
    pub kbin: Option<PathBuf>,
}

/// Claims the session for this process and starts Claude on the task. Returns only when the run did not start or (not `Exec`)
/// when it has ended: the exit code.
pub fn claude(g: Go<'_>, taker: &Taker<'_>, pid: i64, how: Engine) -> i32 {
    let Go {
        agent,
        kernel,
        mut env,
        mut args,
        task,
        owner,
        claim_person,
        settings_file,
        permit_mcp,
        alone,
        sid_file,
        kbin,
    } = g;
    // A fresh run (or a fork) is given its id now, so there is one to claim; a resumed one holds the session it continues.
    let (csid, made) = match session::session_of(&args) {
        Ok(x) => x,
        Err(m) => {
            eprintln!("{m}");
            return 1;
        }
    };
    if made {
        args.extend(["--session-id".to_owned(), csid.clone()]);
    }
    let lock = session::lock_for(&csid, &env, kbin.as_deref());
    let stub = match &lock {
        Locked::Refused(m) => {
            eprintln!("{m}");
            return 1;
        }
        Locked::Unlocked(line) => {
            if let Some(l) = line {
                eprintln!("{l}");
            }
            None
        }
        Locked::Stub(p) => Some(p.clone()),
    };
    let mut lock_alone = alone;
    // A one-shot takes the claim (kind oneshot) for this process: the engine is this process or its child, so the claim is
    // the engine's own and ends with it. A held session is waited for a little (KITH_CLAIM_WAIT), then refused.
    if !csid.is_empty() {
        let claimed = taker.claim_start(
            &csid,
            pid,
            "oneshot",
            claim_wait(&|k| env.get(k).cloned()),
            Some(&claim_person),
            Some(agent),
        );
        eprint!("{}", taker.said());
        if let Err(m) = claimed {
            eprintln!("kith: {m}");
            return 1;
        }
        lock_alone |= taker.alone();
    }
    if !csid.is_empty() && !sid_file.is_empty() {
        let _ = std::fs::write(&sid_file, format!("{csid}\n"));
    }
    // A permission prompt in this run asks for approval through a card (--permission-prompt-tool) instead of being refused.
    // A guest run was given KITH_RUN_TOKEN/KITH_RUN_SOCKET by the host, an owner run KITH_ASK_*, or gets them here, bound to this
    // pid and to the session. No token, no host, no answer: no prompt tool, asks are refused as before, and the run goes on.
    let has = |env: &Env, k: &str| !get(env, k).is_empty();
    if !has(&env, "KITH_RUN_TOKEN") && !has(&env, "KITH_ASK_TOKEN") && owner {
        let ask_session = if csid.is_empty() {
            format!(
                "continue:{}",
                std::env::current_dir().unwrap_or_default().display()
            )
        } else {
            csid.clone()
        };
        let out = permit_run(
            &env,
            kernel,
            &["issue", agent, &ask_session, &pid.to_string()],
        );
        eprint!("{}", out.stderr);
        if out.code == 0 {
            for (k, v) in session::exports(&out.stdout) {
                env.insert(k, v);
            }
        }
    }
    let run_pair = has(&env, "KITH_RUN_TOKEN") && has(&env, "KITH_RUN_SOCKET");
    if run_pair || (has(&env, "KITH_ASK_TOKEN") && has(&env, "KITH_ASK_SOCKET")) {
        args.extend([
            "--mcp-config".to_owned(),
            permit_mcp,
            "--permission-prompt-tool".to_owned(),
            "mcp__kithpermit__approve".to_owned(),
        ]);
    }
    // F11 step 2b: what this session was given beyond the box (a yes on an escape card, an "always") goes into the settings
    // file now, after the claim above and before the engine starts: a once grant is consumed by the one run that is about to
    // start, never by a profile built earlier (a read made before the claim could be spent twice).
    if !owner && run_pair {
        let out = permit_run(&env, kernel, &["apply", &settings_file.to_string_lossy()]);
        eprint!("{}", out.stderr);
    }
    let mut argv: Vec<String> = match &stub {
        Some(p) => session::stub_argv(p, get(&env, "KITH_MACHINE"), &csid, lock_alone),
        None => vec![],
    };
    let program = if argv.is_empty() {
        "claude".to_owned()
    } else {
        argv.remove(0)
    };
    if stub.is_some() {
        argv.push("claude".to_owned());
    }
    argv.extend(args);
    argv.extend(["-p".to_owned(), task]);
    start(&program, &argv, &env, how)
}

/// `kith-permit VERB ...` in this process, with the run's environment.
pub(super) fn permit_run(env: &Env, kernel: &Path, args: &[&str]) -> permit::Out {
    let pairs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let mut e = permit::Env::with(&pairs);
    e.home = PathBuf::from(get(env, "HOME"));
    let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
    permit::run(&e, kernel, &args, &mut std::io::empty())
}

/// Starts `program args...` with exactly `env`.
pub fn start(program: &str, args: &[String], env: &Env, how: Engine) -> i32 {
    let mut cmd = Command::new(program);
    cmd.args(args).env_clear().envs(env);
    #[cfg(unix)]
    if how == Engine::Exec {
        use std::os::unix::process::CommandExt as _;
        let e = cmd.exec();
        return failed(program, &e);
    }
    match cmd.status() {
        Ok(s) => code_of(s),
        Err(e) => failed(program, &e),
    }
}

/// What a shell says when it cannot start a program: 127 when there is none, 126 when it can't run.
fn failed(program: &str, e: &std::io::Error) -> i32 {
    eprintln!("kith: can't run {program}: {e}");
    if e.kind() == std::io::ErrorKind::NotFound {
        127
    } else {
        126
    }
}

/// A process a signal ended reads as 128 + the signal, as a shell reports it.
#[cfg(unix)]
fn code_of(s: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt as _;
    s.code().unwrap_or_else(|| 128 + s.signal().unwrap_or(0))
}

#[cfg(not(unix))]
fn code_of(s: std::process::ExitStatus) -> i32 {
    s.code().unwrap_or(1)
}

/// Another engine's one-shot: the same run handed to `kernel engine-run` (the prompt in a file, directories and MCP configs as lists).
pub struct Adapter {
    pub env: Env,
    pub harness: String,
    /// The engine run's flags before the task: `--permission-mode`, `--resume`, `--output`, then `--`.
    pub flags: Vec<String>,
    pub task: String,
}

/// Runs the engine-run child and waits. Bash trapped TERM, INT and HUP and passed them on, so a TERM to this run ends the engine
/// (and a guest's run claim does not outlive a still-running engine); elsewhere there is no signal to pass.
#[cfg(unix)]
fn run_engine(cmd: &mut Command) -> std::io::Result<std::process::ExitStatus> {
    crate::threads::runproc::run_passing_signals(cmd)
}

#[cfg(not(unix))]
fn run_engine(cmd: &mut Command) -> std::io::Result<std::process::ExitStatus> {
    cmd.status()
}

/// Runs the task over ACP (`kith kernel engine-run`); its exit code is final, 78 (no ACP one-shot for this engine, nothing
/// started) included: there is no adapter script behind it any more.
pub fn other(a: Adapter) -> i32 {
    let me = match std::env::current_exe() {
        Ok(me) => me,
        Err(e) => return failed("kith", &e),
    };
    let mut args = vec!["kernel".to_owned(), "engine-run".to_owned(), a.harness];
    args.extend(a.flags);
    args.push(a.task);
    let mut cmd = Command::new(me);
    cmd.args(&args).env_clear().envs(&a.env);
    match run_engine(&mut cmd) {
        Ok(s) => code_of(s),
        Err(e) => failed("kith", &e),
    }
}
