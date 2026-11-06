//! One answer to "is that process still there, and is it the one we recorded": the OS's own record, no `ps`, no
//! local-time text. A start is a *stamp*, a string compared with plain equality, never a tolerance:
//!
//! - macOS and Windows: `e<epoch ms>` (a time zone changes nothing; the record is the process' own creation time, which
//!   a wall-clock step does not move).
//! - Linux: `b<boot id>:<ticks>`, the boot id from `/proc/sys/kernel/random/boot_id` and `/proc/<pid>/stat` field 22
//!   (clock ticks since boot). No `btime`: it moves with every wall-clock step, so a step would free live holders.
//!
//! Sources: macOS `proc_pidinfo(PROC_PIDTBSDINFO)`, the record `ps` prints from; Windows `OpenProcess` +
//! `GetProcessTimes`, and a zero-timeout wait on the handle for a process that has ended but whose handle someone holds.
//!
//! The answer is three-valued so a failed probe can never be taken for a dead process: [`Probe::Gone`] only when the
//! system says there is no such process (ESRCH; on Windows an invalid pid, or one that has exited), [`Probe::Alive`] when
//! it exists (we may not be allowed to read its start, so that is optional), [`Probe::Unknown`] for anything else.
//!
//! Two stamps of different forms (`e…` against `b…`, or old `ps -o lstart=` text or nothing against either) cannot be
//! told apart or told the same: see [`same_process`], which calls that an *unknown start* and never a different process.

#![allow(unsafe_code)]

/// What the system said about a pid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Probe {
    /// The process exists; its start stamp when it could be read.
    Alive(Option<String>),
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

/// The start stamp of `pid`; `None` when it is gone or cannot be read.
#[must_use]
pub fn start_stamp(pid: u32) -> Option<String> {
    match probe(pid) {
        Probe::Alive(start) => start,
        Probe::Gone | Probe::Unknown => None,
    }
}

/// The session id of `pid` (`getsid(2)`): its own pid for a process that called `setsid`, which every holder spawn does.
/// `None` when it is gone, cannot be asked, or on a system without sessions (Windows).
#[must_use]
pub fn session_id(pid: u32) -> Option<u32> {
    imp::session_id(pid)
}

/// True only when the system says no process is left in the process group `pgid` (`kill(-pgid, 0)` is ESRCH). A leader
/// that has died does not empty its group: an engine it started in that group may still run (and write). A refusal or
/// any other error is not "gone". Windows has no process groups (a job object ends each spawn): always true there.
#[must_use]
pub fn group_gone(pgid: u32) -> bool {
    imp::group_gone(pgid)
}

/// The recorded form of an epoch start.
#[must_use]
pub fn epoch_stamp(ms: i64) -> String {
    format!("e{ms}")
}

/// The recorded form of a Linux start (the boot it began in, and its ticks since that boot).
#[must_use]
pub fn boot_stamp(boot_id: &str, ticks: u64) -> String {
    format!("b{}:{ticks}", boot_id.trim())
}

/// Which family a stamp is in; `None` for old `lstart` text, test markers, or nothing.
fn form(s: &str) -> Option<char> {
    if let Some(r) = s.strip_prefix('e') {
        return r.parse::<i64>().is_ok().then_some('e');
    }
    let r = s.strip_prefix('b')?;
    let (id, ticks) = r.split_once(':')?;
    (!id.is_empty() && ticks.parse::<u64>().is_ok()).then_some('b')
}

/// Whether a recorded start and a current reading are one process: plain equality when both are in one form (a
/// different boot id is a different process), and *yes* when the forms differ or the record is unknown (empty, old
/// `lstart` text, `e…` against `b…`): that cannot be told apart, and a wrong "no" would free a live holder. Such a
/// holder is freed only when the pid is gone (ESRCH / exited).
#[must_use]
pub fn same_process(recorded: &str, current: &str) -> bool {
    match (form(recorded), form(current)) {
        (Some(a), Some(b)) if a == b => recorded == current,
        _ => true,
    }
}

/// Whether `s` is a stamp at all (not old `lstart` text, a test marker or nothing).
#[must_use]
pub fn is_stamp(s: &str) -> bool {
    form(s).is_some()
}

/// Whether two stamps are in one form, so [`same_process`] answers from a real comparison and not from "cannot tell".
/// A caller that signals a process on a *yes* asks this first: [`same_process`] is the safe answer for "may I call it
/// dead", not for "may I kill it".
#[must_use]
pub fn comparable(a: &str, b: &str) -> bool {
    matches!((form(a), form(b)), (Some(x), Some(y)) if x == y)
}

