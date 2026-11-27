//! A named event, the host's stop signal: the running host waits on it, `kith serve stop` signals it (plan W2a/W11).
//!
//! The event is made with this process's default security, so only the same user (and SYSTEM and Administrators) can
//! open it. It lives in the session's namespace (`Local\` is added when `name` has no prefix of its own).

use std::ffi::OsStr;
use std::io;
use std::path::Path;
use std::time::Duration;

use windows_sys::Win32::Foundation::{WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::System::Threading::{
    CreateEventW, EVENT_MODIFY_STATE, OpenEventW, ResetEvent, SYNCHRONIZATION_SYNCHRONIZE,
    SetEvent, WaitForSingleObject,
};

use crate::{Handle, last_err, wide};

/// An open named event.
pub struct StopEvent(Handle);

// SAFETY: a kernel event handle may be used from any thread; the calls here take it by shared reference.
unsafe impl Send for StopEvent {}
// SAFETY: as above.
unsafe impl Sync for StopEvent {}

/// The stop event's name for the host whose data folder is `dir`: one per folder, so hosts with other folders (the
/// tests', a dev identity's) never see each other's signal. The host and `kith serve stop` both compute it from the folder.
#[must_use]
pub fn stop_event_name(dir: &Path) -> String {
    // FNV-1a over the lowercased path, as the pipe names do (a path differs by case only on this file system).
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for b in dir.to_string_lossy().to_lowercase().bytes() {
        hash = (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
    }
    format!("kith-serve-stop-{hash:016x}")
}

fn full_name(name: &str) -> String {
    if name.contains('\\') {
        name.to_owned()
    } else {
        format!("Local\\{name}")
    }
}

impl StopEvent {
    /// Makes the event (manual reset, not signalled) or opens it when it exists already.
    pub fn create(name: &str) -> io::Result<Self> {
        let n = wide(OsStr::new(&full_name(name)));
        // SAFETY: `n` is NUL-ended; null attributes mean the default security.
        let h = unsafe { CreateEventW(std::ptr::null(), 1, 0, n.as_ptr()) };
        if h.is_null() {
            return Err(last_err());
        }
        Ok(Self(Handle(h)))
    }

    /// Opens an event somebody made. `Ok(None)` when there is none (nothing is running to stop).
    pub fn open(name: &str) -> io::Result<Option<Self>> {
        let n = wide(OsStr::new(&full_name(name)));
        // SAFETY: `n` is NUL-ended.
        let h = unsafe {
            OpenEventW(
                EVENT_MODIFY_STATE | SYNCHRONIZATION_SYNCHRONIZE,
                0,
                n.as_ptr(),
            )
        };
        if h.is_null() {
            let e = last_err();
            // ERROR_FILE_NOT_FOUND
            return if e.kind() == io::ErrorKind::NotFound {
                Ok(None)
            } else {
                Err(e)
            };
        }
        Ok(Some(Self(Handle(h))))
    }

    /// Signals the event; every waiter wakes and it stays signalled.
    pub fn signal(&self) -> io::Result<()> {
        // SAFETY: the handle is an open event with modify access.
        if unsafe { SetEvent(self.0.0) } == 0 {
            Err(last_err())
        } else {
            Ok(())
        }
    }

    /// Clears the signal. A starting host does this to the event it made or opened, so a stop sent to its predecessor
    /// (whose handle somebody still held, which kept the event alive) is not read as a stop for it.
    pub fn reset(&self) -> io::Result<()> {
        // SAFETY: the handle is an open event with modify access.
        if unsafe { ResetEvent(self.0.0) } == 0 {
            Err(last_err())
        } else {
            Ok(())
        }
    }

    /// Waits up to `timeout` for the signal. `true` when it came.
    pub fn wait(&self, timeout: Duration) -> io::Result<bool> {
        let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
        // SAFETY: the handle is an open event with synchronize access.
        match unsafe { WaitForSingleObject(self.0.0, ms) } {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            WAIT_FAILED => Err(last_err()),
            other => Err(io::Error::other(format!(
                "kith-winsys: unexpected wait result {other}"
            ))),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn a_signal_from_another_handle_wakes_the_waiter() {
        let name = format!("kith-winsys-test-{}", std::process::id());
        assert!(StopEvent::open(&name).unwrap().is_none());
        let waiter = StopEvent::create(&name).unwrap();
        assert!(!waiter.wait(Duration::from_millis(20)).unwrap());
        StopEvent::open(&name).unwrap().unwrap().signal().unwrap();
        assert!(waiter.wait(Duration::from_secs(5)).unwrap());
        waiter.reset().unwrap();
        assert!(!waiter.wait(Duration::from_millis(20)).unwrap());
    }

    #[test]
    fn the_name_follows_the_folder() {
        let a = stop_event_name(Path::new(r"C:\Users\a\kith"));
        assert_eq!(a, stop_event_name(Path::new(r"C:\USERS\a\KITH")));
        assert_ne!(a, stop_event_name(Path::new(r"C:\Users\b\kith")));
    }
}
