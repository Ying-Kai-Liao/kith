//! What the apps' Memory tab asks of an hq agent's memory files: the list, a file, the user's save, a file's
//! versions, a diff and a restore. Every call names a file by `shared` (the shared user layer, or the agent's own
//! memory/) and its `name` inside that layer; the answers are JSON values the API passes on as they are.

use super::tidy::{agent_dir, target_of};
use super::{Layout, Target, Who, put, read, record_restore, sync, write};
use anyhow::{Result, anyhow, bail};
use kith_mem::files::{self, Author, Cause, Version};
use kith_mem::rusqlite::Connection;
use serde_json::{Value, json};
use std::io::Read as _;
use std::path::Path;

/// The most of a file one read returns.
const READ_CAP: u64 = 512 * 1024;
/// The most one save takes.
const SAVE_CAP: usize = 1024 * 1024;
const DEPTH: usize = 4;

fn target(layout: &Layout, agent: &str, shared: bool, name: &str) -> Result<Target> {
    target_of(layout, agent, shared, name).ok_or_else(|| anyhow!("`{name}` isn't a memory file of {agent}"))
}

fn modified(path: &Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(0))
}

fn walk(dir: &Path, prefix: &str, depth: usize, shared: bool, out: &mut Vec<Value>) {
    let Ok(read) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = read.flatten().collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        let path = e.path();
        if name.starts_with('.') || path.is_symlink() {
            continue;
        }
        let rel = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
        if path.is_dir() {
            if depth < DEPTH {
                walk(&path, &rel, depth + 1, shared, out);
            }
        } else if super::is_markdown(&name) {
            let size = std::fs::metadata(&path).map_or(0, |m| m.len());
            out.push(json!({"name": rel, "shared": shared, "path": path, "size": size, "modified": modified(&path)}));
        }
    }
}

/// The agent's memory files, then the shared user layer's (`user.md`, `people/`, `projects/`).
pub fn list_files(layout: &Layout, agent: &str) -> Result<Value> {
    let dir = agent_dir(layout, agent).ok_or_else(|| anyhow!("{agent} has no folder on this Mac"))?;
    let (mut own, mut shared) = (Vec::new(), Vec::new());
    walk(&dir.join("memory"), "", 0, false, &mut own);
    let user = layout.state.join("user.md");
    if user.is_file() {
        let size = std::fs::metadata(&user).map_or(0, |m| m.len());
        shared
            .push(json!({"name": "user.md", "shared": true, "path": user, "size": size, "modified": modified(&user)}));
    }
    for sub in ["people", "projects"] {
        walk(&layout.state.join(sub), sub, 1, true, &mut shared);
    }
    Ok(json!({"agent": own, "shared": shared}))
}

/// One file as text, at most [`READ_CAP`] bytes. A file that doesn't exist yet reads as empty text.
pub fn read_file(layout: &Layout, agent: &str, shared: bool, name: &str) -> Result<Value> {
    let t = target(layout, agent, shared, name)?;
    if !t.path.exists() && !t.path.is_symlink() {
        return Ok(json!({"name": name, "shared": shared, "text": "", "bytes": 0, "modified": 0, "exists": false}));
    }
    // Like kithMemoryFile: no link followed, no pipe waited on, only the open handle's own metadata trusted.
    let file = super::open_regular(&t.path, true).ok_or_else(|| anyhow!("`{name}` isn't a regular file"))?;
    let meta = file.metadata()?;
    let mut bytes = Vec::new();
    file.take(READ_CAP).read_to_end(&mut bytes)?;
    Ok(json!({
        "name": name, "shared": shared, "text": String::from_utf8_lossy(&bytes), "bytes": meta.len(),
        "modified": modified(&t.path), "truncated": meta.len() > READ_CAP, "exists": true,
    }))
}

/// Saves the user's text: never refused, the checks only warn. The version it made is in the answer.
pub fn save(conn: &Connection, layout: &Layout, agent: &str, shared: bool, name: &str, text: &str) -> Result<Value> {
    if text.len() > SAVE_CAP {
        bail!("a memory file holds at most {} KB", SAVE_CAP / 1024);
    }
    let t = target(layout, agent, shared, name)?;
    match write(conn, &t, Who::Kai, text)? {
        Ok(written) => Ok(json!({"version": written.version.as_ref().map(version_json), "warnings": written.warnings})),
        // `Who::Kai` is never refused.
        Err(refusal) => bail!("{}", refusal.message()),
    }
}

