//! What the wrapper of an interactive run needs from the session lifecycle (F12), the half of `kernel/lib/kith_lifecycle.py`
//! that `kith-sessions run` calls: the idle clock, the work still outstanding in a transcript (rule 7), and the two
//! settings that time it. Every judgment is structural (sizes, ids, timestamps); nothing calls a model. The decision half
//! (`decide`, the 54-minute sweep) stays in `kith-lifecycle`, which the wrapper starts as a program.

use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::{clock, os};
use crate::paths::Env;

/// A human session idle this long with nothing outstanding is stopped by its wrapper (rule 3).
const DEFAULT_SLEEP_AFTER: f64 = 3600.0;
const DEFAULT_CACHE_LIFE: i64 = 3600;
/// A cache this short has no timer: the decision to compact is made when the wrapper puts the session to sleep.
pub const SHORT_CACHE: i64 = 300;
const TAIL_BYTES: u64 = 4_000_000;

/// Seconds the prompt cache lives (`KITH_CACHE_LIFE`, e.g. 300 for an API key); default one hour.
pub fn cache_life(env: Env) -> i64 {
    env("KITH_CACHE_LIFE").map_or(DEFAULT_CACHE_LIFE, |v| {
        v.trim()
            .parse::<i64>()
            .map_or(DEFAULT_CACHE_LIFE, |n| n.max(0))
    })
}

/// Seconds of idle after which a wrapper stops its own child (`KITH_SLEEP_MINUTES`, default 60).
pub fn sleep_after(env: Env) -> f64 {
    env("KITH_SLEEP_MINUTES")
        .and_then(|v| v.trim().parse::<f64>().ok())
        .map_or(DEFAULT_SLEEP_AFTER, |m| (m * 60.0).max(1.0))
}

