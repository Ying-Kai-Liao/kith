//! Schedules in the core (M4-2; docs/plans/runtime-design.md section 2, core-design section 9): an index over the
//! agents' `jobs.json` files joined with the run state in core.db (`sched_tick`, `sched_fires`, `sched_runs`).
//!
//! - Definitions stay in `<agent>/jobs.json` (architect, 2026-11-04): they version and sync with the agent. This module
//!   only READS them, where they live, under the same `<machine>/sched/<agent>.jobs.lock` that `kith-sched` writes under.
//!   It never writes a jobs.json, and until M4-5 `kith-sched` is the only writer of both the file and its state files.
//! - Run state is core.db's. Nothing here writes it but the import (`import/schedules.rs`); firing is M4-3.
//! - A thing that is wrong with one agent is said about that agent (`problems`), never a failure of the listing: an agent
//!   folder that is not mounted, a jobs.json that is missing or malformed.
//! - `next` is the cron rule's, which lives in the kernel (`core/kernel/src/schedule/`, M4-1). Until that is wired in,
//!   [`NoRule`] answers, and a row's `next` is `null` with a note; the wiring is one `Rule` impl and one argument.

mod call;
// The import it is tested with is Unix-only for now (like the authority one).
#[cfg(all(test, unix))]
mod tests;

use std::path::{Path, PathBuf};

use kith_kernel::paths::Paths;
use serde_json::Value;

use crate::error::{Error, Result};

#[cfg(test)]
pub(crate) use call::call_with;
pub(crate) use call::{call, serves};

/// The cron rule `next` needs: when a job runs after `now_ms`, as the local minute `YYYY-MM-DDTHH:MM` that
/// `kith-sched` prints, or `None` when it never does. The kernel's Trigger (M4-1) is the real one.
pub(crate) trait Rule {
    fn next(&self, cron: &str, now_ms: i64) -> std::result::Result<Option<String>, String>;
}

/// Stands in until the kernel's Trigger is wired: every job's `next` is unknown, and says so.
pub(crate) struct NoRule;

impl Rule for NoRule {
    fn next(&self, _cron: &str, _now_ms: i64) -> std::result::Result<Option<String>, String> {
        Err("the cron rule is not wired into the core yet".into())
    }
}

/// The file in the machine folder that names this machine, for the `mac` field of schedule rows and, later, of a
/// job. Random, made owner-only (0600) the first time it is needed, and never changed after: a copy of the state
/// folder to another Mac is the user's to rename.
///
/// It sits in the machine folder (the part of `~/.kith` that is not synced), not the synced state folder: a synced
/// id would be one id for every Mac, which is the opposite of what it is for.
pub(crate) const MAC_ID_FILE: &str = "mac-id";

/// This machine's id, made when missing. 32 hex characters. A file that exists but holds something else is refused
/// rather than replaced, since every row written under the old id would be orphaned.
pub(crate) fn mac_id(machine: &Path) -> Result<String> {
    let path = machine.join(MAC_ID_FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => return valid_mac_id(&path, text.trim()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(Error::Io(format!("{}: {e}", path.display()))),
    }
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|_| Error::Io("kith-core: random bytes unavailable".into()))?;
    let id = bytes.iter().fold(String::new(), |mut s, b| {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
        s
    });
    std::fs::create_dir_all(machine)?;
    match create_new_private(&path) {
        Ok(mut f) => {
            use std::io::Write as _;
            f.write_all(id.as_bytes())?;
            Ok(id)
        }
        // Another process made it between the read and now: its id is the one.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            valid_mac_id(&path, std::fs::read_to_string(&path)?.trim())
        }
        Err(e) => Err(Error::Io(format!("{}: {e}", path.display()))),
    }
}

fn valid_mac_id(path: &Path, id: &str) -> Result<String> {
    if id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(id.to_owned())
    } else {
        Err(Error::Refused(format!(
            "{} does not hold a machine id (32 hex characters); not replacing it. Remove it to make a new one.",
            path.display()
        )))
    }
}

fn create_new_private(path: &Path) -> std::io::Result<std::fs::File> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        o.mode(0o600);
    }
    o.open(path)
}

/// What was found for one agent's jobs.
#[derive(Debug)]
pub(crate) struct AgentJobs {
    pub agent: String,
    /// The jobs in the file in file order; empty when there is none or it could not be read.
    pub jobs: Vec<Value>,
    /// Why the file could not be read as a list of jobs, if so. A missing file is not a problem: it is no jobs.
    pub problem: Option<String>,
}

/// Every agent's jobs.
#[derive(Debug)]
pub(crate) struct Index {
    /// False when the agents folder is not there (an unmounted volume): the listing is then empty and says so.
    pub mounted: bool,
    pub agents: Vec<AgentJobs>,
    /// The one agent asked for, if any.
    pub only: Option<String>,
}

/// The agents folder's jobs, as `kith-sched` finds them: agents are the folders holding an `agent.md` (or
/// `role/agent.md`), and an agent with a `role` folder keeps `jobs.json` beside it.
pub(crate) fn index(paths: &Paths, only: Option<&str>) -> Index {
    let Ok(entries) = std::fs::read_dir(&paths.agents) else {
        return Index {
            mounted: false,
            agents: Vec::new(),
            only: only.map(str::to_owned),
        };
    };
    let mut names: Vec<String> = entries
        .filter_map(std::result::Result::ok)
        .filter(|e| {
            let p = e.path();
            p.join("agent.md").is_file() || p.join("role").join("agent.md").is_file()
        })
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| only.is_none_or(|o| o == n))
        .collect();
    names.sort();
    let agents = names
        .into_iter()
        .map(|agent| {
            let file = jobs_file(paths, &agent);
            match read_jobs(&paths.machine, &agent, &file) {
                Ok(jobs) => AgentJobs {
                    agent,
                    jobs,
                    problem: None,
                },
                Err(e) => AgentJobs {
                    agent,
                    jobs: Vec::new(),
                    problem: Some(format!("{}: {e}", file.display())),
                },
            }
        })
        .collect();
    Index {
        mounted: true,
        agents,
        only: only.map(str::to_owned),
    }
}

/// Where an agent's jobs live (`jobs_file` of kith-sched).
fn jobs_file(paths: &Paths, agent: &str) -> PathBuf {
    let pkg = paths.agents.join(agent);
    if pkg.join("role").is_dir() {
        pkg.join("jobs.json")
    } else {
        paths.state.join("agents").join(agent).join("jobs.json")
    }
}

/// The jobs in `file` (none when there is no file), read under the shared side of kith-sched's `<agent>.jobs.lock`
/// when that lock file exists. Never creates the lock: a reader leaves nothing behind.
fn read_jobs(machine: &Path, agent: &str, file: &Path) -> std::result::Result<Vec<Value>, String> {
    let lock = std::fs::File::open(machine.join("sched").join(format!("{agent}.jobs.lock"))).ok();
    if let Some(l) = &lock {
        // A writer holds it for a moment; a lock that cannot be taken (an odd filesystem) is read without.
        let _ = fs2::FileExt::lock_shared(l);
    }
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.to_string()),
    };
    drop(lock);
    let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let list = v.as_array().ok_or("not a list of jobs")?;
    if list
        .iter()
        .all(|j| j.as_object().is_some_and(|o| o.contains_key("id")))
    {
        Ok(list.clone())
    } else {
        Err("not a list of jobs".into())
    }
}
