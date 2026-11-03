//! The session registry: `<state>/live/terminal/<sid>.json`, one per listable `kith <agent>` session. A port of
//! `record` (the `SessionStart` hook) and its helpers in `kernel/bin/kith-sessions`, and of the transcript lookups
//! `resolve` needs. The host reads these files with its own parser (`terminal/registry.rs`); the writer is this module
//! or the Python, never both for one run.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::{Ctx, iso_utc, os, write_json};

/// The one-shot kinds `bin/kith` labels a run with (`KITH_RUN_KIND`).
pub const ONE_SHOTS: [&str; 3] = ["ask", "schedule", "helper"];

fn truthy(s: Option<String>) -> Option<String> {
    s.filter(|v| !v.is_empty())
}

/// What kind of listable run this is, else `None`: `terminal` (interactive `kith <agent>`), or a one-shot of the owner's
/// that `bin/kith` labelled for this run. The host's own runs and anyone else's (`KITH_PERSON`) are never listed.
pub fn run_kind(c: &Ctx) -> Option<String> {
    let env = |k: &str| (c.env)(k);
    if truthy(env("KITH_PERSON")).is_some()
        || env("KITH_HIDE_SESSION").as_deref() == Some("1")
        || truthy(env("KITH_AGENT")).is_none()
    {
        return None;
    }
    if env("KITH_TERMINAL_SESSION").as_deref() == Some("1") {
        return (env("KITH_VIA").unwrap_or_else(|| "terminal".to_owned()) == "terminal")
            .then(|| "terminal".to_owned());
    }
    env("KITH_RUN_KIND").filter(|k| ONE_SHOTS.contains(&k.as_str()))
}

/// How the room says who started a one-shot; `None` for a terminal.
pub fn run_label(c: &Ctx, kind: &str) -> Option<String> {
    let who: String = (c.env)("KITH_RUN_LABEL")
        .unwrap_or_default()
        .trim()
        .chars()
        .take(60)
        .collect();
    match kind {
        "ask" => Some(if who.is_empty() {
            "asked from a terminal".to_owned()
        } else {
            format!("asked by {who}")
        }),
        "schedule" => Some(if who.is_empty() {
            "scheduled".to_owned()
        } else {
            format!("scheduled: {who}")
        }),
        "helper" => Some(if who.is_empty() {
            "helper".to_owned()
        } else {
            format!("helper of {who}")
        }),
        _ => None,
    }
}

/// A run in an agent worktree is not listed.
pub fn listable(c: &Ctx, cwd: &str) -> bool {
    run_kind(c).is_some() && !format!("{cwd}/").contains("/.claude/worktrees/")
}

/// The claude process above this hook: the ancestor whose `<claude dir>/sessions/<pid>.json` names our session, else the
/// first one called claude.
fn claude_pid(c: &Ctx, sid: &str) -> Option<i64> {
    let (mut pid, mut named) = (os::parent_pid(), 0);
    for _ in 0..12 {
        if pid <= 1 {
            break;
        }
        let file = c.claude_dir().join("sessions").join(format!("{pid}.json"));
        let session_file = std::fs::read_to_string(file)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok());
        if session_file
            .as_ref()
            .and_then(|v| v.get("sessionId"))
            .and_then(Value::as_str)
            == Some(sid)
        {
            return Some(pid);
        }
        let (up, comm) = os::ps_row(pid);
        if named == 0 && Path::new(&comm).file_name().is_some_and(|n| n == "claude") {
            named = pid;
        }
        pid = up;
    }
    (named != 0).then_some(named)
}

