//! Reading transcripts: the wake recovery question (`transcript_has` in `kernel/bin/kith-wake`) and the read half of
//! `kernel/bin/kith-sessions` (`recent`, `pick`, `latest`, and the part of `resolve` that names the session and its
//! folder). Writes nothing and takes no claim: the claim half of `resolve` is M3-2 / M3-11, and the question to a running
//! host is the host's. Messages are the lines `kith-sessions` dies with, after `kith: `.
//!
//! Reused from `kith_kernel::threads::registry` rather than ported again: the roots of every Claude login
//! (`transcript_roots`), `cwd_in`, and `transcript_cwd`.

use std::io::BufRead as _;
use std::path::{Path, PathBuf};

use kith_kernel::threads::registry::{cwd_in, transcript_cwd, transcript_roots};
use kith_kernel::threads::{Ctx, Resolved, safe_id};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

use super::rows;
use super::{SessionPtr, find_everywhere};
use crate::error::Result;

/// What the wake lane asks of the transcripts. The wake worker may define its own trait; the manager joins them.
pub(crate) trait TranscriptReader {
    /// Whether the conversation's transcript holds `[kith wake RUN]`: the turn for that run reached the session, so a
    /// wake whose deliverer died before saying so is recovered rather than re-sent.
    fn answered(&self, conversation: &str, run: &str) -> bool;
}

/// The reader over the files on this Mac: the conversation's recorded pointers, then every Claude login's folders by
/// session id, as `transcript_has` looks.
pub(crate) struct FsReader<'a> {
    pub conn: &'a Connection,
    pub ctx: &'a Ctx<'a>,
}

fn file_has(path: &Path, needle: &[u8]) -> bool {
    let Ok(f) = std::fs::File::open(path) else {
        return false;
    };
    std::io::BufReader::new(f)
        .split(b'\n')
        .map_while(std::result::Result::ok)
        .any(|line| line.windows(needle.len()).any(|w| w == needle))
}

impl TranscriptReader for FsReader<'_> {
    fn answered(&self, conversation: &str, run: &str) -> bool {
        let needle = format!("[kith wake {run}]");
        let mut ptrs: Vec<SessionPtr> = Vec::new();
        let mut sids: Vec<String> = Vec::new();
        for s in rows::list(self.conn, conversation).unwrap_or_default() {
            if s.engine != "claude" {
                continue;
            }
            ptrs.extend(SessionPtr::from_text(&s.pointer));
            sids.push(s.session_id);
        }
        if sids.is_empty() {
            // Not imported: the conversation id of a terminal session is its Claude session id.
            sids.push(conversation.to_owned());
        }
        for sid in sids.iter().filter(|s| safe_id(s)) {
            ptrs.extend(find_everywhere(self.ctx, sid));
        }
        ptrs.iter().any(|p| file_has(&p.jsonl(), needle.as_bytes()))
    }
}

type LedgerRow = (String, Option<String>, Option<String>, Option<String>);

/// One past session of an agent: `(session, started, via, project, first text)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Recent {
    pub session: String,
    pub started: Option<String>,
    pub via: Option<String>,
    pub project: Option<String>,
    pub text: String,
}

/// The first thing the person typed in a transcript, on one line and cut to `width`; empty when there is none.
pub(crate) fn first_text(transcript: &Path, width: usize) -> String {
    let Ok(f) = std::fs::File::open(transcript) else {
        return String::new();
    };
    for line in std::io::BufReader::new(f)
        .split(b'\n')
        .take(400)
        .map_while(std::result::Result::ok)
    {
        let Ok(Value::Object(d)) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        let flag = |k: &str| {
            d.get(k)
                .is_some_and(|v| !matches!(v, Value::Null | Value::Bool(false)))
        };
        if d.get("type").and_then(Value::as_str) != Some("user")
            || flag("isMeta")
            || flag("isSidechain")
        {
            continue;
        }
        let content = d.get("message").and_then(|m| m.get("content"));
        let texts: Vec<&str> = match content {
            Some(Value::String(s)) => vec![s.as_str()],
            Some(Value::Array(blocks)) => blocks
                .iter()
                .filter_map(|b| b.as_object())
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .map(|b| b.get("text").and_then(Value::as_str).unwrap_or(""))
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
                    cut.push('\u{2026}');
                    cut
                };
            }
        }
    }
    String::new()
}

fn real(p: &str) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| PathBuf::from(p))
}

