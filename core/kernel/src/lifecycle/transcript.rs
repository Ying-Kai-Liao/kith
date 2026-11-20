//! What the lifecycle reads from a transcript and the 54-minute decision (F12 rule 5): the idle clock, outstanding work and
//! `decide`. A port of the transcript half of `kernel/lib/kith_lifecycle.py`; every judgment is structural (sizes, ids,
//! timestamps) and nothing calls a model.

use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

/// A transcript below this is not worth a `/compact` (rule 5; the wrapper sets Claude's own floor to it).
pub const FLOOR_TOKENS: i64 = 100_000;
/// A subscription's 1 h cache. An API key's is 5 min: no timer then.
pub const DEFAULT_CACHE_LIFE: i64 = 3600;
pub const SHORT_CACHE: i64 = 300;
/// Of the cache lifetime: the earliest a decision is made (rule 5).
pub const DECIDE_AT: f64 = 0.9;
const TAIL_BYTES: u64 = 4_000_000;

/// Seconds the prompt cache lives (`KITH_CACHE_LIFE`, e.g. 300 for an API key); default one hour.
pub fn cache_life(env: crate::paths::Env) -> i64 {
    env("KITH_CACHE_LIFE").map_or(DEFAULT_CACHE_LIFE, |v| {
        v.trim()
            .parse::<i64>()
            .map_or(DEFAULT_CACHE_LIFE, |n| n.max(0))
    })
}

/// Python's truthiness of a JSON value.
pub fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// The last objects of the file (the last 4 MB; a first line that may be cut is dropped), unreadable lines skipped.
pub fn tail_lines(transcript: &Path) -> Vec<Map<String, Value>> {
    let read = || -> std::io::Result<(Vec<u8>, u64)> {
        let mut f = std::fs::File::open(transcript)?;
        let size = f.seek(SeekFrom::End(0))?;
        f.seek(SeekFrom::Start(size.saturating_sub(TAIL_BYTES)))?;
        let mut raw = Vec::new();
        f.read_to_end(&mut raw)?;
        Ok((raw, size))
    };
    let Ok((raw, size)) = read() else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&raw);
    let mut lines: Vec<&str> = text.lines().collect();
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

fn message(e: &Map<String, Value>) -> Option<&Map<String, Value>> {
    e.get("message").and_then(Value::as_object)
}

