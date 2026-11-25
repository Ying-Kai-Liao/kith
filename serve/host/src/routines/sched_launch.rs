//! The cron job's run as the launch (design 2026-11-24-sched-turn, S2): `<kith> kernel launch AGENT --exec --kernel K --turn N
//! --claim C`, which attaches the claim and runs the engine in this one process, instead of `kith-sched run-turn`. Only a program
//! that lists `sched-turn` is sent it (S-R3); a program without it keeps `run-turn` (`core_cron.rs`).
//!
//! What `kith-sched run-turn` built before it exec'd, the host builds here: the run's origin (the call key of the agent's chat
//! with the person and the run outbox, `calls::run_origin`, as the host's `runEnv` answers it) goes to the launch in the spec on
//! stdin, and the log (`<logs>/sched/runs/AGENT/JOB-YYYYMMDD-HHMM.log`) is opened here and handed over as stdout and stderr: a
//! file, never a pipe, so a host restart does not cut the run's output. It is opened without truncating: the launch truncates and
//! heads it once its attach has won, so a second launch for the same turn leaves the winner's log as it is.
//!
//! The process group (Windows: its own group and no console) is the run door's [`detach`](super::run_door::detach).

use crate::hub::Hub;
use kith_kernel::paths::Paths;
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The capability token of a program whose `launch --exec` takes `--turn N --claim C`.
const TOKEN: &str = "sched-turn";

/// The program the launch is started from: `$KITH_BIN`, else this very binary (the host and the kernel crate are one program).
/// A test of the runner gives it a stand-in.
#[cfg(not(test))]
fn program_here() -> Option<PathBuf> {
    std::env::var_os("KITH_BIN").map(PathBuf::from).filter(|p| p.is_file()).or_else(|| std::env::current_exe().ok())
}

#[cfg(test)]
fn program_here() -> Option<PathBuf> {
    TEST_PROGRAM.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
}

/// The program a test of the runner starts the launch from (`None`: no program lists the token).
#[cfg(test)]
pub(crate) static TEST_PROGRAM: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

/// The environment a test gives the launch in place of the test process's own (`None`: the process's, as a host's is).
#[cfg(test)]
pub(crate) static TEST_ENV: std::sync::Mutex<Option<Vec<(String, String)>>> = std::sync::Mutex::new(None);

/// The program to start the launch from when it lists `sched-turn` (`kernel version`), else `None`: the run is `run-turn`'s.
pub(super) fn program() -> Option<PathBuf> {
    let program = program_here()?;
    let out = Command::new(&program)
        .args(["kernel", "version"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    String::from_utf8_lossy(&out.stdout).contains(&format!("\"{TOKEN}\"")).then_some(program)
}

/// The job as its agent's `jobs.json` has it (the turn's settings win over it in the launch), else just its id.
fn job_of(folders: &Paths, agent: &str, id: &str) -> Value {
    let own = folders.agents.join(agent);
    let file = if own.join("role").is_dir() { own } else { folders.state.join("agents").join(agent) }.join("jobs.json");
    let found = std::fs::read_to_string(file)
        .ok()
        .and_then(|t| serde_json::from_str::<Vec<Value>>(&t).ok())
        .and_then(|jobs| jobs.into_iter().find(|j| j["id"] == id));
    match found {
        Some(Value::Object(mut j)) => {
            j.remove("task");
            Value::Object(j)
        }
        _ => json!({"id": id}),
    }
}

/// The run's log, `<logs>/sched/runs/AGENT/JOB-YYYYMMDD-HHMM.log`, opened for writing without truncating (made if needed).
fn open_log(folders: &Paths, agent: &str, job: &str) -> std::io::Result<(std::fs::File, PathBuf)> {
    let dir = folders.logs.join("sched/runs").join(agent);
    std::fs::create_dir_all(&dir)?;
    let name = format!("{job}-{}.log", chrono::Local::now().format("%Y%m%d-%H%M"));
    let path = dir.join(name.replace(['/', '\\'], "_"));
    Ok((std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&path)?, path))
}

/// The run's origin as `kith-sched`'s `run_env` made it: the call key and chat of the person's app chat with the agent, and the
/// run's outbox (not for the owner with no app chat: what they send goes where it always went). A guest with no chat has an
/// outbox too, so what the run sends is dropped, never the owner's.
fn origin_of(hub: &Hub, agent: &str, job: &Value, person: Option<&str>) -> Map<String, Value> {
    let origin = crate::calls::run_origin(hub, agent, person, false);
    let has_chat = origin["chat"].is_string();
    let mut env = origin["env"].as_object().cloned().unwrap_or_default();
    if has_chat || person.is_some() {
        match crate::run_outbox::for_schedule(agent, job, person) {
            Ok(dir) => {
                let dir = Value::from(dir.to_string_lossy().into_owned());
                env.insert("KITH_RUN_OUTBOX".to_owned(), dir.clone());
                env.insert("HQ_RUN_OUTBOX".to_owned(), dir);
            }
            Err(error) => tracing::warn!(agent, error = format!("{error:#}"), "no outbox for a scheduled run"),
        }
    }
    env
}

/// The launch, not yet started, and the spec it reads on stdin: the turn's prompt and settings, the turn's person (`null`: the
/// owner), the job and the origin.
pub(super) struct Built {
    pub cmd: Command,
    pub spec: Value,
    /// The run's log: the reason of a refusal is its last line.
    pub log: PathBuf,
    /// The outbox made for this run: removed by [`remove_outbox`] when the launch never starts.
    pub outbox: Option<PathBuf>,
}

/// Removes the outbox (and its meta file) made for a run that did not happen; the launch does the same for one it refuses.
pub(super) fn remove_outbox(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
    let mut meta = dir.to_path_buf().into_os_string();
    meta.push(".json");
    let _ = std::fs::remove_file(meta);
}

/// A test makes every launch fail to start (a folder to run in that is not there).
#[cfg(test)]
pub(crate) static TEST_BAD_CWD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The pids the host started launches as (a test reads them; a host has no use for the pid: the launch attaches itself).
#[cfg(test)]
pub(crate) static SPAWNED: std::sync::Mutex<Vec<u32>> = std::sync::Mutex::new(Vec::new());

/// Told the started process's pid: kept for a test, nothing for a host.
pub(super) fn note_pid() -> Option<super::run_door::Bind> {
    #[cfg(test)]
    return Some(Box::new(|pid| {
        SPAWNED.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(pid);
        Box::new(())
    }));
    #[cfg(not(test))]
    None
}

/// What a launch is built from.
pub(super) struct Which<'a> {
    pub agent: &'a str,
    pub job: &'a str,
    /// `None`: the owner.
    pub person: Option<&'a str>,
    pub turn: i64,
    pub claim: i64,
}

