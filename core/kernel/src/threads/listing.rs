//! The registry as the host reads it: `serve/host/src/terminal/registry.rs`. [`super::registry`] writes the files
//! (`record`); this reads the same ones the way the host does, defensively: a file that is malformed, too big, for an
//! unlisted kind, or looks like it belongs to a person is skipped. `read_all` is the host's scan, `liveness` its answer to
//! "is this session being worked on right now". Nothing calls this yet.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{clock, os, registry_dir, safe_id};

/// Sessions older than this (by start or by the transcript's last write) are no longer listed.
pub const MAX_AGE_MS: i64 = 14 * 24 * 60 * 60 * 1000;

/// A registry file longer than this is not read.
const MAX_FILE: u64 = 64 * 1024;

/// The kinds the kernel registers.
pub const KINDS: [&str; 4] = ["terminal", "ask", "schedule", "helper"];

/// One registry file's content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Registry {
    pub v: i64,
    pub session_id: String,
    pub agent: String,
    pub cwd: String,
    pub transcript_path: String,
    pub pid: i64,
    pub project: Option<String>,
    pub source: String,
    pub resumed_from: Option<String>,
    pub started_at: String,
    /// What started the run: `terminal`, or a one-shot the owner's machine ran for the agent: `ask`, `schedule`, `helper`.
    /// Files written before one-shots were listed have none: terminal.
    pub kind: String,
    /// Who or what started a one-shot, as the room shows it ("asked by architect"); `None` for a terminal.
    pub label: Option<String>,
    /// When the claude process started (`ps -o lstart`): a one-shot's liveness, since `claude -p` may keep no session file.
    pub proc_start: Option<String>,
    /// The login the session was made under (`claude`, `claude:work`).
    pub account: Option<String>,
}

/// A required field of the type asked for, or `None` (serde's rule: a missing key and a wrong type both fail the file).
fn must<'a>(o: &'a serde_json::Map<String, Value>, k: &str) -> Option<&'a Value> {
    o.get(k)
}

fn text(v: &Value) -> Option<String> {
    v.as_str().map(str::to_owned)
}

/// An optional field: missing and `null` are `Some(None)`, a wrong type fails the file (`None`).
#[allow(clippy::option_option)]
fn opt_text(o: &serde_json::Map<String, Value>, k: &str) -> Option<Option<String>> {
    match o.get(k) {
        None | Some(Value::Null) => Some(None),
        Some(v) => text(v).map(Some),
    }
}

fn plain_text(o: &serde_json::Map<String, Value>, k: &str, default: &str) -> Option<String> {
    match o.get(k) {
        None => Some(default.to_owned()),
        Some(v) => text(v),
    }
}

/// serde's `u32`: a whole number from 0 to 4294967295 (a float or a negative number fails).
fn uint32(v: &Value) -> Option<i64> {
    v.as_u64()
        .filter(|n| u32::try_from(*n).is_ok())
        .and_then(|n| i64::try_from(n).ok())
}

impl Registry {
    /// A one-shot the machine ran (not a terminal a person sits in).
    pub fn one_shot(&self) -> bool {
        self.kind != "terminal"
    }

    /// When the session started (ms), 0 when the file doesn't say.
    pub fn started_ms(&self) -> i64 {
        clock::rfc3339_ms(&self.started_at).unwrap_or(0)
    }

    pub fn transcript(&self) -> &Path {
        Path::new(&self.transcript_path)
    }

    /// Claude's own folder for this session (`~/.claude`, or the account's): two levels above `projects/`.
    fn config_dir(&self) -> Option<PathBuf> {
        let projects = self.transcript().parent()?.parent()?;
        (projects.file_name()? == "projects").then(|| projects.parent().map(Path::to_path_buf))?
    }
}

