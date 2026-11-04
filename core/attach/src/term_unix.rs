//! The Unix terminal: raw mode on the keys' tty, restored by a guard on every normal path and by a signal handler when the
//! process is told to die (SIGTERM, SIGHUP, SIGINT, SIGQUIT). SIGKILL cannot be caught; nothing can restore after it.

use std::cell::UnsafeCell;
use std::fs::File;
use std::io::{self, Read};
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, RawFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

/// Where the keys come from, and the terminal they belong to.
pub(super) struct Tty {
    /// Held so the descriptor in use stays open as long as the guard may restore it.
    file: Option<File>,
}

impl Tty {
    fn fd(&self) -> RawFd {
        self.file.as_ref().map_or(0, AsRawFd::as_raw_fd)
    }

    /// The size of the terminal: stdout's (where the output goes), else the keys' tty.
    pub(super) fn size_fn(&self) -> Arc<dyn Fn() -> Option<(u16, u16)> + Send + Sync> {
        let fds = [1, self.fd(), 2];
        Arc::new(move || fds.iter().find_map(|fd| window_size(*fd)))
    }

    /// Raw mode, or `None` when the keys do not come from a terminal (a pipe, a file: the headless use).
    pub(super) fn enter_raw(self) -> Option<Box<dyn Send>> {
        let fd = self.fd();
        // SAFETY: isatty only reads the descriptor's type.
        if unsafe { libc::isatty(fd) } != 1 {
            return None;
        }
        RawGuard::enter(fd, self.file).map(|g| Box::new(g) as Box<dyn Send>)
    }
}

/// Stdin, or with `tty` the controlling terminal. A pipe is fine: it is read as it is.
pub(super) fn open_input(tty: bool) -> io::Result<(Box<dyn Read + Send>, Tty)> {
    if tty {
        let file = File::open("/dev/tty")?;
        let reader = file.try_clone()?;
        return Ok((Box::new(reader), Tty { file: Some(file) }));
    }
    Ok((Box::new(io::stdin()), Tty { file: None }))
}

fn window_size(fd: RawFd) -> Option<(u16, u16)> {
    let mut ws = MaybeUninit::<libc::winsize>::zeroed();
    // SAFETY: TIOCGWINSZ writes one winsize through the pointer; a non-tty fd just fails.
    let ok = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ as _, ws.as_mut_ptr()) } == 0;
    // SAFETY: zeroed is a valid winsize, and ioctl only filled it in.
    let ws = unsafe { ws.assume_init() };
    (ok && ws.ws_col > 0 && ws.ws_row > 0).then_some((ws.ws_col, ws.ws_row))
}

/// The termios to put back, kept where a signal handler can read it.
struct Saved(UnsafeCell<MaybeUninit<libc::termios>>);
// SAFETY: written once before ARMED is set (Release) and read only after ARMED is cleared by the one that restores
// (AcqRel), so the two never overlap.
unsafe impl Sync for Saved {}
static SAVED: Saved = Saved(UnsafeCell::new(MaybeUninit::uninit()));
static SAVED_FD: AtomicI32 = AtomicI32::new(-1);
static ARMED: AtomicBool = AtomicBool::new(false);

const DYING: [libc::c_int; 4] = [libc::SIGTERM, libc::SIGHUP, libc::SIGINT, libc::SIGQUIT];

/// Puts the saved settings back, once. Async-signal-safe: an atomic swap and tcsetattr.
fn restore_now(flush: bool) {
    if ARMED.swap(false, Ordering::AcqRel) {
        let when = if flush {
            libc::TCSADRAIN
        } else {
            libc::TCSANOW
        };
        // SAFETY: SAVED was filled in before ARMED was set; this is the only caller that got `true` from the swap.
        unsafe {
            libc::tcsetattr(
                SAVED_FD.load(Ordering::Acquire),
                when,
                (*SAVED.0.get()).as_ptr(),
            );
        }
    }
}

extern "C" fn on_signal(sig: libc::c_int) {
    restore_now(false);
    // SA_RESETHAND put the default action back and SA_NODEFER left the signal unblocked: this ends the process the way the
    // signal would have, so the parent still sees "killed by SIGTERM".
    // SAFETY: raise is async-signal-safe.
    unsafe {
        libc::raise(sig);
    }
}

/// Installs the restoring handler for each fatal signal that is not ignored (a `nohup` SIGHUP, or SIGINT / SIGQUIT in a
/// background job, stay ignored) and returns what was there, to put back.
fn install_handlers() -> Vec<(libc::c_int, libc::sigaction)> {
    let mut old = Vec::new();
    for sig in DYING {
        // SAFETY: a zeroed sigaction with the fields below set is a valid one; sigaction only reads `sa` and fills `prev`.
        unsafe {
            let mut prev: libc::sigaction = std::mem::zeroed();
            if libc::sigaction(sig, std::ptr::null(), &raw mut prev) != 0
                || prev.sa_sigaction == libc::SIG_IGN
            {
                continue;
            }
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as usize;
            sa.sa_flags = libc::SA_RESETHAND | libc::SA_NODEFER;
            libc::sigemptyset(&raw mut sa.sa_mask);
            if libc::sigaction(sig, &raw const sa, std::ptr::null_mut()) == 0 {
                old.push((sig, prev));
            }
        }
    }
    old
}

fn put_back(old: &[(libc::c_int, libc::sigaction)]) {
    for (sig, prev) in old {
        // SAFETY: `prev` came from sigaction for this signal.
        unsafe { libc::sigaction(*sig, prev, std::ptr::null_mut()) };
    }
}

/// Raw mode on a tty until dropped.
struct RawGuard {
    _keep: Option<File>,
    old: Vec<(libc::c_int, libc::sigaction)>,
}

impl RawGuard {
    fn enter(fd: RawFd, keep: Option<File>) -> Option<Self> {
        if ARMED.load(Ordering::Acquire) {
            return None;
        }
        let mut t = MaybeUninit::<libc::termios>::zeroed();
        // SAFETY: tcgetattr fills the termios on success; it is only used then.
        if unsafe { libc::tcgetattr(fd, t.as_mut_ptr()) } != 0 {
            return None;
        }
        // SAFETY: tcgetattr succeeded, so it is initialised.
        let saved = unsafe { t.assume_init() };
        let mut raw = saved;
        // SAFETY: cfmakeraw edits the struct it is given.
        unsafe { libc::cfmakeraw(&raw mut raw) };
        raw.c_cc[libc::VMIN] = 1;
        raw.c_cc[libc::VTIME] = 0;
        // SAFETY: nothing reads SAVED while ARMED is false.
        unsafe { (*SAVED.0.get()).write(saved) };
        SAVED_FD.store(fd, Ordering::Release);
        ARMED.store(true, Ordering::Release);
        // Handlers first: a signal between raw mode going on and the handler being there would leave the terminal raw.
        let old = install_handlers();
        // SAFETY: raw is a valid termios for this fd.
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw const raw) } != 0 {
            ARMED.store(false, Ordering::Release);
            put_back(&old);
            return None;
        }
        Some(Self { _keep: keep, old })
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        // Terminal first, then the handlers: the other way round leaves a window where a signal kills us raw.
        restore_now(true);
        put_back(&self.old);
    }
}
