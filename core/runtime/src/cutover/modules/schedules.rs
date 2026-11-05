//! The `schedules` module (M4-5a, the core side): the host's routines become jobs in the owning agent's `jobs.json`, run
//! with the state `kith-sched` kept, and the core's fire (M4-3, M4-4) becomes the one scheduler.
//!
//! Shipped OFF: registering this module changes nothing; only `kith migrate step schedules` (a deliberate act, never
//! automatic) switches it. M4-5b landed: once switched, the host's routines runner and webhook listener forward their work
//! to the core. The host reads that gate when it starts, so `step` and `rollback` both refuse while the host runs (stop
//! it, step or roll back, start it). `step --dry-run` and `check` work any time.
//!
//! # What moves
//! - **Routines**: serve.db's kv row `routines.v1` (the host's `Routine`: bot, name, instruction, triggers, enabled,
//!   webhook key) into `<agent>/jobs.json` of the kith agent that owns the bot (`hq_agent` of the bot's config), under the
//!   same `<machine>/sched/<agent>.jobs.lock` `kith-sched` and the core's reader take. Definitions stay in the agent's
//!   folder (architect, 2026-11-04). serve.db is only ever read here.
//!   - One job per trigger. The job id is the routine's id when it has one trigger, else `<routine id>-<n>` (n counts
//!     from 1, in the routine's order). Derived from the routine and nothing else, so a re-import finds its own jobs.
//!   - The job: `id`, `task` (the instruction), `name`, `trigger` (the kernel's `Trigger`: cron in its zone, interval,
//!     once, webhook, event), `paused` (when the routine is disabled), `mac` (this Mac's id: the fire runs the job here
//!     and nowhere else, so a jobs.json that syncs between Macs fires its timed triggers on exactly one), and `routine` (where it came
//!     from: routine id, bot, trigger number, timeout). No `cron` field, which is how `kith-sched` knows it is not its own.
//!   - Idempotent: a job of the same id that this import made on this Mac is brought up to date (a re-import of an
//!     unchanged routine writes nothing); a job of that id that is not this routine's is a collision and that routine is
//!     refused, never overwritten; one made on another Mac is theirs and is left alone, with the same refusal.
//!   - A routine whose bot has no kith agent (a host-native bot, `hq_agent` empty) is refused: "make it a kith agent
//!     first" (C5). So is one whose agent is not on this Mac, whose trigger does not validate, or whose id cannot be a job
//!     id. A refused routine is a difference for `same_answer`, so the step cannot switch while one is left.
//!   - A job this import made whose routine is gone from serve.db is removed again.
//! - **Webhook keys**: a `webhook` or `event` job's key is the secret record `routine-webhook/<agent>/<job>`, which
//!   `schedules::hooks` reads. Written through the secrets envelope when missing or different; no value is ever printed.
//! - **`kith-sched`'s state** (`<machine>/sched/state.json`: the last tick and last runs): the existing schedules import,
//!   through the held core (`import::schedules::run_in`). The tick is not compared by `same_answer` (it moves every minute
//!   while `kith-sched` ticks); every last run must be in the core.
//!
//! # Switch and rollback
//! - `tombstones`: `sched/state.json`. `kith-sched` fails closed on it (`kith_tombstone`): tick, ls and the rest stop with
//!   the note's message instead of starting every job again from no state.
//! - `rollback`: the keys and the enabled flags go back into serve.db's `routines.v1` (`restore_secrets`, before any file),
//!   then the jobs this import made on this Mac are removed from the agents' jobs.json, their key records are deleted, and
//!   `state.json` is written again (the last tick and each job's last run from the core, beside whatever other keys the
//!   original had) over the tombstone. A routine deleted from serve.db since is not brought back (said, not guessed). The
//!   host keeps routines in memory and saves them over serve.db's `routines.v1`, so a rollback refuses while the host
//!   runs (`live`; `--force` overrides it like any live reason): stop the host first, roll back, start it.
//!   Jobs made on another Mac stay (their `mac` is not this one's); `check` names them.
//! - `live`: scheduled turns that have not finished.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use kith_kernel::paths::Paths;
use kith_kernel::schedule::{Trigger, Zone};
use rusqlite::{Connection, OpenFlags, OptionalExtension as _};
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};

use crate::cutover::state::{self, Phase};
use crate::cutover::{Env, Module, Opts};
use crate::db::Core;
use crate::error::{Error, Result};
use crate::secrets::Envelope;
use crate::secrets::keystore::KeyStore;

/// The host's kv key for its routines (`serve/host/src/routines/mod.rs`).
const ROUTINES_KEY: &str = "routines.v1";
/// How long the import waits for another writer of an agent's jobs.json.
const LOCK_WAIT: Duration = Duration::from_secs(30);

type KeyFactory = std::sync::Arc<dyn Fn() -> Box<dyn KeyStore>>;

pub(crate) struct Schedules {
    /// Tests: the key store the envelope opens, in place of this computer's. `None`: the operating system's.
    keys: Option<KeyFactory>,
}

impl Schedules {
    pub(crate) const fn new() -> Self {
        Self { keys: None }
    }

    #[cfg(test)]
    pub(crate) fn with_keys(keys: KeyFactory) -> Self {
        Self { keys: Some(keys) }
    }

    /// Runs `f` with the core's envelope, opening it first when this core has not yet.
    fn with_envelope<T>(
        &self,
        core: &Core,
        f: impl FnOnce(&mut Envelope, &Connection) -> Result<T>,
    ) -> Result<T> {
        let mut guard = core
            .secrets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if guard.is_none() {
            *guard = Some(match &self.keys {
                Some(make) => Envelope::with_store(make()),
                None => Envelope::os()?,
            });
        }
        let envelope = guard.as_mut().ok_or_else(|| {
            Error::Io("cutover: schedules: the secrets envelope is unavailable".into())
        })?;
        f(envelope, core.conn())
    }
}

