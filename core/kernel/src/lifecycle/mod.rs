//! `kith kernel lifecycle sweep|status|intent`: what Kith does with a session no process runs (F12,
//! designs/2026-10-24-lifecycle.md), a port of `kernel/bin/kith-lifecycle` and the lifecycle half of
//! `kernel/lib/kith_lifecycle.py`, with the Python's output, exit codes and rules:
//!
//! - one compactor per session: only a session with no live process and no claim is touched, and one the host keeps a
//!   conversation for (`resolveThread` knows it) is skipped, the host's own compactor handles those (rules 5, 6);
//! - compact runs only while holding the writer claim, checked again under it (rule 11), under kith-stub; the claim names
//!   the stub's pid (claude's when there is none); end is written only under the claim too (rule 12);
//! - a failed `/compact` is tried once per idle period; a conversation already locked is `busy`, no decision;
//! - one sweep at a time (`flock` on `sessions/lifecycle.lock`, the same lock the Python takes).
//!
//! The claims are taken through [`crate::threads::writer::Taker`], the one implementation of the writer claims; this
//! module has no claim code of its own.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::threads::{Ctx, intent};

pub mod cli;
pub mod proc;
pub mod sweep;
pub mod transcript;

#[cfg(test)]
mod tests;

/// `$KITH_STATE` when it is set and not empty (`lc.state_dir()`).
pub fn state_dir(c: &Ctx) -> Option<PathBuf> {
    c.state.clone().filter(|s| !s.as_os_str().is_empty())
}

/// The 54-minute decision of one idle period (`<state>/sessions/decisions/<sid>.json`).
pub mod decision {
    use super::{Map, Path, Value, intent};

    pub fn read(state: &Path, sid: &str) -> Option<Map<String, Value>> {
        intent::read_decision(state, sid)
    }

    pub fn write(state: &Path, sid: &str, rec: &Value) {
        intent::write_decision(state, sid, rec);
    }
}

/// Why a session with no process stopped: the intent's reason (`idle`, `quit`, `revoked`, ...), or `crash` when a process
/// ran and ended with none. The intent must be for the process that ended (its pid, and its start time when both sides
/// know it). A run under kith-stub is also known by the stub's pid and start (`launcher`): the host writes the intent for the
/// claim's pid. A session never seen run is `None`: nothing is claimed about it.
pub fn how_it_ended(
    state: &Path,
    sid: &str,
    had_process: bool,
    entry: Option<&Map<String, Value>>,
) -> Option<String> {
    let field = |k: &str| {
        entry
            .and_then(|e| e.get(k))
            .filter(|v| transcript::truthy(Some(v)))
    };
    let (pid, start) = (
        entry.and_then(|e| e.get("pid")),
        field("procStart").and_then(Value::as_str),
    );
    let (launcher, launcher_start) = (
        field("launcher"),
        field("launcherStart").and_then(Value::as_str),
    );
    let differs = |i: &Map<String, Value>, other: Option<&str>| {
        other.is_some_and(|o| {
            i.get("procStart")
                .and_then(Value::as_str)
                .filter(|p| !p.is_empty())
                .is_some_and(|p| !crate::procstart::same_process(p, o))
        })
    };
    if let Some(i) = intent::read_intent(state, sid) {
        let why = || {
            i.get("why")
                .cloned()
                .and_then(|w| w.as_str().map(str::to_owned))
        };
        if let Some(l) = launcher
            && i.get("pid") == Some(l)
            && !differs(&i, launcher_start)
        {
            return why();
        }
        let pid_ok = pid.is_none_or(Value::is_null)
            || i.get("pid").is_none_or(Value::is_null)
            || i.get("pid") == pid;
        if pid_ok && !differs(&i, start) {
            return why();
        }
    }
    had_process.then(|| "crash".to_owned())
}