/// `str.splitlines`: Python splits on more than `\n`.
fn split_lines(text: &str) -> Vec<&str> {
    const BREAKS: [char; 8] = [
        '\n', '\r', '\u{b}', '\u{c}', '\u{1c}', '\u{1d}', '\u{1e}', '\u{85}',
    ];
    let mut out = Vec::new();
    let (mut start, mut chars) = (0, text.char_indices().peekable());
    while let Some((i, c)) = chars.next() {
        if BREAKS.contains(&c) || c == '\u{2028}' || c == '\u{2029}' {
            out.push(&text[start..i]);
            start = i + c.len_utf8();
            if c == '\r'
                && let Some((j, '\n')) = chars.peek().copied()
            {
                chars.next();
                start = j + 1;
            }
        }
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

/// The entries of the last four megabytes of a transcript (the first line of a cut file may be cut), objects only.
fn tail_lines(transcript: &Path) -> Vec<Map<String, Value>> {
    let Ok(mut f) = std::fs::File::open(transcript) else {
        return Vec::new();
    };
    let Ok(size) = f.seek(SeekFrom::End(0)) else {
        return Vec::new();
    };
    let mut raw = Vec::new();
    if f.seek(SeekFrom::Start(size.saturating_sub(TAIL_BYTES)))
        .is_err()
        || f.read_to_end(&mut raw).is_err()
    {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&raw);
    let mut lines = split_lines(&text);
    if size > TAIL_BYTES && !lines.is_empty() {
        lines.remove(0);
    }
    lines
        .into_iter()
        .filter_map(|l| match serde_json::from_str::<Value>(l) {
            Ok(Value::Object(o)) => Some(o),
            _ => None,
        })
        .collect()
}

fn is_true(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// The text of a message: a string, or the text blocks of a list joined by newlines.
fn message_text(msg: Option<&Value>) -> String {
    match msg.and_then(|m| m.get("content")) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .map(|b| b.get("text").and_then(Value::as_str).unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// A user or model turn, not what a compaction or a command writes: the summary, its boundary, command echoes, hook output.
/// A compaction's own write must not restart the idle clock (rule 4).
fn is_turn(e: &Map<String, Value>) -> bool {
    let kind = e.get("type").and_then(Value::as_str);
    if !matches!(kind, Some("user" | "assistant"))
        || is_true(e.get("isSidechain"))
        || is_true(e.get("isMeta"))
        || is_true(e.get("isCompactSummary"))
    {
        return false;
    }
    if kind == Some("user") {
        let t = message_text(e.get("message"));
        let t = t.trim_start();
        if ["<command-name>", "<local-command", "<system-reminder>"]
            .iter()
            .any(|p| t.starts_with(p))
        {
            return false;
        }
    }
    true
}

/// Epoch seconds an entry's `timestamp` names.
fn when(e: &Map<String, Value>) -> Option<f64> {
    let text = match e.get("timestamp") {
        None => "None".to_owned(),
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
    };
    clock::parse(&text).map(clock::Stamp::epoch)
}

/// Epoch seconds of the last user or model turn (from the entries' own timestamps), else `None`.
pub fn last_turn_at(transcript: &Path) -> Option<f64> {
    tail_lines(transcript)
        .iter()
        .rev()
        .find(|e| is_turn(e) && when(e).is_some())
        .and_then(when)
}

fn word_after(text: &str, prefix: &str, suffix: &str) -> Vec<String> {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find(prefix) {
        rest = &rest[i + prefix.len()..];
        let n: usize = rest
            .chars()
            .take_while(|c| word(*c))
            .map(char::len_utf8)
            .sum();
        if n > 0 && rest[n..].starts_with(suffix) {
            out.push(rest[..n].to_owned());
        }
    }
    out
}

/// What the transcript shows still running: tool calls with no result yet (a running tool, a running subagent), and
/// background shells started with no `<task-notification>` since (rule 7). Short reasons.
pub fn pending_work(transcript: &Path) -> Vec<String> {
    let mut started: Vec<(String, String)> = Vec::new();
    let (mut results, mut shells, mut notified) = (
        std::collections::HashSet::new(),
        std::collections::BTreeSet::new(),
        std::collections::HashSet::new(),
    );
    for e in tail_lines(transcript) {
        let blocks = match e.get("message").and_then(|m| m.get("content")) {
            Some(Value::Array(a)) => a.iter().filter(|b| b.is_object()).collect(),
            _ => Vec::new(),
        };
        for b in blocks {
            match b.get("type").and_then(Value::as_str) {
                Some("tool_use") if is_true(b.get("id")) => {
                    let id = b.get("id").map(Value::to_string).unwrap_or_default();
                    let name = match b.get("name") {
                        Some(Value::String(s)) if !s.is_empty() => s.clone(),
                        _ => "tool".to_owned(),
                    };
                    match started.iter_mut().find(|(i, _)| *i == id) {
                        Some(slot) => slot.1 = name,
                        None => started.push((id, name)),
                    }
                }
                Some("tool_result") => {
                    results.insert(b.get("tool_use_id").map(Value::to_string));
                    let body = match b.get("content") {
                        Some(Value::String(s)) => s.clone(),
                        Some(Value::Array(a)) => a
                            .iter()
                            .filter(|x| x.is_object())
                            .map(|x| x.get("text").and_then(Value::as_str).unwrap_or(""))
                            .collect::<Vec<_>>()
                            .join(" "),
                        _ => String::new(),
                    };
                    shells.extend(word_after(&body, "running in background with ID: ", ""));
                }
                _ => {}
            }
        }
        if e.get("type").and_then(Value::as_str) == Some("user") {
            let t = message_text(e.get("message"));
            if t.contains("<task-notification>") {
                notified.extend(word_after(&t, "<task-id>", "</task-id>"));
            }
        }
    }
    let mut out = Vec::new();
    for (id, name) in &started {
        if !results.contains(&Some(id.clone())) {
            out.push(if name == "Task" || name == "Agent" {
                "a subagent is running".to_owned()
            } else {
                format!("{name} has not returned")
            });
        }
    }
    out.extend(
        shells
            .iter()
            .filter(|s| !notified.contains(*s))
            .map(|s| format!("background shell {s} has not reported")),
    );
    out
}

/// Claude's MCP task sidecars of the session: `<projects>/<slug>/<sid>/mcp-tasks/*.meta.json` (deleted once delivered).
fn sidecars(transcript: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(transcript.with_extension("").join("mcp-tasks"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().ends_with(".meta.json"))
        })
        .collect()
}

/// Descendants of `pid` that live in another session than `pid`'s own: a dev server or build that outlives the shell that
/// started it. Unsure counts as busy (the same rule as the host's idle check): `[-1]`.
fn detached_children(pid: i64) -> Vec<i64> {
    let Some(own) = os::session_of(pid) else {
        return vec![-1];
    };
    let mut kids: std::collections::HashMap<i64, Vec<i64>> = std::collections::HashMap::new();
    for (p, pp) in os::ps_table() {
        kids.entry(pp).or_default().push(p);
    }
    let (mut found, mut stack) = (Vec::new(), vec![pid]);
    while let Some(top) = stack.pop() {
        for c in kids.get(&top).into_iter().flatten() {
            // Gone since the listing.
            let Some(s) = os::session_of(*c) else {
                continue;
            };
            if s != own {
                found.push(*c);
            }
            stack.push(*c);
        }
    }
    found
}

/// Why the session must not be signalled now (rule 7); empty when nothing is outstanding.
pub fn outstanding(transcript: Option<&Path>, pid: Option<i64>) -> Vec<String> {
    let mut why = transcript.map(pending_work).unwrap_or_default();
    if transcript.is_some_and(|t| !sidecars(t).is_empty()) {
        why.push("an MCP task is open".to_owned());
    }
    if pid.is_some_and(|p| p != 0 && !detached_children(p).is_empty()) {
        why.push("a child process runs detached".to_owned());
    }
    why
}

/// The latest of the last user or model turn, the process start and the last keystroke (else a terminal resumed after an
/// idle hour would be stopped before the person types). A compaction's own write is not a turn.
pub fn idle_since(transcript: Option<&Path>, started: f64, keystroke: Option<f64>) -> f64 {
    [transcript.and_then(last_turn_at), Some(started), keystroke]
        .into_iter()
        .flatten()
        .fold(f64::MIN, f64::max)
}
