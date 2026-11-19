//! The `schedule.jobs`, `schedule.show`, `schedule.next` and `schedule.runs` rows (M4-2): the agents' jobs.json definitions
//! joined with the run state in core.db. Reads only; the verbs that change a jobs.json are `edit.rs`'s. `schedule.jobs` prints
//! what `kith-sched ls --json` prints for `jobs`, plus what the core adds (`problems`, `orphans`, `tick`).

use rusqlite::{Connection, OptionalExtension as _, params};
use serde_json::{Map, Value, json};

use super::{AgentJobs, Index, Rule, RuleError, index};
use crate::db::Core;
use crate::error::{Error, Result};

/// The rows this module answers.
pub(crate) fn serves(method: &str) -> bool {
    matches!(
        method,
        "schedule.jobs" | "schedule.show" | "schedule.next" | "schedule.runs"
    )
}

/// Answers a `schedule.*` read for the caller's view of the folders (`params`), with the cron rule that is wired.
pub(crate) fn call(core: &Core, method: &str, params: &Value) -> Result<Value> {
    call_with(core, method, params, &super::TriggerRule, now_ms())
}

/// [`call`] with the rule and the clock given (a test fixes both).
pub(crate) fn call_with(
    core: &Core,
    method: &str,
    params: &Value,
    rule: &dyn Rule,
    now: i64,
) -> Result<Value> {
    let (paths, _) = core.folders_for(params)?;
    let agent = params.get("agent").and_then(Value::as_str);
    let conn = core.conn();
    let local = local_mac(&paths.machine);
    let ctx = Ctx {
        conn,
        rule,
        now,
        local: local.as_deref(),
    };
    match method {
        "schedule.jobs" => ctx.list(&known_agent(&paths, agent)?),
        "schedule.next" => ctx.next(&known_agent(&paths, agent)?),
        "schedule.runs" => {
            known_agent(&paths, agent)?;
            ctx.latest_runs(agent, params.get("id").and_then(Value::as_str))
        }
        "schedule.show" => {
            let agent = agent.ok_or_else(|| bad("agent is required"))?;
            let id = params
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| bad("id is required"))?;
            let idx = index(&paths, Some(agent));
            ctx.show(&idx, agent, id)
        }
        _ => Err(Error::NotServed(format!(
            "kith-core: {method} is not answered in the core yet"
        ))),
    }
}

/// The index for the agent asked for (all when none). An agent that is not there is refused as `kith-sched` refuses it,
/// naming the agents there are; with the agents folder unmounted nothing is known, so that is a status, not a refusal.
pub(super) fn known_agent(paths: &kith_kernel::paths::Paths, agent: Option<&str>) -> Result<Index> {
    let idx = index(paths, agent);
    if let Some(a) = agent
        && idx.mounted
        && idx.agents.is_empty()
    {
        let have: Vec<String> = index(paths, None)
            .agents
            .into_iter()
            .map(|a| a.agent)
            .collect();
        return Err(bad(&format!("no agent '{a}' (have: {})", have.join(" "))));
    }
    Ok(idx)
}

/// One job as `schedule.jobs` shows it (what `kith schedule add` answers with), on this machine's clock and cron rule.
pub(super) fn row_of(
    core: &Core,
    paths: &kith_kernel::paths::Paths,
    agent: &str,
    job: &Value,
) -> Result<Value> {
    let local = local_mac(&paths.machine);
    Ctx {
        conn: core.conn(),
        rule: &super::TriggerRule,
        now: now_ms(),
        local: local.as_deref(),
    }
    .job_row(agent, job)
}

fn bad(what: &str) -> Error {
    Error::BadParams(format!("kith-core: schedule: {what}"))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(0))
}

/// This machine's id when one was ever made. A read never makes it.
fn local_mac(machine: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(machine.join(super::MAC_ID_FILE))
        .ok()
        .map(|s| s.trim().to_owned())
}

struct Ctx<'a> {
    conn: &'a Connection,
    rule: &'a dyn Rule,
    now: i64,
    local: Option<&'a str>,
}

/// A run row as a client reads it.
struct Run {
    json: Value,
}

