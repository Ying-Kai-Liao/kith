//! The few process facts the claim protocol needs, and the only `unsafe` in the crate (`kill(2)`). Process start times
//! are stamps (`crate::procstart`: `e<ms>` or `b<boot id>:<ticks>`), the string `kith-sessions` (`kith_procstart.py`) and the
//! host's `terminal/registry.rs` write and read too. Claims from before carry `ps -o lstart=` text, compared the old way (`lstart_matches`).

#[cfg(unix)]
use std::process::Command;

#[cfg(unix)]
fn as_pid(pid: i64) -> Option<libc::pid_t> {
    // Zero and below would mean a process group to kill(2); no process has such a pid.
    libc::pid_t::try_from(pid).ok().filter(|p| *p > 0)
}

#[cfg(unix)]
/// Whether the process exists. One we may not signal still does (EPERM). `kith-sessions` reads a claim with pid 0 or a
/// negative one as live (kill(2) then names a group); here it is not a process, so the claim is stale.
pub fn pid_runs(pid: i64) -> bool {
    let Some(p) = as_pid(pid) else { return false };
    // SAFETY: signal 0 only checks that the process exists; nothing is sent.
    unsafe {
        libc::kill(p, 0) == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

#[cfg(unix)]
/// `os.kill(pid, 0)` for any pid, as `kith-pkg` asks it of the pids in a snapshot's `.runs`: zero and negative ones name a
/// process group. Alive (or not ours to signal) is true; no such process is false.
pub fn pid_probe(pid: i64) -> bool {
    let Ok(p) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 only checks; nothing is sent.
    unsafe {
        libc::kill(p, 0) == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

#[cfg(unix)]
/// Whether a signal to the process would be allowed (`os.kill(pid, 0)` without an error): `live_pid` in kith-sessions
/// reads a process it may not signal as gone.
pub fn pid_signalable(pid: i64) -> bool {
    let Some(p) = as_pid(pid) else { return false };
    // SAFETY: as in `pid_runs`.
    unsafe { libc::kill(p, 0) == 0 }
}

#[cfg(unix)]
/// SIGTERM or SIGKILL for `pid`; errors (it is gone, not ours) are ignored.
pub fn signal(pid: i64, sig: libc::c_int) {
    if let Some(p) = as_pid(pid) {
        // SAFETY: a plain kill(2) on a pid read from `ps`; the callers judged it (claim live: pid and start time).
        unsafe { libc::kill(p, sig) };
    }
}

#[cfg(unix)]
/// SIGTERM or SIGKILL for every process of the group `pgid` (a child started with `process_group(0)` leads the group of its own
/// pid); errors (nobody is left) are ignored. A group id is not handed out again while a member runs.
pub fn signal_group(pgid: i64, sig: libc::c_int) {
    if let Some(p) = as_pid(pgid) {
        // SAFETY: a plain killpg(2); the callers started the group themselves.
        unsafe { libc::killpg(p, sig) };
    }
}

#[cfg(unix)]
pub const SIGTERM: libc::c_int = libc::SIGTERM;
#[cfg(unix)]
pub const SIGKILL: libc::c_int = libc::SIGKILL;

/// The start stamp of `pid`; `None` when it can't be read (or the process is gone).
pub fn proc_start(pid: i64) -> Option<String> {
    crate::procstart::start_stamp(u32::try_from(pid).ok()?)
}

#[cfg(unix)]
/// `ps -o lstart=` for `pid` in the C locale (UTC when asked), whitespace squeezed; `None` when `ps` cannot read it.
pub fn lstart(pid: i64, utc: bool) -> Option<String> {
    let mut ps = Command::new("ps");
    ps.env("LC_ALL", "C");
    if utc {
        ps.env("TZ", "UTC");
    }
    let out = ps
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let text = String::from_utf8(out.stdout)
        .ok()?
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    (!text.is_empty()).then_some(text)
}

#[cfg(unix)]
/// Whether `pid` started at the old `ps -o lstart=` text `claimed` (local time or UTC); `None` when `ps` cannot read the
/// start. The one comparison of the old text left, as the host's `registry::lstart_matches` and `kith_procstart.py` do.
pub fn lstart_matches(pid: i64, claimed: &str) -> Option<bool> {
    let claimed = claimed.split_whitespace().collect::<Vec<_>>().join(" ");
    let local = lstart(pid, false)?;
    Some(local == claimed || lstart(pid, true).is_some_and(|utc| utc == claimed))
}

#[cfg(unix)]
/// `(parent pid, command)` of `pid`; `(0, "")` when `ps` has no such row.
pub fn ps_row(pid: i64) -> (i64, String) {
    let Ok(out) = Command::new("ps")
        .args(["-o", "ppid=,comm=", "-p", &pid.to_string()])
        .output()
    else {
        return (0, String::new());
    };
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut it = text.trim_start().splitn(2, char::is_whitespace);
    match (it.next().and_then(|p| p.parse().ok()), it.next()) {
        (Some(parent), Some(comm)) => (parent, comm.trim().to_owned()),
        _ => (0, String::new()),
    }
}

#[cfg(unix)]
/// Every `(pid, ppid)` pair `ps` lists.
pub fn ps_table() -> Vec<(i64, i64)> {
    let Ok(out) = Command::new("ps").args(["-A", "-o", "pid=,ppid="]).output() else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let (a, b) = (it.next()?, it.next()?);
            if it.next().is_some() {
                return None;
            }
            Some((a.parse().ok()?, b.parse().ok()?))
        })
        .collect()
}

#[cfg(unix)]
/// This process' parent (the launcher's shell, for a verb run from `kith`).
pub fn parent_pid() -> i64 {
    i64::from(std::os::unix::process::parent_id())
}

#[cfg(unix)]
/// `os.getuid()`: the user this process runs as (the launchd domain `gui/<uid>` is named by it).
pub fn uid() -> u32 {
    // SAFETY: getuid has no arguments and cannot fail.
    unsafe { libc::getuid() }
}

#[cfg(unix)]
/// The home folder of this process's real user from the password database, as Python's `pwd.getpwuid(os.getuid()).pw_dir`
/// (`getpwuid_r`, no subprocess, no `$HOME`). `None` when the user has no entry or an empty home: the caller counts that
/// as a guest.
pub fn passwd_home() -> Option<std::path::PathBuf> {
    // SAFETY: getuid has no arguments and cannot fail.
    passwd_entry_home(Err(unsafe { libc::getuid() }))
}

#[cfg(unix)]
/// The home folder of the named user from the password database (`getpwnam_r`, as Python's `pwd.getpwnam(name).pw_dir`).
/// `None` for an unknown name, an empty home or a name with a NUL.
pub fn passwd_home_of(name: &str) -> Option<std::path::PathBuf> {
    let name = std::ffi::CString::new(name).ok()?;
    passwd_entry_home(Ok(&name))
}

#[cfg(unix)]
fn passwd_entry_home(who: Result<&std::ffi::CStr, u32>) -> Option<std::path::PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;
    let mut size = 1024;
    loop {
        let mut buf = vec![0 as libc::c_char; size];
        // SAFETY: an all-zero passwd is a valid value to pass for getpwuid_r to fill in.
        let mut pw: libc::passwd = unsafe { std::mem::zeroed() };
        let mut found: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: `pw` and `buf` outlive the call and `buf.len()` is its true size; on success `pw_dir` points into `buf`.
        let rc = unsafe {
            match who {
                Ok(name) => libc::getpwnam_r(
                    name.as_ptr(),
                    &raw mut pw,
                    buf.as_mut_ptr(),
                    buf.len(),
                    &raw mut found,
                ),
                Err(uid) => libc::getpwuid_r(
                    uid,
                    &raw mut pw,
                    buf.as_mut_ptr(),
                    buf.len(),
                    &raw mut found,
                ),
            }
        };
        if rc == libc::ERANGE && size < 1 << 20 {
            size *= 4;
            continue;
        }
        if rc != 0 || found.is_null() || pw.pw_dir.is_null() {
            return None;
        }
        // SAFETY: getpwuid_r set `pw_dir` to a NUL-terminated string inside `buf`.
        let dir = unsafe { std::ffi::CStr::from_ptr(pw.pw_dir) };
        let bytes = dir.to_bytes();
        return (!bytes.is_empty())
            .then(|| std::path::PathBuf::from(std::ffi::OsStr::from_bytes(bytes)));
    }
}

// --- Windows ---------------------------------------------------------------------------------------------------------
//
// The same facts from the process list and `GetProcessTimes` (`kith-procstart`, `kith-winsys`): no `ps`, no signals, no old
// `lstart` text (no claim written there is older than the stamps).

/// What `kill -TERM` and `kill -KILL` are called by the callers. Windows has one way to end a process from outside, and it is
/// the hard one, so the two do the same here ([`signal`]).
#[cfg(windows)]
pub const SIGTERM: i32 = 15;
#[cfg(windows)]
pub const SIGKILL: i32 = 9;

/// Whether the process exists. A process the system will not let us look at, or a probe that failed, counts as running: a wrong
/// "gone" would let a second writer in.
#[cfg(windows)]
pub fn pid_runs(pid: i64) -> bool {
    u32::try_from(pid)
        .ok()
        .filter(|p| *p > 0)
        .is_some_and(|p| !matches!(crate::procstart::probe(p), crate::procstart::Probe::Gone))
}

/// Whether the process exists and may be looked at (`live_pid`). Unix reads a holder it may not signal as gone; here a refused
/// look is "cannot tell", which keeps the holder.
#[cfg(windows)]
pub fn pid_signalable(pid: i64) -> bool {
    pid_runs(pid)
}

/// Ends `pid` now (`TerminateProcess`) but only when it began at the claimed `stamp` (`e<ms>`), through one open handle: a pid given
/// to a stranger since is left alone, and so is a process whose start cannot be read (H4: never by bare pid). `sig` is [`SIGTERM`]
/// or [`SIGKILL`]: Windows has no gentler end to ask for (docs/plans/windows-owner.md D6).
#[cfg(windows)]
pub fn signal_started(pid: i64, stamp: Option<&str>, _sig: i32) {
    let ms = stamp
        .and_then(|s| s.strip_prefix('e'))
        .and_then(|s| s.parse::<i64>().ok());
    if let (Some(p), Some(ms)) = (u32::try_from(pid).ok().filter(|p| *p > 0), ms) {
        let _ = kith_winsys::kill::terminate_if_started_at(p, ms);
    }
}

/// `(pid, ppid)` pairs where the child really is the parent's child: a parent pid is not cleared when the parent ends, and pids
/// are reused fast, so a row whose parent began after the child (or either start cannot be read) is dropped.
#[cfg(windows)]
pub fn child_table() -> Vec<(i64, i64)> {
    let began = |pid: i64| u32::try_from(pid).ok().and_then(kith_winsys::tree::creation_time);
    ps_table()
        .into_iter()
        .filter(|(pid, ppid)| matches!((began(*pid), began(*ppid)), (Some(c), Some(p)) if p <= c))
        .collect()
}

/// No old `ps -o lstart=` text was ever written on Windows: nothing to compare, so the start cannot be told.
#[cfg(windows)]
pub fn lstart_matches(_pid: i64, _claimed: &str) -> Option<bool> {
    None
}

/// `(parent pid, program name)` of `pid`; `(0, "")` when there is no such process.
#[cfg(windows)]
pub fn ps_row(pid: i64) -> (i64, String) {
    let Ok(rows) = kith_winsys::procs::table() else {
        return (0, String::new());
    };
    rows.into_iter()
        .find(|r| i64::from(r.pid) == pid)
        .map_or((0, String::new()), |r| (i64::from(r.parent), r.exe))
}

/// Every `(pid, ppid)` pair of the system.
#[cfg(windows)]
pub fn ps_table() -> Vec<(i64, i64)> {
    kith_winsys::procs::table()
        .unwrap_or_default()
        .into_iter()
        .map(|r| (i64::from(r.pid), i64::from(r.parent)))
        .collect()
}

/// This process' parent (0 when the list cannot be read).
#[cfg(windows)]
pub fn parent_pid() -> i64 {
    ps_row(i64::from(std::process::id())).0
}
