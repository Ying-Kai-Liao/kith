//! The terminal session registry the kernel writes (`$KITH_STATE/live/terminal/<session>.json`, one per listable
//! `kith <agent>` session) and whether each session is alive. Defensive: a file that is malformed, or looks like
//! it belongs to a person, is skipped.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Sessions older than this (by start or by the transcript's last write) are no longer listed.
pub const MAX_AGE_MS: i64 = 14 * 24 * 60 * 60 * 1000;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Registry {
    pub v: u32,
    pub session_id: String,
    pub agent: String,
    pub cwd: String,
    pub transcript_path: String,
    pub pid: u32,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub resumed_from: Option<String>,
    #[serde(default)]
    pub started_at: String,
    /// What started the run: `terminal` (an interactive `kith <agent>`), or a one-shot the owner's machine ran for the
    /// agent: `ask`, `schedule`, `helper`. Files written before one-shots were listed have none: terminal.
    #[serde(default = "default_kind")]
    pub kind: String,
    /// Who or what started a one-shot, as the room shows it ("asked by architect"); None for a terminal.
    #[serde(default)]
    pub label: Option<String>,
    /// When the claude process started (`ps -o lstart`): a one-shot's liveness, since `claude -p` may keep no session file.
    #[serde(default)]
    pub proc_start: Option<String>,
}

fn default_kind() -> String {
    "terminal".to_owned()
}

/// The kinds the kernel registers.
pub const KINDS: [&str; 4] = ["terminal", "ask", "schedule", "helper"];

impl Registry {
    /// A one-shot the machine ran (not a terminal a person sits in).
    pub fn one_shot(&self) -> bool {
        self.kind != "terminal"
    }

    /// When the session started (ms), 0 when the file doesn't say.
    pub fn started_ms(&self) -> i64 {
        chrono::DateTime::parse_from_rfc3339(&self.started_at).map_or(0, |t| t.timestamp_millis())
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

/// A session id is what names files: only what Claude makes.
pub fn safe_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Where the registry is: `<state>/live/terminal`.
pub fn dir(state: &Path) -> PathBuf {
    state.join("live").join("terminal")
}

/// One registry file's content, when it is a v1 file for a listable session.
pub fn parse(raw: &str) -> Option<Registry> {
    let v: Value = serde_json::from_str(raw).ok()?;
    // Sessions of invited people are never listed.
    let person = v.as_object()?.iter().any(|(k, x)| k.to_ascii_lowercase().contains("person") && !x.is_null());
    if person {
        return None;
    }
    let reg: Registry = serde_json::from_value(v).ok()?;
    let sane = reg.v == 1
        && safe_id(&reg.session_id)
        && !reg.agent.is_empty()
        && Path::new(&reg.cwd).is_absolute()
        && reg.transcript().is_absolute()
        && reg.transcript().file_name().is_some_and(|n| n.to_string_lossy() == format!("{}.jsonl", reg.session_id))
        && KINDS.contains(&reg.kind.as_str())
        && reg.pid > 1
        && reg.pid != std::process::id();
    sane.then_some(reg)
}

/// The registry file of one session, read now (not the host's copy).
pub fn read_one(state: &Path, sid: &str) -> Option<Registry> {
    let path = dir(state).join(format!("{sid}.json"));
    if std::fs::metadata(&path).ok()?.len() > 64 * 1024 {
        return None;
    }
    parse(&std::fs::read_to_string(path).ok()?)
}

/// Every readable registry file in `state`.
pub fn read_all(state: &Path) -> Vec<Registry> {
    let Ok(read) = std::fs::read_dir(dir(state)) else { return Vec::new() };
    let mut all: Vec<Registry> = read
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter_map(|p| {
            let size = std::fs::metadata(&p).ok()?.len();
            if size > 64 * 1024 {
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

/// Where Claude keeps `sessions/<pid>.json` for the sessions of this Mac's default login.
pub fn default_sessions_dir() -> Option<PathBuf> {
    let base = std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".claude")))?;
    Some(base.join("sessions"))
}

/// Live = the pid runs and its session file names this session (a reused pid names another). Busy = live and that
/// file says it is doing something: anything but `idle`, so an unknown state never lets a take-over kill work.
pub fn liveness(reg: &Registry, sessions: Option<&Path>) -> Live {
    if !pid_runs(reg.pid) {
        return Live::Closed;
    }
    let candidates = [sessions.map(Path::to_path_buf), reg.config_dir().map(|c| c.join("sessions"))];
    for dir in candidates.into_iter().flatten() {
        let Ok(raw) = std::fs::read_to_string(dir.join(format!("{}.json", reg.pid))) else { continue };
        let Ok(file) = serde_json::from_str::<Value>(&raw) else { continue };
        if file["sessionId"] != reg.session_id.as_str() || !started_as(reg.pid, file["procStart"].as_str()) {
            continue;
        }
        return if file["status"] == "idle" { Live::Idle } else { Live::Busy };
    }
    // A `claude -p` run may keep no session file: while its process runs (the same one that started) it is working.
    // It ends with the process, so a crashed run never stays busy.
    if reg.one_shot() && reg.proc_start.is_some() && started_as(reg.pid, reg.proc_start.as_deref()) {
        return Live::Busy;
    }
    Live::Closed
}

/// Whether the process `pid` started when the session file says (`procStart`, as `ps -o lstart` prints it). A pid
/// reused after a crash starts at another time. When either side can't be read the file is believed. Claude Code
/// writes its own session file's start in UTC, the kernel its files' in the Mac's local time: either one matches.
fn started_as(pid: u32, claimed: Option<&str>) -> bool {
    let Some(claimed) = claimed else { return true };
    let claimed = normalized(claimed);
    match proc_start(pid) {
        Some(actual) if actual == claimed => true,
        Some(_) => lstart(pid, &[("TZ", "UTC"), ("LC_ALL", "C")]).is_some_and(|utc| utc == claimed),
        None => true,
    }
}

fn normalized(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The start time of `pid` as `ps` prints it.
pub fn proc_start(pid: u32) -> Option<String> {
    lstart(pid, &[])
}

/// The start time of `pid` as `ps` prints it with `env` set (a time zone, a locale).
fn lstart(pid: u32, env: &[(&str, &str)]) -> Option<String> {
    let out = std::process::Command::new("ps")
        .envs(env.iter().copied())
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    let text = normalized(&text);
    (!text.is_empty()).then_some(text)
}

/// Whether the process `pid` exists (one we may not signal still does).
#[cfg(unix)]
pub fn pid_runs(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else { return false };
    // SAFETY: signal 0 only checks that the process exists; nothing is sent.
    unsafe { libc::kill(pid, 0) == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) }
}

#[cfg(not(unix))]
pub fn pid_runs(_pid: u32) -> bool {
    false
}

/// Ends the process `pid` politely.
#[cfg(unix)]
pub fn terminate(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else { return false };
    // SAFETY: the caller checked that `pid` is the session's claude process (its session file names the session).
    unsafe { libc::kill(pid, libc::SIGTERM) == 0 }
}

#[cfg(not(unix))]
pub fn terminate(_pid: u32) -> bool {
    false
}