fn text(job: &Value, key: &str) -> Option<String> {
    job.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn job_id(job: &Value) -> String {
    match job.get("id") {
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

impl Ctx<'_> {
    /// The job as `kith-sched --json` shows it: the stored fields (older files may lack some), when it runs next, how it
    /// last went.
    fn job_row(&self, agent: &str, job: &Value) -> Result<Value> {
        let paused = job
            .get("paused")
            .is_some_and(|p| !matches!(p, Value::Null | Value::Bool(false)));
        let mut row = Map::new();
        row.insert("agent".into(), json!(agent));
        row.insert("id".into(), json!(job_id(job)));
        row.insert(
            "cron".into(),
            job.get("cron").cloned().unwrap_or(Value::Null),
        );
        row.insert(
            "task".into(),
            job.get("task").cloned().unwrap_or(Value::Null),
        );
        row.insert("paused".into(), json!(paused));
        for k in ["person", "model", "effort", "harness"] {
            if let Some(v) = text(job, k).filter(|v| !v.is_empty()) {
                row.insert(k.into(), json!(v));
            }
        }
        let mut next = Value::Null;
        if !paused {
            let has_trigger = job.get("trigger").is_some();
            match job.get("cron").and_then(Value::as_str) {
                None if !has_trigger => {
                    row.insert("error".into(), json!("bad cron: no cron"));
                }
                _ => match self.rule.next(job, self.now) {
                    Ok(Some(n)) => next = json!(n),
                    Ok(None) => {}
                    Err(RuleError::NotWired) => {
                        row.insert("note".into(), json!("trigger not wired"));
                    }
                    Err(RuleError::Bad(e)) => {
                        row.insert("error".into(), json!(format!("bad cron: {e}")));
                    }
                },
            }
        }
        row.insert("next".into(), next);
        let last = self.last_run(agent, &job_id(job))?;
        row.insert("last".into(), last.map_or(Value::Null, |r| r.json));
        Ok(Value::Object(row))
    }

    fn run_json(&self, r: &rusqlite::Row<'_>) -> rusqlite::Result<Run> {
        let (mac, state): (String, String) = (r.get("mac")?, r.get("state")?);
        let pid: Option<i64> = r.get("pid")?;
        let finished: Option<i64> = r.get("finished_at")?;
        // A pid is only meaningful on the Mac that started it.
        let running = state == "started"
            && finished.is_none()
            && self.local == Some(mac.as_str())
            && pid.and_then(|p| u32::try_from(p).ok()).is_some_and(|p| {
                !matches!(
                    kith_kernel::procstart::probe(p),
                    kith_kernel::procstart::Probe::Gone
                )
            });
        Ok(Run {
            json: json!({
                "at": r.get::<_, String>("at")?,
                "slot": r.get::<_, String>("slot")?,
                "turn": r.get::<_, Option<String>>("turn_id")?,
                "why": r.get::<_, Option<String>>("why")?,
                "pid": pid,
                "log": r.get::<_, Option<String>>("log")?,
                "running": running,
                "state": state,
                "outcome": r.get::<_, Option<String>>("outcome")?,
                "mac": mac,
            }),
        })
    }

    fn last_run(&self, agent: &str, job: &str) -> Result<Option<Run>> {
        Ok(self
            .conn
            .query_row(
                "SELECT * FROM sched_runs WHERE agent = ?1 AND job = ?2 ORDER BY id DESC LIMIT 1",
                params![agent, job],
                |r| self.run_json(r),
            )
            .optional()?)
    }

    fn runs(&self, agent: &str, job: &str, limit: i64) -> Result<Vec<Value>> {
        let mut stmt = self.conn.prepare(
            "SELECT * FROM sched_runs WHERE agent = ?1 AND job = ?2 ORDER BY id DESC LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![agent, job, limit], |r| self.run_json(r))?;
        Ok(rows
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .map(|r| r.json)
            .collect())
    }

    /// The newest run of every job that has one (all agents', or one agent's, or one job's): what `kith schedule log` lists
    /// and `ls` shows as `last`. Once the schedules are the core's a run is a fire that made a turn, so the newest fire with a
    /// turn is the run, with how its turn went and its claim's process; a job that has none yet shows the run the import
    /// carried over from `kith-sched` (`sched_runs`), which nothing writes after the switch.
    fn latest_runs(&self, agent: Option<&str>, job: Option<&str>) -> Result<Value> {
        let mut stmt = self.conn.prepare(
            "SELECT agent, job, slot, turn_id, at, tstate, pid, pid_started, ended FROM (
               SELECT f.agent, f.job, f.slot, f.turn_id, f.at, t.state AS tstate, c.pid, c.pid_started,
                      c.end_reason AS ended,
                      ROW_NUMBER() OVER (PARTITION BY f.agent, f.job ORDER BY f.at DESC, f.slot DESC) AS n
               FROM sched_fires f
                 LEFT JOIN turns t ON t.id = CAST(f.turn_id AS INTEGER)
                 LEFT JOIN claims c ON c.claim_id = t.claim_id
               WHERE f.outcome = 'fired' AND f.turn_id IS NOT NULL
                 AND (?1 IS NULL OR f.agent = ?1) AND (?2 IS NULL OR f.job = ?2))
             WHERE n = 1",
        )?;
        let mut runs: Vec<(String, Value)> = stmt
            .query_map(params![agent, job], |r| {
                let (agent, job, slot): (String, String, String) =
                    (r.get(0)?, r.get(1)?, r.get(2)?);
                let at: i64 = r.get(4)?;
                let tstate: Option<String> = r.get(5)?;
                let pid: Option<i64> = r.get(6)?;
                let alive = matches!(tstate.as_deref(), Some("claimed" | "running"))
                    && pid.is_some_and(|p| {
                        super::running::alive(
                            p,
                            r.get::<_, Option<String>>(7).ok().flatten().as_deref(),
                        )
                    });
                let state = match tstate.as_deref() {
                    Some("claimed" | "running") => "running",
                    Some("queued" | "parked") => "queued",
                    Some("failed") => "failed",
                    Some("cancelled") => "cancelled",
                    _ => "done",
                };
                let at_local = super::local_minute(at).unwrap_or_default();
                let why = if slot.starts_with("manual:") {
                    "run by hand".to_owned()
                } else if slot.starts_with("event:") {
                    "event".to_owned()
                } else {
                    let due = super::ms_of_slot(&slot).and_then(super::local_minute);
                    format!(
                        "due {}",
                        due.as_deref().and_then(|d| d.get(11..16)).unwrap_or("?")
                    )
                };
                Ok((
                    at_local.clone(),
                    json!({
                        "agent": agent, "id": job, "at": at_local, "slot": slot,
                        "turn": r.get::<_, Option<String>>(3)?, "why": why, "pid": pid, "log": null,
                        "running": alive, "state": state,
                        "outcome": r.get::<_, Option<String>>(8)?,
                    }),
                ))
            })?
            .collect::<std::result::Result<_, _>>()?;
        // The imported history of a job the fire has not run since.
        let mut hist = self.conn.prepare(
            "SELECT * FROM sched_runs WHERE id IN (SELECT MAX(id) FROM sched_runs GROUP BY agent, job)
               AND (?1 IS NULL OR agent = ?1) AND (?2 IS NULL OR job = ?2)",
        )?;
        let imported = hist
            .query_map(params![agent, job], |r| {
                let mut run = self.run_json(r)?.json;
                run["agent"] = json!(r.get::<_, String>("agent")?);
                run["id"] = json!(r.get::<_, String>("job")?);
                Ok(run)
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for run in imported {
            let own = runs
                .iter()
                .any(|(_, x)| x["agent"] == run["agent"] && x["id"] == run["id"]);
            if !own {
                runs.push((run["at"].as_str().unwrap_or_default().to_owned(), run));
            }
        }
        runs.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(json!({"runs": runs.into_iter().map(|r| r.1).collect::<Vec<_>>()}))
    }

    fn tick(&self) -> Result<Value> {
        let rows = self
            .conn
            .prepare("SELECT mac, tick FROM sched_tick ORDER BY mac")?
            .query_map([], |r| {
                Ok(json!({"mac": r.get::<_, String>(0)?, "tick": r.get::<_, String>(1)?}))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(Value::Array(rows))
    }

    fn problems(idx: &Index) -> Vec<Value> {
        idx.agents
            .iter()
            .filter_map(|a| {
                a.problem
                    .as_ref()
                    .map(|p| json!({"agent": a.agent, "error": p}))
            })
            .collect()
    }

    fn note(idx: &Index) -> Option<&'static str> {
        (!idx.mounted).then_some("agent folder not mounted")
    }

    fn list(&self, idx: &Index) -> Result<Value> {
        let mut jobs = Vec::new();
        for AgentJobs {
            agent, jobs: js, ..
        } in &idx.agents
        {
            for j in js {
                jobs.push(self.job_row(agent, j)?);
            }
        }
        Ok(json!({
            "jobs": jobs,
            "mounted": idx.mounted,
            "note": Self::note(idx),
            "problems": Self::problems(idx),
            "orphans": self.orphans(idx)?,
            "ticks": self.tick()?,
        }))
    }

    /// Run state for a job no jobs.json holds any more (removed, or its agent gone). An agent whose file could not be
    /// read is skipped: its jobs are unknown, not removed. With the agent folder unmounted nothing is known either.
    fn orphans(&self, idx: &Index) -> Result<Vec<Value>> {
        if !idx.mounted {
            return Ok(Vec::new());
        }
        let mut stmt = self
            .conn
            .prepare("SELECT agent, job FROM sched_runs GROUP BY agent, job ORDER BY agent, job")?;
        let keys = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut out = Vec::new();
        for (agent, job) in keys {
            let known = idx.agents.iter().find(|a| a.agent == agent);
            // `index` may have been asked for one agent: runs of the others are not this listing's business.
            if idx.only.as_deref().is_some_and(|o| o != agent) {
                continue;
            }
            if known.is_some_and(|a| a.problem.is_some() || a.jobs.iter().any(|j| job_id(j) == job))
            {
                continue;
            }
            let last = self.last_run(&agent, &job)?.map_or(Value::Null, |r| r.json);
            out.push(json!({"agent": agent, "id": job, "last": last}));
        }
        Ok(out)
    }

    fn next(&self, idx: &Index) -> Result<Value> {
        let mut rows = Vec::new();
        for a in &idx.agents {
            for j in &a.jobs {
                let row = self.job_row(&a.agent, j)?;
                if row["paused"] == json!(true) {
                    continue;
                }
                let mut o = Map::new();
                for k in ["agent", "id", "cron", "next", "error"] {
                    if let Some(v) = row.get(k) {
                        o.insert(k.into(), v.clone());
                    }
                }
                rows.push(Value::Object(o));
            }
        }
        // The soonest first; a job that never runs, or whose time is unknown, last. The minute text sorts as time.
        rows.sort_by(|a, b| match (a["next"].as_str(), b["next"].as_str()) {
            (Some(x), Some(y)) => x.cmp(y),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        });
        Ok(json!({
            "next": rows,
            "mounted": idx.mounted,
            "note": Self::note(idx),
            "problems": Self::problems(idx),
        }))
    }

    fn show(&self, idx: &Index, agent: &str, id: &str) -> Result<Value> {
        if !idx.mounted {
            return Err(Error::Refused(
                "kith-core: schedule: agent folder not mounted".into(),
            ));
        }
        let Some(a) = idx.agents.iter().find(|a| a.agent == agent) else {
            return Err(bad(&format!("no agent {agent:?}")));
        };
        if let Some(p) = &a.problem {
            return Err(Error::Refused(format!("kith-core: schedule: {p}")));
        }
        let Some(job) = a.jobs.iter().find(|j| job_id(j) == id) else {
            let have: Vec<String> = a.jobs.iter().map(job_id).collect();
            return Err(bad(&format!(
                "{agent} has no job '{id}' (have: {})",
                if have.is_empty() {
                    "none".into()
                } else {
                    have.join(" ")
                }
            )));
        };
        let mut fires = self.conn.prepare(
            "SELECT f.slot, f.turn_id, f.at, f.outcome, t.state, c.end_reason
             FROM sched_fires f
               LEFT JOIN turns t ON t.id = CAST(f.turn_id AS INTEGER)
               LEFT JOIN claims c ON c.claim_id = t.claim_id
             WHERE f.agent = ?1 AND f.job = ?2 ORDER BY f.at DESC, f.slot DESC LIMIT 20",
        )?;
        // `outcome` is the fire's decision (`fired`); `turnState` and `ended` say how the turn went (queued, running, done,
        // failed, or cancelled with the reason the claim was ended by: paused, revoked, rollback, a host's no-runner reason).
        let fires = fires
            .query_map(params![agent, id], |r| {
                Ok(json!({"slot": r.get::<_, String>(0)?, "turn": r.get::<_, Option<String>>(1)?, "at": r.get::<_, i64>(2)?, "outcome": r.get::<_, String>(3)?,
                          "turnState": r.get::<_, Option<String>>(4)?, "ended": r.get::<_, Option<String>>(5)?}))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(json!({
            "job": self.job_row(agent, job)?,
            "definition": job,
            "runs": self.runs(agent, id, 20)?,
            "fires": fires,
        }))
    }
}