/// `/proc/<pid>/stat` text: (state letter, starttime ticks). The command (field 2) is in parentheses and may hold
/// anything, so count from the last `)`. Pure, so the Linux reading is tested on every platform.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_linux_stat(stat: &str) -> Option<(char, u64)> {
    let rest = stat.rfind(')').map(|i| &stat[i + 1..])?;
    let mut fields = rest.split_whitespace();
    let state = fields.next()?.chars().next()?;
    // Field 3 (state) is consumed; field 22 (starttime) is 18 on.
    let ticks = fields.nth(18)?.parse().ok()?;
    Some((state, ticks))
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

    pub(super) fn group_gone(pgid: u32) -> bool {
        // Zero or a negative number would name another target to kill(2); no group has that id.
        let Some(g) = libc::pid_t::try_from(pgid).ok().filter(|g| *g > 0) else {
            return true;
        };
        // SAFETY: signal 0 to a group only checks that one of its processes exists; nothing is sent.
        if unsafe { libc::killpg(g, 0) } == 0 {
            return false;
        }
        std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }

    pub(super) fn session_id(pid: u32) -> Option<u32> {
        let p = libc::pid_t::try_from(pid).ok().filter(|p| *p > 0)?;
        // SAFETY: getsid only reads the process table.
        let sid = unsafe { libc::getsid(p) };
        u32::try_from(sid).ok().filter(|_| sid > 0)
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
            // It was there for kill(2) a moment ago: gone in between, or not readable this way (proc_pidinfo refuses
            // another user's process, init's say). The process table answers for any process, from the same record.
            return match std::io::Error::last_os_error().raw_os_error() {
                Some(libc::ESRCH) => Probe::Gone,
                _ => table_start(pid),
            };
        }
        // SZOMB: ended, waiting for its parent; nothing runs.
        if info.pbi_status == 5 {
            return Probe::Gone;
        }
        let secs = i64::try_from(info.pbi_start_tvsec).ok();
        let micros = i64::try_from(info.pbi_start_tvusec).ok();
        Probe::Alive(
            secs.zip(micros)
                .map(|(s, u)| super::epoch_stamp(s * 1000 + u / 1000)),
        )
    }

    /// `sysctl(kern.proc.pid)`: a `kinfo_proc` whose first field is the start (`tv_sec` i64 at 0, `tv_usec` i32 at 8, the
    /// record `proc_pidinfo` copies from) and whose `p_stat` byte is at 36; the same on Intel and Apple silicon. Read as bytes
    /// because libc has no `kinfo_proc` for macOS.
    #[cfg(target_os = "macos")]
    fn table_start(pid: libc::c_int) -> Probe {
        let mut mib = [libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_PID, pid];
        let mut buf = [0_u8; 1024];
        let mut len = buf.len();
        // SAFETY: `mib` is four ints; `buf` is `len` bytes and outlives the call, which writes at most `len` bytes into it.
        let rc = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                4,
                buf.as_mut_ptr().cast::<libc::c_void>(),
                &raw mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 {
            return Probe::Alive(None);
        }
        // No row: the process is not in the table.
        if len == 0 {
            return Probe::Gone;
        }
        if len < 40 {
            return Probe::Alive(None);
        }
        let (Some(sec), Some(micros), Some(&stat)) = (
            buf.get(..8).and_then(|b| <[u8; 8]>::try_from(b).ok()),
            buf.get(8..12).and_then(|b| <[u8; 4]>::try_from(b).ok()),
            buf.get(36),
        ) else {
            return Probe::Alive(None);
        };
        // SZOMB: ended, waiting for its parent; nothing runs.
        if stat == 5 {
            return Probe::Gone;
        }
        let ms = i64::from_ne_bytes(sec) * 1000 + i64::from(i32::from_ne_bytes(micros)) / 1000;
        Probe::Alive(Some(super::epoch_stamp(ms)))
    }

    #[cfg(target_os = "linux")]
    fn start(pid: u32) -> Probe {
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            // kill(2) said it exists; its stat file is unreadable or vanished: do not guess.
            return Probe::Alive(None);
        };
        let Some((state, ticks)) = super::parse_linux_stat(&stat) else {
            return Probe::Alive(None);
        };
        if state == 'Z' {
            return Probe::Gone;
        }
        let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok();
        Probe::Alive(boot.map(|b| super::boot_stamp(&b, ticks)))
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
        WAIT_OBJECT_0,
    };
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        WaitForSingleObject,
    };

    /// 100 ns ticks between 1601-01-01 (FILETIME) and 1970-01-01.
    const EPOCH_DIFF_TICKS: i64 = 116_444_736_000_000_000;

    pub(super) fn session_id(_: u32) -> Option<u32> {
        None
    }

    pub(super) fn group_gone(_: u32) -> bool {
        true
    }

    pub(super) fn probe(pid: u32) -> Probe {
        if pid == 0 {
            return Probe::Gone;
        }
        // SAFETY: plain Win32 call with a pid; a null handle means failure.
        let h = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                pid,
            )
        };
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
        // A process object is signalled once it has ended; unlike an exit code of 259 (STILL_ACTIVE), which a process may
        // itself exit with, this cannot be mistaken for "running".
        // SAFETY: `h` is an open process handle; a zero timeout only polls.
        let ended = unsafe { WaitForSingleObject(h, 0) } == WAIT_OBJECT_0;
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
        if ended {
            return Probe::Gone;
        }
        if !got_times {
            return Probe::Alive(None);
        }
        let ticks = (i64::from(created.dwHighDateTime) << 32) | i64::from(created.dwLowDateTime);
        Probe::Alive(Some(super::epoch_stamp(
            (ticks - EPOCH_DIFF_TICKS) / 10_000,
        )))
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    use super::Probe;

    pub(super) fn probe(_: u32) -> Probe {
        Probe::Unknown
    }

    pub(super) fn session_id(_: u32) -> Option<u32> {
        None
    }

    pub(super) fn group_gone(_: u32) -> bool {
        true
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn this_process_is_alive_with_a_stable_stamp() {
        let Probe::Alive(Some(stamp)) = probe(std::process::id()) else {
            panic!("this process is alive with a readable start");
        };
        assert!(form(&stamp).is_some(), "{stamp}");
        if cfg!(target_os = "linux") {
            assert!(stamp.starts_with('b'), "{stamp}");
        } else {
            assert!(stamp.starts_with('e'), "{stamp}");
        }
        assert_eq!(
            start_stamp(std::process::id()),
            Some(stamp),
            "stable across calls"
        );
    }

    // A real Linux line: the command holds spaces and parentheses; field 22 is 123456.
    const STAT: &str = "4242 (my (odd) cmd) S 1 4242 4242 0 -1 4194560 100 0 0 0 5 3 0 0 20 0 1 0 123456 1000000 200 18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0";

    #[test]
    fn linux_stat_and_boot_id_text_make_a_boot_stamp() {
        assert_eq!(parse_linux_stat(STAT), Some(('S', 123_456)));
        assert_eq!(
            parse_linux_stat(&STAT.replace(") S ", ") Z ")).map(|s| s.0),
            Some('Z')
        );
        assert_eq!(parse_linux_stat("garbage"), None);
        assert_eq!(parse_linux_stat("1 (x) S 1 2"), None);
        let id = "6f1c2d3e-0000-4000-8000-0123456789ab\n";
        assert_eq!(
            boot_stamp(id, 123_456),
            "b6f1c2d3e-0000-4000-8000-0123456789ab:123456"
        );
        assert_eq!(form(&boot_stamp(id, 1)), Some('b'));
    }

    #[test]
    fn same_form_compares_with_equality_and_no_tolerance() {
        assert!(same_process("e1000", "e1000"));
        assert!(!same_process("e1000", "e1001"));
        assert!(same_process("bAAA:10", "bAAA:10"));
        assert!(
            !same_process("bAAA:10", "bAAA:11"),
            "another start in one boot"
        );
        assert!(
            !same_process("bAAA:10", "bBBB:10"),
            "same tick, another boot"
        );
    }

    #[test]
    fn a_different_form_is_an_unknown_start_never_another_process() {
        for (recorded, current) in [
            ("e1000", "bAAA:10"),
            ("bAAA:10", "e1000"),
            ("Mon Nov  4 10:00:00 2026", "e1000"),
            ("Mon Nov  4 10:00:00 2026", "bAAA:10"),
            ("", "e1000"),
            ("", "bAAA:10"),
            ("e", "bAAA:10"),
            ("bAAA", "e1000"),
        ] {
            assert!(
                same_process(recorded, current),
                "{recorded:?} vs {current:?}"
            );
        }
    }

    #[test]
    fn comparable_needs_two_stamps_of_one_form() {
        assert!(comparable("e1", "e2"));
        assert!(comparable("bA:1", "bB:2"));
        assert!(!comparable("e1", "bA:1"));
        assert!(!comparable("Mon Nov  4 10:00:00 2026", "e1"));
        assert!(!comparable("", ""));
        assert!(is_stamp("e1") && is_stamp("bA:1"));
        assert!(!is_stamp("") && !is_stamp("Mon Nov  4 10:00:00 2026"));
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
        assert!(group_gone(0));
    }

    /// A leader that exits leaves its group alive while a child in it runs: the pid is gone, the group is not.
    #[cfg(unix)]
    #[test]
    fn a_group_outlives_its_dead_leader_until_its_last_process_ends() {
        use std::os::unix::process::CommandExt;
        let mut leader = std::process::Command::new("sh")
            .args(["-c", "sleep 30 >/dev/null 2>&1 & echo $!"])
            .stdout(std::process::Stdio::piped())
            .process_group(0)
            .spawn()
            .unwrap();
        let pgid = leader.id();
        let mut out = String::new();
        std::io::Read::read_to_string(leader.stdout.as_mut().unwrap(), &mut out).unwrap();
        leader.wait().unwrap();
        let child: libc::pid_t = out.trim().parse().unwrap();
        assert_eq!(probe(pgid), Probe::Gone, "the leader is reaped");
        assert!(!group_gone(pgid), "its child still runs in the group");
        // SAFETY: the group this test started.
        assert_eq!(
            unsafe { libc::killpg(libc::pid_t::try_from(pgid).unwrap(), libc::SIGKILL) },
            0
        );
        let mut empty = false;
        for _ in 0..100 {
            if group_gone(pgid) {
                empty = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            empty,
            "the group of {pgid} (child {child}) ends once its last process does"
        );
    }
}
