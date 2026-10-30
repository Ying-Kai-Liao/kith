//! Terminal sessions as threads. A session started with `kith <agent>` in a terminal is listed by the kernel in
//! `$KITH_STATE/live/terminal/<session>.json` (see `registry`). The host turns each into a thread under the owner's
//! bot of that agent, imports the Claude transcript into it (see `jsonl`), and lets the apps continue the same
//! Claude session once the terminal is closed or taken over. One writer at a time: the terminal while it lives, else
//! the host.
//!
//! kv state, all keyed by thread (`<bot>/<root>`) or session:
//! - `terminal.sid.<session>` -> `<bot>/<root>`: which thread a session belongs to.
//! - `terminal.info.<thread>`: the newest registry file of the thread's session.
//! - `terminal.cursor.<thread>`: the byte offset of the transcript imported so far.
//! - `terminal.final.<thread>`: the imported agent reply still marked final.
//! - `terminal.live.<thread>`: `1` while the terminal was seen alive (for the writer-change notices).
//!
//! Privacy: threads live in the owner's bot only, so invitees never reach them (see `api::roles`). What a
//! transcript says is never logged.

pub mod claim;
pub mod intent;
mod jsonl;
mod registry;
mod sync;
#[cfg(test)]
mod tests;

#[cfg(test)]
pub use sync::scan;
pub use sync::{SendGate, bot_gate, carry_over, host_turn_ended, resolve, send_gate, start, start_gate, take_over};

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Mutex;

/// Where the host looks: the Kith state folder, and Claude's `sessions/` for liveness (None: the default login's).
#[derive(Clone, Debug)]
pub struct Env {
    pub state: PathBuf,
    pub sessions: Option<PathBuf>,
}

impl Default for Env {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_default();
        Self { state: crate::kernel::machine_dir(&home), sessions: registry::default_sessions_dir() }
    }
}

/// The host's terminal-thread state in memory.
#[derive(Default)]
pub struct Terminal {
    env: Mutex<Option<Env>>,
    /// Threads being taken over right now: nothing is sent to them.
    taking: Mutex<HashSet<String>>,
    /// One lock per thread: imports, state changes and cursor moves of a thread take turns.
    locks: Mutex<std::collections::HashMap<String, std::sync::Arc<Mutex<()>>>>,
    /// Sessions without a real user message yet, and the transcript size they were last checked at.
    empty: Mutex<std::collections::HashMap<String, u64>>,
    /// Threads whose actor was told to forget its cached session (tests look here; no actor runs in them).
    #[cfg(test)]
    forgets: Mutex<Vec<String>>,
}

impl Terminal {
    pub fn env(&self) -> Env {
        use crate::LockExt;
        #[cfg_attr(not(test), allow(unused_mut))]
        let mut env = self.env.locked();
        // A test host never reads or writes this machine's state (turn claims would collide across parallel tests).
        #[cfg(test)]
        if env.is_none() {
            let dir = std::env::temp_dir().join(format!("kith-terminal-env-{}", uuid::Uuid::new_v4()));
            *env = Some(Env { state: dir.join("state"), sessions: Some(dir.join("sessions")) });
        }
        env.clone().unwrap_or_default()
    }

    #[cfg(test)]
    pub fn take_for_test(&self, key: &str) {
        use crate::LockExt;
        self.taking.locked().insert(key.to_owned());
    }

    pub(crate) fn thread_lock(&self, key: &str) -> std::sync::Arc<Mutex<()>> {
        use crate::LockExt;
        self.locks.locked().entry(key.to_owned()).or_default().clone()
    }

    /// Points the host at another state folder (tests, scratch hosts).
    #[cfg(test)]
    pub fn set_env(&self, env: Env) {
        use crate::LockExt;
        *self.env.locked() = Some(env);
    }
}

/// A send to a thread another writer holds. Its text starts with the code the apps look for.
#[derive(Debug)]
pub struct HeldByWriter {
    /// `liveInTerminal`, or `heldByOtherWriter` for any other writer.
    pub code: &'static str,
}

impl std::fmt::Display for HeldByWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let why = if self.code == "liveInTerminal" {
            "This conversation is open in a terminal on the computer. Take it over, or carry on there."
        } else {
            "Something else is writing to this conversation right now."
        };
        write!(f, "{}: {why}", self.code)
    }
}

impl std::error::Error for HeldByWriter {}

/// A take-over the terminal's work stops (it is busy, or didn't end).
#[derive(Debug)]
pub struct TerminalBusy(pub &'static str);

impl std::fmt::Display for TerminalBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for TerminalBusy {}
