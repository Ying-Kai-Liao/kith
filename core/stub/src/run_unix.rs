//! Runs the engine as a child on Unix and holds the caller (and so the lock) until it is gone.
//!
//! Terminal: when stdin is a tty and the stub is its foreground group, the engine gets its own process group and
//! the terminal's foreground, so Ctrl-C / Ctrl-Z / Ctrl-\ from the keyboard go straight to the engine and not to
//! the stub (which must outlive it). The child makes itself foreground before exec, so it never reads the tty as a
//! background job. When the engine stops (Ctrl-Z), the stub gives the terminal back and stops itself, so the
//! user's shell sees its job stop; on SIGCONT it hands the terminal to the engine again and continues it.
//! Without a tty the engine stays in the stub's group and nothing is handed over.

use std::ffi::OsString;
use std::io::ErrorKind;
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::sync::atomic::{AtomicI32, Ordering};

use libc::c_int;

static CHILD: AtomicI32 = AtomicI32::new(0);

/// Signals aimed at the stub by something other than the terminal (kill, hangup, job control tools) belong to
/// the engine: the stub passes them on and keeps waiting.
extern "C" fn forward(sig: c_int) {
    let pid = CHILD.load(Ordering::Relaxed);
    if pid > 0 {
        // SAFETY: kill is async-signal-safe.
        unsafe { libc::kill(pid, sig) };
    }
}

fn install_forwarders() {
    for sig in [
        libc::SIGINT,
        libc::SIGTERM,
        libc::SIGHUP,
        libc::SIGTSTP,
        libc::SIGCONT,
        libc::SIGWINCH,
    ] {
        // SAFETY: a zeroed sigaction with only handler and empty mask set; no SA_RESTART so waitpid sees EINTR.
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = forward as extern "C" fn(c_int) as usize;
            libc::sigemptyset(&mut sa.sa_mask);
            libc::sigaction(sig, &sa, std::ptr::null_mut());
        }
    }
}

/// Makes `pgid` the terminal's foreground group. A background caller gets SIGTTOU for this, so it is ignored
/// for the duration (and never left ignored: an ignored signal would be inherited by the engine).
fn set_foreground(pgid: libc::pid_t) {
    // SAFETY: plain libc calls on fd 0.
    unsafe {
        let old = libc::signal(libc::SIGTTOU, libc::SIG_IGN);
        libc::tcsetpgrp(0, pgid);
        libc::signal(libc::SIGTTOU, old);
    }
}

pub fn run(engine: &[OsString]) -> i32 {
    // SAFETY: isatty / tcgetpgrp / getpgrp have no preconditions.
    let handoff = unsafe { libc::isatty(0) == 1 && libc::tcgetpgrp(0) == libc::getpgrp() };
    install_forwarders();

    let mut cmd = Command::new(&engine[0]);
    cmd.args(&engine[1..]);
    if handoff {
        // SAFETY: only async-signal-safe calls between fork and exec.
        unsafe {
            cmd.pre_exec(|| {
                libc::setpgid(0, 0);
                libc::signal(libc::SIGTTOU, libc::SIG_IGN);
                libc::tcsetpgrp(0, libc::getpid());
                libc::signal(libc::SIGTTOU, libc::SIG_DFL);
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

    let code = loop {
        let mut status: c_int = 0;
        // SAFETY: status is a valid out pointer; pid is our child.
        let r = unsafe { libc::waitpid(pid, &mut status, libc::WUNTRACED) };
        if r < 0 {
            if std::io::Error::last_os_error().kind() == ErrorKind::Interrupted {
                continue;
            }
            eprintln!(
                "kith-stub: wait failed: {}",
                std::io::Error::last_os_error()
            );
            break 1;
        }
        if libc::WIFSTOPPED(status) {
            // Without a handoff the engine shares our group, so a group stop already stopped us too.
            if handoff {
                // SAFETY: getpgrp, getpid and kill have no preconditions beyond valid arguments.
                unsafe {
                    set_foreground(libc::getpgrp());
                    libc::kill(libc::getpid(), libc::SIGSTOP);
                    // Resumed by SIGCONT.
                    set_foreground(pid);
                    libc::kill(pid, libc::SIGCONT);
                }
            }
            continue;
        }
        if libc::WIFEXITED(status) {
            break libc::WEXITSTATUS(status);
        }
        if libc::WIFSIGNALED(status) {
            break 128 + libc::WTERMSIG(status);
        }
    };
    CHILD.store(0, Ordering::Relaxed);
    if handoff {
        // SAFETY: getpgrp has no preconditions.
        set_foreground(unsafe { libc::getpgrp() });
    }
    code
}
