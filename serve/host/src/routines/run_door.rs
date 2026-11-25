//! The one host module that starts and reports a detached run, whatever kind it is (design 2026-11-21-wake-s4 R3, settled
//! 2026-11-24; the cron job of `kith-sched` was the first kind, `core_cron.rs`, and the wake turn is the second,
//! `cards/resume/wake_clock.rs`). A detached run is a turn the core queued that no host began: the host claims it, starts a
//! process of its own in a group of its own, waits for it and tells the core how it ended.
//!
//! Per pulled turn:
//! 1. **Claim** it with no process yet (`CoreWriter::claim_run_as`, `conversations::sched_run` in the core). A host change
//!    before the run attaches gives the turn back (adopt requeues it); a pause, a revoke or the rollback cancel it.
//! 2. **Spawn** the kind's command in its own process group (Windows: its own process group and no console window), with the
//!    claim's id where the kind says. It outlives a host restart. The process records its own pid on the claim (`claim.attach`,
//!    a check-and-set) before it starts the engine, and goes no further when that fails.
//! 3. **Report**: a thread waits for the child and tells the core `done` (clean exit) or `exit` (anything else: the turn is
//!    `failed`, never put back: its prompt may have gone out). A host that was restarted meanwhile has no such thread: the
//!    core's `end_gone` ends the claim when the process is gone.
//!
//! A call that frees the claim (the cancel of a run that could not start, the cancel and the report of one that ended) is asked
//! again until the core answers ([`until_told`]): a claim of kind `turn` with no process would otherwise hold the conversation
//! for as long as this host lives (only an adopt at the next start ends it) and every later turn would park behind it. A
//! refusal is an answer: the claim or turn is gone. The retry lives on the run's own thread, which ends with the host process
//! (adopt handles what a stop leaves).
//!
//! What a kind gives: its claim kind, the command (its words and environment) and, when it has them, the turn on stdin and the
//! file its stderr goes to. Everything else is here.

use crate::terminal::core_writer::{CoreWriter, RunClaim, Told};
use serde_json::Value;
use std::any::Any;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Arc;
use std::time::Duration;

/// What [`start`] came to.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Started {
    /// Claimed and running as a process of its own.
    Ran,
    /// Not now (a writer holds the conversation, the core did not answer): the turn stays queued and is pulled again.
    Later,
    /// The turn is gone (the core cancelled it, it was claimed elsewhere, its person was revoked): nothing to do.
    Gone,
    /// It cannot run and the claim is given back: the turn is cancelled with this reason.
    Failed(String),
}

/// The turn a kind runs.
pub(crate) struct Run<'a> {
    pub turn: i64,
    pub conversation: &'a str,
    pub person: &'a str,
    /// The core's claim kind: `scheduleRun`, `wakeRun` or `askRun`.
    pub kind: &'static str,
    /// What the run is called in a reason ("kith-sched run-turn"): "couldn't start <what>".
    pub what: &'static str,
    /// What the run's end is called in a reason ("the scheduled run"): "<ended> ended with <status>".
    pub ended: &'static str,
}

/// What a kind starts once its turn is claimed.
pub(crate) struct Launch {
    /// The command, not yet started. [`detach`] is applied here.
    pub cmd: Command,
    /// The turn the process reads on stdin (the cron job's); `None` for one that reads none.
    pub stdin: Option<Value>,
    /// A file the process's stderr goes to (the run's log): the reason of a refusal is read from its last line. `None`: stderr
    /// is a pipe the report reads.
    pub err_file: Option<PathBuf>,
    /// Told the process's pid once it started (a run token binds to it); what it answers is held until the process ended and
    /// was reported (the token's grant).
    pub bind: Option<Box<dyn FnOnce(u32) -> Box<dyn Any + Send> + Send>>,
}

/// Claims `run`'s turn, starts what `launch` makes of the claim's id and reports its end. Blocking until the process is started
/// (the wait and the report are a thread of their own).
pub(crate) fn start(
    writer: &Arc<CoreWriter>,
    run: &Run<'_>,
    launch: impl FnOnce(i64) -> Result<Launch, String>,
) -> Started {
    let claim = match writer.claim_run_as(run.kind, run.conversation, run.person, run.turn) {
        RunClaim::Claimed(id) => id,
        RunClaim::Later | RunClaim::Down(_) => return Started::Later,
        RunClaim::Gone => return Started::Gone,
        RunClaim::Refused(why) => return Started::Failed(why),
    };
    let launched = match launch(claim) {
        Ok(l) => l,
        Err(why) => {
            free(writer, run.turn, &why);
            return Started::Failed(why);
        }
    };
    let Launch { cmd, stdin, err_file, bind } = launched;
    match spawn_with(cmd, stdin.as_ref()) {
        Ok(child) => {
            let keep = bind.map(|bind| bind(child.id()));
            let (writer, turn, ended) = (writer.clone(), run.turn, run.ended);
            std::thread::spawn(move || {
                over_with(&writer, child, claim, turn, &Pace::LIVE, err_file.as_deref(), ended);
                drop(keep);
            });
            Started::Ran
        }
        Err(error) => {
            let why = format!("couldn't start {}: {error}", run.what);
            free(writer, run.turn, &why);
            Started::Failed(why)
        }
    }
}

