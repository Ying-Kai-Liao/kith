//! The Windows console: raw mode is the input mode without line editing, echo and Ctrl+C handling, plus virtual-terminal
//! input (keys arrive as the escape sequences a Unix terminal sends) and virtual-terminal output processing. Restored by a
//! guard and by a console control handler (Ctrl+Break, closing the window, logoff, shutdown).

use std::fs::File;
use std::io::{self, Read};
use std::os::windows::io::AsRawHandle;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use windows_sys::Win32::System::Console::{
    CONSOLE_SCREEN_BUFFER_INFO, GetConsoleMode, GetConsoleScreenBufferInfo, GetStdHandle, ReadConsoleW,
    STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleCtrlHandler, SetConsoleMode,
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
        let clone = file.try_clone()?;
        let reader = ConsoleReader::new(clone.as_raw_handle() as usize, Some(clone));
        return Ok((Box::new(reader), Tty { file: Some(file) }));
    }
    // SAFETY: GetStdHandle takes no pointers; GetConsoleMode only writes the local.
    let (handle, is_console) = unsafe {
        let h = GetStdHandle(STD_INPUT_HANDLE);
        let mut mode = 0u32;
        (h as usize, GetConsoleMode(h, &raw mut mode) != 0)
    };
    // A console is read as UTF-16 text: Rust's own console stdin would turn a lone Ctrl+Z into end of input.
    if is_console {
        return Ok((Box::new(ConsoleReader::new(handle, None)), Tty { file: None }));
    }
    Ok((Box::new(io::stdin()), Tty { file: None }))
}

/// Console keys as UTF-8, through `ReadConsoleW` (so any code page and Ctrl+Z pass through unchanged).
struct ConsoleReader {
    handle: usize,
    /// Keeps a `CONIN$` handle open while this reads it.
    _file: Option<File>,
    /// Decoded bytes not yet handed out.
    out: std::collections::VecDeque<u8>,
    /// A high surrogate at the end of the last read, waiting for its pair.
    high: Option<u16>,
}

impl ConsoleReader {
    fn new(handle: usize, file: Option<File>) -> Self {
        Self { handle, _file: file, out: std::collections::VecDeque::new(), high: None }
    }
}

impl Read for ConsoleReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.out.is_empty() {
            let mut units = [0u16; 512];
            let mut got = 0u32;
            // SAFETY: the buffer holds 512 units, the count says so, and `got` is a local the call fills in.
            let ok = unsafe {
                ReadConsoleW(self.handle as _, units.as_mut_ptr().cast(), 512, &raw mut got, std::ptr::null())
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            if got == 0 {
                return Ok(0);
            }
            let mut all: Vec<u16> = self.high.take().into_iter().collect();
            all.extend_from_slice(&units[..got as usize]);
            if let Some(last) = all.last().copied()
                && (0xD800..0xDC00).contains(&last)
            {
                self.high = all.pop();
            }
            for c in char::decode_utf16(all).map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER)) {
                let mut b = [0u8; 4];
                self.out.extend(c.encode_utf8(&mut b).as_bytes());
            }
        }
        let n = buf.len().min(self.out.len());
        for (slot, byte) in buf.iter_mut().zip(self.out.drain(..n)) {
            *slot = byte;
        }
        Ok(n)
    }
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