/// The text of a message: a string content, or the text blocks joined by newlines.
fn text(msg: Option<&Map<String, Value>>) -> String {
    match msg.and_then(|m| m.get("content")) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(Value::as_object)
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .map(|b| b.get("text").and_then(Value::as_str).unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn kind(e: &Map<String, Value>) -> &str {
    e.get("type").and_then(Value::as_str).unwrap_or("")
}

/// A user or model turn, not what a compaction or a command writes: the summary, its boundary, command echoes, hook
/// output. A compaction's own write must not restart the idle clock (rule 4).
pub fn is_turn(e: &Map<String, Value>) -> bool {
    if !matches!(kind(e), "user" | "assistant")
        || truthy(e.get("isSidechain"))
        || truthy(e.get("isMeta"))
        || truthy(e.get("isCompactSummary"))
    {
        return false;
    }
    if kind(e) == "user" {
        let t = text(message(e));
        let t = t.trim_start();
        if t.starts_with("<command-name>")
            || t.starts_with("<local-command")
            || t.starts_with("<system-reminder>")
        {
            return false;
        }
    }
    true
}

/// Epoch seconds of an entry's own timestamp (`datetime.fromisoformat(...).timestamp()`; a stamp with no zone is local time).
pub fn when(e: &Map<String, Value>) -> Option<f64> {
    use chrono::TimeZone as _;
    let s = match e.get("timestamp")? {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let micros = if let Ok(d) = chrono::DateTime::parse_from_rfc3339(&s.replace(' ', "T")) {
        d.timestamp() * 1_000_000 + i64::from(d.timestamp_subsec_micros())
    } else {
        let naive = ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M", "%Y-%m-%d"]
            .iter()
            .find_map(|f| {
                chrono::NaiveDateTime::parse_from_str(&s, f)
                    .ok()
                    .or_else(|| {
                        chrono::NaiveDate::parse_from_str(&s, f)
                            .ok()
                            .and_then(|d| d.and_hms_opt(0, 0, 0))
                    })
            })?;
        let d = chrono::Local.from_local_datetime(&naive).earliest()?;
        d.timestamp() * 1_000_000 + i64::from(d.timestamp_subsec_micros())
    };
    #[allow(clippy::cast_precision_loss)]
    Some(micros as f64 / 1e6)
}

/// Epoch seconds of the last user or model turn, else `None`.
pub fn last_turn_at(transcript: &Path) -> Option<f64> {
    tail_lines(transcript)
        .iter()
        .rev()
        .find(|e| is_turn(e) && when(e).is_some())
        .and_then(when)
}

fn int_of(v: Option<&Value>) -> i64 {
    match v {
        Some(Value::Number(n)) => n
            .as_i64()
            // int() truncates a float
            .or_else(|| {
                #[allow(clippy::cast_possible_truncation)]
                n.as_f64().map(|f| f as i64)
            })
            .unwrap_or(0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0),
        Some(Value::Bool(b)) => i64::from(*b),
        _ => 0,
    }
}

/// What the last model request read: input + cached + new cache tokens of the last assistant turn that reports usage;
/// else a quarter of the file's bytes. After a compaction the next turn reports the smaller number.
pub fn context_tokens(transcript: &Path) -> i64 {
    for e in tail_lines(transcript).iter().rev() {
        let usage = if kind(e) == "assistant" {
            message(e).and_then(|m| m.get("usage"))
        } else {
            None
        };
        if truthy(usage)
            && let Some(u) = usage.and_then(Value::as_object)
        {
            return [
                "input_tokens",
                "cache_read_input_tokens",
                "cache_creation_input_tokens",
            ]
            .iter()
            .map(|k| int_of(u.get(*k)))
            .sum();
        }
    }
    std::fs::metadata(transcript).map_or(0, |m| i64::try_from(m.len() / 4).unwrap_or(i64::MAX))
}

/// Whether a compaction (Claude's own, at 54 min, or ours) wrote its boundary at or after epoch `t`.
pub fn compacted_since(transcript: &Path, t: f64) -> bool {
    for e in tail_lines(transcript).iter().rev() {
        if kind(e) == "system"
            && e.get("subtype").and_then(Value::as_str) == Some("compact_boundary")
        {
            return when(e).is_some_and(|w| w >= t - 1.0);
        }
        if is_turn(e) && when(e).unwrap_or(0.0) < t - 1.0 {
            return false;
        }
    }
    false
}

/// The last model turn ends by asking the person something (its text ends with a question mark).
pub fn asked_a_question(transcript: &Path) -> bool {
    for e in tail_lines(transcript).iter().rev() {
        if kind(e) == "assistant" && is_turn(e) {
            return text(message(e)).trim_end().ends_with(['?', '\u{ff1f}']);
        }
    }
    false
}

fn blocks(e: &Map<String, Value>) -> Vec<&Map<String, Value>> {
    match message(e).and_then(|m| m.get("content")) {
        Some(Value::Array(b)) => b.iter().filter_map(Value::as_object).collect(),
        _ => Vec::new(),
    }
}

/// The word characters (`\w`) that start `s`.
fn word(s: &str) -> &str {
    let end = s
        .char_indices()
        .find(|(_, c)| !(c.is_alphanumeric() || *c == '_'))
        .map_or(s.len(), |(i, _)| i);
    &s[..end]
}

/// Every `\w+` that follows `marker` in `text` (`re.findall`).
fn after_all<'a>(text: &'a str, marker: &str, out: &mut Vec<&'a str>) {
    let mut rest = text;
    while let Some(i) = rest.find(marker) {
        rest = &rest[i + marker.len()..];
        let w = word(rest);
        if !w.is_empty() {
            out.push(w);
        }
    }
}

/// What the transcript shows still running: tool calls with no result yet (a running tool, a running subagent), and
/// background shells started with no `<task-notification>` since (rule 7). A list of short reasons.
pub fn pending_work(transcript: &Path) -> Vec<String> {
    let mut started: Vec<(String, String)> = Vec::new();
    let mut results: Vec<String> = Vec::new();
    let mut shells: Vec<String> = Vec::new();
    let mut notified: Vec<String> = Vec::new();
    for e in tail_lines(transcript) {
        for b in blocks(&e) {
            match b.get("type").and_then(Value::as_str) {
                Some("tool_use") if truthy(b.get("id")) => {
                    let id = b.get("id").and_then(Value::as_str).unwrap_or("").to_owned();
                    let name = b
                        .get("name")
                        .and_then(Value::as_str)
                        .filter(|n| !n.is_empty())
                        .unwrap_or("tool")
                        .to_owned();
                    match started.iter_mut().find(|(i, _)| *i == id) {
                        Some(s) => s.1 = name,
                        None => started.push((id, name)),
                    }
                }
                Some("tool_result") => {
                    if let Some(id) = b.get("tool_use_id").and_then(Value::as_str) {
                        results.push(id.to_owned());
                    }
                    let body = match b.get("content") {
                        Some(Value::String(s)) => s.clone(),
                        Some(Value::Array(a)) => a
                            .iter()
                            .filter_map(Value::as_object)
                            .map(|x| x.get("text").and_then(Value::as_str).unwrap_or(""))
                            .collect::<Vec<_>>()
                            .join(" "),
                        _ => String::new(),
                    };
                    let mut found = Vec::new();
                    after_all(&body, "running in background with ID: ", &mut found);
                    shells.extend(found.into_iter().map(str::to_owned));
                }
                _ => {}
            }
        }
        let t = if kind(&e) == "user" {
            text(message(&e))
        } else {
            String::new()
        };
        if t.contains("<task-notification>") {
            let mut found = Vec::new();
            after_all(&t, "<task-id>", &mut found);
            // `<task-id>(\w+)</task-id>`: the word must be closed by the tag
            notified.extend(
                found
                    .into_iter()
                    .filter(|w| t.contains(&format!("<task-id>{w}</task-id>")))
                    .map(str::to_owned),
            );
        }
    }
    let mut out: Vec<String> = started
        .iter()
        .filter(|(id, _)| !results.contains(id))
        .map(|(_, name)| {
            if matches!(name.as_str(), "Task" | "Agent") {
                "a subagent is running".to_owned()
            } else {
                format!("{name} has not returned")
            }
        })
        .collect();
    let mut open: Vec<&String> = shells.iter().filter(|s| !notified.contains(s)).collect();
    open.sort();
    open.dedup();
    out.extend(
        open.into_iter()
            .map(|s| format!("background shell {s} has not reported")),
    );
    out
}

/// Claude's MCP task sidecars of the session: `<projects>/<slug>/<sid>/mcp-tasks/*.meta.json` (deleted once delivered).
pub fn sidecars(transcript: &Path) -> Vec<PathBuf> {
    let dir = transcript.with_extension("").join("mcp-tasks");
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().ends_with(".meta.json"))
        })
        .collect();
    found.sort();
    found
}

