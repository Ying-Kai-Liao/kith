//! The process tree, for the run-token ancestry check (`kith-core`'s `descends`).
//!
//! Windows records a process's parent pid at creation and never updates it, so when the parent exits and its pid is
//! given to a new process, the child's parent pid points at a stranger. That is why each hop also compares creation
//! times: a real parent began before its child.

use std::collections::HashMap;
use std::io;

use windows_sys::Win32::Foundation::{FILETIME, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};

use crate::{Handle, last_err};

/// Longest chain walked: a process tree this deep is not a run's.
const MAX_HOPS: usize = 64;

/// The parent pid of every process alive now, from one snapshot.
fn parents() -> io::Result<HashMap<u32, u32>> {
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
    let mut map = HashMap::new();
    // SAFETY: `snap` is an open snapshot and `entry.dwSize` is set, as the call requires.
    let mut more = unsafe { Process32FirstW(snap.0, &raw mut entry) } != 0;
    while more {
        map.insert(entry.th32ProcessID, entry.th32ParentProcessID);
        // SAFETY: as above.
        more = unsafe { Process32NextW(snap.0, &raw mut entry) } != 0;
    }
    Ok(map)
}

/// When `pid` began, as 100 ns ticks (the FILETIME), or `None` when it cannot be read or is gone.
pub fn creation_time(pid: u32) -> Option<u64> {
    // SAFETY: a plain call with a pid; a null handle means failure.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if h.is_null() {
        return None;
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
    (ok != 0).then(|| (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
}

/// Whether `pid` is `ancestor` or has it among its parents. Any trouble (a process gone, a pid reused by a stranger,
/// a time that cannot be read, a chain too long) is "no".
pub fn descends(pid: u32, ancestor: u32) -> bool {
    if pid == ancestor {
        return pid != 0;
    }
    let Ok(parents) = parents() else {
        return false;
    };
    let mut cur = pid;
    for _ in 0..MAX_HOPS {
        let Some(&parent) = parents.get(&cur) else {
            return false;
        };
        if parent == 0 || parent == cur {
            return false;
        }
        // The parent must have begun before the child, or its pid now belongs to another process.
        let (Some(child_began), Some(parent_began)) = (creation_time(cur), creation_time(parent))
        else {
            return false;
        };
        if parent_began > child_began {
            return false;
        }
        if parent == ancestor {
            return true;
        }
        cur = parent;
    }
    false
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn a_spawned_child_descends_from_this_process_and_a_stranger_does_not() {
        let me = std::process::id();
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "ping -n 20 127.0.0.1 >NUL"])
            .spawn()
            .unwrap();
        let ok = descends(child.id(), me);
        let reversed = descends(me, child.id());
        // Pid 4 is the System process: not a descendant of this one.
        let stranger = descends(4, me);
        let _ = child.kill();
        let _ = child.wait();
        assert!(ok, "the child descends from its parent");
        assert!(!reversed, "a parent is not below its child");
        assert!(!stranger, "an unrelated pid is refused");
        assert!(!descends(0, me));
        assert!(descends(me, me));
    }
}
