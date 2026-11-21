//! The client's circuit breaker: one per core in this process, so a core that answers `busy` or does not answer in
//! time is not piled on by every caller of the process (core-lock-hang design B).
//!
//! It opens on a `busy` answer or a timeout and holds for 1 s, doubling up to 30 s while the core keeps failing. While
//! open no call opens a connection: [`Gate::Hold`] is "core down", nothing sent. When the window is over exactly one
//! caller is let through as the test call ([`Gate::Probe`]); the rest keep holding until it ends. Any answer closes the
//! breaker, and anything but `busy` or a timeout counts as one (the core is reachable, or plainly not there).
//!
//! The breaker only stops sending. It never repeats a call, so a write is never sent twice because of it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

const FIRST_WAIT: Duration = Duration::from_secs(1);
const MAX_WAIT: Duration = Duration::from_secs(30);

/// What a caller may do now.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum Gate {
    /// Closed: go ahead.
    Pass,
    /// Open, window over: this caller is the one test call. It must report back with [`Breakers::end`].
    Probe,
    /// Open: open no connection.
    Hold,
}

#[derive(Default)]
struct Breaker {
    /// `Some` while open: no call before this instant, then one test call.
    until: Option<Instant>,
    /// How long the next window lasts.
    wait: Duration,
    /// The test call of this window is out.
    probing: bool,
}

/// The breakers of the cores this process talks to, by owner socket.
#[derive(Default)]
pub(crate) struct Breakers(Mutex<HashMap<PathBuf, Breaker>>);

/// The breakers of this process.
pub(crate) fn process() -> &'static Breakers {
    static PROCESS: std::sync::OnceLock<Breakers> = std::sync::OnceLock::new();
    PROCESS.get_or_init(Breakers::default)
}

impl Breakers {
    fn map(&self) -> MutexGuard<'_, HashMap<PathBuf, Breaker>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn gate(&self, core: &Path, now: Instant) -> Gate {
        let mut map = self.map();
        let Some(b) = map.get_mut(core) else {
            return Gate::Pass;
        };
        match b.until {
            None => Gate::Pass,
            Some(until) if now < until || b.probing => Gate::Hold,
            Some(_) => {
                b.probing = true;
                Gate::Probe
            }
        }
    }

    /// Closes the breaker of `core`: a test of the server asks again at once, as the breaker would not let it.
    #[cfg(test)]
    pub(crate) fn forget(&self, core: &Path) {
        self.map().remove(core);
    }

    /// A call ended. `tripped`: it came to `busy` or a timeout. A call that was let through (`Pass` or `Probe`)
    /// reports exactly once.
    pub(crate) fn end(&self, core: &Path, tripped: bool, now: Instant) {
        let mut map = self.map();
        if !tripped {
            map.remove(core);
            return;
        }
        let b = map.entry(core.to_path_buf()).or_default();
        // Calls that passed before it opened may still report: they do not lengthen a window that is running.
        if b.until.is_some_and(|until| now < until) && !b.probing {
            return;
        }
        b.wait = if b.wait.is_zero() {
            FIRST_WAIT
        } else {
            (b.wait * 2).min(MAX_WAIT)
        };
        b.until = Some(now + b.wait);
        b.probing = false;
    }
}

#[cfg(test)]
#[path = "breaker_tests.rs"]
mod tests;
