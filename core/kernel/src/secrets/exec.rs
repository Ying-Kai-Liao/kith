//! `kith-auth exec [--for AGENT] SERVICE -- CMD...` in process (retire-python P1-15, design 2026-11-21-mcp-exec 2.3): start CMD
//! with the service's secrets in its environment only. Every shared MCP server is started this way; the config `mcp-config`
//! writes names this verb as the server's command.
//!
//! In order: the tables (the file, or the core's on the tombstone), the profile the agent is granted, the service's settings,
//! the secrets (the Keychain through `/usr/bin/security`, a variable or a file: never argv, never a file of ours), for anyone
//! but the owner the scrub of what leads into a folder they write, one line in auth.log, and then CMD replaces this process
//! (on Windows it runs as a child and this exits with its code). A usage error is exit 2 and every refusal exit 1, the line
//! starting `kith-auth: ` as it does in the script. No value is held after the start: the process is the server.
//!
//! The one-time room secret (`exec --secret`) stays in `kith-auth`: the core has no `once` table yet.

use std::io::Write as _;
use std::path::PathBuf;

use serde_json::Value;

use super::authority::Authority;
use super::guest::Folders;
use super::session::{Env, Session};
use super::{Ctx, Die, Outcome, R, access_of, die, is_owner, row_of};

const USAGE: &str = "exec [--for AGENT] <service> -- CMD...";

/// What `exec` is about to start.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Start {
    /// CMD and its arguments.
    pub cmd: Vec<String>,
    /// The whole environment CMD gets.
    pub env: Env,
    /// A folder to start in (a guest's run that stood in a folder they write), else where this is.
    pub cwd: Option<PathBuf>,
    /// The line auth.log gets.
    pub audit: String,
}

/// `take_for`: the agent is `--for`, else the run's own (`KITH_AGENT`).
fn take_for<'a>(env: &Env, args: &'a [String]) -> R<(String, &'a [String])> {
    match args {
        [flag, agent, rest @ ..] if flag == "--for" => Ok((agent.clone(), rest)),
        [flag] if flag == "--for" => Err(usage()),
        _ => Ok((env.get("KITH_AGENT").cloned().unwrap_or_default(), args)),
    }
}

fn usage() -> Die {
    Die {
        msg: USAGE.to_owned(),
        code: 2,
    }
}

/// `kith kernel secrets exec ...`: on Unix it does not return when CMD starts; any `Outcome` is a refusal (or, on Windows,
/// CMD's end).
pub(super) fn run(c: &Ctx, args: &[String]) -> R<Outcome> {
    // vars() panics on a non-UTF-8 value; the launch itself converts lossily.
    let env: Env = std::env::vars_os()
        .map(|(k, v)| {
            (
                k.to_string_lossy().into_owned(),
                v.to_string_lossy().into_owned(),
            )
        })
        .collect();
    let (agent, rest) = take_for(&env, args)?;
    if rest.first().map(String::as_str) == Some("--secret") {
        return Err(Die {
            msg: "exec --secret is not in this program: use kith-auth exec --secret".to_owned(),
            code: 2,
        });
    }
    let (svc, cmd) = match rest {
        [svc, dash, cmd @ ..] if dash == "--" && !cmd.is_empty() => (svc, cmd),
        _ => return Err(usage()),
    };
    let start = prepare(c, env, &agent, svc, cmd)?;
    write_audit(c, &start.audit);
    start_process(&start)
}