/// The owner's past Claude sessions of `agent` that still have a transcript, newest first (only those begun in `where_`
/// when it is not empty): the cost ledger's `runs` (`<machine>/cost.db`, opened read-only) against the transcripts on
/// disk. Empty when there is no ledger.
pub(crate) fn recent(ctx: &Ctx<'_>, agent: &str, limit: usize, where_: &str) -> Vec<Recent> {
    let Some(machine) = ctx.machine.as_deref() else {
        return Vec::new();
    };
    let db = machine.join("cost.db");
    if !db.is_file() {
        return Vec::new();
    }
    let Ok(c) = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return Vec::new();
    };
    // 'codex:...' sessions are not Claude's; a person's runs are not the owner's.
    let Ok(mut q) = c.prepare(
        "select session, started, via, project from runs where agent = ?1 and coalesce(person, '') = '' \
         and session not like '%:%' order by started desc",
    ) else {
        return Vec::new();
    };
    let ledger: Vec<LedgerRow> = q
        .query_map([agent], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .map(|rows| rows.filter_map(std::result::Result::ok).collect())
        .unwrap_or_default();
    // The first transcript found under each stem wins, roots in order.
    let mut on_disk: std::collections::HashMap<String, PathBuf> = std::collections::HashMap::new();
    for root in transcript_roots(ctx) {
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
                .filter(|f| f.extension().is_some_and(|e| e == "jsonl"))
                .collect();
            files.sort();
            for f in files {
                if let Some(stem) = f.file_stem().and_then(|s| s.to_str()) {
                    on_disk.entry(stem.to_owned()).or_insert(f);
                }
            }
        }
    }
    let here = (!where_.is_empty()).then(|| real(where_));
    let mut out = Vec::new();
    for (session, started, via, project) in ledger {
        let Some(t) = on_disk.get(&session) else {
            continue;
        };
        if let Some(h) = &here {
            let cwd = cwd_in(t);
            let cwd = if cwd.is_empty() { "/nowhere" } else { &cwd };
            if &real(cwd) != h {
                continue;
            }
        }
        out.push(Recent {
            text: first_text(t, 70),
            session,
            started,
            via,
            project,
        });
        if out.len() == limit {
            break;
        }
    }
    out
}

/// `latest`: the id of the agent's newest session begun in `where_`.
pub(crate) fn latest(
    ctx: &Ctx<'_>,
    agent: &str,
    where_: &str,
) -> std::result::Result<String, String> {
    recent(ctx, agent, 1, where_)
        .into_iter()
        .next()
        .map(|r| r.session)
        .ok_or_else(|| {
            format!("no {agent} session started in {where_} to continue (kith {agent} -r lists them all)")
        })
}

/// `pick`, minus the terminal: given the list (`recent` with its default limit of 15) and what the person typed, the
/// session chosen. `Err("")` is a cancel (an empty answer: exit 1 with no message). The numbered list on stderr is the
/// caller's (it prints times in the local zone).
pub(crate) fn pick(
    agent: &str,
    rows: &[Recent],
    answer: &str,
) -> std::result::Result<String, String> {
    if rows.is_empty() {
        return Err(format!(
            "no {agent} sessions to resume (none in the cost ledger with a transcript on this Mac)"
        ));
    }
    let a = answer.trim();
    if a.is_empty() {
        return Err(String::new());
    }
    match a.parse::<usize>() {
        Ok(n) if (1..=rows.len()).contains(&n) && a.bytes().all(|b| b.is_ascii_digit()) => {
            Ok(rows[n - 1].session.clone())
        }
        _ => Err(format!("no session {a} in the list")),
    }
}

/// The reading half of `resolve` for a session id the host does not know: the Claude session `thread`, the folder it
/// ran in, and the registry's view of it (the imported conversation: its agent and folder). The claim, the live-process
/// check and the host's answers are not here.
pub(crate) fn resolve_read(
    ctx: &Ctx<'_>,
    conn: &Connection,
    agent: &str,
    thread: &str,
) -> Result<std::result::Result<Resolved, String>> {
    let Some(found) = transcript_cwd(ctx, thread) else {
        return Ok(Err(format!(
            "unknown thread \"{thread}\" (the Kith host is not running, so only a Claude session id can be resumed)"
        )));
    };
    let theirs: Option<String> = conn
        .query_row(
            "SELECT agent FROM conversations WHERE id = ?1",
            [thread],
            |r| r.get(0),
        )
        .ok();
    if let Some(theirs) = theirs.filter(|t| !t.is_empty() && t != agent) {
        return Ok(Err(format!(
            "that session belongs to {theirs}, not {agent}"
        )));
    }
    let mut cwd = found;
    if cwd.is_empty() {
        cwd = rows::list(conn, thread)?
            .into_iter()
            .map(|s| s.cwd)
            .find(|c| !c.is_empty())
            .unwrap_or_default();
    }
    if !cwd.is_empty() && !Path::new(&cwd).is_dir() {
        return Ok(Err(format!("the session's folder {cwd} is gone")));
    }
    Ok(Ok(Resolved {
        sid: thread.to_owned(),
        cwd,
    }))
}