/// Why the session must not be signalled now (rule 7); empty when nothing is outstanding. (The detached-children check
/// of the Python needs a pid; `status` never gives one.)
pub fn outstanding(transcript: &Path) -> Vec<String> {
    let mut why = pending_work(transcript);
    if !sidecars(transcript).is_empty() {
        why.push("an MCP task is open".to_owned());
    }
    why
}

/// compact | sleep | end | `None` (too early or no timer), from structural signals only.
///
/// `kind`: the registry's run kind. `waiting`: a parked card, a waiter or a wake request is still open for the session.
/// The earliest is 0.9 of the cache lifetime after the last turn, and never on a 5-minute cache: there the caller asks at
/// the moment the session goes to sleep (`at` = idle since + life).
pub fn decide(
    transcript: &Path,
    kind: &str,
    waiting: bool,
    at: Option<f64>,
    life: i64,
    now: f64,
) -> Option<&'static str> {
    let since = last_turn_at(transcript)?;
    let t = at.unwrap_or(now);
    #[allow(clippy::cast_precision_loss)]
    if life <= SHORT_CACHE {
        at?;
    } else if t - since < DECIDE_AT * life as f64 {
        return None;
    }
    let question = asked_a_question(transcript);
    if matches!(kind, "ask" | "schedule" | "helper") && !question && !waiting {
        return Some("end");
    }
    let big = context_tokens(transcript) >= FLOOR_TOKENS;
    Some(if big && (waiting || question) {
        "compact"
    } else {
        "sleep"
    })
}
