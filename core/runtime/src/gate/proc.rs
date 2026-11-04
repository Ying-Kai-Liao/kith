//! The one liveness rule the gate, card claims, doors and conversation claims share, and the real system behind it.
//!
//! A start is recorded as a stamp (kernel `procstart`): `e<epoch ms>` on macOS and Windows, `b<boot id>:<ticks>` on
//! Linux; compared with plain equality, no tolerance (a wall-clock step moves neither). Rows written before this recorded
//! `ps -o lstart=` text (local time) or nothing, and a database may hold `e…` rows from Linux or `b…` rows read on
//! another OS: a recorded value and a reading in different forms is an *unknown start*. It never frees a live holder;
//! only a dead pid (ESRCH / exited) frees it.
//!
//! Card claims recorded without a start get no backfill (unlike the gate): `claim_until` expires them anyway.

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
        procstart::start_stamp(pid)
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

/// Whether a recorded start and a current reading are one process; see [`procstart::same_process`]: equality in one
/// form, "unknown start" (kept alive unless the pid is gone) across forms.
pub(crate) fn same_process(recorded: &str, current: &str) -> bool {
    procstart::same_process(recorded, current)
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
    fn stamps_compare_by_equality_and_unknown_starts_never_free() {
        assert!(!same_process("e1000", "e1500"));
        assert!(same_process("e1000", "e1000"));
        assert!(!same_process("bA:1", "bB:1"));
        assert!(same_process("", "e9000"));
        assert!(same_process("Mon Nov  4 10:00:00 2026", "e9000"));
        assert!(same_process("Mon Nov  4 10:00:00 2026", "bA:1"));
        assert!(same_process("e1000", "bA:1"));
        assert!(same_process("bA:1", "e1000"));
        assert!(!same_process("e1", "e2"));
    }

    #[test]
    fn the_real_system_knows_this_process() {
        let me = std::process::id();
        let recorded = System.start(me).expect("start");
        assert!(recorded.starts_with(['e', 'b']));
        assert!(holder_alive(&System, me, &recorded));
        assert!(!System.gone(me));
    }
}
