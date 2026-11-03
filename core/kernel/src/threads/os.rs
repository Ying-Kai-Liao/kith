//! The few process facts the claim protocol needs, and the only `unsafe` in the crate (`kill(2)`). Process start times
//! come from `ps` in the C locale, because that is the text the claims carry (`kith-sessions proc_start` and the host's
//! `terminal/registry.rs` write and read the same one).

use std::process::Command;

fn as_pid(pid: i64) -> Option<libc::pid_t> {
    // Zero and below would mean a process group to kill(2); no process has such a pid.
    libc::pid_t::try_from(pid).ok().filter(|p| *p > 0)
}

/// Whether the process exists. One we may not signal still does (EPERM). `kith-sessions` reads a claim with pid 0 or a
/// negative one as live (kill(2) then names a group); here it is not a process, so the claim is stale.
pub fn pid_runs(pid: i64) -> bool {
    let Some(p) = as_pid(pid) else { return false };
    // SAFETY: signal 0 only checks that the process exists; nothing is sent.
    unsafe {
        libc::kill(p, 0) == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

/// Whether a signal to the process would be allowed (`os.kill(pid, 0)` without an error): `live_pid` in kith-sessions
/// reads a process it may not signal as gone.
pub fn pid_signalable(pid: i64) -> bool {
    let Some(p) = as_pid(pid) else { return false };
    // SAFETY: as in `pid_runs`.
    unsafe { libc::kill(p, 0) == 0 }
}

/// SIGTERM or SIGKILL for `pid`; errors (it is gone, not ours) are ignored.
pub fn signal(pid: i64, sig: libc::c_int) {
    if let Some(p) = as_pid(pid) {
        // SAFETY: a plain kill(2) on a pid read from `ps`; the callers judged it (claim live: pid and start time).
        unsafe { libc::kill(p, sig) };
    }
}

pub const SIGTERM: libc::c_int = libc::SIGTERM;
pub const SIGKILL: libc::c_int = libc::SIGKILL;

/// The start time of `pid` as `ps` prints it in the C locale, whitespace squeezed; `None` when it can't be read.
pub fn proc_start(pid: i64) -> Option<String> {
    let out = Command::new("ps")
        .env("LC_ALL", "C")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let squeezed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!squeezed.is_empty()).then_some(squeezed)
}

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

/// This process' parent (the launcher's shell, for a verb run from `kith`).
pub fn parent_pid() -> i64 {
    i64::from(std::os::unix::process::parent_id())
}

/// The home folder of this process's real user from the password database, as Python's `pwd.getpwuid(os.getuid()).pw_dir`
/// (`getpwuid_r`, no subprocess, no `$HOME`). `None` when the user has no entry or an empty home: the caller counts that
/// as a guest.
pub fn passwd_home() -> Option<std::path::PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;
    // SAFETY: getuid has no arguments and cannot fail.
    let uid = unsafe { libc::getuid() };
    let mut size = 1024;
    loop {
        let mut buf = vec![0 as libc::c_char; size];
        // SAFETY: an all-zero passwd is a valid value to pass for getpwuid_r to fill in.
        let mut pw: libc::passwd = unsafe { std::mem::zeroed() };
        let mut found: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: `pw` and `buf` outlive the call and `buf.len()` is its true size; on success `pw_dir` points into `buf`.
        let rc = unsafe {
            libc::getpwuid_r(
                uid,
                &raw mut pw,
                buf.as_mut_ptr(),
                buf.len(),
                &raw mut found,
            )
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
