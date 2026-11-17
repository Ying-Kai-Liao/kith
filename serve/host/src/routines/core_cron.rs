//! The host's part of running a cron job's turn the core fired (M4-5 package C; design 2026-11-17-schedules-handover Q1/Q2).
//! A job of `kith-sched` (not a routine's bot) runs as a detached process, `kith-sched run-turn`, which `exec`s `kith <agent>
//! task`: the pid IS the run. It does not go through a bot's actor (a guest bot's run would be new surface for the revoke and
//! run-token checks) and not through kith-holder (opt-in); the run is the CLI path those checks already cover, and a guest's
//! job runs as its person (`KITH_PERSON` is built by `kith-sched`, as it does for its own tick).
//!
//! Per pulled turn:
//! 1. **Claim** it with no process yet (`CoreWriter::claim_run`, `conversations::sched_run` in the core). A host change before the
//!    run attaches gives the turn back (adopt requeues it); a pause, a revoke or the rollback cancel it.
//! 2. **Spawn** `kith-sched run-turn AGENT JOB --turn N --claim C` in its own process group, with the turn's prompt and its
//!    fire-time settings as JSON on stdin. It outlives a host restart. The process records its own pid on the claim
//!    (`claim.attach`, a check-and-set) before it starts the engine, and goes no further when that fails.
//! 3. **Report**: a thread waits for the child and tells the core `done` (clean exit) or `exit` (anything else: the turn
//!    is `failed`, never put back: its prompt may have gone out). A host that was restarted meanwhile has no such thread: the
//!    core's `end_gone` ends the claim when the process is gone, with the turn `failed` (outcome unknown).
//!
//! `KITH_SCHED_PERMISSION_MODE` (the mode a scheduled run starts in, else the agent's, else kith's default) is not in the launchd
//! plist of `dev.kith.sched` either: it comes from the environment `kith-sched` runs in. The host passes its own environment's
//! value through, so a host started with it set runs the cron jobs the way `kith-sched` did with it set.

use super::core_runs::{Ops, Started};
use crate::terminal::core_writer::{CoreWriter, RunClaim};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;

/// The environment variable `kith-sched` reads for the permission mode of a scheduled run; the host passes it on.
pub(super) const PERMISSION_MODE: &str = "KITH_SCHED_PERMISSION_MODE";

/// The ops over the real core and the system.
pub(super) struct Live(pub Arc<CoreWriter>);

impl Ops for Live {
    fn cancel(&self, turn: i64, why: &str) -> bool {
        self.0.cancel_turn(turn, why)
    }

    fn cron(&self, t: &Value, job: &str) -> Started {
        let (Some(turn), Some(conversation), Some(person)) =
            (t["turn"].as_i64(), t["conversation"].as_str(), t["person"].as_str())
        else {
            return Started::Failed("the core's turn has no id, conversation or person".to_owned());
        };
        let claim = match self.0.claim_run(conversation, person, turn) {
            RunClaim::Claimed(id) => id,
            RunClaim::Later | RunClaim::Down(_) => return Started::Later,
            RunClaim::Gone => return Started::Gone,
            RunClaim::Refused(why) => return Started::Failed(why),
        };
        let Some(kernel) = crate::kernel::current_kernel() else {
            self.0.cancel_turn(turn, "no kith kernel here to run the job");
            return Started::Failed("no kith kernel here to run the job".to_owned());
        };
        let agent = t["agent"].as_str().unwrap_or_default();
        let spec = json!({"prompt": t["prompt"], "settings": t["settings"]});
        match spawn(&kernel, agent, job, turn, claim, &spec) {
            Ok(child) => {
                let writer = self.0.clone();
                std::thread::spawn(move || over(&writer, child, claim, turn));
                Started::Ran
            }
            Err(error) => {
                let why = format!("couldn't start kith-sched run-turn: {error}");
                self.0.cancel_turn(turn, &why);
                Started::Failed(why)
            }
        }
    }
}

/// `kith-sched run-turn` for one turn, with the turn on stdin. The caller waits for the child.
pub(super) fn spawn(
    kernel: &Path,
    agent: &str,
    job: &str,
    turn: i64,
    claim: i64,
    spec: &Value,
) -> std::io::Result<Child> {
    spawn_cmd(command(kernel, agent, job, turn, claim), spec)
}

/// Starts `cmd` and hands it the turn on stdin.
pub(super) fn spawn_cmd(mut cmd: Command, spec: &Value) -> std::io::Result<Child> {
    let mut child = cmd.spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        // A write failing means the child is already gone; its exit code says why.
        let _ = stdin.write_all(spec.to_string().as_bytes());
    }
    Ok(child)
}

/// The command line: `<kernel>/bin/kith-sched run-turn AGENT JOB --turn N --claim C`, stdin the turn, stderr read for the reason it
/// refused, stdout nowhere (the run's output goes to its log once it is the engine). Its own process group (it leads it, as a
/// run of `start` leads its session), so a host stop or restart does not signal it and a kill by pid reaches the group; the
/// environment is the host's own (`KITH_SCHED_PERMISSION_MODE` among it, see the module doc).
pub(super) fn command(kernel: &Path, agent: &str, job: &str, turn: i64, claim: i64) -> Command {
    command_with(kernel, agent, job, turn, claim, std::env::var_os(PERMISSION_MODE))
}

/// [`command`] with the permission mode given (the host's own `KITH_SCHED_PERMISSION_MODE`, when it has one).
pub(super) fn command_with(
    kernel: &Path,
    agent: &str,
    job: &str,
    turn: i64,
    claim: i64,
    mode: Option<std::ffi::OsString>,
) -> Command {
    let mut cmd = Command::new(kernel.join("bin/kith-sched"));
    cmd.args(["run-turn", agent, job, "--turn", &turn.to_string(), "--claim", &claim.to_string()])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if let Some(mode) = mode {
        cmd.env(PERMISSION_MODE, mode);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd
}

/// Waits for the run and tells the core how it ended. Runs on a thread of its own: the run may take hours.
fn over(writer: &CoreWriter, mut child: Child, claim: i64, turn: i64) {
    let pid = child.id();
    // Until the run is the engine, stderr is its refusal; the engine's own output goes to its log, so this is EOF by then.
    let mut said = String::new();
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut said);
    }
    let status = child.wait();
    let clean = status.as_ref().is_ok_and(std::process::ExitStatus::success);
    let why = said.lines().last().filter(|l| !l.is_empty()).map_or_else(
        || format!("the scheduled run ended with {}", status.map_or_else(|e| e.to_string(), |s| s.to_string())),
        str::to_owned,
    );
    if let Err(error) = writer.run_over(claim, turn, pid, clean, &why) {
        // The core is down: the claim's process is gone, so `end_gone` ends it (the turn `failed`) once the core is back.
        tracing::warn!(error, turn, "routines: a scheduled run ended and the core was not told");
    }
}

#[cfg(test)]
#[path = "core_cron_tests.rs"]
mod tests;
