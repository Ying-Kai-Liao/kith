//! The `schedule.jobs`, `schedule.show` and `schedule.next` rows (M4-2): the agents' jobs.json definitions joined with the
//! run state in core.db. Reads only; the write rows (`schedule.add`, `.edit`, `.pause` ...) are not served here, so the
//! daemon runs nothing and `kith-sched` keeps writing. `schedule.jobs` prints what `kith-sched ls --json` prints for
//! `jobs`, plus what the core adds (`problems`, `orphans`, `tick`).

use rusqlite::{Connection, OptionalExtension as _, params};
use serde_json::{Map, Value, json};

use super::{AgentJobs, Index, Rule, RuleError, index};
use crate::db::Core;
use crate::error::{Error, Result};

/// The rows this module answers.
pub(crate) fn serves(method: &str) -> bool {
    matches!(method, "schedule.jobs" | "schedule.show" | "schedule.next")
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
fn known_agent(paths: &kith_kernel::paths::Paths, agent: Option<&str>) -> Result<Index> {
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
        return Err(bad(&format!("no agent {a:?} (have: {})", have.join(" "))));
    }
    Ok(idx)
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
            match job.get("cron").and_then(Value::as_str) {
                None => {
                    row.insert("error".into(), json!("bad cron: no cron"));
                }
                Some(c) => match self.rule.next(c, self.now) {
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
            && pid
                .and_then(|p| u32::try_from(p).ok())
                .is_some_and(|p| crate::lock::process_start(p).is_some());
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
                "{agent} has no job {id:?} (have: {})",
                if have.is_empty() {
                    "none".into()
                } else {
                    have.join(" ")
                }
            )));
        };
        let mut fires = self.conn.prepare(
            "SELECT slot, turn_id, at, outcome FROM sched_fires WHERE agent = ?1 AND job = ?2 ORDER BY at DESC, slot DESC LIMIT 20",
        )?;
        let fires = fires
            .query_map(params![agent, id], |r| {
                Ok(json!({"slot": r.get::<_, String>(0)?, "turn": r.get::<_, Option<String>>(1)?, "at": r.get::<_, i64>(2)?, "outcome": r.get::<_, String>(3)?}))
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
