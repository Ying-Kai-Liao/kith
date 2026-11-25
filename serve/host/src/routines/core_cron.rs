//! The host's part of running a cron job's turn the core fired (M4-5 package C; design 2026-11-17-schedules-handover Q1/Q2).
//! A job of `kith-sched` (not a routine's bot) runs as a detached process, `kith-sched run-turn`, which `exec`s `kith <agent>
//! task`: the pid IS the run. It does not go through a bot's actor (a guest bot's run would be new surface for the revoke and
//! run-token checks) and not through kith-holder (opt-in); the run is the CLI path those checks already cover, and a guest's
//! job runs as its person (`KITH_PERSON` is built by `kith-sched`, as it does for its own tick).
//!
//! The claim, the spawn in a group of its own and the report of the end are the detached-run module's (`run_door.rs`), shared
//! with the wake turns; this file is what a cron job adds: the command `kith-sched run-turn AGENT JOB --turn N --claim C`, the
//! turn and its fire-time settings as JSON on stdin, and the environment below.
//!
//! The child gets the host's environment minus the run token and `KITH_PERSON`/`HQ_PERSON` (`kith-sched` builds the person
//! from the turn's, which the spec carries so `run-turn` can check it is the claim's). `KITH_HOME` is inherited as the host
//! has it; unset, `kith-sched` finds the folder by the same rule as the host does.
//!
//! `KITH_SCHED_PERMISSION_MODE` (the mode a scheduled run starts in, else the agent's, else kith's default) is not in the launchd
//! plist of `dev.kith.sched` either: it comes from the environment `kith-sched` runs in. The host passes its own environment's
//! value through, so a host started with it set runs the cron jobs the way `kith-sched` did with it set.

use super::core_runs::Ops;
use super::run_door::{Launch, Run, Started, detach, start};
use super::sched_launch;
use crate::hub::Hub;
use crate::terminal::core_writer::CoreWriter;
use serde_json::{Value, json};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;

#[cfg(test)]
use super::run_door::{Pace, over, until_told};

/// The environment variable `kith-sched` reads for the permission mode of a scheduled run; the host passes it on.
pub(super) const PERMISSION_MODE: &str = "KITH_SCHED_PERMISSION_MODE";

/// The ops over the real core and the system.
pub(super) struct Live(pub Arc<CoreWriter>);

impl Ops for Live {
    fn cancel(&self, turn: i64, why: &str) -> bool {
        self.0.cancel_turn(turn, why)
    }

    fn cron(&self, hub: &Hub, t: &Value, job: &str) -> Started {
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
        let agent = t["agent"].as_str().unwrap_or_default();
        let spec = json!({"prompt": t["prompt"], "settings": t["settings"], "person": spec_person});
        // A program that lists `sched-turn` runs the turn as the launch; one that does not (an older one, a Mac whose program
        // list lacks it) keeps `kith-sched run-turn` (design 2026-11-24-sched-turn S-R3).
        let program = sched_launch::program();
        let run = Run {
            turn,
            conversation,
            person,
            kind: "scheduleRun",
            what: if program.is_some() { "kith kernel launch" } else { "kith-sched run-turn" },
            ended: "the scheduled run",
        };
        let outbox = std::cell::RefCell::new(None);
        let started = start(&self.0, &run, |claim| {
            let kernel = kernel_here().ok_or_else(|| "no kith kernel here to run the job".to_owned())?;
            if let Some(program) = &program {
                let w = sched_launch::Which { agent, job, person: spec_person.as_str(), turn, claim };
                let built = sched_launch::build(hub, self.0.folders(), program, &kernel, t, &w)?;
                *outbox.borrow_mut() = built.outbox;
                return Ok(Launch {
                    cmd: built.cmd,
                    stdin: Some(built.spec),
                    err_file: Some(built.log),
                    bind: sched_launch::note_pid(),
                });
            }
            Ok(Launch { cmd: command(&kernel, agent, job, turn, claim), stdin: Some(spec), err_file: None, bind: None })
        });
        // A launch that never started leaves no run for its outbox to belong to.
        if let (Started::Cancelled(_), Some(dir)) = (&started, outbox.borrow().as_deref()) {
            sched_launch::remove_outbox(dir);
        }
        started
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

/// The command line: `<kernel>/bin/kith-sched run-turn AGENT JOB --turn N --claim C`, stdin the turn, stderr read for the reason it
/// refused, stdout nowhere (the run's output goes to its log once it is the engine). Its own process group (the module's
/// [`detach`]), so a host stop or restart does not signal it; the environment is the host's own (`KITH_SCHED_PERMISSION_MODE`
/// among it, see the module doc).
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
    for var in [
        crate::runauth::TOKEN_ENV,
        crate::runauth::SOCKET_ENV,
        crate::runauth::CORE_SOCKET_ENV,
        "KITH_PERSON",
        "HQ_PERSON",
    ] {
        cmd.env_remove(var);
    }
    cmd.args(["run-turn", agent, job, "--turn", &turn.to_string(), "--claim", &claim.to_string()])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if let Some(mode) = mode {
        cmd.env(PERMISSION_MODE, mode);
    }
    detach(&mut cmd);
    cmd
}

// Unix-only until windows-owner W2b: uses std::os::unix / UnixListener / libc.
#[cfg(test)]
#[cfg(unix)]
#[path = "core_cron_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "core_cron_retry_tests.rs"]
mod retry_tests;