// --- reading the host's routines ---------------------------------------------------------------------------------------

/// One job made from one trigger of one routine. Holds the webhook key, so it has no `Debug` and is never formatted.
struct Row {
    agent: String,
    job: String,
    routine: String,
    bot: String,
    def: Value,
    key: Option<String>,
}

#[derive(Default)]
struct Plan {
    rows: Vec<Row>,
    /// Routines the import will not take, and why (one line each).
    refused: Vec<String>,
    /// The routines in `refused`: their jobs from an earlier import stay (the host still runs the routine; a refusal is
    /// a reason to look, not to take its jobs away).
    refused_ids: BTreeSet<String>,
    notes: Vec<String>,
}

fn serve_db_of(env: &Env) -> PathBuf {
    if let Some(p) = &env.serve_db {
        return p.clone();
    }
    let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
    if let Some(p) = var("KITH_SERVE_DB") {
        return PathBuf::from(p);
    }
    var("KITH_SERVE_HOME")
        .map_or_else(
            || {
                var("HOME")
                    .map_or_else(|| env.home.clone(), PathBuf::from)
                    .join(".kith")
                    .join("serve")
            },
            PathBuf::from,
        )
        .join("serve.db")
}

/// Whether a host holds `host.lock` in its data folder (the folder of serve.db): an advisory lock the OS drops with the
/// process, so a stale file or a reused pid never reads as running. Read-only: the file is never created. Anything but
/// "no file" or "lock free" counts as running (the conservative answer; `rollback --force` still goes through).
fn host_running(env: &Env) -> bool {
    let Some(dir) = serve_db_of(env).parent().map(Path::to_path_buf) else {
        return false;
    };
    let file = match std::fs::File::open(dir.join("host.lock")) {
        Ok(f) => f,
        Err(e) => return e.kind() != std::io::ErrorKind::NotFound,
    };
    match fs2::FileExt::try_lock_shared(&file) {
        Ok(()) => {
            let _ = fs2::FileExt::unlock(&file);
            false
        }
        Err(_) => true,
    }
}

/// Routine jobs in the agents' files that another Mac made: a rollback here leaves them, and a reinstalled Mac has a new
/// id, so its old jobs never fire. Informational (not a same-answer difference). Jobs without `mac` are never listed.
fn other_mac_notes(env: &Env) -> Vec<String> {
    let Some(paths) = env.paths.as_ref() else {
        return Vec::new();
    };
    let mac = mac_peek(&env.machine);
    if mac.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for agent in agent_names(paths) {
        let file = crate::schedules::jobs_file(paths, &agent);
        let Ok((_, jobs)) = crate::schedules::read_jobs(&env.machine, &agent, &file) else {
            continue;
        };
        for j in jobs.iter().filter(|j| j.get("routine").is_some()) {
            if let Some(m) = text(j, "mac").filter(|m| !m.is_empty() && *m != mac) {
                out.push(format!(
                    "{agent}/{}: made on another Mac ({m}): rollback here leaves it; if this Mac was reinstalled, remove it by hand",
                    text(j, "id").unwrap_or_default()
                ));
            }
        }
    }
    out
}

fn paths_of(env: &Env) -> Result<&Paths> {
    env.paths.as_ref().ok_or_else(|| {
        Error::BadParams("cutover: schedules needs the machine's folders (agents, state)".into())
    })
}

/// This Mac's id without making one (the check and the dry run write nothing); empty when there is none yet.
fn mac_peek(machine: &Path) -> String {
    std::fs::read_to_string(machine.join(crate::schedules::MAC_ID_FILE))
        .map(|s| s.trim().to_owned())
        .unwrap_or_default()
}

fn agent_exists(paths: &Paths, agent: &str) -> bool {
    let d = paths.agents.join(agent);
    d.join("agent.md").is_file() || d.join("role").join("agent.md").is_file()
}

/// Folder and file names: letters, digits, `-`, `_` and `.`, not starting with `.`.
fn plain_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 120
        && !s.starts_with('.')
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

fn text<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}

/// The host's trigger (`serve/host/src/routines/schedule.rs`, camelCase, tagged by `type`) as the kernel's.
fn convert(t: &Value) -> std::result::Result<Trigger, String> {
    let kind = text(t, "type").ok_or("a trigger has no type")?;
    let word = |k: &str| text(t, k).ok_or_else(|| format!("a {kind} trigger has no {k}"));
    let number = |k: &str| {
        t.get(k)
            .and_then(Value::as_i64)
            .ok_or_else(|| format!("a {kind} trigger has no {k}"))
    };
    let k = match kind {
        "interval" => Trigger::Interval {
            every: u64::try_from(number("seconds")?).map_err(|_| "an interval below zero")?,
        },
        "once" => Trigger::Once { at: number("at")? },
        "cron" => Trigger::Cron {
            expr: word("expression")?.to_owned(),
            tz: Zone::parse(word("timeZone")?).map_err(|e| e.0)?,
        },
        "webhook" => Trigger::Webhook,
        "event" => Trigger::Event {
            source: word("source")?.to_owned(),
            event: word("event")?.to_owned(),
            filters: t
                .get("filters")
                .and_then(Value::as_object)
                .map(|o| {
                    o.iter()
                        .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_owned())))
                        .collect::<BTreeMap<_, _>>()
                })
                .unwrap_or_default(),
        },
        other => return Err(format!("unknown trigger type {other:?}")),
    };
    k.validate().map_err(|e| e.0)?;
    Ok(k)
}

