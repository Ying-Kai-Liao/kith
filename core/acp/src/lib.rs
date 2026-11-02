//! The ACP (Agent Client Protocol) client and one-shot runs over it. The one async crate of the core: the `kith`
//! program and the host both link it, so an engine behaves the same whichever starts it. No web server, no host types.

mod client;
mod run_once;

pub use client::*;
pub use run_once::*;

use std::sync::{Mutex, MutexGuard, PoisonError};

/// A lock that survives a panic elsewhere: every update behind it is self-contained.
trait LockExt<T> {
    fn locked(&self) -> MutexGuard<'_, T>;
}

impl<T> LockExt<T> for Mutex<T> {
    fn locked(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
