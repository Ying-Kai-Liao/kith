//! The schedules switch's locks and the host's next runs (M4-5 handover; architect MUST 2, rule H1 on the core side,
//! H2). Wiring only: `schedules.rs` calls these.
//!
//! - [`HostHold`]: `host.lock` in the host's data folder (`KITH_SERVE_HOME`, else `~/.kith/serve`: where the host puts
//!   it, whatever `KITH_SERVE_DB` says), held EXCLUSIVELY from where `step` / `rollback` take their source
//!   lock to the end. The host takes the same file with `try_lock_exclusive` when it starts (`serve/host/src/service/
//!   mod.rs`), so no host can start between the refusal check and the tombstone (a host that started then would read no
//!   switch file, run as `Gate::Own`, fire serve.db's routines and save `next_runs` after the import read them; during a
//!   rollback it would save its in-memory routines over the write-back). The file is created when missing. A held lock
//!   is the usual "the host is running" refusal. A reusable guard: the rollback's content (package B) only calls it.
//! - [`TickHold`]: `<machine>/sched/tick.lock`, the lock `kith-sched` takes (flock) around its read-decide-record-save
//!   of a tick. Held from before the import reads state.json to the tombstone, so a tick in flight finishes first and
//!   none starts: the state the import reads is the state the tombstone replaces. A lock-order case: a kith-sched tick
//!   that holds `tick.lock` while it asks kith-auth (30 s) about a person's job, against a core this step holds, and
//!   the step that waits for `tick.lock`, can time out together. Both fail closed: the step refuses "tick.lock is held"
//!   (run it again) and that minute's person job is skipped by kith-sched.
//! - [`next_of`]: what the host would do next with one trigger of a routine, as the import writes it (H2, see
//!   `schedules::anchor`).

use std::fs::{File, OpenOptions};
use std::path::Path;
use std::time::{Duration, Instant};

use fs2::FileExt as _;
use kith_kernel::schedule::Trigger;
use serde_json::Value;

use super::schedules_gate::{self as gate, Lister};
use crate::cutover::Env;
use crate::error::{Error, Result};

/// How long a step waits for a tick: for one that holds `tick.lock`, and (condition 6) for one that started before it
/// took the lock to end. A tick's work before the lock is the prune, the 30 s ask sweep and the 30 s wake drain, and then
/// its own 30 s wait for the lock; past this the step refuses and changes nothing.
pub(super) const TICK_WAIT: Duration = Duration::from_secs(120);

/// A test of the real binary shortens the wait with this (milliseconds).
const TICK_WAIT_ENV: &str = "KITH_SCHEDULES_TICK_WAIT_MS";

fn tick_wait_of(default: Duration) -> Duration {
    std::env::var(TICK_WAIT_ENV)
        .ok()
        .and_then(|v| v.parse().ok())
        .map_or(default, Duration::from_millis)
}

/// `host.lock`, exclusive; let go on drop.
pub(super) struct HostHold(#[allow(dead_code, reason = "held for its drop")] File);

/// Takes `host.lock` for good (no wait: a running host does not let go). `running` is the verb's "the host is running".
pub(super) fn hold_host(env: &Env, running: &str) -> Result<HostHold> {
    let dir = super::schedules::host_dir_of(env);
    std::fs::create_dir_all(&dir)
        .map_err(|e| Error::Io(format!("cutover: schedules: {}: {e}", dir.display())))?;
    let path = dir.join("host.lock");
    // Never truncated: the host writes nothing into it, and a stale pid would only mislead.
    let f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| {
            Error::Refused(format!(
                "cutover: schedules: host.lock cannot be checked: {}: {e}",
                path.display()
            ))
        })?;
    match f.try_lock_exclusive() {
        Ok(()) => Ok(HostHold(f)),
        Err(e) if e.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
            Err(Error::Refused(format!("cutover: schedules: {running}")))
        }
        Err(e) => Err(Error::Refused(format!(
            "cutover: schedules: host.lock cannot be checked: {e}"
        ))),
    }
}