fn routines_of(db: &Connection) -> Result<Vec<Value>> {
    let raw: Option<String> = db
        .query_row("SELECT v FROM kv WHERE k = ?1", [ROUTINES_KEY], |r| r.get(0))
        .optional()
        .map_err(|e| {
            Error::Refused(format!(
                "cutover: schedules: serve.db cannot be read as the host's store ({e}); nothing was imported"
            ))
        })?;
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    let state: Value = serde_json::from_str(&raw).map_err(|e| {
        Error::Refused(format!(
            "cutover: schedules: serve.db's {ROUTINES_KEY} is not JSON ({e}); nothing was imported"
        ))
    })?;
    Ok(state
        .get("routines")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

/// Who owns a bot.
enum Owner {
    /// No such bot (or its config is not JSON).
    Missing,
    /// A host-native bot: no kith agent.
    Native,
    Agent(String),
}

/// The kith agent that owns a bot: the `hq_agent` of its config.
fn agent_of(db: &Connection, bot: &str) -> Owner {
    let config: Option<String> = db
        .query_row(
            "SELECT config FROM bots WHERE id = ?1 AND deleted = 0",
            [bot],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten();
    let Some(config) = config.and_then(|c| serde_json::from_str::<Value>(&c).ok()) else {
        return Owner::Missing;
    };
    // The host's BotConfig serializes camelCase (`hqAgent`); `hq_agent` is read too, for rows written by hand.
    match text(&config, "hqAgent")
        .filter(|a| !a.is_empty())
        .or_else(|| text(&config, "hq_agent").filter(|a| !a.is_empty()))
    {
        Some(a) => Owner::Agent(a.to_owned()),
        None => Owner::Native,
    }
}

/// What serve.db says, as jobs. `mac` goes into every job.
fn plan(env: &Env, mac: &str) -> Result<Plan> {
    let paths = paths_of(env)?;
    let mut plan = Plan::default();
    let file = serve_db_of(env);
    if !file.is_file() {
        plan.notes.push(format!(
            "no serve.db at {}: no routines to import",
            file.display()
        ));
        return Ok(plan);
    }
    let db = Connection::open_with_flags(&file, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| Error::Io(format!("cutover: schedules: {}: {e}", file.display())))?;
    db.busy_timeout(Duration::from_secs(5)).ok();
    for r in routines_of(&db)? {
        if r.get("deleted").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let id = text(&r, "id").unwrap_or_default().to_owned();
        let name = text(&r, "name").unwrap_or_default();
        let who = format!("routine {name:?} ({id})");
        if let Err(why) = routine_rows(&db, paths, &r, &id, mac, &mut plan.rows) {
            plan.refused.push(format!("{who}: {why}"));
            plan.refused_ids.insert(id);
        }
    }
    Ok(plan)
}

fn routine_rows(
    db: &Connection,
    paths: &Paths,
    r: &Value,
    id: &str,
    mac: &str,
    out: &mut Vec<Row>,
) -> std::result::Result<(), String> {
    if !plain_name(id) {
        return Err("its id cannot be a job id (letters, digits, - _ . only)".into());
    }
    let bot = text(r, "botId").unwrap_or_default();
    let agent = match agent_of(db, bot) {
        Owner::Missing => return Err(format!("its bot {bot:?} no longer exists")),
        Owner::Native => {
            return Err(format!(
                "bot {bot:?} is a host-native bot with no kith agent; make it a kith agent first (C5)"
            ));
        }
        Owner::Agent(a) => a,
    };
    if !plain_name(&agent) || !agent_exists(paths, &agent) {
        return Err(format!("its agent {agent:?} is not on this Mac"));
    }
    let triggers = r
        .get("triggers")
        .and_then(Value::as_array)
        .filter(|t| !t.is_empty())
        .ok_or("it has no triggers")?;
    let instruction = text(r, "instruction").unwrap_or_default();
    let key = text(r, "webhookKey").unwrap_or_default();
    let paused = r.get("enabled").and_then(Value::as_bool) == Some(false);
    let mut rows = Vec::new();
    for (i, t) in triggers.iter().enumerate() {
        let trigger = convert(t).map_err(|e| format!("trigger {}: {e}", i + 1))?;
        let hooked = matches!(trigger, Trigger::Webhook | Trigger::Event { .. });
        if hooked && key.is_empty() {
            return Err(format!(
                "trigger {} takes deliveries but the routine has no webhook key",
                i + 1
            ));
        }
        let job = if triggers.len() == 1 {
            id.to_owned()
        } else {
            format!("{id}-{}", i + 1)
        };
        let mut def = Map::new();
        def.insert("id".into(), json!(job));
        def.insert("task".into(), json!(instruction));
        def.insert("name".into(), json!(text(r, "name").unwrap_or_default()));
        def.insert(
            "trigger".into(),
            serde_json::to_value(&trigger).map_err(|e| e.to_string())?,
        );
        if paused {
            def.insert("paused".into(), json!(true));
        }
        def.insert("mac".into(), json!(mac));
        def.insert(
            "routine".into(),
            json!({
                "id": id,
                "bot": bot,
                "trigger": i + 1,
                "timeoutSeconds": r.get("timeoutSeconds").and_then(Value::as_u64).unwrap_or(3600),
            }),
        );
        rows.push(Row {
            agent: agent.clone(),
            job,
            routine: id.to_owned(),
            bot: bot.to_owned(),
            def: Value::Object(def),
            key: hooked.then(|| key.to_owned()),
        });
    }
    out.extend(rows);
    Ok(())
}

// --- jobs.json ---------------------------------------------------------------------------------------------------------

/// The jobs in a file, as `kith-sched` reads it (none when there is no file). Takes no lock: the caller holds one.
fn read_list(file: &Path) -> std::result::Result<Vec<Value>, String> {
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.to_string()),
    };
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

/// `jobs.json` replaced whole (a temp file in the same folder, synced, then renamed), as `kith-sched` writes it.
fn write_list(file: &Path, jobs: &[Value]) -> Result<()> {
    let io = |e: std::io::Error| Error::Io(format!("cutover: schedules: {}: {e}", file.display()));
    let dir = file.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir).map_err(io)?;
    let tmp = dir.join(format!(".jobs.json.{}.tmp", std::process::id()));
    let mut body = serde_json::to_string_pretty(&Value::Array(jobs.to_vec()))
        .map_err(|e| Error::Io(format!("cutover: schedules: {e}")))?;
    body.push('\n');
    {
        use std::io::Write as _;
        let mut f = std::fs::File::create(&tmp).map_err(io)?;
        f.write_all(body.as_bytes()).map_err(io)?;
        f.sync_all().map_err(io)?;
    }
    std::fs::rename(&tmp, file).map_err(io)
}

/// The exclusive side of `<machine>/sched/<agent>.jobs.lock`, the lock `kith-sched` edits under; let go on drop.
struct JobsLock(#[allow(dead_code, reason = "held for its drop")] std::fs::File);

fn lock_jobs(machine: &Path, agent: &str) -> Result<JobsLock> {
    use fs2::FileExt as _;
    let dir = machine.join("sched");
    std::fs::create_dir_all(&dir)
        .map_err(|e| Error::Io(format!("cutover: schedules: {}: {e}", dir.display())))?;
    let path = dir.join(format!("{agent}.jobs.lock"));
    let f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| Error::Io(format!("cutover: schedules: {}: {e}", path.display())))?;
    let until = Instant::now() + LOCK_WAIT;
    loop {
        match f.try_lock_exclusive() {
            Ok(()) => return Ok(JobsLock(f)),
            Err(e) if e.kind() == fs2::lock_contended_error().kind() => {
                if Instant::now() >= until {
                    return Err(Error::Refused(format!(
                        "cutover: schedules: {} is held by another writer (kith-sched?) for more than {}s; try again",
                        path.display(),
                        LOCK_WAIT.as_secs()
                    )));
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => {
                return Err(Error::Io(format!(
                    "cutover: schedules: {}: {e}",
                    path.display()
                )));
            }
        }
    }
}

enum Verdict {
    New,
    Same,
    Update(usize),
    Refuse(String),
}

/// Whether a job is one this import made for `routine` (of `bot`).
fn made_for(job: &Value, routine: &str, bot: &str) -> bool {
    job.get("routine")
        .is_some_and(|r| text(r, "id") == Some(routine) && text(r, "bot") == Some(bot))
}

fn verdict(jobs: &[Value], row: &Row, mac: &str) -> Verdict {
    let Some(at) = jobs
        .iter()
        .position(|j| text(j, "id") == Some(row.job.as_str()))
    else {
        return Verdict::New;
    };
    let have = &jobs[at];
    if !made_for(have, &row.routine, &row.bot) {
        return Verdict::Refuse(format!(
            "{}'s jobs.json already has a job {:?} that is not this routine's; rename or remove it, then import again",
            row.agent, row.job
        ));
    }
    if text(have, "mac").is_some_and(|m| m != mac) {
        return Verdict::Refuse(format!(
            "{}/{} was imported on another Mac (the job names it); it is left to that Mac",
            row.agent, row.job
        ));
    }
    if have == &row.def {
        Verdict::Same
    } else {
        Verdict::Update(at)
    }
}

/// Jobs this import made on `mac` in `agent`'s file that no accepted routine asks for any more: its routine is gone, or
/// has fewer triggers now (`r-2` after 2 became 1), or moved to another agent. Left alone: a job of a routine that is
/// only refused this round.
fn orphans(jobs: &[Value], agent: &str, mac: &str, plan: &Plan) -> Vec<usize> {
    jobs.iter()
        .enumerate()
        .filter(|(_, j)| {
            text(j, "mac") == Some(mac)
                && j.get("routine")
                    .and_then(|r| text(r, "id"))
                    .is_some_and(|rid| !plan.refused_ids.contains(rid))
                && !plan
                    .rows
                    .iter()
                    .any(|r| r.agent == agent && text(j, "id") == Some(r.job.as_str()))
        })
        .map(|(i, _)| i)
        .collect()
}

fn agent_names(paths: &Paths) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(&paths.agents)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            let p = e.path();
            p.join("agent.md").is_file() || p.join("role").join("agent.md").is_file()
        })
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}

