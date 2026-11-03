//! Runs the engine as a child on Windows.
//!
//! The engine shares the stub's console, so Ctrl-C and Ctrl-Break reach both. The stub installs a handler that
//! swallows them (a handler is not inherited, unlike "ignore Ctrl-C", so the engine still sees the event) and
//! keeps waiting: it must outlive the engine, because it holds the lock. Console close / logoff / shutdown fall
//! through to the default and end the stub, which releases the lock.
//!
//! Gaps: no job object, so engine descendants are not killed with the engine or the stub; a stub that is
//! terminated hard leaves the engine running unguarded (same as Unix SIGKILL).

use std::ffi::OsString;
use std::io::ErrorKind;
use std::process::Command;

use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT, SetConsoleCtrlHandler};

unsafe extern "system" fn handler(event: u32) -> i32 {
    // Nonzero = handled.
    (event == CTRL_C_EVENT || event == CTRL_BREAK_EVENT) as i32
}

pub fn run(engine: &[OsString]) -> i32 {
    // SAFETY: handler matches the PHANDLER_ROUTINE signature.
    unsafe { SetConsoleCtrlHandler(Some(handler), 1) };
    match Command::new(&engine[0]).args(&engine[1..]).status() {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            eprintln!("kith-stub: cannot start {:?}: {e}", engine[0]);
            if e.kind() == ErrorKind::NotFound { 127 } else { 126 }
        }
    }
}