/// `<machine>/sched/tick.lock`, exclusive; let go on drop.
pub(super) struct TickHold(#[allow(dead_code, reason = "held for its drop")] File);

/// Waits up to `wait` for the tick lock.
pub(super) fn hold_tick(machine: &Path, wait: Duration) -> Result<TickHold> {
    let dir = machine.join("sched");
    std::fs::create_dir_all(&dir)
        .map_err(|e| Error::Io(format!("cutover: schedules: {}: {e}", dir.display())))?;
    let path = dir.join("tick.lock");
    let f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| Error::Io(format!("cutover: schedules: {}: {e}", path.display())))?;
    let until = Instant::now() + wait;
    let mut said = false;
    loop {
        match f.try_lock_exclusive() {
            Ok(()) => return Ok(TickHold(f)),
            Err(e) if e.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
                if !said {
                    // Where the step is, for whoever runs it (the end-to-end test reads this, not the lock).
                    eprintln!("cutover: schedules: waiting for {}", path.display());
                    said = true;
                }
                if Instant::now() >= until {
                    return Err(Error::Refused(format!(
                        "cutover: schedules: {} is held by a kith-sched tick for more than {}s (a tick waiting on kith-auth for a person's job holds it that long); nothing was changed, try again",
                        path.display(),
                        wait.as_secs()
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

/// What a step or rollback holds: the host's lock, then the tick lock (in that order; dropped together).
pub(super) struct SwitchHold {
    _tick: TickHold,
    _host: HostHold,
}

/// What a step does about a tick that started before it took `tick.lock` (condition 6): refuse while one is running, or,
/// for a rollback (the escape hatch, and an old tick that does not take the lock stops at the tombstone), only say so.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum OldTicks {
    Refuse,
    Note,
}

pub(super) fn hold_both(
    env: &Env,
    running: &str,
    tick_wait: Duration,
    list: Lister,
    old: OldTicks,
) -> Result<SwitchHold> {
    let wait = tick_wait_of(tick_wait);
    let host = hold_host(env, running)?;
    let tick = hold_tick(&env.machine, wait)?;
    if let Some(program) = gate::sched_program(&gate::user_home(env)) {
        match old {
            OldTicks::Refuse => {
                eprintln!("cutover: schedules: waiting for a running kith-sched tick to end");
                gate::wait_out_old_ticks(list, &program, wait).map_err(|pid| {
                    Error::Refused(format!(
                        "cutover: schedules: 6: a kith-sched tick (pid {pid}) is still running; try again in a minute; nothing was changed"
                    ))
                })?;
            }
            OldTicks::Note => {
                for t in list(&program) {
                    eprintln!(
                        "cutover: schedules: note: a kith-sched tick (pid {}) is running; it stops at the tombstone",
                        t.pid
                    );
                }
            }
        }
    }
    Ok(SwitchHold {
        _tick: tick,
        _host: host,
    })
}

/// What the host would do next with one trigger of a routine.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Next {
    /// Fires at this instant (ms): the job's anchor.
    At(i64),
    /// A `once` the host already fired: the slot (ms) that gets a decided row, so the core never refires it.
    OnceFired(i64),
    /// Nothing to carry: a webhook or event trigger, a paused routine, a trigger with no next run that is not a fired
    /// `once`.
    Nothing,
}

/// `routine` is the host's JSON (camelCase), `i` the trigger's index, `now` the clock (ms).
pub(super) fn next_of(routine: &Value, i: usize, trigger: &Trigger, now: i64) -> Next {
    if routine.get("enabled").and_then(Value::as_bool) == Some(false) {
        return Next::Nothing;
    }
    if matches!(trigger, Trigger::Webhook | Trigger::Event { .. }) {
        return Next::Nothing;
    }
    let next = routine
        .get("nextRuns")
        .and_then(Value::as_array)
        .and_then(|a| a.get(i))
        .and_then(Value::as_i64);
    match (next, trigger) {
        // A cron slot is a whole minute, but the host keeps the next run with the sub-minute part of the clock it counted
        // from (found by the real-process end-to-end test: `14:00:00.406`). Taken as it is, the core would count from just
        // before an instant that is past the slot, and the first run after the switch would be a whole period late.
        (Some(t), Trigger::Cron { .. }) => Next::At(t.div_euclid(60_000) * 60_000),
        (Some(t), _) => Next::At(t),
        // The host stores no next run for a `once` after it fired it. One whose time is still ahead was never fired
        // (the host always stores its next run), so it is left to the plain rule.
        (None, Trigger::Once { at }) if *at <= now => {
            Next::OnceFired(crate::schedules::anchor::slot_ms(*at))
        }
        (None, _) => Next::Nothing,
    }
}