/// One registry file's content, when it is a v1 file for a listable session.
pub fn parse(raw: &str) -> Option<Registry> {
    let v: Value = serde_json::from_str(raw).ok()?;
    let o = v.as_object()?;
    // Sessions of invited people are never listed.
    if o.iter()
        .any(|(k, x)| k.to_ascii_lowercase().contains("person") && !x.is_null())
    {
        return None;
    }
    let reg = Registry {
        v: uint32(must(o, "v")?)?,
        session_id: text(must(o, "sessionId")?)?,
        agent: text(must(o, "agent")?)?,
        cwd: text(must(o, "cwd")?)?,
        transcript_path: text(must(o, "transcriptPath")?)?,
        pid: uint32(must(o, "pid")?)?,
        project: opt_text(o, "project")?,
        source: plain_text(o, "source", "")?,
        resumed_from: opt_text(o, "resumedFrom")?,
        started_at: plain_text(o, "startedAt", "")?,
        kind: plain_text(o, "kind", "terminal")?,
        label: opt_text(o, "label")?,
        proc_start: opt_text(o, "procStart")?,
        account: opt_text(o, "account")?,
    };
    let sane = reg.v == 1
        && safe_id(&reg.session_id)
        && !reg.agent.is_empty()
        && Path::new(&reg.cwd).is_absolute()
        && reg.transcript().is_absolute()
        && reg
            .transcript()
            .file_name()
            .is_some_and(|n| n.to_string_lossy() == format!("{}.jsonl", reg.session_id))
        && KINDS.contains(&reg.kind.as_str())
        && reg.pid > 1
        && reg.pid != i64::from(std::process::id());
    sane.then_some(reg)
}

/// The registry file of one session, read now.
pub fn read_one(state: &Path, sid: &str) -> Option<Registry> {
    let path = registry_dir(state).join(format!("{sid}.json"));
    if std::fs::metadata(&path).ok()?.len() > MAX_FILE {
        return None;
    }
    parse(&std::fs::read_to_string(path).ok()?)
}

/// Every readable registry file in `state`, oldest first.
pub fn read_all(state: &Path) -> Vec<Registry> {
    let Ok(read) = std::fs::read_dir(registry_dir(state)) else {
        return Vec::new();
    };
    let mut all: Vec<Registry> = read
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter_map(|p| {
            if std::fs::metadata(&p).ok()?.len() > MAX_FILE {
                return None;
            }
            parse(&std::fs::read_to_string(p).ok()?)
        })
        .collect();
    all.sort_by_key(|r| (r.started_ms(), r.session_id.clone()));
    all
}

/// Whether the session is being worked on right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Live {
    Closed,
    Idle,
    Busy,
}

impl Live {
    pub fn is_live(self) -> bool {
        self != Self::Closed
    }
}

/// Where Claude keeps `sessions/<pid>.json` for the sessions of the default login: `$CLAUDE_CONFIG_DIR`, else `~/.claude`.
pub fn default_sessions_dir(env: crate::paths::Env, home: &Path) -> PathBuf {
    env("CLAUDE_CONFIG_DIR")
        .filter(|d| !d.is_empty())
        .map_or_else(|| home.join(".claude"), PathBuf::from)
        .join("sessions")
}

/// Live = the pid runs and its session file names this session (a reused pid names another). Busy = live and that file
/// says it is doing something: anything but `idle`, so an unknown state never lets a take-over kill work. `sessions` is
/// where the default login keeps its session files; the login the session was made under keeps its own.
pub fn liveness(reg: &Registry, sessions: Option<&Path>) -> Live {
    if !os::pid_runs(reg.pid) {
        return Live::Closed;
    }
    let candidates = [
        sessions.map(Path::to_path_buf),
        reg.config_dir().map(|c| c.join("sessions")),
    ];
    for dir in candidates.into_iter().flatten() {
        let Ok(raw) = std::fs::read_to_string(dir.join(format!("{}.json", reg.pid))) else {
            continue;
        };
        let Ok(file) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        if file["sessionId"] != reg.session_id.as_str()
            || !started_as(reg.pid, file["procStart"].as_str())
        {
            continue;
        }
        return if file["status"] == "idle" {
            Live::Idle
        } else {
            Live::Busy
        };
    }
    // A `claude -p` run may keep no session file: while its process runs (the same one that started) it is working. It ends
    // with the process, so a crashed run never stays busy.
    if reg.one_shot() && reg.proc_start.is_some() && started_as(reg.pid, reg.proc_start.as_deref())
    {
        return Live::Busy;
    }
    Live::Closed
}

fn squeezed(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether the process `pid` started when the session file says (`procStart`, as `ps -o lstart` prints it). A pid reused
/// after a crash starts at another time. When either side can't be read the file is believed. Claude Code writes its own
/// session file's start in UTC, the kernel its files' in the Mac's local time: either one matches.
fn started_as(pid: i64, claimed: Option<&str>) -> bool {
    let Some(claimed) = claimed else { return true };
    let claimed = squeezed(claimed);
    match os::proc_start(pid) {
        Some(actual) if actual == claimed => true,
        Some(_) => os::proc_start_utc(pid).is_some_and(|utc| utc == claimed),
        None => true,
    }
}
