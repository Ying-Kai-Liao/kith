//! The one liveness rule the gate, card claims, doors and conversation claims share, and the real system behind it.
//!
//! A start time is recorded as `e<epoch ms>` (kernel `procstart`: the OS's own record, no `ps`, no local-time text), so a
//! change of time zone changes nothing. Rows written before this recorded `ps -o lstart=` text (local time) or nothing;
//! those are an *unknown start*: they cannot be compared with an epoch, so they never free a live holder, and a dead
//! pid (ESRCH) still frees them.

use kith_kernel::procstart::{self, Probe};

/// What a caller asks the system about a holder's process. A parameter so tests can end a "process" without killing one.
pub(crate) trait Liveness {
    /// The start time of a pid in the recorded form ([`stamp`]); `None` when it cannot be read (gone, or refused).
    fn start(&self, pid: u32) -> Option<String>;
    /// True only when the system says there is no such process. An error, a refusal or a missing record is not "gone":
    /// a failed probe must never free a live holder.
    fn gone(&self, pid: u32) -> bool;
}

/// The real system.
pub(crate) struct System;

impl Liveness for System {
    fn start(&self, pid: u32) -> Option<String> {
        procstart::start_ms(pid).map(stamp)
    }
    fn gone(&self, pid: u32) -> bool {
        pid_gone(pid)
    }
}

/// The system says there is no such process (ESRCH; on Windows an invalid pid or one that has exited). Anything else,
/// including a probe that failed, is not gone.
pub(crate) fn pid_gone(pid: u32) -> bool {
    procstart::probe(pid) == Probe::Gone
}

/// How far two readings of one process' start may differ and still be the same process: Linux's record is clock ticks
/// since boot added to `btime`, which the clock can nudge by a second or so.
const TOLERANCE_MS: i64 = 2000;

/// The recorded form of a start time.
pub(crate) fn stamp(ms: i64) -> String {
    format!("e{ms}")
}

fn epoch(s: &str) -> Option<i64> {
    s.strip_prefix('e')?.parse().ok()
}

/// Whether a recorded start time and a current reading are one process.
/// - nothing was recorded, or old `lstart` text (an unknown start) against an epoch reading: yes, it cannot be told apart
///   and a wrong "no" would free a live holder;
/// - two epochs: within [`TOLERANCE_MS`];
/// - anything else (a test's own markers): equal text.
pub(crate) fn same_process(recorded: &str, current: &str) -> bool {
    if recorded.is_empty() {
        return true;
    }
    match (epoch(recorded), epoch(current)) {
        (Some(a), Some(b)) => (a - b).abs() <= TOLERANCE_MS,
        (None, Some(_)) => true,
        _ => recorded == current,
    }
}

/// The rule, one place. The start time reads and is the recorded one: alive. It reads and is another: the pid was
/// reused, dead. It cannot be read: dead only when the system says there is no such process.
pub(crate) fn holder_alive(live: &dyn Liveness, pid: u32, recorded: &str) -> bool {
    match live.start(pid) {
        Some(current) => same_process(recorded, &current),
        None => !live.gone(pid),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epochs_compare_with_a_tolerance_and_old_text_never_frees() {
        assert!(same_process("e1000", "e1500"));
        assert!(!same_process("e1000", "e9000"));
        assert!(same_process("", "e9000"));
        assert!(same_process("Mon Nov  4 10:00:00 2026", "e9000"));
        assert!(!same_process("t1", "t2"));
        assert!(same_process("t1", "t1"));
    }

    #[test]
    fn the_real_system_knows_this_process() {
        let me = std::process::id();
        let recorded = System.start(me).expect("start");
        assert!(recorded.starts_with('e'));
        assert!(holder_alive(&System, me, &recorded));
        assert!(!System.gone(me));
    }
}
