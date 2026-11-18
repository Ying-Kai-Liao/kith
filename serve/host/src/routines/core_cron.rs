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
//! A call that frees the claim (the cancel of a run that could not start, the cancel and the report of one that ended) is asked
//! again until the core answers ([`until_told`]): a claim of kind `turn` with no process would otherwise hold the job's
//! conversation for as long as this host lives (only an adopt at the next start ends it) and every later fire would park
//! behind it. A refusal is an answer: the claim or turn is gone. The retry lives on the run's own thread, which ends with
//! the host process (adopt handles what a stop leaves).
//!
//! The child gets the host's environment minus the run token and `KITH_PERSON`/`HQ_PERSON` (`kith-sched` builds the person
//! from the turn's, which the spec carries so `run-turn` can check it is the claim's). `KITH_HOME` is inherited as the host
//! has it; unset, `kith-sched` finds the folder by the same rule as the host does.
//!
//! `KITH_SCHED_PERMISSION_MODE` (the mode a scheduled run starts in, else the agent's, else kith's default) is not in the launchd
//! plist of `dev.kith.sched` either: it comes from the environment `kith-sched` runs in. The host passes its own environment's
//! value through, so a host started with it set runs the cron jobs the way `kith-sched` did with it set.

use super::core_runs::{Ops, Started};
use crate::terminal::core_writer::{CoreWriter, RunClaim, Told};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

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
        // The owner's turns carry the owner's people id; `run-turn` knows the owner as no person. Unreadable: say so, as the
        // routines' path does, instead of running as someone unchecked.
        let Some(spec_person) = person_in_spec(t, person) else {
            return Started::Failed("the core can't say whose turn this is (no owner read)".to_owned());
        };
        let claim = match self.0.claim_run(conversation, person, turn) {
            RunClaim::Claimed(id) => id,
            RunClaim::Later | RunClaim::Down(_) => return Started::Later,
            RunClaim::Gone => return Started::Gone,
            RunClaim::Refused(why) => return Started::Failed(why),
        };
        let Some(kernel) = kernel_here() else {
            let why = "no kith kernel here to run the job".to_owned();
            self.free(turn, &why);
            return Started::Failed(why);
        };
        let agent = t["agent"].as_str().unwrap_or_default();
        let spec = json!({"prompt": t["prompt"], "settings": t["settings"], "person": spec_person});
        match spawn(&kernel, agent, job, turn, claim, &spec) {
            Ok(child) => {
                let writer = self.0.clone();
                std::thread::spawn(move || over(&writer, child, claim, turn, &Pace::LIVE));
                Started::Ran
            }
            Err(error) => {
                let why = format!("couldn't start kith-sched run-turn: {error}");
                self.free(turn, &why);
                Started::Failed(why)
            }
        }
    }
}

/// The kernel the run starts from; a test of the runner gives it a stand-in (`TEST_KERNEL`) whose `bin/kith-sched` ends at once.
fn kernel_here() -> Option<std::path::PathBuf> {
    #[cfg(test)]
    if let Some(k) = TEST_KERNEL.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone() {
        return Some(k);
    }
    crate::kernel::current_kernel()
}

#[cfg(test)]
pub(crate) static TEST_KERNEL: std::sync::Mutex<Option<std::path::PathBuf>> = std::sync::Mutex::new(None);

/// The `person` of the spec `run-turn` checks: null for the owner (the core's `owner: true`, whatever people id it is), else the
/// turn's person. `None`: the core could not read the owner, so the turn cannot be placed.
pub(super) fn person_in_spec(t: &Value, person: &str) -> Option<Value> {
    t["owner"].as_bool().map(|owner| if owner { Value::Null } else { json!(person) })
}

impl Live {
    /// Gives a claimed turn back for good (cancelled): once now, then on a thread of its own until the core answers, so the
    /// puller is never held and the claim never outlives a core that was briefly down.
    fn free(&self, turn: i64, why: &str) {
        if matches!(self.0.cancel_told(turn, why), Told::Down(_)) {
            let (writer, why) = (self.0.clone(), why.to_owned());
            std::thread::spawn(move || {
                until_told("cancel", turn, &Pace::LIVE, || writer.cancel_told(turn, &why));
            });
        }
    }
}

/// How long to wait between two asks of a core that does not answer: from `first`, doubling, at most `cap`.
pub(super) struct Pace {
    pub first: Duration,
    pub cap: Duration,
}

impl Pace {
    const LIVE: Pace = Pace { first: Duration::from_secs(2), cap: Duration::from_secs(60) };
}

/// Asks until the core answers (anything but [`Told::Down`]), pacing the asks by `pace`: never a tight loop, and unbounded
/// while the host lives. Logs the first failure only (a core down for minutes would otherwise fill the log). A refusal ends it:
/// the claim or turn it was about is gone. `what` names the call in the log.
pub(super) fn until_told(what: &str, turn: i64, pace: &Pace, mut ask: impl FnMut() -> Told) -> Told {
    let mut wait = pace.first;
    let mut logged = false;
    loop {
        match ask() {
            Told::Down(why) => {
                if !std::mem::replace(&mut logged, true) {
                    tracing::warn!(
                        why,
                        turn,
                        what,
                        "routines: the core did not answer a scheduled run's end; asking again"
                    );
                }
                std::thread::sleep(wait);
                wait = (wait * 2).min(pace.cap);
            }
            told => return told,
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
    // The run's identity comes from `kith-sched` (`KITH_PERSON` is built from the turn's person; a run token is issued to a run,
    // not inherited from whatever started the host), as every other kernel spawn of the host does.
    for var in [crate::runauth::TOKEN_ENV, crate::runauth::SOCKET_ENV, "KITH_PERSON", "HQ_PERSON"] {
        cmd.env_remove(var);
    }
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
fn over(writer: &CoreWriter, mut child: Child, claim: i64, turn: i64, pace: &Pace) {
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
    // A run that never attached has its turn cancelled first (a no-op for one that did); both are asked until the core answers.
    if !clean {
        until_told("cancel", turn, pace, || writer.cancel_told(turn, &why));
    }
    until_told("report", turn, pace, || writer.run_over(claim, pid, clean, &why));
}

#[cfg(test)]
#[path = "core_cron_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "core_cron_retry_tests.rs"]
mod retry_tests;
