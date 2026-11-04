//! One answer to "is that process still there, and is it the one we recorded": the OS's own record, no `ps`, no
//! local-time text. A start time is milliseconds since the Unix epoch, so a change of time zone changes nothing (a
//! change of the wall clock moves Linux's boot-relative record by the same amount; callers compare with a tolerance).
//!
//! - macOS: `proc_pidinfo(PROC_PIDTBSDINFO)`, the record `ps` itself prints from.
//! - Linux: `/proc/<pid>/stat` field 22 (clock ticks since boot) plus `btime` from `/proc/stat`.
//! - Windows: `OpenProcess` + `GetProcessTimes` (creation time), `GetExitCodeProcess` for a process that has ended but
//!   whose handle someone still holds.
//!
//! The answer is three-valued so a failed probe can never be taken for a dead process: [`Probe::Gone`] only when the
//! system says there is no such process (ESRCH; on Windows an invalid pid, or one that has exited), [`Probe::Alive`] when
//! it exists (we may not be allowed to read its start time, so that is optional), [`Probe::Unknown`] for anything else.

#![allow(unsafe_code)]

/// What the system said about a pid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Probe {
    /// The process exists; its start time in epoch milliseconds when it could be read.
    Alive(Option<i64>),
    /// The system says there is no such process (or it is a zombie / has exited).
    Gone,
    /// The probe failed for another reason (a refusal, an unreadable record): neither alive nor gone.
    Unknown,
}

/// Looks at `pid` once.
#[must_use]
pub fn probe(pid: u32) -> Probe {
    imp::probe(pid)
}

/// The start time of `pid` in epoch milliseconds; `None` when it is gone or cannot be read.
#[must_use]
pub fn start_ms(pid: u32) -> Option<i64> {
    match probe(pid) {
        Probe::Alive(start) => start,
        Probe::Gone | Probe::Unknown => None,
    }
}

#[cfg(unix)]
mod imp {
    use super::Probe;

    pub(super) fn probe(pid: u32) -> Probe {
        // Zero would name a process group to kill(2); no process has that pid.
        let Some(p) = libc::pid_t::try_from(pid).ok().filter(|p| *p > 0) else {
            return Probe::Gone;
        };
        // SAFETY: signal 0 only checks that the process exists; nothing is sent.
        let exists = unsafe { libc::kill(p, 0) } == 0;
        if !exists {
            return match std::io::Error::last_os_error().raw_os_error() {
                Some(libc::ESRCH) => Probe::Gone,
                // We may not signal it, but it is there.
                Some(libc::EPERM) => start(pid),
                _ => Probe::Unknown,
            };
        }
        start(pid)
    }

    #[cfg(target_os = "macos")]
    fn start(pid: u32) -> Probe {
        let Ok(pid) = libc::c_int::try_from(pid) else {
            return Probe::Unknown;
        };
        // SAFETY: an all-zero proc_bsdinfo is a valid value for proc_pidinfo to fill in.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let Ok(size) = libc::c_int::try_from(size_of::<libc::proc_bsdinfo>()) else {
            return Probe::Unknown;
        };
        // SAFETY: `info` is `size` bytes and outlives the call; proc_pidinfo writes at most `size` bytes into it.
        let got = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                (&raw mut info).cast::<libc::c_void>(),
                size,
            )
        };
        if got != size {
            // It was there for kill(2) a moment ago: not readable (EPERM) or gone in between.
            return match std::io::Error::last_os_error().raw_os_error() {
                Some(libc::ESRCH) => Probe::Gone,
                _ => Probe::Alive(None),
            };
        }
        // SZOMB: ended, waiting for its parent; nothing runs.
        if info.pbi_status == 5 {
            return Probe::Gone;
        }
        let secs = i64::try_from(info.pbi_start_tvsec).ok();
        let micros = i64::try_from(info.pbi_start_tvusec).ok();
        Probe::Alive(secs.zip(micros).map(|(s, u)| s * 1000 + u / 1000))
    }

    #[cfg(target_os = "linux")]
    fn start(pid: u32) -> Probe {
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            // kill(2) said it exists; its stat file is unreadable or vanished: do not guess.
            return Probe::Alive(None);
        };
        // The command (field 2) is in parentheses and may hold anything, so count from the last ')'.
        let Some(rest) = stat.rfind(')').map(|i| &stat[i + 1..]) else {
            return Probe::Alive(None);
        };
        let mut fields = rest.split_whitespace();
        if fields.next() == Some("Z") {
            return Probe::Gone;
        }
        // Field 3 (state) is consumed; field 22 (starttime) is 18 on.
        let ticks: Option<i64> = fields.nth(18).and_then(|t| t.parse().ok());
        let btime: Option<i64> = std::fs::read_to_string("/proc/stat").ok().and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("btime ")?.trim().parse().ok())
        });
        // SAFETY: sysconf has no preconditions.
        let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        // `c_long` is `i32` on 32-bit targets, so the widening is needed there.
        #[allow(clippy::useless_conversion)]
        let hz = if hz > 0 { i64::from(hz) } else { 100 };
        Probe::Alive(ticks.zip(btime).map(|(t, b)| b * 1000 + t * 1000 / hz))
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    fn start(_: u32) -> Probe {
        Probe::Alive(None)
    }
}

