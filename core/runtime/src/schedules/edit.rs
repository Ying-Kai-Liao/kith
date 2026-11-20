//! `schedule.put`, `schedule.drop` and `schedule.setPaused` (the write verbs of `kith schedule` once the schedules are the
//! core's): what `kith-sched add`, `rm`, `pause` and `resume` do to an agent's `jobs.json`, done by the core.
//!
//! The definitions stay in the file (see the module doc). Each verb edits it under the exclusive side of the same
//! `<machine>/sched/<agent>.jobs.lock` `kith-sched` takes, replaces it whole (temp file, synced, renamed) with the list as
//! the edit left it, and then brings the index up to the file in one claim transaction ([`super::fire::settle`]), so a job
//! that was paused or removed has its waiting turns cancelled now, not at the next fire. A file that cannot be read is
//! refused, never overwritten as if it were empty. The fire is untouched: it still re-reads the file's hash before it
//! decides anything.

use std::path::Path;
use std::time::Duration;

use kith_kernel::paths::Paths;
use serde_json::{Map, Value, json};

use super::call::{known_agent, row_of};
use super::jobs_io::{lock_jobs, write_list};
use super::{jobs_file, trigger_of};
use crate::db::Core;
use crate::error::{Error, Result};

/// The rows this module answers.
pub(crate) fn serves(method: &str) -> bool {
    matches!(
        method,
        "schedule.put" | "schedule.drop" | "schedule.setPaused"
    )
}

/// How long an edit waits for another writer of the same jobs.json (a routine import, `kith-sched`, another edit).
const LOCK_WAIT: Duration = Duration::from_secs(10);

/// What `kith-sched add -e` accepts.
const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

fn bad(what: impl std::fmt::Display) -> Error {
    Error::BadParams(format!("kith-core: schedule: {what}"))
}

/// Answers a write verb on this core, as the caller's view of the folders sees it.
pub(crate) fn call(core: &mut Core, method: &str, params: &Value, now: i64) -> Result<Value> {
    let (paths, _) = core.folders_for(params)?;
    let text = |k: &str| params.get(k).and_then(Value::as_str);
    let need = |k: &str| {
        text(k)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| bad(format!("{k} is required")))
    };
    let agent = need("agent")?;
    // Same refusal as the listing: the agents there are, named.
    known_agent(&paths, Some(agent))?;
    // An unmounted agents folder knows no agent, and the write would make the folder: refuse, as the listing says it is off.
    if !paths.agents.is_dir() {
        return Err(Error::Refused(
            "kith-core: schedule: the agents folder is not mounted".into(),
        ));
    }
    let mut put = None;
    let mut out = match method {
        "schedule.put" => {
            let job = new_job(params)?;
            let id = text_of(&job, "id");
            let out = edit(&paths, agent, |jobs| {
                let replaced = jobs.iter().any(|j| text_of(j, "id") == id);
                // A job the routine import made is the routine's: replacing it would leave its webhook key behind and a
                // rollback would bring the routine back over it. The import refuses the same collision.
                if jobs
                    .iter()
                    .any(|j| text_of(j, "id") == id && j.get("routine").is_some())
                {
                    return Err(Error::Refused(format!(
                        "kith-core: schedule: {agent}/{id} was imported from a routine; give the new job another --id (the routine is changed in the app)"
                    )));
                }
                jobs.retain(|j| text_of(j, "id") != id);
                jobs.push(job.clone());
                Ok(json!({"replaced": replaced}))
            })?;
            put = Some(job);
            out
        }
        "schedule.drop" => {
            let id = need("id")?;
            edit(&paths, agent, |jobs| {
                let at = find(jobs, agent, id)?;
                // A rollback rebuilds the routines from the jobs still in the files: a dropped one would stay enabled in
                // serve.db and run again, its webhook key left behind. Pausing is the way to stop it.
                if jobs[at].get("routine").is_some() {
                    return Err(Error::Refused(format!(
                        "kith-core: schedule: {agent}/{id} was imported from a routine; pause it instead (a routine with several triggers is paused in the app, a pause of one trigger is lost on rollback)"
                    )));
                }
                jobs.remove(at);
                Ok(json!({"changed": true}))
            })?
        }
        _ => {
            let id = need("id")?;
            let pause = params
                .get("paused")
                .and_then(Value::as_bool)
                .ok_or_else(|| bad("paused is required (true or false)"))?;
            edit(&paths, agent, |jobs| {
                let at = find(jobs, agent, id)?;
                let job = jobs[at]
                    .as_object_mut()
                    .ok_or_else(|| bad("the job is not an object"))?;
                let was = job
                    .get("paused")
                    .is_some_and(|p| !matches!(p, Value::Null | Value::Bool(false)));
                job.shift_remove("paused");
                if pause {
                    job.insert("paused".into(), json!(true));
                }
                let mut out = json!({"changed": was != pause});
                if let Some(w) = mixed_triggers(jobs, at) {
                    out["warning"] = json!(w);
                }
                Ok(out)
            })?
        }
    };
    // Said, not hidden: the write is done either way, and a reason here is the next fire's to put right.
    let indexed = settle_agent(core, &paths, agent, now);
    out["indexed"] = json!(indexed.is_none());
    if let Some(why) = indexed {
        out["indexNote"] = json!(why);
    }
    out["ok"] = json!(true);
    out["agent"] = json!(agent);
    match put {
        Some(job) => out["job"] = row_of(core, &paths, agent, &job)?,
        None => out["id"] = json!(need("id")?),
    }
    Ok(out)
}

