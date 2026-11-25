//! The interactive run of `kith AGENT` (no task): the steps of `kernel/bin/kith` after the plan, in its order, ending in the engine
//! as this process's CHILD (`threads::run::run`, in process: the terminal's claim names this pid, and claude descends from it under
//! the conversation lock's stub, I2). Design 2026-11-21-one-run-path, section 3.
//!
//! The order (thread-writers I1-I7): the session id and the lock, `--held` (hands the session to the core's holder), the terminal
//! check, the owner's ask token for the `PermissionDenied` hook alone, the terminal's claim (not for a resumed session: `resolve`
//! took it), then the run.

use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};

use super::assemble::{Env, get};
use super::exec::Deps;
use super::held::{self, Held, Outcome};
use super::session::{self, Locked};
use super::start::{self, Engine, permit_run};
use crate::threads::Ctx;
use crate::threads::writer::Taker;

/// A Claude terminal run, ready to claim and start.
#[allow(clippy::struct_excessive_bools)]
pub struct Term<'a> {
    pub agent: &'a str,
    pub kernel: &'a Path,
    /// The run's environment, after the plan and the run state.
    pub env: Env,
    /// Claude's arguments (the caller's extras included).
    pub args: Vec<String>,
    pub owner: bool,
    /// Another run started this one.
    pub nested: bool,
    /// Who the claim names: the person kith-auth resolved ("" for the owner).
    pub claim_person: String,
    /// The run's settings file (its hooks say whether the owner's ask token is wanted).
    pub settings_file: PathBuf,
    /// The Claude session `--resume`/`-r`/`-c` took the claim of in `resolve` ("" for none).
    pub resumed: String,
    /// The resumed session was claimed under a core that gave no answer: the lock alone holds it.
    pub alone: bool,
    /// `--held`.
    pub held: bool,
    /// The kith program that is running (where the stub sits beside).
    pub kbin: Option<PathBuf>,
    pub ctx: &'a Ctx<'a>,
    pub deps: &'a Deps<'a>,
}

