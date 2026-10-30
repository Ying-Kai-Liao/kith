//! Intent before signal (F12 rules 8 and 14, designs/2026-10-24-lifecycle.md). Whoever is about to end a Claude process on
//! purpose writes `<state>/sessions/intent/<session>.json` first, so a session that is gone with no intent reads as a
//! crash, and a deliberate stop never does. The kernel writes and clears the same file (`kernel/lib/kith_lifecycle.py`:
//! the wrapper's idle stop and `/exit`; a process taking the session's writer claim clears it).
//!
//! `{"why": "...", "at": "<rfc3339>", "pid": N, "procStart": "<ps lstart>"}`; the start time ties it to the process it
//! was written for. Best effort: a stop never waits on bookkeeping that failed.

use super::registry::{proc_start, safe_id};
use serde_json::json;
use std::path::{Path, PathBuf};

/// Where the intent of `session` is.
pub fn path(state: &Path, session: &str) -> PathBuf {
    state.join("sessions").join("intent").join(format!("{session}.json"))
}

/// Records that `pid` (the process about to be signalled) is ended on purpose, and why. Returns whether it was written.
pub fn write(state: &Path, session: &str, why: &str, pid: u32) -> bool {
    if !safe_id(session) {
        return false;
    }
    let body = json!({
        "why": why,
        "at": chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        "pid": pid,
        "procStart": proc_start(pid),
    });
    let p = path(state, session);
    let Some(parent) = p.parent() else { return false };
    let tmp = parent.join(format!(".{session}.{}.tmp", std::process::id()));
    let made = std::fs::create_dir_all(parent).is_ok() && std::fs::write(&tmp, body.to_string()).is_ok();
    #[cfg(unix)]
    if made {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    made && std::fs::rename(&tmp, &p).is_ok()
}

/// A process took the session's writer claim: its intent and the pending 54-minute decision (`sessions/decisions`, written
/// by `kith-lifecycle`) are over.
pub fn taken(state: &Path, session: &str) {
    if safe_id(session) {
        let _ = std::fs::remove_file(path(state, session));
        let _ = std::fs::remove_file(state.join("sessions").join("decisions").join(format!("{session}.json")));
    }
}

/// The intent on record for `session`: its reason (the kernel is the reader in production; the host's tests read it too).
#[cfg(test)]
pub fn read(state: &Path, session: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path(state, session)).ok()?).ok()?;
    v["why"].as_str().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kith-intent-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn an_intent_is_written_before_the_signal_and_names_its_process() {
        let dir = scratch();
        assert_eq!(read(&dir, "s-1"), None);
        assert!(write(&dir, "s-1", "idle release", std::process::id()));
        assert_eq!(read(&dir, "s-1").as_deref(), Some("idle release"));
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path(&dir, "s-1")).unwrap()).unwrap();
        assert_eq!(v["pid"], std::process::id());
        assert!(v["at"].as_str().is_some_and(|at| at.ends_with('Z')));
    }

    #[test]
    fn taking_the_claim_clears_the_intent_and_the_decision() {
        let dir = scratch();
        assert!(write(&dir, "s-2", "idle release", 1));
        let decision = dir.join("sessions").join("decisions");
        std::fs::create_dir_all(&decision).unwrap();
        std::fs::write(decision.join("s-2.json"), "{}").unwrap();
        super::super::claim::take(&dir, "s-2", super::super::claim::Kind::Bot, std::process::id(), false)
            .unwrap()
            .unwrap();
        assert_eq!(read(&dir, "s-2"), None);
        assert!(!decision.join("s-2.json").exists());
    }

    #[test]
    fn a_name_that_could_leave_the_folder_is_refused() {
        let dir = scratch();
        assert!(!write(&dir, "../x", "idle", 1));
        assert!(!write(&dir, "", "idle", 1));
        assert!(!dir.join("sessions").exists());
    }
}
