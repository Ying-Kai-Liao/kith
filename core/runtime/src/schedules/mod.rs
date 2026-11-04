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
// The fire decides with the authority tables, which are Unix only for now.
#[cfg(unix)]
mod fire;
#[cfg(all(test, unix))]
mod fire_tests;
#[cfg(unix)]
mod hooks;
#[cfg(all(test, unix))]
mod hooks_tests;
#[cfg(unix)]
mod sync;
// The import it is tested with is Unix-only for now (like the authority one).
#[cfg(all(test, unix))]
mod tests;

use std::path::{Path, PathBuf};

use kith_kernel::paths::Paths;
use kith_kernel::schedule::{Trigger, Zone};
use serde_json::Value;

use crate::error::{Error, Result};

#[cfg(all(test, unix))]
pub(crate) use call::call_with;
pub(crate) use call::{call, serves};
#[cfg(unix)]
pub(crate) use fire::{call_write, serves_write};
#[cfg(unix)]
pub(crate) use hooks::key_name as hook_key_name;

/// The rule `next` needs: when a job runs after `now_ms`, as the local minute `YYYY-MM-DDTHH:MM` that `kith-sched`
/// prints, or `None` when it never does. It gets the job's definition, so the zone and the trigger are the job's own.
/// The kernel's Trigger (M4-1) is the real one.
pub(crate) trait Rule {
    fn next(&self, job: &Value, now_ms: i64) -> std::result::Result<Option<String>, RuleError>;
}

/// Why a rule gave no time.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RuleError {
    /// The cron rule is not wired into the core yet: not a fault of the job.
    NotWired,
    /// The job's cron is no cron.
    Bad(String),
}

/// The job's trigger: its `trigger` object when it has one, else the `cron` of a `kith-sched` job (follows the Mac unless
/// the job names a `tz`). The fire and `schedule.jobs` both ask it, so their `next` is the same.
pub(crate) fn trigger_of(def: &Value) -> std::result::Result<Trigger, String> {
    let t = if let Some(t) = def.get("trigger") {
        serde_json::from_value::<Trigger>(t.clone()).map_err(|e| format!("trigger: {e}"))?
    } else {
        let expr = def
            .get("cron")
            .and_then(Value::as_str)
            .ok_or("no cron or trigger")?;
        let tz = match def.get("tz").and_then(Value::as_str) {
            Some(z) => Zone::parse(z).map_err(|e| e.0)?,
            None => Zone::Local,
        };
        Trigger::Cron {
            expr: expr.to_owned(),
            tz,
        }
    };
    t.validate().map_err(|e| e.0)?;
    Ok(t)
}

/// The kernel's Trigger (M4-1) as the rule `next` asks, built the way the fire builds it.
pub(crate) struct TriggerRule;

impl Rule for TriggerRule {
    fn next(&self, job: &Value, now_ms: i64) -> std::result::Result<Option<String>, RuleError> {
        let t = trigger_of(job).map_err(RuleError::Bad)?;
        Ok(t.next_after(now_ms).and_then(local_minute))
    }
}

/// The local minute `YYYY-MM-DDTHH:MM` of an instant, as `kith-sched` prints it.
fn local_minute(ms: i64) -> Option<String> {
    chrono::DateTime::from_timestamp_millis(ms).map(|t| {
        t.with_timezone(&chrono::Local)
            .format("%Y-%m-%dT%H:%M")
            .to_string()
    })
}

/// Stands in where no rule is wired (a test of the listing alone): every job's `next` is unknown, and says so.
pub(crate) struct NoRule;

impl Rule for NoRule {
    fn next(&self, _job: &Value, _now_ms: i64) -> std::result::Result<Option<String>, RuleError> {
        Err(RuleError::NotWired)
    }
}

/// What a fire decided: `fired`, or `skipped:<reason>`. Checked here, not in SQL, so a new reason needs no rebuild.
pub(crate) fn valid_fire_outcome(s: &str) -> bool {
    s == "fired"
        || s.strip_prefix("skipped:")
            .is_some_and(|r| !r.trim().is_empty())
}

/// The states of a run. Checked here, not in SQL, since firing (M4-3) grows the set.
pub(crate) fn valid_run_state(s: &str) -> bool {
    matches!(s, "started" | "done" | "failed" | "skipped")
}

/// A slot: a UTC instant in whole minutes (`2026-11-04T00:00:00Z`), `manual:<id>` (a run started by hand) or
/// `event:<id>` (a webhook or event delivery).
pub(crate) fn valid_slot(s: &str) -> bool {
    if let Some(id) = s
        .strip_prefix("manual:")
        .or_else(|| s.strip_prefix("event:"))
    {
        return !id.is_empty()
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b));
    }
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%SZ")
        .is_ok_and(|t| chrono::Timelike::second(&t) == 0)
}