fn is_tty() -> bool {
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

/// "No terminal on this side": an interactive session needs one, or a task.
fn no_terminal(agent: &str) -> i32 {
    eprintln!(
        "kith: no terminal for an interactive session; give a task (kith {agent} \"...\") or run it in a terminal"
    );
    1
}

/// Does the run's settings carry the `PermissionDenied` hook (auto mode)? `grep -q 'permit.* denied --dir'`, line by line.
fn has_denied_hook(settings: &str) -> bool {
    settings.lines().any(|l| {
        l.find("permit")
            .is_some_and(|i| l[i..].contains(" denied --dir"))
    })
}

/// `$PWD` as a shell names it (a symlinked folder keeps its own name), else the physical folder.
fn shell_pwd(env: &Env) -> String {
    let cwd = std::env::current_dir().unwrap_or_default();
    let pwd = get(env, "PWD");
    if !pwd.is_empty() && std::fs::canonicalize(pwd).ok() == std::fs::canonicalize(&cwd).ok() {
        return pwd.to_owned();
    }
    cwd.to_string_lossy().into_owned()
}

/// Claims the session for this process (a terminal claim) and runs Claude as this process's child, under the lock. Returns the
/// exit code the run ends with, or the one for a run that did not start.
pub fn claude(t: Term<'_>, taker: &Taker<'_>, pid: i64) -> i32 {
    let Term {
        agent,
        kernel,
        mut env,
        mut args,
        owner,
        nested,
        claim_person,
        settings_file,
        resumed,
        alone,
        held: want_held,
        kbin,
        ctx,
        deps,
    } = t;
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
    let stub = match session::lock_for(&csid, &env, kbin.as_deref()) {
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
        Locked::Stub(p) => Some(p),
    };
    let mut lock_alone = alone;
    let tty = is_tty();
    // --held: the same launch, handed to the core to start in the holder (it returns here to run directly, saying why). The ask token
    // below is for a run that descends from this process: a held engine does not, and the core would drop it from the env anyway.
    if want_held {
        if owner && !nested {
            env.insert("KITH_TERMINAL_SESSION".to_owned(), "1".to_owned());
            if !resumed.is_empty() {
                env.insert("KITH_RESUMED_FROM".to_owned(), resumed.clone());
            }
            if let Some(port) = deps.held {
                let endpoint = ctx
                    .machine
                    .clone()
                    .unwrap_or_default()
                    .join("core/attach.sock");
                let h = Held {
                    agent,
                    sid: &csid,
                    rsid: &resumed,
                    args: &args,
                    env: &env,
                    taker,
                    port,
                    pid,
                    endpoint,
                    tty,
                };
                match held::launch(&h) {
                    Outcome::Exit(code) => return code,
                    Outcome::Direct { alone } => lock_alone |= alone,
                }
            } else {
                eprintln!(
                    "kith: no core client here, so nothing can hold it; running here (--direct)"
                );
            }
        } else {
            eprintln!("kith: --held is for the owner's own terminal; running here (--direct)");
        }
    }
    if !tty {
        return no_terminal(agent);
    }
    // An interactive owner run gets an ask token too, for the PermissionDenied hook alone (its card in the apps): no prompt tool is
    // added, so ordinary permission prompts stay in the terminal. Only when the settings carry that hook (auto mode). The pid is this
    // process's: the engine is its child, so the hook still descends from it. No host: the run starts as before.
    let has = |env: &Env, k: &str| !get(env, k).is_empty();
    if owner
        && !nested
        && !has(&env, "KITH_RUN_TOKEN")
        && !has(&env, "KITH_ASK_TOKEN")
        && std::fs::read_to_string(&settings_file).is_ok_and(|s| has_denied_hook(&s))
    {
        let session = if csid.is_empty() {
            format!("continue:{}", shell_pwd(&env))
        } else {
            csid.clone()
        };
        let mut asking = env.clone();
        asking.insert("KITH_PERMIT_NO_RESTART".to_owned(), "1".to_owned());
        let out = permit_run(
            &asking,
            kernel,
            &["issue", agent, &session, &pid.to_string()],
        );
        eprint!("{}", out.stderr);
        if out.code == 0 {
            for (k, v) in session::exports(&out.stdout) {
                env.insert(k, v);
            }
        }
    }
    // The terminal's claim (kind terminal), for this process, which keeps it until the run has ended: `run` gives the claim back when
    // Claude has exited. -r/-c/--resume took it in resolve already. A held session is refused at once.
    if !csid.is_empty() && csid != resumed {
        let claimed = taker.claim_start(
            &csid,
            pid,
            "terminal",
            std::time::Duration::ZERO,
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
    // An interactive owner run is listed in the apps (the SessionStart hook) and may be taken over from there, which ends Claude: it
    // runs as a child of this process so the note about getting it back can follow it (signals and the exit code pass through).
    if owner {
        env.insert("KITH_TERMINAL_SESSION".to_owned(), "1".to_owned());
    }
    if !resumed.is_empty() {
        env.insert("KITH_RESUMED_FROM".to_owned(), resumed);
    }
    let mut cmd: Vec<String> = match &stub {
        Some(p) => session::stub_argv(p, get(&env, "KITH_MACHINE"), &csid, lock_alone),
        None => vec![],
    };
    cmd.push("claude".to_owned());
    cmd.extend(args);
    run_child(ctx, deps, kernel, agent, &cmd, &env)
}

/// Runs `cmd` as a child of this process with `env`, and ends with its code: `threads::run::run` where there are processes to fork
/// (the take-over note and the wake queue follow it), elsewhere a spawn and a wait.
#[cfg(unix)]
fn run_child(
    ctx: &Ctx<'_>,
    deps: &Deps<'_>,
    kernel: &Path,
    agent: &str,
    cmd: &[String],
    env: &Env,
) -> i32 {
    use std::ffi::OsString;
    // The run's own environment answers what the wrapper reads (KITH_IDLE_POLL, the state, the cache life), as it did under bash.
    let lookup = |k: &str| env.get(k).cloned();
    let rctx = Ctx {
        state: ctx.state.clone(),
        machine: ctx.machine.clone(),
        home: ctx.home.clone(),
        env: &lookup,
    };
    let pairs: Vec<(OsString, OsString)> = env
        .iter()
        .map(|(k, v)| (OsString::from(k), OsString::from(v)))
        .collect();
    crate::threads::run::run_in(
        &rctx,
        deps.host,
        deps.core,
        kernel,
        agent,
        cmd,
        &crate::threads::run::say,
        Some(&pairs),
    )
}

#[cfg(not(unix))]
fn run_child(
    _ctx: &Ctx<'_>,
    _deps: &Deps<'_>,
    _kernel: &Path,
    _agent: &str,
    cmd: &[String],
    env: &Env,
) -> i32 {
    start::start(&cmd[0], &cmd[1..], env, Engine::Spawn)
}

/// Another harness at the terminal: its adapter script, with the prompt and folders in the environment (`kernel/bin/kith` ends in
/// `exec python3 adapters/harness.py HARNESS FLAGS`: no program of ours runs a non-Claude terminal yet).
pub fn adapter(
    agent: &str,
    kernel: &Path,
    harness: &str,
    mut flags: Vec<String>,
    env: &Env,
    how: Engine,
) -> i32 {
    if !is_tty() {
        return no_terminal(agent);
    }
    let script = kernel.join("adapters/harness.py");
    let mut args = vec![script.to_string_lossy().into_owned(), harness.to_owned()];
    args.append(&mut flags);
    start::start("python3", &args, env, how)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_denied_hook_is_the_one_the_settings_write() {
        let hook = r#"{"command": "'/k/bin/kith-permit' denied --dir /x/once", "timeout": 660}"#;
        assert!(has_denied_hook(hook));
        assert!(!has_denied_hook(r#"{"command": "denied --dir x"}"#));
        assert!(!has_denied_hook("kith-permit approve"));
    }
}