#[derive(Default)]
struct Counts {
    added: usize,
    updated: usize,
    same: usize,
    removed: usize,
}

/// What `apply_agent` works with and tells.
struct Apply<'a> {
    env: &'a Env,
    plan: &'a Plan,
    mac: &'a str,
    /// False for a dry run: nothing is locked or written.
    write: bool,
    counts: Counts,
    lines: Vec<String>,
    /// (agent, job) of the jobs taken out because their routine is gone: their key records go with them.
    removed: Vec<(String, String)>,
}

/// Brings one agent's jobs.json up to its rows, and takes out the jobs this import made whose routine is gone.
fn apply_agent(cx: &mut Apply<'_>, agent: &str, rows: &[&Row]) -> Result<()> {
    let paths = paths_of(cx.env)?;
    let file = crate::schedules::jobs_file(paths, agent);
    let _lock = if cx.write {
        Some(lock_jobs(&cx.env.machine, agent)?)
    } else {
        None
    };
    let mut jobs = if cx.write {
        read_list(&file)
    } else {
        crate::schedules::read_jobs(&cx.env.machine, agent, &file).map(|(_, j)| j)
    }
    .map_err(|e| {
        Error::Refused(format!(
            "cutover: schedules: {} cannot be read as a list of jobs ({e}); {agent}'s routines were not imported and the file is untouched",
            file.display()
        ))
    })?;
    let mut changed = false;
    for row in rows {
        match verdict(&jobs, row, cx.mac) {
            Verdict::New => {
                jobs.push(row.def.clone());
                cx.counts.added += 1;
                changed = true;
                cx.lines.push(format!("{agent}/{}: job added", row.job));
            }
            Verdict::Update(at) => {
                jobs[at] = row.def.clone();
                cx.counts.updated += 1;
                changed = true;
                cx.lines
                    .push(format!("{agent}/{}: job brought up to date", row.job));
            }
            Verdict::Same => cx.counts.same += 1,
            Verdict::Refuse(why) => cx.lines.push(format!("refused {agent}/{}: {why}", row.job)),
        }
    }
    for &i in orphans(&jobs, agent, cx.mac, cx.plan).iter().rev() {
        let j = jobs.remove(i);
        cx.counts.removed += 1;
        cx.removed.push((
            agent.to_owned(),
            text(&j, "id").unwrap_or_default().to_owned(),
        ));
        changed = true;
        cx.lines.push(format!(
            "{agent}/{}: job removed (its routine is gone, or no longer asks for it)",
            text(&j, "id").unwrap_or_default()
        ));
    }
    if changed && cx.write {
        write_list(&file, &jobs)?;
    }
    Ok(())
}

