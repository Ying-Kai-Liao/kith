//! End one process only if it still began at the recorded time (windows-owner W12-3, rule P1). The check and the kill
//! use one handle, so a pid handed to a stranger between a stamp check and a `taskkill` cannot be hit: the handle names
//! the process it was opened on, and its creation time is read from that same handle just before `TerminateProcess`.

use windows_sys::Win32::Foundation::{ERROR_INVALID_PARAMETER, FILETIME, GetLastError};
use windows_sys::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
    TerminateProcess,
};

use crate::Handle;

/// 100 ns ticks between 1601-01-01 (FILETIME) and 1970-01-01. The same constant as `kith-procstart`, which makes the
/// `e<ms>` start stamp from it; this crate links no other crate of ours, so the formula is repeated, not shared.
const EPOCH_DIFF_TICKS: i64 = 116_444_736_000_000_000;

/// What [`terminate_if_started_at`] did.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Killed {
    /// The process began at the recorded time and was told to end.
    Yes,
    /// No such process, or it began at another time (the pid is a stranger's now): nothing was touched.
    NotThatProcess,
    /// It could not be opened (access denied, say), its start could not be read, or the kill failed: nothing is known.
    Unsure,
}

/// The `e<ms>` start stamp's milliseconds for a creation time of `created` FILETIME ticks.
fn epoch_ms(created: u64) -> i64 {
    // A FILETIME that does not fit i64 is not a real creation time; the compare then fails and nothing is killed.
    (i64::try_from(created).unwrap_or(i64::MAX) - EPOCH_DIFF_TICKS) / 10_000
}

/// Ends `pid` if, and only if, it began at `recorded_epoch_ms` (the number in an `e<ms>` start stamp of `kith-procstart`).
pub fn terminate_if_started_at(pid: u32, recorded_epoch_ms: i64) -> Killed {
    if pid == 0 {
        return Killed::NotThatProcess;
    }
    // SAFETY: a plain call with a pid; a null handle means failure.
    let h = unsafe {
        OpenProcess(
            PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION,
            0,
            pid,
        )
    };
    if h.is_null() {
        // SAFETY: reads the calling thread's last error.
        return if unsafe { GetLastError() } == ERROR_INVALID_PARAMETER {
            Killed::NotThatProcess
        } else {
            Killed::Unsure
        };
    }
    let h = Handle(h);
    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
    // SAFETY: `h` is an open process handle and the four out-pointers outlive the call.
    let ok = unsafe {
        GetProcessTimes(
            h.0,
            &raw mut created,
            &raw mut exited,
            &raw mut kernel,
            &raw mut user,
        )
    };
    if ok == 0 {
        return Killed::Unsure;
    }
    let ticks = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
    if epoch_ms(ticks) != recorded_epoch_ms {
        return Killed::NotThatProcess;
    }
    // SAFETY: `h` is an open handle with PROCESS_TERMINATE, the one whose start was just compared.
    if unsafe { TerminateProcess(h.0, 1) } != 0 {
        Killed::Yes
    } else {
        Killed::Unsure
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn a_process_is_ended_only_at_its_own_start_time() {
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "ping -n 30 127.0.0.1 >NUL"])
            .spawn()
            .unwrap();
        let ms = epoch_ms(crate::tree::creation_time(child.id()).unwrap());
        // Another start time: a stranger holding the pid. Not touched.
        assert_eq!(
            terminate_if_started_at(child.id(), ms + 5_000),
            Killed::NotThatProcess
        );
        assert!(child.try_wait().unwrap().is_none(), "still running");
        assert_eq!(terminate_if_started_at(child.id(), ms), Killed::Yes);
        child.wait().unwrap();
        assert_ne!(terminate_if_started_at(child.id(), ms), Killed::Yes);
        assert_eq!(terminate_if_started_at(0, ms), Killed::NotThatProcess);
    }
}
