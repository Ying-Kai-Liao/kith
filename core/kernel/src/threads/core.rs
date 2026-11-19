//! The Kith core, as the kernel's writer-claim takers reach it (`kernel/lib/kith_claims.py`): the claims are rows in the core
//! once the `claims` module is switched, and the session registry is `session.record` once `transcripts` is.
//!
//! Two modes, picked by files and never by an env var, exactly as the Python reads them (fail closed: a taker never goes
//! back to the files by accident):
//!
//! - Files: no marker. `<state>/live/terminal/<sid>.claim` files; the core is never asked.
//! - Writer: the marker `<state>/live/terminal/claims.tombstone` exists (existence is all that is read), or the module's own
//!   record `<machine>/core/cutover/claims.switch` says more than `state=stepping`. No claim file is read or written.
//!
//! The core is a trait ([`Core`]) because the crate never reaches a socket itself: the program supplies the owner-socket
//! client (`serve/host/src/threads_cmd.rs`), the tests a fake. A call's three ends are told apart on purpose, as
//! `kith_claims.call` does: an answer (a refusal can be data in it), a rejection ([`CoreErr::Refused`]: nothing ran) and no
//! answer ([`CoreErr::NoAnswer`]: no daemon, a timeout, a lost answer; a claim may or may not have been made).

use std::path::Path;
use std::time::Duration;

use serde_json::Value;

use super::Ctx;

/// The claims' marker in `<state>/live/terminal/`.
pub const CLAIMS_MARKER: &str = "claims.tombstone";

/// Why a call of the core did not give an answer.
#[derive(Debug, PartialEq, Eq)]
pub enum CoreErr {
    /// The core rejected the call (a bad param, a row that is not open to the caller); nothing ran.
    Refused(String),
    /// Nothing answered, or the answer was lost.
    NoAnswer(String),
}

impl CoreErr {
    /// The reason as the Python prints it (`getattr(e, 'why', e)`).
    pub fn why(&self) -> &str {
        match self {
            Self::Refused(w) | Self::NoAnswer(w) => w,
        }
    }
}

/// The owner's door onto the core. `timeout` bounds the whole call, finding the program included; none leaves the caller's
/// own bounds (the claims: eight seconds to find the program, eight for the call).
pub trait Core {
    fn call(
        &self,
        method: &str,
        params: &Value,
        timeout: Option<Duration>,
    ) -> Result<Value, CoreErr>;
}

/// Whether `<machine>/core/cutover/<module>.switch` makes the module the core's: it exists and says more than `stepping`
/// (switched, rolling back, or unreadable). No machine folder to look in is no second source.
fn switch_says_more(c: &Ctx, module: &str) -> bool {
    let Some(machine) = c.machine.as_deref() else {
        return false;
    };
    let f = machine
        .join("core")
        .join("cutover")
        .join(format!("{module}.switch"));
    // Only a file that is not there is no second source; one we cannot even look for is no reason to fall back (as the marker).
    if let Err(e) = std::fs::symlink_metadata(&f) {
        return e.kind() != std::io::ErrorKind::NotFound;
    }
    match std::fs::read_to_string(&f) {
        Ok(text) => !text.lines().any(|l| l == "state=stepping"),
        Err(_) => true,
    }
}

fn marker_or_switch(c: &Ctx, marker: &str, module: &str) -> bool {
    let Some(dir) = c.reg_dir() else {
        return false;
    };
    // Exists means lexists (a dangling link counts); a marker we cannot even look for is no reason to fall back.
    let marker_there = match std::fs::symlink_metadata(Path::new(&dir).join(marker)) {
        Ok(_) => true,
        Err(e) => e.kind() != std::io::ErrorKind::NotFound,
    };
    marker_there || switch_says_more(c, module)
}

/// `kith_claims.switched`: the writer claims are the core's on this Mac.
pub fn claims_switched(c: &Ctx) -> bool {
    marker_or_switch(c, CLAIMS_MARKER, "claims")
}

/// `kith_registry.switched`: the session registry is the core's on this Mac.
pub fn registry_switched(c: &Ctx) -> bool {
    marker_or_switch(c, super::registry::TOMBSTONE, "transcripts")
}

/// The stub's key rule (`kith_claims.lockable`): a conversation whose id breaks it runs without the writer lock, so Writer
/// mode refuses it.
pub fn lockable(sid: &str) -> bool {
    !sid.is_empty()
        && sid.len() <= 128
        && sid != "."
        && sid != ".."
        && sid.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}
