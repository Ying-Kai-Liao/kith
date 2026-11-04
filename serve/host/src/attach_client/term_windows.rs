//! The Windows console: raw mode is the input mode without line editing, echo and Ctrl+C handling, plus virtual-terminal
//! input (keys arrive as the escape sequences a Unix terminal sends) and virtual-terminal output processing. Restored by a
//! guard and by a console control handler (Ctrl+Break, closing the window, logoff, shutdown).

use std::fs::File;
use std::io::{self, Read};
use std::os::windows::io::AsRawHandle;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use windows_sys::Win32::System::Console::{
    CONSOLE_SCREEN_BUFFER_INFO, GetConsoleMode, GetConsoleScreenBufferInfo, GetStdHandle, STD_INPUT_HANDLE,
    STD_OUTPUT_HANDLE, SetConsoleCtrlHandler, SetConsoleMode,
};

const ENABLE_PROCESSED_INPUT: u32 = 0x0001;
const ENABLE_LINE_INPUT: u32 = 0x0002;
const ENABLE_ECHO_INPUT: u32 = 0x0004;
const ENABLE_VIRTUAL_TERMINAL_INPUT: u32 = 0x0200;
const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;

pub(super) struct Tty {
    /// `CONIN$` when the keys come from the console instead of stdin.
    file: Option<File>,
}

impl Tty {
    fn in_handle(&self) -> usize {
        match &self.file {
            Some(f) => f.as_raw_handle() as usize,
            // SAFETY: GetStdHandle takes no pointers.
            None => unsafe { GetStdHandle(STD_INPUT_HANDLE) as usize },
        }
    }

    /// The console window's size (what the viewer sees, not the scrollback buffer).
    pub(super) fn size_fn(&self) -> Arc<dyn Fn() -> Option<(u16, u16)> + Send + Sync> {
        Arc::new(|| {
            // SAFETY: the info struct is zeroed and only read after the call said it filled it in.
            unsafe {
                let mut info: CONSOLE_SCREEN_BUFFER_INFO = std::mem::zeroed();
                if GetConsoleScreenBufferInfo(GetStdHandle(STD_OUTPUT_HANDLE), &raw mut info) == 0 {
                    return None;
                }
                let w = i32::from(info.srWindow.Right) - i32::from(info.srWindow.Left) + 1;
                let h = i32::from(info.srWindow.Bottom) - i32::from(info.srWindow.Top) + 1;
                if w <= 0 || h <= 0 {
                    return None;
                }
                Some((u16::try_from(w).ok()?, u16::try_from(h).ok()?))
            }
        })
    }

    pub(super) fn enter_raw(self) -> Option<Box<dyn Send>> {
        RawGuard::enter(self.in_handle(), self.file).map(|g| Box::new(g) as Box<dyn Send>)
    }
}

pub(super) fn open_input(tty: bool) -> io::Result<(Box<dyn Read + Send>, Tty)> {
    if tty {
        let file = File::options().read(true).write(true).open("CONIN$")?;
        let reader = file.try_clone()?;
        return Ok((Box::new(reader), Tty { file: Some(file) }));
    }
    Ok((Box::new(io::stdin()), Tty { file: None }))
}

static ARMED: AtomicBool = AtomicBool::new(false);
static IN_HANDLE: AtomicUsize = AtomicUsize::new(0);
static IN_MODE: AtomicU32 = AtomicU32::new(0);
static OUT_HANDLE: AtomicUsize = AtomicUsize::new(0);
static OUT_MODE: AtomicU32 = AtomicU32::new(0);
/// Whether the output mode was changed (stdout may be a file or pipe with no console mode).
static OUT_CHANGED: AtomicBool = AtomicBool::new(false);

fn restore_now() {
    if ARMED.swap(false, Ordering::AcqRel) {
        // SAFETY: the handles were valid console handles when stored and are the process's own.
        unsafe {
            SetConsoleMode(IN_HANDLE.load(Ordering::Acquire) as _, IN_MODE.load(Ordering::Acquire));
            if OUT_CHANGED.load(Ordering::Acquire) {
                SetConsoleMode(OUT_HANDLE.load(Ordering::Acquire) as _, OUT_MODE.load(Ordering::Acquire));
            }
        }
    }
}

/// Runs on a thread the system starts for a control event. Puts the console back, then lets the default end the process.
unsafe extern "system" fn on_ctrl(_event: u32) -> i32 {
    restore_now();
    0
}

struct RawGuard {
    _keep: Option<File>,
}

impl RawGuard {
    fn enter(input: usize, keep: Option<File>) -> Option<Self> {
        if ARMED.load(Ordering::Acquire) {
            return None;
        }
        // SAFETY: plain console calls on this process's handles; the mode out-parameters are locals.
        unsafe {
            let mut mode = 0u32;
            if GetConsoleMode(input as _, &raw mut mode) == 0 {
                return None; // not a console: headless, relay as is
            }
            IN_HANDLE.store(input, Ordering::Release);
            IN_MODE.store(mode, Ordering::Release);
            let out = GetStdHandle(STD_OUTPUT_HANDLE);
            let mut out_mode = 0u32;
            let out_ok = GetConsoleMode(out, &raw mut out_mode) != 0;
            OUT_HANDLE.store(out as usize, Ordering::Release);
            OUT_MODE.store(out_mode, Ordering::Release);
            OUT_CHANGED.store(out_ok, Ordering::Release);
            ARMED.store(true, Ordering::Release);
            SetConsoleCtrlHandler(Some(on_ctrl), 1);
            let raw = (mode & !(ENABLE_PROCESSED_INPUT | ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT))
                | ENABLE_VIRTUAL_TERMINAL_INPUT;
            if SetConsoleMode(input as _, raw) == 0 {
                restore_now();
                SetConsoleCtrlHandler(Some(on_ctrl), 0);
                return None;
            }
            if out_ok {
                SetConsoleMode(out, out_mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
            }
        }
        Some(Self { _keep: keep })
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        restore_now();
        // SAFETY: removes the handler added in `enter`.
        unsafe { SetConsoleCtrlHandler(Some(on_ctrl), 0) };
    }
}