/// The slot of a local minute as `kith-sched` writes it (`YYYY-MM-DDTHH:MM`, this Mac's local time): the UTC instant.
/// `None` for text that is no minute, or a local time that did not exist (the clock skipped it). A time that happened
/// twice (the clock went back) is the earlier one.
pub(crate) fn slot_of_local(local: &str) -> Option<String> {
    use chrono::TimeZone as _;
    let naive = chrono::NaiveDateTime::parse_from_str(local, "%Y-%m-%dT%H:%M").ok()?;
    let t = chrono::Local.from_local_datetime(&naive).earliest()?;
    Some(
        t.with_timezone(&chrono::Utc)
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string(),
    )
}

/// The slot text of an instant in ms (whole minutes; `None` out of range).
pub(crate) fn slot_of_ms(ms: i64) -> Option<String> {
    chrono::DateTime::from_timestamp_millis(ms).map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
}

/// The instant of a timed slot in ms; `None` for `manual:` and `event:` slots and for text that is no slot.
pub(crate) fn ms_of_slot(slot: &str) -> Option<i64> {
    chrono::NaiveDateTime::parse_from_str(slot, "%Y-%m-%dT%H:%M:%SZ")
        .ok()
        .map(|t| t.and_utc().timestamp_millis())
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
    for _ in 0..3 {
        match std::fs::read_to_string(&path) {
            Ok(text) if text.trim().is_empty() => {
                // Left empty by something other than this function (which never shows a reader a partial file):
                // nobody can have used it, so it is repaired. Another repairer may win; the next read says which.
                let _ = std::fs::remove_file(&path);
            }
            Ok(text) => return valid_mac_id(&path, text.trim()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(Error::Io(format!("{}: {e}", path.display()))),
        }
        make_mac_id(machine, &path)?;
    }
    Err(Error::Io(format!(
        "{}: could not settle on a machine id",
        path.display()
    )))
}

/// Writes the id to a private temp file and hard-links it into place: the name appears whole or not at all, and of two
/// creators exactly one links (the other finds the name taken and reads the winner's id on the next turn).
fn make_mac_id(machine: &Path, path: &Path) -> Result<()> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|_| Error::Io("kith-core: random bytes unavailable".into()))?;
    let id = bytes.iter().fold(String::new(), |mut s, b| {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
        s
    });
    std::fs::create_dir_all(machine)?;
    let tmp = machine.join(format!(".{MAC_ID_FILE}.{}.{id}.tmp", std::process::id()));
    {
        use std::io::Write as _;
        let mut f = create_new_private(&tmp)?;
        f.write_all(id.as_bytes())?;
        f.sync_all()?;
    }
    let linked = std::fs::hard_link(&tmp, path);
    let _ = std::fs::remove_file(&tmp);
    match linked {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
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
    /// Hash of the file's text as read (of no text when there is no file); empty when it could not be read.
    pub hash: String,
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
                Ok((hash, jobs)) => AgentJobs {
                    agent,
                    jobs,
                    hash,
                    problem: None,
                },
                Err(e) => AgentJobs {
                    agent,
                    jobs: Vec::new(),
                    hash: String::new(),
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

pub(crate) fn sha_hex(bytes: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut s, b| {
            use std::fmt::Write as _;
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// Where an agent's jobs live (`jobs_file` of kith-sched).
pub(crate) fn jobs_file(paths: &Paths, agent: &str) -> PathBuf {
    let pkg = paths.agents.join(agent);
    if pkg.join("role").is_dir() {
        pkg.join("jobs.json")
    } else {
        paths.state.join("agents").join(agent).join("jobs.json")
    }
}

/// The jobs in `file` (none when there is no file) and the hash of the text they were read from, read under the shared
/// side of kith-sched's `<agent>.jobs.lock` when that lock file exists. Never creates the lock: a reader leaves nothing
/// behind. The fire compares the hash with the index's (`sync`).
pub(crate) fn read_jobs(
    machine: &Path,
    agent: &str,
    file: &Path,
) -> std::result::Result<(String, Vec<Value>), String> {
    let lock = std::fs::File::open(machine.join("sched").join(format!("{agent}.jobs.lock"))).ok();
    if let Some(l) = &lock {
        // A writer holds it for a moment; a lock that cannot be taken (an odd filesystem) is read without.
        let _ = fs2::FileExt::lock_shared(l);
    }
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok((sha_hex(b""), Vec::new()));
        }
        Err(e) => return Err(e.to_string()),
    };
    drop(lock);
    let hash = sha_hex(text.as_bytes());
    let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let list = v.as_array().ok_or("not a list of jobs")?;
    if list
        .iter()
        .all(|j| j.as_object().is_some_and(|o| o.contains_key("id")))
    {
        Ok((hash, list.clone()))
    } else {
        Err("not a list of jobs".into())
    }
}
