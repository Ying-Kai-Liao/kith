//! The process list and a hard stop of one process, for the kernel's claim sweep and its compaction's timeout (W12-4). Both are
//! what `ps` and `kill -9` are on a Unix: a row per process with its parent, and `TerminateProcess`.
//!
//! Windows has no signal to ask a process to leave and no process group to name, so the callers walk the rows to find what is
//! below a process and stop each one.

use std::io;

use windows_sys::Win32::Foundation::{ERROR_INVALID_PARAMETER, GetLastError, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::Foundation::FILETIME;
use windows_sys::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE, TerminateProcess,
};

use crate::{Handle, last_err};

/// One process of the system.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub pid: u32,
    /// The pid recorded at creation. It is not updated when the parent ends, so it can name a stranger that was given the
    /// pid after (see `tree`): callers who stop what is below a process check the start stamps first.
    pub parent: u32,
    /// The program's file name (`claude.exe`), as the snapshot gives it.
    pub exe: String,
}

/// Every process alive now, from one snapshot.
pub fn table() -> io::Result<Vec<Row>> {
    // SAFETY: a plain call; the result is checked against the invalid value.
    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snap == INVALID_HANDLE_VALUE {
        return Err(last_err());
    }
    let snap = Handle(snap);
    let mut entry = PROCESSENTRY32W {
        dwSize: u32::try_from(size_of::<PROCESSENTRY32W>()).unwrap_or(0),
        cntUsage: 0,
        th32ProcessID: 0,
        th32DefaultHeapID: 0,
        th32ModuleID: 0,
        cntThreads: 0,
        th32ParentProcessID: 0,
        pcPriClassBase: 0,
        dwFlags: 0,
        szExeFile: [0; 260],
    };
    let mut rows = Vec::new();
    // SAFETY: `snap` is an open snapshot and `entry.dwSize` is set, as the call requires.
    let mut more = unsafe { Process32FirstW(snap.0, &raw mut entry) } != 0;
    while more {
        let len = entry
            .szExeFile
            .iter()
            .position(|c| *c == 0)
            .unwrap_or(entry.szExeFile.len());
        rows.push(Row {
            pid: entry.th32ProcessID,
            parent: entry.th32ParentProcessID,
            exe: String::from_utf16_lossy(&entry.szExeFile[..len]),
        });
        // SAFETY: as above.
        more = unsafe { Process32NextW(snap.0, &raw mut entry) } != 0;
    }
    Ok(rows)
}

/// Ends the process at once, with `exit_code` (what `TerminateProcess` does: no clean-up runs in it). A pid that names no
/// process is not an error.
pub fn terminate(pid: u32, exit_code: u32) -> io::Result<()> {
    if pid == 0 {
        return Ok(());
    }
    // SAFETY: a plain call with a pid; a null handle means failure.
    let h = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) };
    if h.is_null() {
        // SAFETY: reads the calling thread's last error.
        return match unsafe { GetLastError() } {
            ERROR_INVALID_PARAMETER => Ok(()),
            _ => Err(last_err()),
        };
    }
    let h = Handle(h);
    // SAFETY: `h` is an open process handle with the terminate right.
    if unsafe { TerminateProcess(h.0, exit_code) } == 0 {
        return Err(last_err());
    }
    Ok(())
}

/// 100 ns ticks between 1601-01-01 (FILETIME) and 1970-01-01.
const EPOCH_DIFF_TICKS: i64 = 116_444_736_000_000_000;

/// Ends the process only if it began at `started_ms` (milliseconds since 1970, the number inside a start stamp `e<ms>`): the
/// start is read and the process ended through one open handle, so a pid given to a stranger after the claim was written is never
/// ended. `Ok(true)` when it was ended; `Ok(false)` when it is gone, began at another time or its start cannot be read (none of
/// those is ours to end).
pub fn terminate_started(pid: u32, started_ms: i64, exit_code: u32) -> io::Result<bool> {
    if pid == 0 {
        return Ok(false);
    }
    // SAFETY: a plain call with a pid; a null handle means failure.
    let h = unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if h.is_null() {
        // SAFETY: reads the calling thread's last error.
        return match unsafe { GetLastError() } {
            ERROR_INVALID_PARAMETER => Ok(false),
            _ => Err(last_err()),
        };
    }
    let h = Handle(h);
    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
    // SAFETY: `h` is an open process handle and the four out-pointers outlive the call.
    let got = unsafe {
        GetProcessTimes(h.0, &raw mut created, &raw mut exited, &raw mut kernel, &raw mut user)
    } != 0;
    if !got {
        return Ok(false);
    }
    let ticks = (i64::from(created.dwHighDateTime) << 32) | i64::from(created.dwLowDateTime);
    if (ticks - EPOCH_DIFF_TICKS) / 10_000 != started_ms {
        return Ok(false);
    }
    // SAFETY: `h` is an open process handle with the terminate right.
    if unsafe { TerminateProcess(h.0, exit_code) } == 0 {
        return Err(last_err());
    }
    Ok(true)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn the_table_lists_this_process_and_its_child_and_terminate_ends_the_child() {
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "ping -n 20 127.0.0.1 >NUL"])
            .spawn()
            .unwrap();
        let rows = table().unwrap();
        let me = std::process::id();
        assert!(rows.iter().any(|r| r.pid == me));
        let row = rows.iter().find(|r| r.pid == child.id()).unwrap();
        assert_eq!(row.parent, me);
        assert!(row.exe.eq_ignore_ascii_case("cmd.exe"));
        terminate(child.id(), 3).unwrap();
        assert_eq!(child.wait().unwrap().code(), Some(3));
    }

    #[test]
    fn a_pid_nobody_has_is_not_an_error() {
        // A pid that no process holds: one past the top of the table.
        let top = table().unwrap().iter().map(|r| r.pid).max().unwrap();
        terminate(top + 100_000, 1).unwrap();
        terminate(0, 1).unwrap();
    }

    #[test]
    fn terminate_started_ends_only_the_process_that_began_then() {
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "ping -n 20 127.0.0.1 >NUL"])
            .spawn()
            .unwrap();
        let ticks = crate::tree::creation_time(child.id()).unwrap();
        let ms = (i64::try_from(ticks).unwrap() - EPOCH_DIFF_TICKS) / 10_000;
        assert!(!terminate_started(child.id(), ms + 5_000, 3).unwrap());
        assert!(child.try_wait().unwrap().is_none(), "a different start is left alone");
        assert!(terminate_started(child.id(), ms, 3).unwrap());
        assert_eq!(child.wait().unwrap().code(), Some(3));
        assert!(!terminate_started(child.id(), ms, 3).unwrap(), "gone is not an error");
    }
}