/// Deletes the user's file (a version of its own, so `restore` brings it back).
pub fn delete(conn: &Connection, layout: &Layout, agent: &str, shared: bool, name: &str) -> Result<Value> {
    let t = target(layout, agent, shared, name)?;
    let version = super::remove(conn, layout, &t, Who::Kai)?;
    Ok(json!({"version": version_json(&version)}))
}

fn author(v: &Version) -> &'static str {
    match v.author {
        Author::User => "kai",
        Author::Bot => "agent",
        Author::Tidy => "tidy",
        Author::Computer => "unknown",
    }
}

fn cause(v: &Version) -> &'static str {
    match v.cause {
        Cause::Edit => "edit",
        Cause::Delete => "delete",
        Cause::Restore => "restore",
        Cause::Undo => "undo",
        Cause::Tidy => "tidy",
        Cause::Baseline => "baseline",
    }
}

fn version_json(v: &Version) -> Value {
    let (bytes, lines) = v.content.as_deref().map_or((0, 0), |c| (c.len(), c.lines().count()));
    json!({
        "id": v.id, "at": v.at, "author": author(v), "authorBot": v.author_bot, "cause": cause(v),
        "deleted": v.content.is_none(), "bytes": bytes, "lines": lines,
    })
}

/// A file's versions, newest first. The file is brought up to date first, so an edit made outside the hook shows
/// as an `unknown` version.
pub fn history(
    conn: &Connection,
    layout: &Layout,
    agent: &str,
    shared: bool,
    name: &str,
    limit: usize,
) -> Result<Value> {
    let t = target(layout, agent, shared, name)?;
    sync(conn, &t)?;
    let versions = files::versions(conn, t.bot.as_deref(), &t.name, limit.max(1))?;
    Ok(json!({"name": name, "shared": shared, "versions": versions.iter().map(version_json).collect::<Vec<_>>()}))
}

fn version_of(conn: &Connection, t: &Target, id: i64) -> Result<Version> {
    files::version(conn, id)?
        .filter(|v| v.bot == t.bot && v.name == t.name)
        .ok_or_else(|| anyhow!("no version {id} of {}", t.label()))
}

/// What changed from one version to another (ids from `history`); by default from the one before the newest to the
/// newest.
pub fn diff(
    conn: &Connection,
    layout: &Layout,
    agent: &str,
    shared: bool,
    name: &str,
    from: Option<i64>,
    to: Option<i64>,
) -> Result<Value> {
    let t = target(layout, agent, shared, name)?;
    sync(conn, &t)?;
    let all = files::versions(conn, t.bot.as_deref(), &t.name, 2)?;
    let to = match to {
        Some(id) => version_of(conn, &t, id)?,
        None => all.first().cloned().ok_or_else(|| anyhow!("{} has no versions yet", t.label()))?,
    };
    let from = match from {
        Some(id) => version_of(conn, &t, id)?,
        None => {
            all.get(1).cloned().ok_or_else(|| anyhow!("{} has one version; there is nothing to compare", t.label()))?
        }
    };
    let text = files::diff(from.content.as_deref().unwrap_or_default(), to.content.as_deref().unwrap_or_default());
    Ok(json!({"from": from.id, "to": to.id, "diff": text}))
}

/// Puts a version back as the file (the user's act, so a version of its own; never refused).
pub fn restore(conn: &Connection, layout: &Layout, agent: &str, shared: bool, name: &str, id: i64) -> Result<Value> {
    let t = target(layout, agent, shared, name)?;
    sync(conn, &t)?;
    let wanted = version_of(conn, &t, id)?;
    if read(&t.path) == wanted.content {
        return Ok(json!({"version": Value::Null, "warnings": []}));
    }
    put(&t.path, wanted.content.as_deref())?;
    let made = record_restore(conn, &t, Who::Kai)?;
    Ok(json!({"version": made.as_ref().map(version_json), "warnings": []}))
}