/// Everything `exec` does before it starts CMD, except writing the audit line.
pub(super) fn prepare(c: &Ctx, env: Env, agent: &str, svc: &str, cmd: &[String]) -> R<Start> {
    let tables = Authority::open(c)?.tables()?;
    let mut s = Session::new(c, &tables, env);
    let profile = s.grant_for(agent, svc)?;
    // os.environ as it was before `settings()` edited it: that is what CMD inherits
    let mut env = s.env.clone();
    let settings = s.settings(agent, svc)?;
    let secrets = s.secrets(svc, &profile, agent)?;
    for (var, value) in settings {
        let Value::String(v) = value else {
            return die(format!("{svc}: the setting {var} is not a string"));
        };
        env.insert(var, v);
    }
    env.extend(secrets);
    crate::launch::assemble::sync_env(&mut env);
    let person = s.acting();
    let mut cwd = None;
    if !is_owner(&tables, &person) && person != "-" {
        // A guest's server runs outside their sandbox: nothing in its environment or its working folder may lead into a
        // folder they write.
        let extra = write_folders(c, &tables, agent, &person);
        let folders = Folders::new(&s.paths, &person, &extra);
        env = folders.scrub(&env);
        let here = std::env::current_dir().map_err(|e| Die {
            msg: format!("the working folder: {e}"),
            code: 1,
        })?;
        let now = if folders.contains(&here.to_string_lossy()) {
            let home = s
                .env
                .get("HOME")
                .map_or_else(crate::paths::process_home, PathBuf::from);
            cwd = Some(home.clone());
            std::fs::canonicalize(&home).unwrap_or(home)
        } else {
            here
        };
        env.insert("PWD".to_owned(), now.to_string_lossy().into_owned());
    }
    let audit = audit_line(&s.env, agent, svc, &profile);
    Ok(Start {
        cmd: cmd.to_vec(),
        env,
        cwd,
        audit,
    })
}

/// `access_of(row_of(auth, agent, person) or {})['write']`: the folders the membership was granted to write.
fn write_folders(c: &Ctx, tables: &Value, agent: &str, person: &str) -> Vec<String> {
    let row = if c.is_builtin(agent) {
        None
    } else {
        row_of(tables, agent, person)
    };
    let access = access_of(row.unwrap_or(&Value::Null));
    access
        .get("write")
        .and_then(Value::as_array)
        .map(|w| {
            w.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// The line of auth.log for a secret handed out: when, how, to whom, and who was really running. `env` is `os.environ` after
/// `settings()`, so `run=` is the agent it named.
fn audit_line(env: &Env, agent: &str, svc: &str, profile: &str) -> String {
    format!(
        "{}\texec\t{svc}\t{profile}\tfor={agent}\trun={}\tppid={}\tperson={}\n",
        chrono::Local::now().format("%Y-%m-%dT%H:%M:%S"),
        env.get("KITH_AGENT").map_or("-", String::as_str),
        parent_pid(),
        env.get("KITH_PERSON")
            .filter(|p| !p.is_empty())
            .map_or("owner", String::as_str),
    )
}

#[cfg(unix)]
fn parent_pid() -> u32 {
    std::os::unix::process::parent_id()
}

#[cfg(not(unix))]
fn parent_pid() -> u32 {
    0
}

/// Appends the audit line, best effort: a log that can't be written never stops a server.
fn write_audit(c: &Ctx, line: &str) {
    let log = c.auth.with_file_name("auth.log");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(log)
    {
        let _ = f.write_all(line.as_bytes());
    }
}

/// CMD replaces this process: nothing of the tables or the secrets stays in a process of ours.
#[cfg(unix)]
fn start_process(start: &Start) -> R<Outcome> {
    use std::os::unix::process::CommandExt as _;
    let (program, args) = start.cmd.split_first().ok_or_else(usage)?;
    let mut cmd = std::process::Command::new(program);
    cmd.args(args).env_clear().envs(&start.env);
    if let Some(dir) = &start.cwd {
        cmd.current_dir(dir);
    }
    let err = cmd.exec();
    die(format!("{program}: {err}"))
}

/// Windows has no `execve`: CMD runs as a child with the same environment and this ends with its code.
#[cfg(not(unix))]
fn start_process(start: &Start) -> R<Outcome> {
    let (program, args) = start.cmd.split_first().ok_or_else(usage)?;
    let mut cmd = std::process::Command::new(program);
    cmd.args(args).env_clear().envs(&start.env);
    if let Some(dir) = &start.cwd {
        cmd.current_dir(dir);
    }
    let status = cmd.status().map_err(|e| Die {
        msg: format!("{program}: {e}"),
        code: 1,
    })?;
    Ok(Outcome {
        stdout: String::new(),
        stderr: String::new(),
        code: status.code().unwrap_or(1),
    })
}