#[cfg(windows)]
mod imp {
    use super::Probe;
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER, FILETIME, GetLastError,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    /// `GetExitCodeProcess` gives this while the process runs.
    const STILL_ACTIVE: u32 = 259;
    /// 100 ns ticks between 1601-01-01 (FILETIME) and 1970-01-01.
    const EPOCH_DIFF_TICKS: i64 = 116_444_736_000_000_000;

    pub(super) fn probe(pid: u32) -> Probe {
        if pid == 0 {
            return Probe::Gone;
        }
        // SAFETY: plain Win32 call with a pid; a null handle means failure.
        let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if h.is_null() {
            // SAFETY: reads the calling thread's last error.
            return match unsafe { GetLastError() } {
                // No process has this id.
                ERROR_INVALID_PARAMETER => Probe::Gone,
                // It exists; we may not look at it.
                ERROR_ACCESS_DENIED => Probe::Alive(None),
                _ => Probe::Unknown,
            };
        }
        let mut code = 0u32;
        // SAFETY: `h` is an open process handle and `code` outlives the call.
        let got_code = unsafe { GetExitCodeProcess(h, &raw mut code) } != 0;
        let zero = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
        // SAFETY: `h` is an open process handle and the four out-pointers outlive the call.
        let got_times = unsafe {
            GetProcessTimes(
                h,
                &raw mut created,
                &raw mut exited,
                &raw mut kernel,
                &raw mut user,
            )
        } != 0;
        // SAFETY: `h` came from OpenProcess and is closed once.
        unsafe { CloseHandle(h) };
        // A process that has ended but whose handle someone still holds is a crashed holder: gone.
        if got_code && code != STILL_ACTIVE {
            return Probe::Gone;
        }
        if !got_times {
            return Probe::Alive(None);
        }
        let ticks = (i64::from(created.dwHighDateTime) << 32) | i64::from(created.dwLowDateTime);
        Probe::Alive(Some((ticks - EPOCH_DIFF_TICKS) / 10_000))
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    use super::Probe;

    pub(super) fn probe(_: u32) -> Probe {
        Probe::Unknown
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn this_process_is_alive_and_started_in_the_past() {
        let Probe::Alive(Some(ms)) = probe(std::process::id()) else {
            panic!("this process is alive with a readable start time");
        };
        let now = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis(),
        )
        .unwrap();
        assert!(
            ms <= now + 2000 && ms > now - 24 * 3600 * 1000,
            "{ms} vs {now}"
        );
        assert_eq!(
            start_ms(std::process::id()),
            Some(ms),
            "stable across calls"
        );
    }

    #[test]
    fn a_child_that_ended_and_was_reaped_is_gone() {
        let mut cmd = if cfg!(windows) {
            let mut c = std::process::Command::new("cmd");
            c.args(["/C", "exit"]);
            c
        } else {
            std::process::Command::new("true")
        };
        let mut child = cmd.spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        assert_eq!(probe(pid), Probe::Gone);
    }

    #[test]
    fn pid_zero_is_nobodys() {
        assert_eq!(probe(0), Probe::Gone);
    }
}
