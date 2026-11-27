//! What an agent keeps about one person: `<agent>/memory/people/<person>/`. The owner's person page lists it and
//! can forget it, a file or the whole folder. Each file goes through [`super::remove`], so every deletion is a
//! version and `kithMemoryRestore` brings it back.

use super::tidy::target_of;
use super::{Layout, Who, browse};
use anyhow::{Result, anyhow, bail};
use kith_mem::rusqlite::Connection;
use serde_json::{Value, json};
use std::path::{Component, Path};

/// One plain name: no separator, `.`/`..`, empty or control character, so it can't leave `memory/people/`.
fn plain(what: &str, s: &str) -> Result<()> {
    let mut parts = Path::new(s).components();
    let ok = matches!((parts.next(), parts.next()), (Some(Component::Normal(n)), None) if n == s)
        && !s.starts_with('.')
        && !s.contains(['/', '\\'])
        && !s.chars().any(char::is_control);
    if ok { Ok(()) } else { bail!("{what} must be one plain name, not `{s}`") }
}

/// The `people/<person>/` prefix of a memory file name.
pub fn prefix(person: &str) -> Result<String> {
    plain("person", person)?;
    Ok(format!("people/{person}/"))
}

/// `list_files` for one person: only the agent's files under `people/<person>/`; the shared layer isn't theirs.
pub fn list(layout: &Layout, agent: &str, person: &str) -> Result<Value> {
    let prefix = prefix(person)?;
    let all = browse::list_files(layout, agent)?;
    let own: Vec<Value> = all["agent"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|f| f["name"].as_str().is_some_and(|n| n.starts_with(&prefix)))
        .cloned()
        .collect();
    Ok(json!({"agent": own, "shared": []}))
}

/// Forgets what `agent` kept about `person`: one file (`name`, inside the person's folder) or all of it. A folder
/// that isn't there, or is already forgotten, is a success with nothing removed.
pub fn forget(conn: &Connection, layout: &Layout, agent: &str, person: &str, name: Option<&str>) -> Result<Value> {
    let prefix = prefix(person)?;
    let mut names = Vec::new();
    match name {
        Some(n) => {
            for part in n.split('/') {
                plain("name", part)?;
            }
            names.push(format!("{prefix}{n}"));
        }
        None => {
            // Through `list`, so only the files `kithMemoryList` shows for the person are removed.
            for f in list(layout, agent, person)?["agent"].as_array().into_iter().flatten() {
                names.extend(f["name"].as_str().map(str::to_owned));
            }
        }
    }
    let mut removed = Vec::new();
    for n in names {
        let t = target_of(layout, agent, false, &n).ok_or_else(|| anyhow!("`{n}` isn't a memory file of {agent}"))?;
        match super::remove(conn, layout, &t, Who::Kai) {
            Ok(v) => removed.push(json!({"name": n, "version": browse::version_json(&v)})),
            // A named file that is already gone: nothing to forget.
            Err(e)
                if name.is_some() && e.downcast_ref::<super::RemoveError>() == Some(&super::RemoveError::Missing) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(json!({"person": person, "removed": removed}))
}
