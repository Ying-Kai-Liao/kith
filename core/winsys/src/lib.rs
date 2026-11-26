//! The Win32 calls the core and kernel need, in one place (docs/plans/windows-owner.md D5). Safe functions over unsafe
//! FFI: `kith-core` calls these and keeps `forbid(unsafe_code)`.
//!
//! - [`acl`]: set and check an owner-only access list natively, by SID (no account names, no locale text, no
//!   `USERNAME`).
//! - [`cred`]: one generic Credential Manager item.
//! - [`tree`]: the parent chain of a process (Toolhelp32), for the run-token ancestry check.
//! - [`event`]: a named event, the host's stop signal.
//! - [`job`]: a named job object, and a process created inside it in one call (W12's launcher rule).
//! - [`procs`]: the process list with parents, and a hard stop of one process (the kernel's claim sweep and compaction).
//! - [`pipe`]: a named pipe only this account can open, the host's run channel for `kith permit`.
//!
//! Compiled on Windows only; on every other platform the crate is empty.
#![cfg(windows)]
#![allow(unsafe_code)]

pub mod acl;
pub mod cred;
pub mod event;
pub mod job;
pub mod pipe;
pub mod procs;
pub mod tree;

mod sid;

/// A Win32 error code as an `io::Error`.
pub(crate) fn win_err(code: u32) -> std::io::Error {
    // Win32 codes are unsigned and `from_raw_os_error` takes the same bits as `i32`.
    #[allow(clippy::cast_possible_wrap)]
    std::io::Error::from_raw_os_error(code as i32)
}

/// The calling thread's last Win32 error.
pub(crate) fn last_err() -> std::io::Error {
    std::io::Error::last_os_error()
}

/// A NUL-ended UTF-16 copy of `s`.
pub(crate) fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt as _;
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// A kernel handle, closed on drop.
pub(crate) struct Handle(pub(crate) windows_sys::Win32::Foundation::HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle came from a Win32 call that returned it open, and is closed once, here.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
    }
}