impl Module for Schedules {
    fn name(&self) -> &'static str {
        "schedules"
    }

    fn tombstones(&self, env: &Env) -> Vec<PathBuf> {
        vec![env.machine.join("sched").join("state.json")]
    }

    fn step_refusal(&self, env: &Env) -> Option<String> {
        // The host reads the switch when it starts. A running host started before the step would keep running every
        // routine from serve.db next to the core's fire, so each would run twice.
        host_running(env).then(|| {
            "not stepped: the host is running and reads the schedules switch only when it starts; stop the host, step schedules, start the host. `step schedules --dry-run` and `check schedules` work while it runs"
                .into()
        })
    }

    fn check_notes(&self, env: &Env) -> Vec<String> {
        other_mac_notes(env)
    }

    fn dry_run_despite_refusal(&self) -> bool {
        true
    }

    fn guards_claims(&self) -> bool {
        // A conversation claim elsewhere is no reason to keep schedules from going back; `live` names the scheduled turns.
        false
    }

    fn live(&self, env: &Env, core: &Core) -> Result<Vec<String>> {
        let mut out = Vec::new();
        // The running host saves its in-memory routines over serve.db's `routines.v1`, undoing the keys and flags the
        // rollback puts back there.
        if host_running(env) {
            out.push(
                "the host is running and would save its routines over the ones put back; stop the host first (then roll back, then start it)"
                    .into(),
            );
        }
        let n: i64 = core.conn().query_row(
            "SELECT COUNT(DISTINCT t.id) FROM sched_fires f JOIN turns t ON t.id = CAST(f.turn_id AS INTEGER)
             WHERE f.turn_id IS NOT NULL AND t.state IN ('queued', 'parked', 'claimed', 'running')",
            [],
            |r| r.get(0),
        )?;
        if n > 0 {
            out.push(format!("{n} scheduled turn(s) have not finished"));
        }
        Ok(out)
    }

    fn import(&self, env: &Env, core: Option<&mut Core>, opts: Opts) -> Result<Vec<String>> {
        let dry = opts.dry_run || core.is_none();
        let paths = paths_of(env)?;
        let mac = if dry {
            let m = mac_peek(&env.machine);
            if m.is_empty() {
                "<this Mac's id, made by the step>".to_owned()
            } else {
                m
            }
        } else {
            crate::schedules::mac_id(&env.machine)?
        };
        let plan = plan(env, &mac)?;
        let mut lines = plan.notes.clone();
        for r in &plan.refused {
            lines.push(format!("refused {r}"));
        }
        let mut by_agent: BTreeMap<&str, Vec<&Row>> = BTreeMap::new();
        for row in &plan.rows {
            by_agent.entry(&row.agent).or_default().push(row);
        }
        let mut visit: BTreeSet<String> = by_agent.keys().map(|a| (*a).to_owned()).collect();
        // An agent with no row may still hold a job whose routine is gone: look at it, lock only if there is one.
        for a in agent_names(paths) {
            if visit.contains(&a) {
                continue;
            }
            let file = crate::schedules::jobs_file(paths, &a);
            if let Ok((_, jobs)) = crate::schedules::read_jobs(&env.machine, &a, &file)
                && !orphans(&jobs, &a, &mac, &plan).is_empty()
            {
                visit.insert(a);
            }
        }
        let mut cx = Apply {
            env,
            plan: &plan,
            mac: &mac,
            write: !dry,
            counts: Counts::default(),
            lines,
            removed: Vec::new(),
        };
        for agent in &visit {
            let rows = by_agent.get(agent.as_str()).cloned().unwrap_or_default();
            if let Err(e) = apply_agent(&mut cx, agent, &rows) {
                // One agent's unreadable file is that agent's; it is a difference, so the step cannot switch.
                cx.lines.push(format!("refused {agent}: {e}"));
            }
        }
        let Apply {
            counts,
            mut lines,
            removed,
            ..
        } = cx;
        let routines = plan
            .rows
            .iter()
            .map(|r| &r.routine)
            .collect::<BTreeSet<_>>()
            .len();
        lines.push(format!(
            "schedules routines: {routines} routine(s) as {} job(s): {} {}, {} {}, {} unchanged, {} removed, {} refused",
            plan.rows.len(),
            counts.added,
            if dry { "to add" } else { "added" },
            counts.updated,
            if dry { "to update" } else { "updated" },
            counts.same,
            counts.removed,
            plan.refused.len(),
        ));
        // The keys, then the state `kith-sched` kept.
        if let (false, Some(core)) = (dry, core) {
            for (agent, job) in &removed {
                Envelope::delete(core.conn(), &crate::schedules::hook_key_name(agent, job))?;
            }
            for row in plan.rows.iter().filter(|r| r.key.is_some()) {
                lines.extend(self.put_key(core, row)?);
            }
            lines.extend(
                crate::import::schedules::run_in(&env.layout, &env.machine, false, Some(core))?
                    .lines,
            );
        } else {
            lines.extend(
                crate::import::schedules::run_in(&env.layout, &env.machine, true, None)?.lines,
            );
        }
        Ok(lines)
    }

    fn same_answer(&self, env: &Env, db: &Connection) -> Result<Vec<String>> {
        if tombstoned(env) {
            let moved = state::read(&env.layout, self.name())?
                .is_some_and(|s| matches!(s.phase, Phase::Switched | Phase::RollingBack));
            if moved {
                return Ok(Vec::new());
            }
            return Ok(vec![
                "sched/state.json is a tombstone but schedules is not switched here (kith rollback schedules brings the file back)"
                    .into(),
            ]);
        }
        let paths = paths_of(env)?;
        let mac = mac_peek(&env.machine);
        let plan = plan(env, &mac)?;
        let mut diffs: Vec<String> = plan
            .refused
            .iter()
            .map(|r| format!("{r}: not imported"))
            .collect();
        let mut agents: BTreeSet<String> = plan.rows.iter().map(|r| r.agent.clone()).collect();
        agents.extend(agent_names(paths));
        for agent in agents {
            let file = crate::schedules::jobs_file(paths, &agent);
            let jobs = match crate::schedules::read_jobs(&env.machine, &agent, &file) {
                Ok((_, j)) => j,
                Err(e) => {
                    if plan.rows.iter().any(|r| r.agent == agent) {
                        diffs.push(format!("{agent}: jobs.json cannot be read: {e}"));
                    }
                    continue;
                }
            };
            for row in plan.rows.iter().filter(|r| r.agent == agent) {
                match verdict(&jobs, row, &mac) {
                    Verdict::Same => {}
                    Verdict::New => {
                        diffs.push(format!("{agent}/{}: a routine, not a job yet", row.job));
                    }
                    Verdict::Update(_) => {
                        diffs.push(format!(
                            "{agent}/{}: the job differs from its routine",
                            row.job
                        ));
                    }
                    Verdict::Refuse(why) => diffs.push(format!("{agent}/{}: {why}", row.job)),
                }
                if let Some(want) = &row.key {
                    if key_record_exists(db, &agent, &row.job)? {
                        // Compared by digest; neither value is ever printed.
                        match self.stored_key_digest(db, &agent, &row.job) {
                            Ok(Some(d)) if d == sha_hex(want.as_bytes()) => {}
                            Ok(_) => diffs.push(format!(
                                "{agent}/{}: the webhook key in the core is not the host's",
                                row.job
                            )),
                            Err(e) => diffs.push(format!(
                                "{agent}/{}: the webhook key in the core cannot be read ({e})",
                                row.job
                            )),
                        }
                    } else {
                        diffs.push(format!(
                            "{agent}/{}: the webhook key is not in the core",
                            row.job
                        ));
                    }
                }
            }
            for i in orphans(&jobs, &agent, &mac, &plan) {
                diffs.push(format!(
                    "{agent}/{}: a job whose routine is gone from serve.db, or no longer asks for it",
                    text(&jobs[i], "id").unwrap_or_default()
                ));
            }
        }
        diffs.extend(crate::import::schedules::run_diffs(db, &env.machine)?);
        Ok(diffs)
    }

    fn restore_secrets(&self, env: &Env, core: &mut Core) -> Result<Vec<String>> {
        self.write_routines_back(env, core)
    }

    fn export_back(&self, env: &Env, core: &mut Core) -> Result<Vec<String>> {
        let paths = paths_of(env)?;
        let mac = mac_peek(&env.machine);
        let mut lines = Vec::new();
        let mut removed: Vec<(String, String)> = Vec::new();
        for agent in agent_names(paths) {
            let file = crate::schedules::jobs_file(paths, &agent);
            let mine = |jobs: &[Value]| -> Vec<usize> {
                jobs.iter()
                    .enumerate()
                    .filter(|(_, j)| {
                        j.get("routine").is_some() && text(j, "mac") == Some(mac.as_str())
                    })
                    .map(|(i, _)| i)
                    .collect()
            };
            // Only lock an agent that has one; a file that cannot be read is left alone and said.
            match crate::schedules::read_jobs(&env.machine, &agent, &file) {
                Ok((_, jobs)) if mine(&jobs).is_empty() => continue,
                Ok(_) => {}
                Err(e) => {
                    lines.push(format!("{agent}: jobs.json not read ({e}); left as it is"));
                    continue;
                }
            }
            let _lock = lock_jobs(&env.machine, &agent)?;
            let mut jobs = read_list(&file).map_err(|e| {
                Error::Refused(format!("cutover: schedules: {}: {e}", file.display()))
            })?;
            let gone = mine(&jobs);
            for &i in gone.iter().rev() {
                let j = jobs.remove(i);
                removed.push((agent.clone(), text(&j, "id").unwrap_or_default().to_owned()));
            }
            write_list(&file, &jobs)?;
        }
        // The jobs are gone, so the keys they held are: `restore_secrets` already put them back in serve.db. By prefix, not
        // by the jobs removed just now: after a crash and a re-run those jobs are already gone, and their records must not
        // be stranded.
        // Only this import writes these records; narrow the prefix if another writer ever appears.
        let prefix = "routine-webhook/";
        let mut dropped = 0;
        for name in Envelope::list(core.conn())? {
            if name.starts_with(prefix) && Envelope::delete(core.conn(), &name)? {
                dropped += 1;
            }
        }
        lines.push(format!(
            "schedules: deleted {dropped} webhook key record(s)"
        ));
        lines.push(format!(
            "schedules: removed {} imported job(s) from the agents' jobs.json",
            removed.len()
        ));
        lines.push(write_state_back(env, core.conn(), &mac)?);
        lines
            .push("start the host so it reads its routines again (it keeps them in memory)".into());
        Ok(lines)
    }

    fn digest(&self, env: &Env, _db: &Connection) -> Result<String> {
        let mut defs: Vec<String> = plan(env, &mac_peek(&env.machine))?
            .rows
            .iter()
            .map(|r| format!("{}/{}={}", r.agent, r.job, r.def))
            .collect();
        defs.sort();
        Ok(Sha256::digest(defs.join("\n").as_bytes())
            .iter()
            .fold(String::new(), |mut s, b| {
                use std::fmt::Write as _;
                let _ = write!(s, "{b:02x}");
                s
            }))
    }
}