/// `record`: the `SessionStart` hook (stdin: the hook's JSON). Writes the registry file for a listable session, nothing
/// otherwise. Never fails the session: an error is returned for the caller to print, and the exit code stays 0.
pub fn record(c: &Ctx, stdin: &str) -> Result<(), String> {
    let hook: Value = serde_json::from_str(stdin).map_err(|e| e.to_string())?;
    let text = |k: &str| {
        hook.get(k)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let (sid, d) = (text("session_id"), c.reg_dir());
    let cwd = text("cwd").unwrap_or_else(|| {
        std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    let (Some(sid), Some(d)) = (sid, d) else {
        return Ok(());
    };
    if !listable(c, &cwd) {
        return Ok(());
    }
    let path = d.join(format!("{sid}.json"));
    let mut started = iso_utc();
    if text("source").as_deref() == Some("compact")
        && let Some(old) = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        && let Some(at) = old.get("startedAt")
    {
        started = at.as_str().map_or_else(|| at.to_string(), str::to_owned);
    }
    let kind = run_kind(c);
    let pid = claude_pid(c, &sid);
    let env = |k: &str| truthy((c.env)(k));
    let mut entry = Map::new();
    let mut put = |k: &str, v: Value| {
        entry.insert(k.to_owned(), v);
    };
    let opt = |v: Option<String>| v.map_or(Value::Null, Value::from);
    put("v", json!(1));
    put("sessionId", json!(sid));
    put("agent", opt(env("KITH_AGENT")));
    put("cwd", json!(cwd));
    put(
        "transcriptPath",
        hook.get("transcript_path").cloned().unwrap_or(Value::Null),
    );
    put("pid", pid.map_or(Value::Null, Value::from));
    put("project", opt(env("KITH_PROJECT")));
    put(
        "source",
        json!(text("source").unwrap_or_else(|| "startup".to_owned())),
    );
    put("resumedFrom", opt(env("KITH_RESUMED_FROM")));
    put("startedAt", json!(started));
    put("account", opt(env("KITH_ACCOUNT_ID")));
    put("kind", opt(kind.clone()));
    put("label", opt(kind.as_deref().and_then(|k| run_label(c, k))));
    put(
        "procStart",
        opt(pid
            .filter(|_| kind.as_deref() != Some("terminal"))
            .and_then(os::proc_start)),
    );
    write_json(&path, &Value::Object(entry), true).map_err(|e| e.to_string())
}

/// A registry file's content, `{}` when it is missing or damaged.
pub fn entry(c: &Ctx, sid: &str) -> Map<String, Value> {
    let Some(d) = c.reg_dir() else {
        return Map::new();
    };
    match std::fs::read_to_string(d.join(format!("{sid}.json")))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
    {
        Some(Value::Object(o)) => o,
        _ => Map::new(),
    }
}

/// The pid of a registry entry whose claude process still runs and names the same session in its own sessions folder
/// (the login the session was made under keeps `<config dir>/sessions/<pid>.json`). A process we may not signal reads as
/// gone here, as in `kith-sessions`.
pub fn live_pid(c: &Ctx, entry: &Map<String, Value>) -> Option<i64> {
    let pid = entry.get("pid").and_then(Value::as_i64)?;
    if !os::pid_signalable(pid) {
        return None;
    }
    let root = match entry
        .get("transcriptPath")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        Some(t) => Path::new(t).ancestors().nth(3)?.to_path_buf(),
        None => c.claude_dir(),
    };
    let said: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("sessions").join(format!("{pid}.json"))).ok()?,
    )
    .ok()?;
    (said.get("sessionId") == entry.get("sessionId")).then_some(pid)
}

/// The places Claude keeps transcripts (`<root>/projects/<p>/<sid>.jsonl`): this run's login, the default one, and each
/// extra account's (`kith account add claude <name>`), so a session made under another main account is still found.
fn transcript_roots(c: &Ctx) -> Vec<PathBuf> {
    let mut roots = vec![c.claude_dir(), c.home.join(".claude")];
    if let Some(m) = &c.machine {
        let mut extra: Vec<PathBuf> = std::fs::read_dir(m.join("accounts").join("claude"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .collect();
        extra.sort();
        roots.extend(extra);
    }
    let mut seen = Vec::new();
    roots.retain(|r| {
        let new = !seen.contains(r);
        seen.push(r.clone());
        new
    });
    roots
}

/// The folder a transcript's session ran in; empty when it names none.
fn cwd_in(transcript: &Path) -> String {
    use std::io::BufRead as _;
    let Ok(f) = std::fs::File::open(transcript) else {
        return String::new();
    };
    for line in std::io::BufReader::new(f).split(b'\n').take(60).flatten() {
        if let Ok(Value::Object(o)) = serde_json::from_slice::<Value>(&line)
            && let Some(Value::String(cwd)) = o.get("cwd")
            && !cwd.is_empty()
        {
            return cwd.clone();
        }
    }
    String::new()
}

/// The folder a Claude session ran in, from its transcript; `None` when there is no such session, empty when the
/// transcript names no folder. `sid` is a safe session id (the glob of `kith-sessions` took any text).
pub fn transcript_cwd(c: &Ctx, sid: &str) -> Option<String> {
    if !super::safe_id(sid) {
        return None;
    }
    for root in transcript_roots(c) {
        let mut projects: Vec<PathBuf> = std::fs::read_dir(root.join("projects"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .collect();
        projects.sort();
        for p in projects {
            let t = p.join(format!("{sid}.jsonl"));
            if t.exists() {
                return Some(cwd_in(&t));
            }
        }
    }
    None
}
