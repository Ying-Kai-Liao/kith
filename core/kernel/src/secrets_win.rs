//! The Windows build of `secrets`: only the pieces other modules stand on. The access decision itself (auth.json with its
//! flock and modes, the password database, `kith-auth`) stays Unix-only until retire-python P3 puts it in the core; nothing
//! that spawns `kith-auth` is reachable from here, and the callers that would are refused by name where they start.

#[path = "secrets/cmd.rs"]
mod cmd;
#[path = "secrets/pyjson.rs"]
pub mod pyjson;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

pub use cmd::run_command_with_timeout;

// ---- what the launch asks of `kith-auth`, and the answers it gets here ---------------------------------------------------
//
// The launch's plan asks `kith-auth` three things: who a run is for (`resolve`: an invited person, a bot guest, a room's
// people), the shared services an agent is granted (`mcp-config`), and the account's home. On Windows there is no `kith-auth`
// (retire-python P3-1 puts the people and the grants in the core), so the first is a refusal by name (the launch also refuses
// a guest before it asks, with the sentence the owner reads) and the second leaves the run without shared services, saying so.

/// What the launch needs to know about this run, of the full module's `Ctx`: the person it is for.
#[derive(Clone, Debug, Default)]
pub struct Ctx {
    /// `KITH_PERSON`: unset or empty is the owner.
    pub person: Option<String>,
}

impl Ctx {
    pub fn from_process(_kernel: &Path) -> Ctx {
        Ctx {
            person: std::env::var("KITH_PERSON").ok().filter(|v| !v.is_empty()),
        }
    }
}

/// What a `kith-auth` command prints and exits with.
#[derive(Debug, PartialEq, Eq)]
pub struct Outcome {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}

/// Every verb: refused, naming the package that ports it.
pub fn run(_c: &Ctx, verb: &str, _args: &[String]) -> Outcome {
    Outcome {
        stdout: String::new(),
        stderr: format!(
            "kith-auth {verb}: not on Windows yet (people, members and grants wait retire-python P3-1)\n"
        ),
        code: 1,
    }
}

/// A refusal `kith-auth` would `die` with: its message and exit code.
#[derive(Debug, PartialEq, Eq)]
pub struct Die {
    pub msg: String,
    pub code: i32,
}

/// How a shared service's server is started (decided once per launch).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Starter {
    Script,
    Program(String),
}

/// The servers of the shared services a run is granted, and what to say about the ones left out.
pub struct McpConfig {
    pub config: Value,
    pub notes: Vec<String>,
}

pub fn exec_program(
    _kith_bin: Option<&str>,
    _secrets_exec: Option<&str>,
    _lists: impl Fn(&Path) -> bool,
) -> Starter {
    Starter::Script
}

/// The shared services need the keys (`kith-auth`'s store), which Windows has no reader for yet: the run goes on with none.
pub fn mcp_config(
    _c: &Ctx,
    _env: &HashMap<String, String>,
    _agent: &str,
    _starter: &Starter,
) -> Result<McpConfig, Die> {
    Err(Die {
        msg: "shared services are not on Windows yet (retire-python P3-1)".to_owned(),
        code: 1,
    })
}

/// The account's home folder: `%USERPROFILE%` (an absolute one), or `$HOME` where Git Bash sets it.
pub(crate) fn os_home() -> Option<PathBuf> {
    let home = crate::paths::process_home();
    (!home.as_os_str().is_empty() && home.is_absolute()).then_some(home)
}

/// Run a program and read its standard output; `None` when it can't start, outlives `limit`, or fails.
pub(crate) fn run_with_timeout(program: &Path, args: &[&str], limit: Duration) -> Option<String> {
    let mut cmd = std::process::Command::new(program);
    cmd.args(args);
    run_command_with_timeout(cmd, limit)
}
