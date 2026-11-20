//! `pick` and `latest`: name a session for `kith AGENT -r` and `-c` (`recent`, `pick`, `latest` in `kernel/bin/kith-sessions`).
//! They only name one: `resolve` still claims and checks it. The sessions come from the cost ledger's runs, which every kith
//! run is tagged in, that still have a transcript on this Mac.

use std::path::{Path, PathBuf};

use std::fmt::Write as _;

use serde_json::Value;

use super::Ctx;
use super::registry::{cwd_in, transcript_roots};

/// One past session: id, when it started (the ledger's text), how it was started, its project, what was first typed.
#[derive(Debug, PartialEq, Eq)]
pub struct Row {
    pub sid: String,
    pub started: Option<String>,
    pub via: Option<String>,
    pub project: Option<String>,
    pub text: String,
}

/// The first thing the person typed in a transcript, on one line and cut to `width`; empty when there is none.
pub fn first_text(transcript: &Path, width: usize) -> String {
    use std::io::BufRead as _;
    let Ok(f) = std::fs::File::open(transcript) else {
        return String::new();
    };
    for line in std::io::BufReader::new(f).split(b'\n').take(400) {
        let Ok(line) = line else { break };
        let Ok(d) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        let flag = |k: &str| d.get(k).is_some_and(super::resolve::truthy);
        if d.get("type").and_then(Value::as_str) != Some("user")
            || flag("isMeta")
            || flag("isSidechain")
        {
            continue;
        }
        let texts: Vec<String> = match d.get("message").and_then(|m| m.get("content")) {
            Some(Value::String(s)) => vec![s.clone()],
            Some(Value::Array(blocks)) => blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .map(|b| {
                    b.get("text")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned()
                })
                .collect(),
            _ => Vec::new(),
        };
        for t in texts {
            let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
            // Command echoes, hook output and reminders come as tags.
            if !t.is_empty() && !t.starts_with('<') {
                return if t.chars().count() <= width {
                    t
                } else {
                    let mut cut: String = t.chars().take(width - 1).collect();
                    cut.push('…');
                    cut
                };
            }
        }
    }
    String::new()
}

type Ledger = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// The transcripts of every Claude login by file stem, the first of a name winning (`*/*.jsonl` under each `projects`).
fn on_disk(c: &Ctx) -> std::collections::HashMap<String, PathBuf> {
    let mut found = std::collections::HashMap::new();
    for root in transcript_roots(c) {
        let mut projects: Vec<PathBuf> = std::fs::read_dir(root.join("projects"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .collect();
        projects.sort();
        for p in projects {
            let mut files: Vec<PathBuf> = std::fs::read_dir(&p)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
                .filter(|f| f.extension().is_some_and(|x| x == "jsonl"))
                .collect();
            files.sort();
            for f in files {
                if let Some(stem) = f.file_stem().and_then(|s| s.to_str()) {
                    found.entry(stem.to_owned()).or_insert(f);
                }
            }
        }
    }
    found
}

fn real(p: &str) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| PathBuf::from(p))
}

/// The owner's past Claude sessions of `agent` that still have a transcript, newest first (only those begun in `here` when
/// given). A ledger that cannot be read is an error, as the Python's traceback.
pub fn recent(c: &Ctx, agent: &str, limit: usize, here: Option<&str>) -> Result<Vec<Row>, String> {
    let Some(machine) = c.machine.as_deref() else {
        return Ok(Vec::new());
    };
    let db = machine.join("cost.db");
    if !db.is_file() {
        return Ok(Vec::new());
    }
    let conn = rusqlite::Connection::open_with_flags(
        &db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|e| e.to_string())?;
    // 'codex:…' is not Claude's.
    let mut stmt = conn
        .prepare(
            "select session, started, via, project from runs where agent = ?1 and coalesce(person, '') = '' \
             and session not like '%:%' order by started desc",
        )
        .map_err(|e| e.to_string())?;
    let text = |r: &rusqlite::Row, i: usize| -> rusqlite::Result<Option<String>> {
        Ok(match r.get_ref(i)? {
            rusqlite::types::ValueRef::Null => None,
            rusqlite::types::ValueRef::Text(t) => Some(String::from_utf8_lossy(t).into_owned()),
            other => Some(match other {
                rusqlite::types::ValueRef::Integer(n) => n.to_string(),
                rusqlite::types::ValueRef::Real(f) => f.to_string(),
                _ => String::new(),
            }),
        })
    };
    let rows: Vec<Ledger> = stmt
        .query_map([agent], |r| {
            Ok((text(r, 0)?, text(r, 1)?, text(r, 2)?, text(r, 3)?))
        })
        .map_err(|e| e.to_string())?
        .collect::<rusqlite::Result<_>>()
        .map_err(|e| e.to_string())?;
    let found = on_disk(c);
    let here = here.filter(|h| !h.is_empty()).map(real);
    let mut out = Vec::new();
    for (sid, started, via, project) in rows {
        let Some(sid) = sid else { continue };
        let Some(t) = found.get(&sid) else { continue };
        if let Some(h) = &here {
            let cwd = cwd_in(t);
            let cwd = if cwd.is_empty() { "/nowhere" } else { &cwd };
            if &real(cwd) != h {
                continue;
            }
        }
        let text = first_text(t, 70);
        out.push(Row {
            sid,
            started,
            via,
            project,
            text,
        });
        if out.len() == limit {
            break;
        }
    }
    Ok(out)
}

/// `%m-%d %H:%M` in local time of a ledger time read as UTC, or `?` padded to 11 for one that is not a date
/// (`fromisoformat(...).replace(tzinfo=utc).astimezone()`).
fn when(started: Option<&str>) -> String {
    use chrono::{Local, NaiveDate, NaiveDateTime};
    let unknown = || format!("{:<11}", "?");
    let Some(raw) = started else { return unknown() };
    let mut s = raw.trim().replace(' ', "T");
    // The offset or `Z` is dropped: the ledger's times are UTC whatever they say.
    if let Some(i) = s.rfind(['+', 'Z']).filter(|i| *i > 10) {
        s.truncate(i);
    } else if let Some(i) = s.rfind('-').filter(|i| *i > 10) {
        s.truncate(i);
    }
    let naive = [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%dT%H",
    ]
    .iter()
    .find_map(|f| NaiveDateTime::parse_from_str(&s, f).ok())
    .or_else(|| {
        NaiveDate::parse_from_str(&s, "%Y-%m-%d")
            .ok()
            .and_then(|d| d.and_hms_opt(0, 0, 0))
    });
    naive.map_or_else(unknown, |n| {
        n.and_utc()
            .with_timezone(&Local)
            .format("%m-%d %H:%M")
            .to_string()
    })
}

/// What `pick` prints on stderr before it reads the number: one numbered line per row, then the prompt.
pub fn table(rows: &[Row]) -> String {
    let mut out = String::new();
    for (i, r) in rows.iter().enumerate() {
        let _ = writeln!(
            out,
            "{:>3}  {}  {:<9} {:<12} {}",
            i + 1,
            when(r.started.as_deref()),
            r.via.as_deref().filter(|v| !v.is_empty()).unwrap_or("?"),
            r.project
                .as_deref()
                .filter(|v| !v.is_empty())
                .unwrap_or("-"),
            r.text
        );
    }
    out.push_str("resume which? (number, Enter to cancel) ");
    out
}
