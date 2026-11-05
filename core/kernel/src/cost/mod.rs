//! `kith kernel cost <verb>`: the verbs of `kernel/bin/kith-cost` that the launch and the apps call, in Rust: `limit`,
//! `spent`, `usage --json`, `agent NAME --json`, `members`, `bots --json` and `sync`. The same command line gives the same
//! output and exit code (the difftest golden `cost.json` holds the Python's answers); `kith-cost` itself stays Python until
//! `cost` is in `lib/core-switched.txt`, and the verbs that print a report for a person (`today`, `agents`, `people`,
//! `session`, `csv`, ...) and the hooks that write the ledger (`tag`, `record`) have no Rust side yet.
//!
//! The ledger (`<machine>/cost.db`) is read from here and written only by `sync`, which copies new requests out of Claude
//! Code's transcripts, as the Python does before every verb. `KITH_COST_NOW` (ISO local time, or epoch seconds) is the
//! clock for tests.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::similar_names,
    // Python compares floats with `==` (a limit against a round amount, a rounded figure against its integer): so does this.
    clippy::float_cmp
)]

mod clock;
mod ledger;
mod pyfmt;
#[cfg(test)]
mod tests;
mod views;

use std::path::{Path, PathBuf};

pub use clock::Now;
pub use ledger::price;

/// `kith-cost --help` and the text it dies with on a line it does not know.
const USAGE: &str = include_str!("usage.txt");

/// Where the ledger and what it reads live.
#[derive(Clone, Debug)]
pub struct Paths {
    /// `KITH_MACHINE`: this Mac's own folder (`cost.db`, `settings.json`, `bridge/`, extra Claude accounts).
    pub machine: PathBuf,
    /// The account's home: Claude Code keeps its transcripts under `~/.claude/projects`.
    pub home: PathBuf,
    /// The kernel folder: `bin/kith-auth` answers the budgets.
    pub kernel: PathBuf,
}

impl Paths {
    /// This process: the folders the kernel finds from its environment.
    pub fn from_process(kernel: &Path) -> Paths {
        Paths {
            machine: crate::paths::from_process(kernel).machine,
            home: crate::paths::process_home(),
            kernel: kernel.to_path_buf(),
        }
    }

    fn db(&self) -> PathBuf {
        self.machine.join("cost.db")
    }

    fn projects(&self) -> PathBuf {
        self.home.join(".claude/projects")
    }

    fn accounts_claude(&self) -> PathBuf {
        self.machine.join("accounts/claude")
    }

    fn bridge_state(&self) -> PathBuf {
        self.machine.join("bridge/state.json")
    }
}

/// What a run prints and exits with.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Out {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}

impl Out {
    fn ok(stdout: String) -> Out {
        Out {
            stdout,
            ..Out::default()
        }
    }

    fn fail(stderr: String, code: i32) -> Out {
        Out {
            stderr,
            code,
            ..Out::default()
        }
    }

    /// `sys.exit(__doc__)`: the usage on stderr, exit 1.
    fn usage() -> Out {
        Out::fail(format!("{USAGE}\n"), 1)
    }
}

/// The environment a run reads besides the command line.
#[derive(Clone, Debug, Default)]
pub struct Env {
    /// `KITH_COST_NOW`.
    pub now: Option<String>,
    /// `KITH_PERSON_WAS`: a person's old ids, comma separated.
    pub person_was: String,
}

impl Env {
    pub fn from_process() -> Env {
        Env {
            now: crate::paths::process_env("KITH_COST_NOW"),
            person_was: crate::paths::process_env("KITH_PERSON_WAS").unwrap_or_default(),
        }
    }
}

/// A verb of `kith-cost` that has no Rust side yet.
fn unported(verb: &str) -> Out {
    Out::fail(
        format!(
            "kith kernel cost: `{verb}` is not in the core yet (kernel/bin/kith-cost answers it)\n"
        ),
        2,
    )
}

