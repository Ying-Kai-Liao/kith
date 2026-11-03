//! `pick` and `latest`: naming a session for `kith AGENT -r` and `kith AGENT -c`. They read the cost ledger's runs (every
//! kith run is tagged in it) and the transcripts on disk, take no claim, and only name a session: `resolve` still claims and
//! checks it. A port of `recent`, `first_text`, `pick` and `latest` in `kernel/bin/kith-sessions`.

use std::collections::HashMap;
use std::io::BufRead as _;
use std::path::{Path, PathBuf};

use rusqlite::OpenFlags;
use serde_json::Value;

use super::registry::{cwd_in, transcript_roots};
use super::{Ctx, clock, os};

/// One past session of an agent.
#[derive(Debug, PartialEq, Eq)]
pub struct Row {
    pub session: String,
    pub started: Option<String>,
    pub via: Option<String>,
    pub project: Option<String>,
    pub text: String,
}

/// Python's `str.split()` also splits on the separators `\x1c`-`\x1f`.
fn one_line(t: &str) -> String {
    t.split(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// The first thing the person typed in a transcript, on one line and cut to `width` characters; empty when there is none.
pub fn first_text(transcript: &Path, width: usize) -> String {
    let Ok(f) = std::fs::File::open(transcript) else {
        return String::new();
    };
    for line in std::io::BufReader::new(f).split(b'\n').take(400).flatten() {
        let Ok(Value::Object(d)) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        if d.get("type").and_then(Value::as_str) != Some("user")
            || truthy(d.get("isMeta"))
            || truthy(d.get("isSidechain"))
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
            let t = one_line(&t);
            // Command echoes, hook output and reminders come as tags.
            if !t.is_empty() && !t.starts_with('<') {
                return if t.chars().count() <= width {
                    t
                } else {
                    format!("{}\u{2026}", t.chars().take(width - 1).collect::<String>())
                };
            }
        }
    }
    String::new()
}

/// `os.path.realpath`: symlinks resolved as far as the path exists.
fn realpath(p: &str) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| {
        let path = PathBuf::from(p);
        if path.is_absolute() {
            path
        } else {
            std::env::current_dir().map_or(path.clone(), |d| d.join(path))
        }
    })
}

/// Every transcript on this Mac by session id (`<root>/projects/<p>/<sid>.jsonl`); the first found for an id wins.
fn on_disk(c: &Ctx) -> HashMap<String, PathBuf> {
    let mut found = HashMap::new();
    for root in transcript_roots(c) {
        let mut projects: Vec<PathBuf> = std::fs::read_dir(root.join("projects"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        projects.sort();
        for p in projects {
            let mut files: Vec<PathBuf> = std::fs::read_dir(&p)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
                .collect();
            files.sort();
            for f in files {
                if let Some(stem) = f
                    .file_name()
                    .and_then(|n| n.to_str())
                    .and_then(|n| n.strip_suffix(".jsonl"))
                {
                    found.entry(stem.to_owned()).or_insert_with(|| f.clone());
                }
            }
        }
    }
    found
}

/// The owner's past Claude sessions of `agent` that still have a transcript, newest first (only those begun in `where_`
/// when given). From the cost ledger's runs.
pub fn recent(
    c: &Ctx,
    agent: &str,
    limit: usize,
    where_: Option<&str>,
) -> Result<Vec<Row>, String> {
    let Some(db) = c.machine.as_ref().map(|m| m.join("cost.db")) else {
        return Ok(Vec::new());
    };
    if !db.is_file() {
        return Ok(Vec::new());
    }
    let conn = rusqlite::Connection::open_with_flags(
        &db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|e| e.to_string())?;
    // 'codex:...' is not Claude's.
    let mut stmt = conn
        .prepare(
            "select session, started, via, project from runs where agent = ? and coalesce(person, '') = '' \
             and session not like '%:%' order by started desc",
        )
        .map_err(|e| e.to_string())?;
    // `Row` without its text yet.
    let rows: Vec<Row> = stmt
        .query_map([agent], |r| {
            Ok(Row {
                session: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                started: r.get(1)?,
                via: r.get(2)?,
                project: r.get(3)?,
                text: String::new(),
            })
        })
        .map_err(|e| e.to_string())?
        .flatten()
        .collect();
    let disk = on_disk(c);
    let here = where_.filter(|w| !w.is_empty()).map(realpath);
    let mut out = Vec::new();
    for Row {
        session: sid,
        started,
        via,
        project,
        ..
    } in rows
    {
        let Some(t) = disk.get(&sid) else { continue };
        if let Some(here) = &here {
            let cwd = cwd_in(t);
            if realpath(if cwd.is_empty() { "/nowhere" } else { &cwd }) != *here {
                continue;
            }
        }
        out.push(Row {
            text: first_text(t, 70),
            session: sid,
            started,
            via,
            project,
        });
        if out.len() == limit {
            break;
        }
    }
    Ok(out)
}

fn pad(s: &str, width: usize) -> String {
    format!("{s}{}", " ".repeat(width.saturating_sub(s.chars().count())))
}

/// `%m-%d %H:%M` of a ledger time (read as UTC) in the Mac's zone; `?` padded when it isn't a time.
fn when(started: Option<&str>) -> String {
    #[allow(clippy::cast_possible_truncation)]
    started
        .and_then(clock::parse)
        .map(|s| os::local_month_day_time(s.clock.floor() as i64))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| pad("?", 11))
}

/// What `pick` ends with when it does not name a session: `None` is a silent exit 1 (the person cancelled).
pub type Cancelled = Option<String>;

/// For `kith AGENT -r`: lists the agent's recent sessions with `err`, reads a number with `line`, names that session.
pub fn pick(
    c: &Ctx,
    agent: &str,
    err: &dyn Fn(&str),
    line: &dyn Fn() -> String,
) -> Result<String, Cancelled> {
    let rows = recent(c, agent, 15, None).map_err(|e| Some(format!("kith-sessions: {e}")))?;
    if rows.is_empty() {
        return Err(Some(format!(
            "kith: no {agent} sessions to resume (none in the cost ledger with a transcript on this Mac)"
        )));
    }
    for (i, r) in rows.iter().enumerate() {
        err(&format!(
            "{:>3}  {}  {} {} {}\n",
            i + 1,
            when(r.started.as_deref()),
            pad(r.via.as_deref().filter(|v| !v.is_empty()).unwrap_or("?"), 9),
            pad(
                r.project
                    .as_deref()
                    .filter(|v| !v.is_empty())
                    .unwrap_or("-"),
                12
            ),
            r.text
        ));
    }
    err("resume which? (number, Enter to cancel) ");
    let answer = line();
    let a = answer.trim();
    if a.is_empty() {
        return Err(None);
    }
    match a.parse::<usize>() {
        Ok(n) if a.bytes().all(|b| b.is_ascii_digit()) && (1..=rows.len()).contains(&n) => {
            Ok(rows[n - 1].session.clone())
        }
        _ => Err(Some(format!("kith: no session {a} in the list"))),
    }
}

/// For `kith AGENT -c`: the id of the agent's newest session begun in `dir`.
pub fn latest(c: &Ctx, agent: &str, dir: &str) -> Result<String, String> {
    let rows = recent(c, agent, 1, Some(dir)).map_err(|e| format!("kith-sessions: {e}"))?;
    rows.into_iter().next().map(|r| r.session).ok_or_else(|| {
        format!(
            "kith: no {agent} session started in {dir} to continue (kith {agent} -r lists them all)"
        )
    })
}
