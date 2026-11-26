//! Liveness and Stop of a recorded process through `procstart` (windows-owner W12-3, rule P1): a pid plus the start stamp
//! recorded with it, never `ps`, `pgrep` or `lstart`. The same stamp is the process, another stamp is a stranger that was
//! handed the pid, and a stamp that cannot be read or compared is neither: only `Probe::Gone` says gone. [`judge`] is the
//! pure rule (tested on every platform); the rest is the Windows side.

use kith_kernel::procstart::{Probe, comparable, is_stamp, same_process};

/// What a probe of a pid says about the process recorded with it.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
// Only the Windows code asks; the rows run everywhere.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) enum Seen {
    /// The recorded process, still there.
    Same,
    /// Another process holds the pid now: the recorded one is gone.
    Other,
    /// The system says no such process.
    Gone,
    /// Not told apart (an unreadable start, a refusal, a record of another form or none): not gone, and never killed.
    Unsure,
}

#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn judge(recorded: Option<&str>, probe: &Probe) -> Seen {
    match probe {
        Probe::Gone => Seen::Gone,
        Probe::Unknown | Probe::Alive(None) => Seen::Unsure,
        Probe::Alive(Some(now)) => match recorded.map(str::trim) {
            // A row written before stamps (old `lstart` text, none): unknown, not gone.
            Some(rec) if is_stamp(rec) && comparable(rec, now) => {
                if same_process(rec, now) { Seen::Same } else { Seen::Other }
            }
            _ => Seen::Unsure,
        },
    }
}

#[cfg(windows)]
pub(crate) use windows::{Ended, alive, end, is_descendant, remember, started};