/// Gives a claimed turn back for good (cancelled): once now, then on a thread of its own until the core answers, so the
/// puller is never held and the claim never outlives a core that was briefly down.
pub(crate) fn free(writer: &Arc<CoreWriter>, turn: i64, why: &str) {
    if matches!(writer.cancel_told(turn, why), Told::Down(_)) {
        let (writer, why) = (writer.clone(), why.to_owned());
        std::thread::spawn(move || {
            until_told("cancel", turn, &Pace::LIVE, || writer.cancel_told(turn, &why));
        });
    }
}

/// How long to wait between two asks of a core that does not answer: from `first`, doubling, at most `cap`.
pub(crate) struct Pace {
    pub first: Duration,
    pub cap: Duration,
}

impl Pace {
    pub(crate) const LIVE: Pace = Pace { first: Duration::from_secs(2), cap: Duration::from_secs(60) };
}

/// Asks until the core answers (anything but [`Told::Down`]), pacing the asks by `pace`: never a tight loop, and unbounded
/// while the host lives. Logs the first failure only (a core down for minutes would otherwise fill the log). A refusal ends it:
/// the claim or turn it was about is gone. `what` names the call in the log.
pub(crate) fn until_told(what: &str, turn: i64, pace: &Pace, mut ask: impl FnMut() -> Told) -> Told {
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
                        "routines: the core did not answer a detached run's end; asking again"
                    );
                }
                std::thread::sleep(wait);
                wait = (wait * 2).min(pace.cap);
            }
            told => return told,
        }
    }
}

/// Starts `cmd` and hands it the turn on stdin.
pub(crate) fn spawn_cmd(cmd: Command, spec: &Value) -> std::io::Result<Child> {
    spawn_with(cmd, Some(spec))
}

fn spawn_with(mut cmd: Command, spec: Option<&Value>) -> std::io::Result<Child> {
    let mut child = cmd.spawn()?;
    if let (Some(mut stdin), Some(spec)) = (child.stdin.take(), spec) {
        // A write failing means the child is already gone; its exit code says why.
        let _ = stdin.write_all(spec.to_string().as_bytes());
    }
    Ok(child)
}

/// Puts the child in a group of its own (it leads it, as a run of `start` leads its session), so a host stop or restart does not
/// signal it and a kill by pid reaches the group. Windows has no process groups to signal by pid: its twin is a group of its
/// own with no console, which the launch's job object (windows-owner design) builds on.
pub(crate) fn detach(cmd: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }
}

/// Waits for the run and tells the core how it ended, for a run whose stderr is a pipe. Runs on a thread of its own: the run may
/// take hours.
#[cfg(test)]
pub(crate) fn over(writer: &CoreWriter, child: Child, claim: i64, turn: i64, pace: &Pace) {
    over_with(writer, child, claim, turn, pace, None, "the scheduled run");
}

/// [`over`] for a run whose stderr may be a file (`err_file`): the reason is the last line of what the run said.
pub(crate) fn over_with(
    writer: &CoreWriter,
    mut child: Child,
    claim: i64,
    turn: i64,
    pace: &Pace,
    err_file: Option<&Path>,
    ended: &str,
) {
    let pid = child.id();
    // Until the run is the engine, stderr is its refusal; the engine's own output goes to its log, so this is EOF by then.
    let mut said = String::new();
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut said);
    }
    let status = child.wait();
    if said.is_empty()
        && let Some(file) = err_file
    {
        said = std::fs::read_to_string(file).unwrap_or_default();
    }
    let clean = status.as_ref().is_ok_and(std::process::ExitStatus::success);
    let why = said.lines().rev().find(|l| !l.is_empty()).map_or_else(
        || format!("{ended} ended with {}", status.map_or_else(|e| e.to_string(), |s| s.to_string())),
        str::to_owned,
    );
    // A run that never attached has its turn cancelled first (a no-op for one that did); both are asked until the core answers.
    if !clean {
        until_told("cancel", turn, pace, || writer.cancel_told(turn, &why));
    }
    until_told("report", turn, pace, || writer.run_over(claim, pid, clean, &why));
}