/// Builds the launch of `t` (the core's listed turn), with `program` and `kernel`.
pub(super) fn build(
    hub: &Hub,
    folders: &Paths,
    program: &Path,
    kernel: &Path,
    t: &Value,
    w: &Which<'_>,
) -> Result<Built, String> {
    let job = job_of(folders, w.agent, w.job);
    // The log first: the outbox is made only for a launch that can be started.
    let (log, log_path) = open_log(folders, w.agent, w.job).map_err(|e| format!("can't open the run's log: {e}"))?;
    let origin = origin_of(hub, w.agent, &job, w.person);
    let outbox = origin.get("KITH_RUN_OUTBOX").and_then(Value::as_str).map(PathBuf::from);
    let err = log.try_clone().map_err(|e| format!("can't share the run's log: {e}"))?;
    let mut cmd = Command::new(program);
    #[cfg(test)]
    if let Some(env) = TEST_ENV.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone() {
        cmd.env_clear().envs(env);
    }
    // The run's identity comes from the launch (the person is the spec's; a run token is issued to a run, not inherited from
    // whatever started the host), as every other kernel spawn of the host does.
    for var in [
        crate::runauth::TOKEN_ENV,
        crate::runauth::SOCKET_ENV,
        crate::runauth::CORE_SOCKET_ENV,
        "KITH_PERSON",
        "HQ_PERSON",
    ] {
        cmd.env_remove(var);
    }
    cmd.args(["kernel", "launch", w.agent, "--exec", "--kernel"])
        .arg(kernel)
        .args(["--turn", &w.turn.to_string(), "--claim", &w.claim.to_string()])
        .env("KITH_BIN", program)
        .stdin(Stdio::piped())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(err));
    super::run_door::detach(&mut cmd);
    let spec =
        json!({"prompt": t["prompt"], "settings": t["settings"], "person": w.person, "job": job, "origin": origin});
    #[cfg(test)]
    if TEST_BAD_CWD.load(std::sync::atomic::Ordering::SeqCst) {
        cmd.current_dir("/nonexistent-kith-test-folder");
    }
    Ok(Built { cmd, spec, log: log_path, outbox })
}

// Unix-only: the stand-ins are shell scripts.
#[cfg(test)]
#[cfg(unix)]
#[path = "sched_launch_tests.rs"]
mod tests;
