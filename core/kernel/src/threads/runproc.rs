//! The process half of `run`: the fork that becomes claude, the signals the wrapper passes on, `waitpid`, and the few `libc`
//! reads the idle check needs. Everything with `unsafe` in `run` lives here (the crate denies it elsewhere).
//!
//! The fork is raw because the child's whole life has to be known: after `fork` it calls only async-signal-safe functions on
//! data made before (`sigaction`, `sigprocmask`, `dup2`, `close`, `execve`, `write`, `_exit`), as the host is multi-threaded.
//! What claude is given (the architect's MUSTs on #1090): every signal back at its default and nothing blocked, so a
//! SIGPIPE the Rust runtime ignores, or a mask this process held, never reaches an engine; no descriptor but 0-2 and the
//! status pipe, so a leaked descriptor of the launcher's shell is not claude's.
#![allow(unsafe_code)]

use std::ffi::{CString, OsStr, OsString};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::io::RawFd;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};

/// The descriptor the stub finds its status pipe on (the number is on its command line; a POSIX shell cannot redirect to one
/// above 9).
pub const STATUS_FD: RawFd = 9;

/// The pid the signals are passed to; 0 until the fork has returned in the parent, and again once the child is reaped (its
/// pid is free to be handed out then).
static CHILD: AtomicI32 = AtomicI32::new(0);
/// A TERM or HUP that arrived while the child's pid was not known yet: passed on as soon as it is.
static PENDING: AtomicI32 = AtomicI32::new(0);

/// Where `errno` lives, so a handler can leave the interrupted code's value as it found it.
#[cfg(target_os = "macos")]
fn errno_slot() -> *mut libc::c_int {
    // SAFETY: __error returns the calling thread's errno slot.
    unsafe { libc::__error() }
}
#[cfg(not(target_os = "macos"))]
fn errno_slot() -> *mut libc::c_int {
    // SAFETY: __errno_location returns the calling thread's errno slot.
    unsafe { libc::__errno_location() }
}

extern "C" fn forward(sig: libc::c_int) {
    // SAFETY: a handler may touch errno (saved and put back) and call kill(2); the atomics are lock-free.
    unsafe {
        let slot = errno_slot();
        let saved = *slot;
        let child = CHILD.load(Ordering::SeqCst);
        if child > 0 {
            libc::kill(child, sig);
        } else {
            PENDING.store(sig, Ordering::SeqCst);
        }
        *slot = saved;
    }
}

