//! `kith kernel permit ...`: the command-line verbs of `kernel/bin/kith-permit`, the permission prompt tool of every `claude -p`
//! run, in Rust: `issue AGENT SESSION PID`, `denied --dir DIR`, `once --dir DIR` and `apply SETTINGS_FILE`. The same command
//! line prints the same thing and exits with the same code (the difftest golden `permit.json` holds the Python's answers);
//! `kith-permit` itself stays Python until `permit` is in `lib/core-switched.txt`.
//!
//! Any other command line starts the stdio MCP server (`mcp.rs`): `approve`, `request_access`, `request_secret`, `report_bug`,
//! `ask_helper` and `run_door`, the permission prompt tool of every `claude -p` run. `bug.rs` and `door.rs` still lean on a Python
//! helper for what has no Rust side (the scrubbing of a report, the door's sandbox).
//!
//! Where Python dies with a traceback (`issue` with a pid that is no number, a hook payload that is not UTF-8) this prints the
//! last line of it (or reads the bytes lossily) and exits 1 (or goes on).

#![allow(
    clippy::doc_markdown,
    clippy::format_push_string,
    clippy::format_collect,
    clippy::cast_possible_truncation
)]

mod access;
mod apply;
mod bug;
mod decide;
mod door;
mod helper;
mod hooks;
pub use hooks::{NO_VERDICT, ONCE_KEEP_S, SHORT_ONCE_S, host_once_key};
mod host;
mod issue;
mod mcp;
#[cfg(test)]
mod mcp_tests;
mod proc;
mod pyval;
mod scrub;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

/// What a run prints and exits with.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Out {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}

/// The environment a run reads (Python's `os.environ`) and the home folder.
#[derive(Clone, Debug, Default)]
pub struct Env {
    vars: BTreeMap<String, String>,
    pub home: PathBuf,
    /// The Python the helper calls run (`python3` when empty).
    pub python: String,
}

impl Env {
    /// This process.
    pub fn from_process() -> Env {
        Env {
            vars: std::env::vars_os()
                .map(|(k, v)| {
                    (
                        k.to_string_lossy().into_owned(),
                        v.to_string_lossy().into_owned(),
                    )
                })
                .collect(),
            home: crate::paths::process_home(),
            python: String::new(),
        }
    }

    /// An environment of exactly these variables (the owner's pair for `denied`).
    pub fn with(pairs: &[(&str, &str)]) -> Env {
        Env {
            vars: pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            home: PathBuf::new(),
            python: String::new(),
        }
    }

    /// Every variable, empty ones too (what a child process is given).
    pub fn vars(&self) -> &BTreeMap<String, String> {
        &self.vars
    }

    /// This environment without these variables.
    #[must_use]
    pub fn without(&self, names: &[&str]) -> Env {
        let mut e = self.clone();
        for n in names {
            e.vars.remove(*n);
        }
        e
    }

    pub fn python(&self) -> &str {
        if self.python.is_empty() {
            "python3"
        } else {
            &self.python
        }
    }

    pub fn set(&mut self, k: &str, v: &str) {
        self.vars.insert(k.to_owned(), v.to_owned());
    }

    /// `os.environ.get(k)` where an empty value counts as unset (every use is `... or ...` or a truth test).
    pub fn get(&self, k: &str) -> Option<&str> {
        self.vars
            .get(k)
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    }

    /// `kith_paths.paths()` for this environment.
    fn paths(&self, kernel: &Path) -> crate::paths::Paths {
        crate::paths::paths(&self.home, kernel, &|n: &str| self.vars.get(n).cloned())
    }
}

/// The verb and its arguments, `kith-permit`'s own `sys.argv` matching. `stdin` is read only by the two hooks (and, for a
/// line that is no verb, to see whether the MCP server would have had a message).
pub fn run(env: &Env, kernel: &Path, args: &[String], stdin: &mut dyn Read) -> Out {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match a.as_slice() {
        ["issue", agent, session, pid] => issue::issue(env, kernel, agent, session, pid),
        [hook @ ("denied" | "once"), "--dir", dir] => {
            let mut raw = Vec::new();
            let _ = stdin.read_to_end(&mut raw);
            let raw = String::from_utf8_lossy(&raw);
            let out = if *hook == "denied" {
                hooks::denied(&raw, Path::new(dir), env)
            } else {
                hooks::once(&raw, Path::new(dir), env)
            };
            Out {
                stdout: if out.is_empty() {
                    String::new()
                } else {
                    format!("{out}\n")
                },
                ..Out::default()
            }
        }
        ["apply", file] => apply::apply(env, kernel, file),
        _ => {
            // the MCP server, with its replies collected (the program streams them: `serve_stdio`)
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let code = mcp::serve(
                env,
                kernel,
                &mut std::io::BufReader::new(stdin),
                &mut out,
                &mut err,
            );
            Out {
                stdout: String::from_utf8_lossy(&out).into_owned(),
                stderr: String::from_utf8_lossy(&err).into_owned(),
                code,
            }
        }
    }
}

/// Whether `args` is one of the four command-line verbs (anything else starts the MCP server, as `kith-permit`'s `sys.argv`
/// matching does).
pub fn is_verb(args: &[String]) -> bool {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    matches!(
        a.as_slice(),
        ["issue", _, _, _] | ["denied" | "once", "--dir", _] | ["apply", _]
    )
}

/// The MCP server on this process's stdin and stdout: each reply is written and flushed as soon as its line has been read.
pub fn serve_stdio(env: &Env, kernel: &Path) -> i32 {
    mcp::serve(
        env,
        kernel,
        &mut std::io::stdin().lock(),
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    )
}
