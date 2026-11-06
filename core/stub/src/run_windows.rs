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

pub struct Spawned(std::process::Child);

/// Starts the engine, or returns the stub's exit code when it cannot start. A resident stub does this before it serves
/// its control channel, as on Unix.
pub fn spawn(engine: &[OsString]) -> Result<Spawned, i32> {
    // SAFETY: handler matches the PHANDLER_ROUTINE signature.
    unsafe { SetConsoleCtrlHandler(Some(handler), 1) };
    match Command::new(&engine[0]).args(&engine[1..]).spawn() {
        Ok(c) => Ok(Spawned(c)),
        Err(e) => {
            eprintln!("kith-stub: cannot start {:?}: {e}", engine[0]);
            Err(if e.kind() == ErrorKind::NotFound {
                127
            } else {
                126
            })
        }
    }
}

pub fn run(engine: &[OsString]) -> i32 {
    match spawn(engine) {
        Ok(child) => wait(child),
        Err(code) => code,
    }
}

/// Holds until the engine is gone; the stub's exit code.
pub fn wait(mut child: Spawned) -> i32 {
    match child.0.wait() {
        Ok(s) => s.code().unwrap_or(1),
        Err(_) => 1,
    }
}