#[cfg(windows)]
mod windows {
    use super::{Probe, Seen, judge};
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};

    /// Whether `pid` may still be running: everything but `Gone` (a process we may not read is still there).
    pub(crate) fn alive(pid: u32) -> bool {
        !matches!(kith_kernel::procstart::probe(pid), Probe::Gone)
    }

    /// Whether `pid` is a process below this host's own (a turn it started).
    pub(crate) fn is_descendant(pid: u32) -> bool {
        pid != std::process::id() && kith_winsys::tree::descends(pid, std::process::id())
    }

    /// Ends `pid` alone, by pid (never by image name), on one handle that also re-reads its start, so a pid handed to a
    /// stranger since the stamp check is not hit. A job with kill on close does the rest. `Some(true)` killed, `Some(false)`
    /// it is another process now, `None` not known.
    fn terminate_pid(pid: u32, stamp: &str) -> Option<bool> {
        let ms: i64 = stamp.trim().strip_prefix('e')?.parse().ok()?;
        match kith_winsys::kill::terminate_if_started_at(pid, ms) {
            kith_winsys::kill::Killed::Yes => Some(true),
            kith_winsys::kill::Killed::NotThatProcess => Some(false),
            kith_winsys::kill::Killed::Unsure => None,
        }
    }

    /// What [`end`] did.
    #[derive(Debug, PartialEq, Eq)]
    pub(crate) enum Ended {
        /// The recorded process was there and was told to end.
        Killed,
        /// It was not there (gone, or the pid belongs to another process now): nothing was touched.
        Nobody,
        /// Its start could not be matched to the record (no stamp, unreadable, another form), or the kill failed: nothing
        /// is known, and nothing was killed.
        NotSure,
    }

    /// Ends the process recorded as `pid` + `recorded` after a start-stamp check.
    pub(crate) fn end(pid: u32, recorded: Option<&str>) -> Ended {
        match judge(recorded, &kith_kernel::procstart::probe(pid)) {
            Seen::Same => match recorded.and_then(|r| terminate_pid(pid, r)) {
                Some(true) => Ended::Killed,
                Some(false) => Ended::Nobody,
                None => Ended::NotSure,
            },
            Seen::Unsure => Ended::NotSure,
            Seen::Gone | Seen::Other => Ended::Nobody,
        }
    }

    /// Start stamps of the channel runs this host started (its Stop has only the pid).
    static STARTED: LazyLock<Mutex<HashMap<u32, String>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

    /// Records the start stamp of a process this host just started.
    pub(crate) fn remember(pid: u32) {
        let Some(stamp) = kith_kernel::procstart::start_stamp(pid) else { return };
        let mut table = STARTED.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        // Entries of runs that ended are dropped here, so the table stays the size of the runs in flight.
        table.retain(|p, s| judge(Some(s), &kith_kernel::procstart::probe(*p)) == Seen::Same);
        table.insert(pid, stamp);
    }

    /// The stamp [`remember`] took for `pid`.
    pub(crate) fn started(pid: u32) -> Option<String> {
        STARTED.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&pid).cloned()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn alive(s: &str) -> Probe {
        Probe::Alive(Some(s.into()))
    }

    #[test]
    fn the_same_stamp_is_the_process_and_another_stamp_is_a_stranger() {
        assert_eq!(judge(Some("e1000"), &alive("e1000")), Seen::Same);
        assert_eq!(judge(Some("e1000"), &alive("e2000")), Seen::Other);
    }

    #[test]
    fn only_gone_says_gone() {
        assert_eq!(judge(Some("e1000"), &Probe::Gone), Seen::Gone);
        assert_eq!(judge(None, &Probe::Gone), Seen::Gone);
        // Access denied (alive, start unreadable) and any other error are not gone.
        assert_eq!(judge(Some("e1000"), &Probe::Alive(None)), Seen::Unsure);
        assert_eq!(judge(Some("e1000"), &Probe::Unknown), Seen::Unsure);
    }

    #[test]
    fn a_missing_or_old_record_is_unknown_not_gone() {
        assert_eq!(judge(None, &alive("e1000")), Seen::Unsure);
        assert_eq!(judge(Some(""), &alive("e1000")), Seen::Unsure);
        // A Mac's `ps -o lstart` text in an old row: never read as a start here, never a crash.
        assert_eq!(judge(Some("Mon Nov 24 10:00:00 2026"), &alive("e1000")), Seen::Unsure);
        // Another form (a Linux boot stamp against an epoch one).
        assert_eq!(judge(Some("babc:12"), &alive("e1000")), Seen::Unsure);
    }

    #[cfg(windows)]
    mod real {
        use super::super::windows::{Ended, alive, end, remember, started};
        use kith_kernel::procstart::start_stamp;

        fn sleeper() -> std::process::Child {
            std::process::Command::new("ping")
                .args(["-n", "30", "127.0.0.1"])
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap()
        }

        #[test]
        fn a_stamp_checked_stop_ends_only_the_recorded_process() {
            let mut child = sleeper();
            let pid = child.id();
            let stamp = start_stamp(pid).unwrap();
            // Wrong stamp (a reused pid), none, and old text: nothing is killed.
            assert_eq!(end(pid, Some("e1")), Ended::Nobody);
            assert_eq!(end(pid, None), Ended::NotSure);
            assert_eq!(end(pid, Some("Mon Nov 24 10:00:00 2026")), Ended::NotSure);
            assert!(child.try_wait().unwrap().is_none() && alive(pid));
            assert_eq!(end(pid, Some(&stamp)), Ended::Killed);
            child.wait().unwrap();
            assert!(!alive(pid), "an exited process is gone");
            assert_eq!(end(pid, Some(&stamp)), Ended::Nobody, "a second stop of a gone process touches nothing");
        }

        #[test]
        fn a_claim_is_judged_by_its_stamp_alone() {
            use crate::terminal::claim::{Claim, Kind};
            let claim = |pid: u32, start: Option<&str>| Claim {
                pid,
                proc_start: start.map(str::to_owned),
                at: String::new(),
                kind: Kind::Terminal,
                takeover: false,
                person: None,
                agent: None,
            };
            let mut child = sleeper();
            let pid = child.id();
            let stamp = start_stamp(pid).unwrap();
            let own = claim(pid, Some(&stamp));
            assert!(own.is_live() && own.is_claimer());
            // A reused pid (another stamp): not live, never the claimer.
            let reused = claim(pid, Some("e1"));
            assert_eq!(reused.identity(), Some(false));
            assert!(!reused.is_live() && !reused.is_claimer());
            // A Mac's lstart text is not read here: held (unknown), never the claimer, no crash.
            let old = claim(pid, Some("Mon Nov 24 10:00:00 2026"));
            assert!(old.is_live() && !old.is_claimer());
            child.kill().unwrap();
            child.wait().unwrap();
            assert!(!own.is_live(), "an exited claimer is gone");
        }

        #[test]
        fn a_remembered_start_is_read_back() {
            let mut child = sleeper();
            remember(child.id());
            assert_eq!(started(child.id()), kith_kernel::procstart::start_stamp(child.id()));
            assert!(started(0).is_none());
            child.kill().unwrap();
            child.wait().unwrap();
        }
    }
}