fn sha_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut s, b| {
            use std::fmt::Write as _;
            let _ = write!(s, "{b:02x}");
            s
        })
}

impl Schedules {
    /// SHA-256 of the key stored for a job, read with an envelope of its own (the check holds only a connection).
    fn stored_key_digest(&self, db: &Connection, agent: &str, job: &str) -> Result<Option<String>> {
        let mut envelope = match &self.keys {
            Some(make) => Envelope::with_store(make()),
            None => Envelope::os()?,
        };
        Ok(envelope
            .get(db, &crate::schedules::hook_key_name(agent, job))?
            .map(|b| sha_hex(&b)))
    }

    /// The job's key as the secret record hooks.rs reads. Written only when missing or different, so a re-import changes
    /// nothing. Says that it wrote, never what.
    fn put_key(&self, core: &Core, row: &Row) -> Result<Vec<String>> {
        let Some(key) = &row.key else {
            return Ok(Vec::new());
        };
        let name = crate::schedules::hook_key_name(&row.agent, &row.job);
        self.with_envelope(core, |sealed, conn| {
            let same = sealed
                .get(conn, &name)?
                .is_some_and(|have| have.as_slice() == key.as_bytes());
            if same {
                return Ok(Vec::new());
            }
            sealed.put(conn, &name, key.as_bytes())?;
            Ok(vec![format!(
                "{}/{}: webhook key stored as {name}",
                row.agent, row.job
            )])
        })
    }

