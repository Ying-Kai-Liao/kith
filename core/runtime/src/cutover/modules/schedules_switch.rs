//! The schedules switch's locks and the host's next runs (M4-5 handover; architect MUST 2, rule H1 on the core side,
//! H2). Wiring only: `schedules.rs` calls these.
//!
//! - [`HostHold`]: `host.lock` in serve.db's folder, held EXCLUSIVELY from where `step` / `rollback` take their source
//!   lock to the end. The host takes the same file with `try_lock_exclusive` when it starts (`serve/host/src/service/
//!   mod.rs`), so no host can start between the refusal check and the tombstone (a host that started then would read no
//!   switch file, run as `Gate::Own`, fire serve.db's routines and save `next_runs` after the import read them; during a
//!   rollback it would save its in-memory routines over the write-back). The file is created when missing. A held lock
//!   is the usual "the host is running" refusal. A reusable guard: the rollback's content (package B) only calls it.
//! - [`TickHold`]: `<machine>/sched/tick.lock`, the lock `kith-sched` takes (flock) around its read-decide-record-save
//!   of a tick. Held from before the import reads state.json to the tombstone, so a tick in flight finishes first and
//!   none starts: the state the import reads is the state the tombstone replaces.
//! - [`next_of`]: what the host would do next with one trigger of a routine, as the import writes it (H2, see
//!   `schedules::anchor`).

use std::fs::{File, OpenOptions};
use std::path::Path;
use std::time::{Duration, Instant};

use fs2::FileExt as _;
use kith_kernel::schedule::Trigger;
use serde_json::Value;

use crate::cutover::Env;
use crate::error::{Error, Result};

/// How long a step waits for a tick that holds `tick.lock` (a tick takes it for milliseconds; prune, the ask sweep and
/// the wake drain are outside it).
pub(super) const TICK_WAIT: Duration = Duration::from_secs(30);

/// `host.lock`, exclusive; let go on drop.
pub(super) struct HostHold(#[allow(dead_code, reason = "held for its drop")] File);

/// Takes `host.lock` for good (no wait: a running host does not let go). `running` is the verb's "the host is running".
pub(super) fn hold_host(env: &Env, running: &str) -> Result<HostHold> {
    let dir = super::schedules::serve_db_of(env)
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| Error::Io("cutover: schedules: serve.db has no folder".into()))?;
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
    loop {
        match f.try_lock_exclusive() {
            Ok(()) => return Ok(TickHold(f)),
            Err(e) if e.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
                if Instant::now() >= until {
                    return Err(Error::Refused(format!(
                        "cutover: schedules: {} is held by a kith-sched tick for more than {}s; try again",
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

pub(super) fn hold_both(env: &Env, running: &str, tick_wait: Duration) -> Result<SwitchHold> {
    let host = hold_host(env, running)?;
    let tick = hold_tick(&env.machine, tick_wait)?;
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
        (Some(t), _) => Next::At(t),
        // The host stores no next run for a `once` after it fired it. One whose time is still ahead was never fired
        // (the host always stores its next run), so it is left to the plain rule.
        (None, Trigger::Once { at }) if *at <= now => {
            Next::OnceFired(crate::schedules::anchor::slot_ms(*at))
        }
        (None, _) => Next::Nothing,
    }
}