fn cstr(s: &OsStr) -> CString {
    CString::new(
        s.as_bytes()
            .iter()
            .copied()
            .filter(|b| *b != 0)
            .collect::<Vec<u8>>(),
    )
    .unwrap_or_default()
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// What the signals were before [`install_handlers`], so they can be given back once the child is gone.
pub struct Saved {
    mask: libc::sigset_t,
    actions: [libc::sigaction; 3],
}

const PASSED_ON: [libc::c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

impl Saved {
    /// The dispositions and the mask this process had: after the run, whatever it does next is as it was.
    pub fn restore(&self) {
        // SAFETY: sigaction and pthread_sigmask with what `install_handlers` read.
        unsafe {
            for (s, a) in PASSED_ON.iter().zip(&self.actions) {
                libc::sigaction(*s, a, std::ptr::null_mut());
            }
            libc::pthread_sigmask(
                libc::SIG_SETMASK,
                &raw const self.mask,
                std::ptr::null_mut(),
            );
        }
    }

    fn unblock(&self) {
        // SAFETY: puts back the mask `install_handlers` returned; a signal held meanwhile is delivered now.
        unsafe {
            libc::pthread_sigmask(
                libc::SIG_SETMASK,
                &raw const self.mask,
                std::ptr::null_mut(),
            )
        };
    }
}

/// `^C` is ours to ignore (claude decides what it means), TERM and HUP are passed on. Done before the fork with these
/// signals blocked, so the child never runs on a handler that does not know its pid and the parent loses none.
fn install_handlers() -> Saved {
    // SAFETY: sigaction and pthread_sigmask on structures made here; the handler is async-signal-safe (see `forward`).
    unsafe {
        let mut saved = Saved {
            mask: std::mem::zeroed(),
            actions: std::mem::zeroed(),
        };
        let mut act: libc::sigaction = std::mem::zeroed();
        libc::sigemptyset(&raw mut act.sa_mask);
        for (s, old) in PASSED_ON.iter().zip(saved.actions.iter_mut()) {
            act.sa_sigaction = if *s == libc::SIGINT {
                libc::SIG_IGN
            } else {
                forward as extern "C" fn(libc::c_int) as usize
            };
            libc::sigaction(*s, &raw const act, old);
        }
        let mut block: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&raw mut block);
        for s in PASSED_ON {
            libc::sigaddset(&raw mut block, s);
        }
        libc::pthread_sigmask(libc::SIG_BLOCK, &raw const block, &raw mut saved.mask);
        saved
    }
}

/// How the child went.
pub struct Spawned {
    pub pid: i32,
    /// Give these back with [`Saved::restore`] when the run is over.
    pub saved: Saved,
    /// The errno of the failed exec (the child has exited 127), else `None`.
    pub exec_failed: Option<i32>,
}

/// The candidate files `execvp` would try: the name alone when it has a directory part, else one per `PATH` entry (the
/// default path when there is none, as Python's `execvp`).
fn candidates(file: &OsStr, path: Option<&OsStr>) -> Vec<CString> {
    if file.as_bytes().contains(&b'/') {
        return vec![cstr(file)];
    }
    let dirs = path.map_or_else(
        || vec![PathBuf::from("/bin"), PathBuf::from("/usr/bin")],
        |p| std::env::split_paths(p).collect(),
    );
    dirs.iter()
        .map(|d| cstr(d.join(file).as_os_str()))
        .collect()
}

/// Forks the child that becomes `argv[0]` (found along this process's `PATH`), with this process's environment and
/// stdio. `status`: the status pipe (read end, write end); the child gets the write end as [`STATUS_FD`], the parent's copy of
/// it is closed here. From the return on, TERM and HUP reach the child and `^C` is ignored.
pub fn spawn(argv: &[OsString], status: Option<(RawFd, RawFd)>) -> std::io::Result<Spawned> {
    let (err_r, err_w) = super::super::lifecycle::proc::pipe()?;
    let cargs: Vec<CString> = argv.iter().map(|a| cstr(a)).collect();
    let mut argv_ptrs: Vec<*const libc::c_char> = cargs.iter().map(|a| a.as_ptr()).collect();
    argv_ptrs.push(std::ptr::null());
    let cenv: Vec<CString> = std::env::vars_os()
        .map(|(k, v)| {
            let mut kv = k.as_bytes().to_vec();
            kv.push(b'=');
            kv.extend_from_slice(v.as_bytes());
            cstr(OsStr::from_bytes(&kv))
        })
        .collect();
    let mut envp: Vec<*const libc::c_char> = cenv.iter().map(|a| a.as_ptr()).collect();
    envp.push(std::ptr::null());
    let tries = candidates(
        argv.first().map_or(OsStr::new(""), OsString::as_os_str),
        std::env::var_os("PATH").as_deref(),
    );
    // SAFETY: getdtablesize has no arguments; the cap keeps the child's closing loop short.
    let maxfd = unsafe { libc::getdtablesize() }.clamp(16, 65_536);
    CHILD.store(0, Ordering::SeqCst);
    PENDING.store(0, Ordering::SeqCst);
    let saved = install_handlers();
    // SAFETY: the child below calls only async-signal-safe functions on data made above, then execs or `_exit`s.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        let e = std::io::Error::last_os_error();
        saved.restore();
        for fd in [err_r, err_w] {
            super::super::lifecycle::proc::close(fd);
        }
        return Err(e);
    }
    if pid == 0 {
        // SAFETY: see above: the child's whole life.
        unsafe {
            for s in 1..65 {
                if s != libc::SIGKILL && s != libc::SIGSTOP {
                    libc::signal(s, libc::SIG_DFL);
                }
            }
            let mut none: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&raw mut none);
            libc::sigprocmask(libc::SIG_SETMASK, &raw const none, std::ptr::null_mut());
            if let Some((r, w)) = status {
                libc::close(r);
                if w == STATUS_FD {
                    let f = libc::fcntl(w, libc::F_GETFD);
                    libc::fcntl(w, libc::F_SETFD, f & !libc::FD_CLOEXEC);
                } else {
                    libc::dup2(w, STATUS_FD);
                }
            }
            // Nothing but 0-2, the status pipe and the pipe that tells the parent the exec failed (it closes by itself at exec).
            for fd in 3..maxfd {
                if fd != err_w && (status.is_none() || fd != STATUS_FD) {
                    libc::close(fd);
                }
            }
            // execvpe's rule: the first error that is not "not there" wins, else the last.
            let (mut first, mut last) = (0, 0);
            for full in &tries {
                libc::execve(full.as_ptr(), argv_ptrs.as_ptr(), envp.as_ptr());
                last = errno();
                if first == 0 && last != libc::ENOENT && last != libc::ENOTDIR {
                    first = last;
                }
            }
            let e = if first != 0 { first } else { last };
            let said = e.to_le_bytes();
            libc::write(err_w, said.as_ptr().cast(), said.len());
            libc::_exit(127);
        }
    }
    CHILD.store(pid, Ordering::SeqCst);
    // A signal held meanwhile is delivered now, to a known pid.
    saved.unblock();
    let held = PENDING.swap(0, Ordering::SeqCst);
    if held > 0 {
        // SAFETY: a signal that arrived before the pid was known, for our own child.
        unsafe { libc::kill(pid, held) };
    }
    super::super::lifecycle::proc::close(err_w);
    if let Some((_, w)) = status {
        super::super::lifecycle::proc::close(w);
    }
    let said = super::super::lifecycle::proc::read_to_end(err_r);
    super::super::lifecycle::proc::close(err_r);
    let exec_failed = said
        .get(..4)
        .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    Ok(Spawned {
        pid,
        saved,
        exec_failed,
    })
}