    /// Rollback, first half: serve.db's `routines.v1` gets its keys and enabled flags back from the jobs and records the
    /// core held. Fields the host owns are not touched; a routine the host no longer has is said, not recreated.
    fn write_routines_back(&self, env: &Env, core: &mut Core) -> Result<Vec<String>> {
        let paths = paths_of(env)?;
        let mac = mac_peek(&env.machine);
        // routine id -> (enabled, key) as the core has it. A routine with several jobs is enabled when any job is.
        let mut back: BTreeMap<String, (bool, Option<String>)> = BTreeMap::new();
        for agent in agent_names(paths) {
            let file = crate::schedules::jobs_file(paths, &agent);
            let Ok((_, jobs)) = crate::schedules::read_jobs(&env.machine, &agent, &file) else {
                continue;
            };
            for j in jobs.iter().filter(|j| text(j, "mac") == Some(mac.as_str())) {
                let Some(routine) = j.get("routine").and_then(|r| text(r, "id")) else {
                    continue;
                };
                let job = text(j, "id").unwrap_or_default();
                let enabled = j.get("paused").and_then(Value::as_bool) != Some(true);
                let key = self.with_envelope(core, |sealed, conn| {
                    Ok(sealed
                        .get(conn, &crate::schedules::hook_key_name(&agent, job))?
                        .and_then(|b| String::from_utf8(b.to_vec()).ok())
                        .filter(|k| !k.is_empty()))
                })?;
                let e = back.entry(routine.to_owned()).or_insert((false, None));
                e.0 |= enabled;
                if e.1.is_none() {
                    e.1 = key;
                }
            }
        }
        let file = serve_db_of(env);
        if back.is_empty() || !file.is_file() {
            return Ok(vec![
                "schedules: no routine to write back to serve.db".to_owned(),
            ]);
        }
        let mut db = Connection::open(&file)
            .map_err(|e| Error::Io(format!("cutover: schedules: {}: {e}", file.display())))?;
        db.busy_timeout(Duration::from_secs(10)).ok();
        let tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| Error::Io(format!("cutover: schedules: serve.db is busy: {e}")))?;
        let raw: Option<String> = tx
            .query_row("SELECT v FROM kv WHERE k = ?1", [ROUTINES_KEY], |r| {
                r.get(0)
            })
            .optional()?;
        let mut lines = Vec::new();
        let Some(raw) = raw else {
            return Ok(vec![
                "schedules: serve.db holds no routines; their jobs stay removed".into(),
            ]);
        };
        let mut state: Value = serde_json::from_str(&raw).map_err(|e| {
            Error::Refused(format!(
                "cutover: schedules: serve.db's {ROUTINES_KEY}: {e}"
            ))
        })?;
        let mut changed = 0;
        let mut seen = BTreeSet::new();
        if let Some(list) = state.get_mut("routines").and_then(Value::as_array_mut) {
            for r in list.iter_mut() {
                let Some(id) = text(r, "id").map(str::to_owned) else {
                    continue;
                };
                let Some((enabled, key)) = back.get(&id) else {
                    continue;
                };
                seen.insert(id);
                let Some(o) = r.as_object_mut() else { continue };
                let mut set = |k: &str, v: Value| {
                    if o.get(k) == Some(&v) {
                        false
                    } else {
                        o.insert(k.to_owned(), v);
                        true
                    }
                };
                let a = set("enabled", json!(enabled));
                let b = key.as_ref().is_some_and(|k| set("webhookKey", json!(k)));
                if a || b {
                    changed += 1;
                }
            }
        }
        for id in back.keys().filter(|id| !seen.contains(*id)) {
            lines.push(format!(
                "routine {id} is no longer in serve.db; its job is removed and it is not brought back"
            ));
        }
        if changed > 0 {
            tx.execute(
                "UPDATE kv SET v = ?2 WHERE k = ?1",
                rusqlite::params![ROUTINES_KEY, state.to_string()],
            )?;
        }
        tx.commit()?;
        lines.push(format!(
            "schedules: wrote {changed} routine(s) back to serve.db (enabled flags and webhook keys)"
        ));
        Ok(lines)
    }
}

