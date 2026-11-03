//! Lifecycle intents and decisions (F12 rules 5, 8, 11, 12, 14): `<state>/sessions/intent/<sid>.json` is written by
//! whoever is about to end a Claude process on purpose, before the signal, so a session that is gone with no intent
//! reads as a crash; `<state>/sessions/decisions/<sid>.json` is the 54-minute compact/end decision. A port of the file
//! half of `kernel/lib/kith_lifecycle.py` and of the host's `terminal/intent.rs`, which write the same files. A process
//! that takes the session's writer claim ends both ([`taken`]).

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::{iso_utc, os, safe_id, write_json};

fn dir(state: &Path, sub: &str) -> PathBuf {
    state.join("sessions").join(sub)
}

/// Where the intent of `sid` is.
pub fn intent_path(state: &Path, sid: &str) -> PathBuf {
    dir(state, "intent").join(format!("{sid}.json"))
}

/// Where the decision on `sid` is.
pub fn decision_path(state: &Path, sid: &str) -> PathBuf {
    dir(state, "decisions").join(format!("{sid}.json"))
}

/// Records that the process `pid` of session `sid` is about to be ended on purpose, and why. Best effort: false when
/// the name is not a session id or the write failed.
pub fn write_intent(state: &Path, sid: &str, why: &str, pid: i64) -> bool {
    if !safe_id(sid) {
        return false;
    }
    let body = json!({"why": why, "at": iso_utc(), "pid": pid, "procStart": os::proc_start(pid)});
    write_json(&intent_path(state, sid), &body, false).is_ok()
}

fn read_object(path: &Path) -> Option<Map<String, Value>> {
    match serde_json::from_str::<Value>(&std::fs::read_to_string(path).ok()?).ok()? {
        Value::Object(o) => Some(o),
        _ => None,
    }
}

pub fn read_intent(state: &Path, sid: &str) -> Option<Map<String, Value>> {
    safe_id(sid)
        .then(|| read_object(&intent_path(state, sid)))
        .flatten()
}

pub fn clear_intent(state: &Path, sid: &str) {
    if safe_id(sid) {
        let _ = std::fs::remove_file(intent_path(state, sid));
    }
}

pub fn read_decision(state: &Path, sid: &str) -> Option<Map<String, Value>> {
    safe_id(sid)
        .then(|| read_object(&decision_path(state, sid)))
        .flatten()
}

pub fn write_decision(state: &Path, sid: &str, rec: &Value) -> bool {
    safe_id(sid) && write_json(&decision_path(state, sid), rec, false).is_ok()
}

pub fn clear_decision(state: &Path, sid: &str) {
    if safe_id(sid) {
        let _ = std::fs::remove_file(decision_path(state, sid));
    }
}

/// A process took the session's writer claim: the intent it ends (rule 14) and the decision it answers (rule 12) are
/// over. Best effort: the claim must never fail on bookkeeping.
pub fn taken(state: &Path, sid: &str) {
    clear_intent(state, sid);
    clear_decision(state, sid);
}

/// Why a session with no process stopped: the intent's reason (`idle`, `quit`, ...), or `crash` when a process ran and
/// ended with none. The intent must be for the process that ended (its pid, and its start time when both sides know
/// it). A session never seen run (`had_process` false) with no intent is `None`.
pub fn how_it_ended(
    state: &Path,
    sid: &str,
    had_process: bool,
    pid: Option<i64>,
    start: Option<&str>,
) -> Option<String> {
    if let Some(i) = read_intent(state, sid) {
        let pid_ok = pid.is_none_or(|p| {
            i.get("pid")
                .and_then(Value::as_i64)
                .is_none_or(|ip| ip == p)
        });
        let start_clash = start.filter(|s| !s.is_empty()).is_some_and(|s| {
            i.get("procStart")
                .and_then(Value::as_str)
                .filter(|p| !p.is_empty())
                .is_some_and(|p| s.split_whitespace().collect::<Vec<_>>().join(" ") != p)
        });
        if pid_ok && !start_clash {
            return i.get("why").and_then(Value::as_str).map(str::to_owned);
        }
    }
    had_process.then(|| "crash".to_owned())
}