fn broken(e: &dyn std::fmt::Display) -> Out {
    Out::fail(format!("kith-cost: {e}\n"), 1)
}

/// `kith-cost ARGS...` (`main()`): the verbs above; any other line is answered as the Python answers it, or says it is not
/// in the core yet.
pub fn run(p: &Paths, env: &Env, args: &[String]) -> Out {
    let week = ["week".to_owned()];
    let given: &[String] = if args.is_empty() { &week } else { args };
    let a: Vec<&str> = given.iter().map(String::as_str).collect();
    if matches!(a[0], "-h" | "--help") {
        return Out::ok(format!("{USAGE}\n"));
    }
    let now = match Now::from_env(env.now.as_deref()) {
        Ok(n) => n,
        Err(e) => return broken(&e.0),
    };
    if a[0] == "limit" && a.len() > 1 {
        // before the ledger: no limit answers without opening it
        return limit(p, &now, a[1], a.get(2).copied().unwrap_or(""));
    }
    if matches!(a[0], "tag" | "record") {
        return unported(a[0]);
    }
    let json = a.contains(&"--json");
    // the verbs that print a report for a person
    let report = match a[0] {
        "session" => a.len() > 1,
        "agents" | "people" | "csv" | "today" | "week" | "month" | "all" => true,
        "agent" => a.len() > 1 && !json,
        _ => false,
    };
    if report {
        return unported(a[0]);
    }
    let mut c = match ledger::open(p) {
        Ok(c) => c,
        Err(e) => return broken(&e),
    };
    if a[0] == "sync" {
        return match ledger::sync(&mut c, p, false) {
            Ok(line) => Out::ok(format!("{}\n", line.unwrap_or_default())),
            Err(e) => broken(&e),
        };
    }
    if let Err(e) = ledger::sync(&mut c, p, true) {
        return broken(&e);
    }
    let said = match a[0] {
        "usage" if json => {
            let period = a[1..]
                .iter()
                .find(|x| **x != "--json")
                .copied()
                .unwrap_or("month");
            views::usage_json(&c, p, &now, period)
        }
        "agent" if a.len() > 1 && json => {
            let rest: Vec<&str> = a[2..].iter().filter(|x| **x != "--json").copied().collect();
            views::agent_json(&c, p, &now, a[1], rest.first().copied().unwrap_or("month"))
        }
        "members" if a.len() > 1 => views::members(&c, p, &now, a[1]),
        "bots" if json => match views::bots_json(&c, p, &now) {
            Ok(Some(text)) => Ok(text),
            Ok(None) => {
                return Out::fail(
                    "kith-cost: kith-auth bots-cap did not answer\n".to_owned(),
                    1,
                );
            }
            Err(e) => Err(e),
        },
        "spent" if a.len() > 1 => views::spent(&c, &now, a[1], a.get(2).copied(), &env.person_was),
        _ => return Out::usage(),
    };
    match said {
        Ok(text) => Out::ok(text),
        Err(e) => broken(&e),
    }
}

/// `limit AGENT [--json|--sh]`: where the agent stands against its monthly limit. No limit answers at once, without a sync.
fn limit(p: &Paths, now: &Now, agent: &str, fmt: &str) -> Out {
    let lim = views::limit_of(p, agent);
    if lim.amount.is_none() {
        if fmt == "--sh" {
            return Out::default();
        }
        let none = serde_json::json!({"agent": agent, "limit": null, "at": lim.at, "over": false});
        return Out::ok(format!("{}\n", crate::secrets::pyjson::dumps(&none)));
    }
    let mut c = match ledger::open(p) {
        Ok(c) => c,
        Err(e) => return broken(&e),
    };
    if let Err(e) = ledger::sync(&mut c, p, true) {
        return broken(&e);
    }
    match views::limit_cmd(&c, now, agent, &lim, fmt) {
        Ok(text) => Out::ok(text),
        Err(e) => broken(&e),
    }
}