fn key_record_exists(db: &Connection, agent: &str, job: &str) -> Result<bool> {
    let name = crate::schedules::hook_key_name(agent, job);
    Ok(db
        .query_row(
            "SELECT 1 FROM secret_records WHERE name = ?1",
            [&name],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .is_some())
}

fn tombstoned(env: &Env) -> bool {
    crate::cutover::tombstone::is_tombstone(&env.machine.join("sched").join("state.json"))
}

/// `state.json` again, over the tombstone: the last tick and each job's last run on this Mac from the core, and any other
/// top-level key the original had (read from the copy the tombstone kept, `state.json.pre-cutover-<time>`).
fn write_state_back(env: &Env, db: &Connection, mac: &str) -> Result<String> {
    let dir = env.machine.join("sched");
    let file = dir.join("state.json");
    let mut out = Map::new();
    // Keys the original had besides the two this file is rebuilt from (`pruned`).
    let kept = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("state.json.pre-cutover-")
        })
        .max_by_key(std::fs::DirEntry::file_name)
        .and_then(|e| std::fs::read(e.path()).ok())
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    if let Some(Value::Object(o)) = kept {
        for (k, v) in o {
            if k != "tick" && k != "last" {
                out.insert(k, v);
            }
        }
    }
    let wm: Option<i64> = db
        .query_row(
            "SELECT at FROM sched_watermark WHERE mac = ?1",
            [mac],
            |r| r.get(0),
        )
        .optional()?;
    let tick = match wm.and_then(crate::schedules::local_minute) {
        Some(t) => Some(t),
        None => db
            .query_row("SELECT tick FROM sched_tick WHERE mac = ?1", [mac], |r| {
                r.get::<_, String>(0)
            })
            .optional()?,
    };
    if let Some(t) = tick {
        out.insert("tick".into(), json!(t));
    }
    let mut last = Map::new();
    let mut st = db.prepare(
        "SELECT agent, job, at, why, pid, log FROM sched_runs
         WHERE mac = ?1 AND id IN (SELECT max(id) FROM sched_runs WHERE mac = ?1 GROUP BY agent, job)
         ORDER BY agent, job",
    )?;
    let rows = st.query_map([mac], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<i64>>(4)?,
            r.get::<_, Option<String>>(5)?,
        ))
    })?;
    for row in rows {
        let (agent, job, at, why, pid, log) = row?;
        if at.is_empty() {
            continue;
        }
        let mut e = Map::new();
        e.insert("at".into(), json!(at));
        if let Some(p) = pid {
            e.insert("pid".into(), json!(p));
        }
        if let Some(l) = log {
            e.insert("log".into(), json!(l));
        }
        if let Some(w) = why {
            e.insert("why".into(), json!(w));
        }
        last.insert(format!("{agent}/{job}"), Value::Object(e));
    }
    let n = last.len();
    out.insert("last".into(), Value::Object(last));
    let io = |e: std::io::Error| Error::Io(format!("cutover: schedules: {}: {e}", file.display()));
    std::fs::create_dir_all(&dir).map_err(io)?;
    let tmp = dir.join("state.json.tmp");
    std::fs::write(
        &tmp,
        serde_json::to_string_pretty(&Value::Object(out)).unwrap_or_default(),
    )
    .map_err(io)?;
    std::fs::rename(&tmp, &file).map_err(io)?;
    Ok(format!(
        "schedules: wrote sched/state.json again (the last tick and {n} job(s)' last run)"
    ))
}
