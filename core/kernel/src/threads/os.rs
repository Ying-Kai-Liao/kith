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

/// The start time of `pid` as `ps` prints it in the C locale, in UTC (Claude Code writes its session files' `procStart`
/// that way; the kernel's files use the Mac's time zone, [`proc_start`]).
pub fn proc_start_utc(pid: i64) -> Option<String> {
    let out = Command::new("ps")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let squeezed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!squeezed.is_empty()).then_some(squeezed)
}

/// The session id of `pid` (`os.getsid`); `None` when the process is gone.
pub fn session_of(pid: i64) -> Option<i64> {
    let p = as_pid(pid)?;
    // SAFETY: getsid(2) only reads the process table.
    let s = unsafe { libc::getsid(p) };
    (s >= 0).then_some(i64::from(s))
}

/// The parent of `pid` (the host's `parent_of`: its own turn's claimer is a child of the host); `None` when `ps` has no
/// row for it.
pub fn parent_of(pid: i64) -> Option<i64> {
    let (up, comm) = ps_row(pid);
    (up != 0 || !comm.is_empty()).then_some(up)
}

/// `%m-%d %H:%M` of an epoch second in the Mac's time zone (`datetime.astimezone().strftime`).
pub fn local_month_day_time(epoch: i64) -> String {
    let t: libc::time_t = epoch;
    // SAFETY: localtime_r fills the zeroed struct we own from a time_t we own.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&raw const t, &raw mut tm).is_null() {
            return String::new();
        }
        tm
    };
    format!(
        "{:02}-{:02} {:02}:{:02}",
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min
    )
}

/// The access time of this process' standard input in seconds (`os.fstat(0).st_atime`): on a terminal, the last keystroke.
pub fn stdin_atime() -> Option<f64> {
    // SAFETY: fstat(2) fills a zeroed struct we own for descriptor 0.
    let st = unsafe {
        let mut st: libc::stat = std::mem::zeroed();
        if libc::fstat(0, &raw mut st) != 0 {
            return None;
        }
        st
    };
    #[allow(clippy::cast_precision_loss)]
    Some(st.st_atime as f64 + st.st_atime_nsec as f64 / 1e9)
}

static CHILD: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

extern "C" fn pass_on(sig: libc::c_int) {
    let pid = CHILD.load(std::sync::atomic::Ordering::Relaxed);
    if pid > 0 {
        // SAFETY: kill(2) is async-signal-safe.
        unsafe { libc::kill(pid, sig) };
    }
}

/// Starts `cmd` as our child with the default signals back (what we ignore or catch is for this process, not for claude).
pub fn spawn_child(cmd: &[String]) -> std::io::Result<std::process::Child> {
    use std::os::unix::process::CommandExt as _;
    let mut c = Command::new(&cmd[0]);
    c.args(&cmd[1..]);
    // SAFETY: only signal(2), which is async-signal-safe, runs between fork and exec.
    unsafe {
        c.pre_exec(|| {
            for s in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
                libc::signal(s, libc::SIG_DFL);
            }
            Ok(())
        });
    }
    c.spawn()
}

/// While `child` runs: ^C is for the child to decide on, TERM and HUP are passed on to it.
pub fn guard_signals(child: i64) {
    CHILD.store(
        i32::try_from(child).unwrap_or(0),
        std::sync::atomic::Ordering::Relaxed,
    );
    // SAFETY: signal(2) with SIG_IGN and a handler that only calls kill(2).
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_IGN);
        let handler = pass_on as *const () as libc::sighandler_t;
        libc::signal(libc::SIGTERM, handler);
        libc::signal(libc::SIGHUP, handler);
    }
}

/// Runs `argv` in a session of its own with nothing attached (`start_new_session=True`), and does not wait for it.
pub fn spawn_detached(argv: &[std::ffi::OsString]) {
    use std::os::unix::process::CommandExt as _;
    use std::process::Stdio;
    let mut c = Command::new(&argv[0]);
    c.args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: setsid(2) is async-signal-safe.
    unsafe {
        c.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let _ = c.spawn();
}