fn text_of(job: &Value, key: &str) -> String {
    match job.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

/// `kith-sched`'s slug of a task, the id of a job added without `--id`: runs of anything but a-z and 0-9 become one
/// dash, the ends are stripped, then it is cut to 24 characters (a cut may end on a dash, as it does there).
pub(super) fn slug(task: &str) -> String {
    let mut out = String::new();
    for c in task.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let cut: String = out.trim_matches('-').chars().take(24).collect();
    if cut.is_empty() { "job".into() } else { cut }
}

/// The job `schedule.put` writes, in the order `kith-sched` writes its keys.
fn new_job(params: &Value) -> Result<Value> {
    let text = |k: &str| {
        params
            .get(k)
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
    };
    let cron = text("cron").ok_or_else(|| bad("cron is required"))?;
    let task = text("task").ok_or_else(|| bad("task is required"))?;
    trigger_of(&json!({"cron": cron})).map_err(bad)?;
    if let Some(e) = text("effort")
        && !EFFORTS.contains(&e)
    {
        return Err(bad(format!("effort must be one of {}", EFFORTS.join(", "))));
    }
    let mut job = Map::new();
    job.insert(
        "id".into(),
        json!(text("id").map_or_else(|| slug(task), str::to_owned)),
    );
    job.insert("cron".into(), json!(cron));
    job.insert("task".into(), json!(task));
    for k in ["person", "model", "effort", "harness"] {
        if let Some(v) = text(k) {
            job.insert(k.into(), json!(v));
        }
    }
    Ok(Value::Object(job))
}

/// The import made one job per trigger of a routine, and a rollback of the schedules module rebuilds the routine from them:
/// one routine, enabled while any trigger is on. So a pause or a resume that leaves the triggers of one routine mixed (some
/// paused, some on) is lost by a rollback; this says so, naming them. `None` for a job that is not a routine's, a routine
/// with one trigger, or triggers that all agree.
fn mixed_triggers(jobs: &[Value], at: usize) -> Option<String> {
    let routine = jobs[at].get("routine")?;
    let same = |j: &Value| {
        j.get("routine")
            .is_some_and(|r| r.get("id") == routine.get("id") && r.get("bot") == routine.get("bot"))
    };
    let paused = |j: &Value| {
        // Only `true` is paused, as the rollback reads it.
        j.get("paused") == Some(&Value::Bool(true))
    };
    let all: Vec<&Value> = jobs.iter().filter(|j| same(j)).collect();
    let (off, on): (Vec<&Value>, Vec<&Value>) = all.iter().copied().partition(|j| paused(j));
    if all.len() < 2 || off.is_empty() || on.is_empty() {
        return None;
    }
    let names = |v: &[&Value]| {
        v.iter()
            .map(|j| text_of(j, "id"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    Some(format!(
        "this routine has several triggers and they now differ (paused: {}; on: {}); rolling back the schedules module makes the routine one thing again, enabled while any trigger is on, so this is lost there (pause or resume the routine in the app)",
        names(&off),
        names(&on)
    ))
}

fn find(jobs: &[Value], agent: &str, id: &str) -> Result<usize> {
    jobs.iter()
        .position(|j| text_of(j, "id") == id)
        .ok_or_else(|| {
            let have: Vec<String> = jobs.iter().map(|j| text_of(j, "id")).collect();
            bad(format!(
                "{agent} has no job '{id}' (have: {})",
                if have.is_empty() {
                    "none".into()
                } else {
                    have.join(" ")
                }
            ))
        })
}

/// Changes the agent's list under the exclusive lock and writes it whole.
fn edit(
    paths: &Paths,
    agent: &str,
    change: impl FnOnce(&mut Vec<Value>) -> Result<Value>,
) -> Result<Value> {
    let file = jobs_file(paths, agent);
    let _lock = lock_jobs(&paths.machine, agent, LOCK_WAIT)?;
    let mut jobs = read_list(&file)?;
    let out = change(&mut jobs)?;
    write_list(&file, &jobs)?;
    Ok(out)
}

/// The jobs in the file, none when there is no file. Read under our own exclusive lock, so not through `read_jobs`
/// (whose shared lock would wait for us).
fn read_list(file: &Path) -> Result<Vec<Value>> {
    let refuse = |why: String| {
        Error::Refused(format!(
            "kith-core: schedule: can't read {} ({why}); fix or remove it first, nothing was changed",
            file.display()
        ))
    };
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(refuse(e.to_string())),
    };
    let v: Value = serde_json::from_str(&text).map_err(|e| refuse(e.to_string()))?;
    match v.as_array() {
        Some(list)
            if list
                .iter()
                .all(|j| j.as_object().is_some_and(|o| o.contains_key("id"))) =>
        {
            Ok(list.clone())
        }
        _ => Err(refuse("not a list of jobs".into())),
    }
}

/// The index brought up to the file just written, so a pause or a removal cancels what the job has waiting. Best effort: the
/// write is done, and a file the index cannot settle on right now is the next fire's to settle (it re-reads the hash). The host
/// gives `kith schedule` thirty seconds, so this must not turn a done write into a long wait or a failure.
fn settle_agent(core: &mut Core, paths: &Paths, agent: &str, now: i64) -> Option<String> {
    let Some(a) = super::index(paths, Some(agent)).agents.pop() else {
        return Some("the agent is gone".into());
    };
    match super::fire::settle(core, paths, a, now) {
        Ok((_, None)) => None,
        Ok((_, Some(why))) => Some(why),
        Err(e) => Some(e.to_string()),
    }
}
