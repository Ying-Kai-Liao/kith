//! Runs the engine as a child on Unix and holds the caller (and so the lock) until it is gone.
//!
//! The engine stays in the stub's process group, on purpose. The holder starts a terminal run in a pty with
//! setsid + TIOCSCTTY, so the stub leads the session and the terminal's foreground group; an engine in that same
//! group has the terminal already (no tcsetpgrp, no SIGTTOU games) and a `killpg(stub, KILL)` or an "is the group
//! empty" check by the holder sees the engine too. An engine in a group of its own would survive that kill with the
//! lock already free: two writers.
//!
//! Signals: Ctrl-C / Ctrl-\ from the keyboard reach the whole foreground group, so the engine gets them itself; the
//! stub catches SIGINT and SIGQUIT with a handler that does nothing (not SIG_IGN, which the engine would inherit
//! through exec; a handler is reset to the default there) so it outlives the engine's reaction to them. SIGTERM and
//! SIGHUP sent to the stub alone are forwarded. SIGTSTP / SIGCONT stay at their defaults: a group stop stops both and
//! the stub never exits while the engine lives, because it only returns from `waitpid` on the engine's exit.

use std::ffi::OsString;
use std::io::ErrorKind;
use std::process::Command;
use std::sync::atomic::{AtomicI32, Ordering};

use libc::c_int;

static CHILD: AtomicI32 = AtomicI32::new(0);

extern "C" fn forward(sig: c_int) {
    let pid = CHILD.load(Ordering::Relaxed);
    if pid > 0 {
        // SAFETY: kill is async-signal-safe.
        unsafe { libc::kill(pid, sig) };
    }
}

extern "C" fn nothing(_: c_int) {}

fn install(sig: c_int, handler: extern "C" fn(c_int)) {
    // SAFETY: a zeroed sigaction with only handler and empty mask set; no SA_RESTART so waitpid sees EINTR.
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = handler as usize;
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaction(sig, &sa, std::ptr::null_mut());
    }
}

pub fn run(engine: &[OsString]) -> i32 {
    install(libc::SIGINT, nothing);
    install(libc::SIGQUIT, nothing);
    install(libc::SIGTERM, forward);
    install(libc::SIGHUP, forward);

    let mut cmd = Command::new(&engine[0]);
    cmd.args(&engine[1..]);
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        // If the stub is killed, the engine goes with it: no unguarded writer (macOS has no equivalent; there the
        // holder's group kill is the safety net).
        // SAFETY: prctl is async-signal-safe.
        unsafe {
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL as libc::c_ulong);
                Ok(())
            });
        }
    }
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("kith-stub: cannot start {:?}: {e}", engine[0]);
            return if e.kind() == ErrorKind::NotFound {
                127
            } else {
                126
            };
        }
    };
    let pid = child.id() as libc::pid_t;
    CHILD.store(pid, Ordering::Relaxed);

    loop {
        let mut status: c_int = 0;
        // SAFETY: status is a valid out pointer; pid is our child.
        let r = unsafe { libc::waitpid(pid, &mut status, 0) };
        if r < 0 {
            if std::io::Error::last_os_error().kind() == ErrorKind::Interrupted {
                continue;
            }
            eprintln!(
                "kith-stub: wait failed: {}",
                std::io::Error::last_os_error()
            );
            return 1;
        }
        if libc::WIFEXITED(status) {
            return libc::WEXITSTATUS(status);
        }
        if libc::WIFSIGNALED(status) {
            return 128 + libc::WTERMSIG(status);
        }
    }
}