/// Reads the status pipe until it ends, but for no more than `limit`: a descendant that kept the write end open must not hold
/// the wrapper's exit hostage. What was read so far counts as what was said.
pub fn read_status(fd: RawFd, limit: std::time::Duration) -> Vec<u8> {
    let end = std::time::Instant::now() + limit;
    let mut all = Vec::new();
    let mut buf = [0u8; 256];
    loop {
        let left = end.saturating_duration_since(std::time::Instant::now());
        let mut p = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let wait = libc::c_int::try_from(left.as_millis()).unwrap_or(libc::c_int::MAX);
        // SAFETY: poll(2) on one descriptor of ours.
        let ready = unsafe { libc::poll(&raw mut p, 1, wait) };
        if ready < 0 && errno() == libc::EINTR {
            continue;
        }
        if ready <= 0 {
            return all;
        }
        // SAFETY: read(2) into a live buffer.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n < 0 && errno() == libc::EINTR {
            continue;
        }
        if n <= 0 {
            return all;
        }
        all.extend_from_slice(&buf[..usize::try_from(n).unwrap_or(0)]);
    }
}

/// The child is reaped: nothing is passed on to its pid any more (it is free to be handed out again).
pub fn forget_child() {
    CHILD.store(0, Ordering::SeqCst);
}

/// How the child stands.
pub enum Wait {
    Running,
    /// No such child (never ours, or reaped already).
    Gone,
    Done(i32),
}

/// `waitpid(pid, WNOHANG)`; the raw status on exit. An interrupted wait is tried again.
pub fn try_wait(pid: i32) -> Wait {
    let mut status = 0;
    loop {
        // SAFETY: waitpid(2) on our own child.
        let got = unsafe { libc::waitpid(pid, &raw mut status, libc::WNOHANG) };
        if got < 0 && errno() == libc::EINTR {
            continue;
        }
        return match got {
            0 => Wait::Running,
            g if g < 0 => Wait::Gone,
            _ => Wait::Done(status),
        };
    }
}

/// Python's `os.waitstatus_to_exitcode`: the exit code, or minus the signal that ended it.
pub fn exit_code(status: i32) -> i32 {
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else if libc::WIFSIGNALED(status) {
        -libc::WTERMSIG(status)
    } else {
        status
    }
}

/// `os.kill(pid, SIGTERM)` for the child the wrapper stops on its own.
pub fn terminate(pid: i32) -> bool {
    // SAFETY: SIGTERM to our own child.
    unsafe { libc::kill(pid, libc::SIGTERM) == 0 }
}

/// The session id of `pid` (`os.getsid`); `None` when it is gone or not ours to ask about.
pub fn session_of(pid: i64) -> Option<i64> {
    let p = libc::pid_t::try_from(pid).ok()?;
    // SAFETY: getsid(2) only reads.
    let s = unsafe { libc::getsid(p) };
    (s >= 0).then_some(i64::from(s))
}

/// Whether stdin is a terminal.
pub fn stdin_is_tty() -> bool {
    // SAFETY: isatty(3) on descriptor 0.
    unsafe { libc::isatty(0) == 1 }
}

/// When the terminal was last read from or written to (stdin's access time, which a keystroke moves), in seconds; `None`
/// when it cannot be read.
pub fn stdin_atime() -> Option<f64> {
    // SAFETY: fstat(2) into a zeroed stat on descriptor 0.
    unsafe {
        let mut st: libc::stat = std::mem::zeroed();
        if libc::fstat(0, &raw mut st) != 0 {
            return None;
        }
        #[allow(clippy::cast_precision_loss)]
        Some(st.st_atime as f64 + st.st_atime_nsec as f64 / 1e9)
    }
}

/// A command that runs on after this process is gone: its own session (no terminal, no signal of ours reaches it) and no
/// stdio. The descriptors are closed by `Command` (every one this crate opens is close-on-exec).
pub fn detached(program: &str, args: &[OsString]) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt as _;
    let mut c = Command::new(program);
    c.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: setsid(2) in the forked child, between fork and exec: async-signal-safe and touches nothing of ours.
    unsafe {
        c.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    c.spawn().map(drop)
}
