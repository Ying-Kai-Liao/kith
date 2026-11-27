//! The fold rules (design section 3), pure: given the row a fold key already has and a new fact, what the row becomes.
//! `Store::put_post` runs this inside its transaction and writes the answer, so the rules and the write can't drift.

use super::{Fold, PostWrite};
use crate::store::Post;
use anyhow::{Result, bail};
use chrono::{Local, TimeZone};
use serde_json::{Value, json};

const TITLE_MAX: usize = 80;
const LINE_MAX: usize = 200;
/// Runs a routine's card remembers.
const RUNS_KEPT: usize = 10;
/// The kinds a card can have, besides the ones a tool names freely (an unknown one shows as a plain card).
const KIND_MAX: usize = 32;
const ATTACHMENTS: [&str; 5] = ["file", "page", "video", "number", "link"];

/// The row key of a write: from the fold and the agent, never from a caller. A tool post with no `fold` gets a key of
/// its own, so it is always a new card.
pub fn key(w: &PostWrite) -> String {
    let agent = &w.agent;
    match &w.fold {
        Fold::Merges { .. } => format!("merges:{agent}:{}", local_day(w.fact_at)),
        Fold::Routine { key, .. } => format!("routine:{agent}:{key}"),
        Fold::Page { name, .. } => format!("page:{agent}:{name}"),
        Fold::Tool { fold: Some(f) } => format!("tool:{agent}:{f}"),
        Fold::Tool { fold: None } => format!("tool:{agent}:{}", uuid::Uuid::new_v4().simple()),
    }
}

/// The fact's own local day (the Mac's time zone), so a late retry lands on the day it happened.
pub fn local_day(ms: i64) -> String {
    Local.timestamp_millis_opt(ms).single().map_or_else(|| "unknown".into(), |t| t.format("%Y-%m-%d").to_string())
}

/// One line, at most `max` characters: newlines and runs of spaces become one space.
pub fn one_line(s: &str, max: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let mut cut: String = flat.chars().take(max.saturating_sub(1)).collect();
    cut.truncate(cut.trim_end().len());
    cut.push('…');
    cut
}

/// Refuses a write no fold could make sense of.
fn check(w: &PostWrite) -> Result<()> {
    if w.agent.trim().is_empty() || w.source.chat.is_empty() || w.fact_id.is_empty() {
        bail!("a post needs an agent, a chat and a fact id");
    }
    let kind_ok = !w.kind.is_empty() && w.kind.len() <= KIND_MAX && w.kind.chars().all(|c| c.is_ascii_alphanumeric());
    if !kind_ok {
        bail!("a post's kind is a short word");
    }
    if let Fold::Tool { fold: Some(f) } = &w.fold {
        let slug = !f.is_empty()
            && f.len() <= 40
            && f.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if !slug {
            bail!("fold is a slug of a-z, 0-9 and -, at most 40 characters");
        }
    }
    match &w.fold {
        Fold::Merges { pr, .. } if pr.trim().is_empty() => bail!("a merges post names its PR"),
        Fold::Routine { ok, .. } if *ok == (w.kind == "failing") => {
            bail!("a run's kind is failing when, and only when, it failed")
        }
        Fold::Merges { .. } => {}
        _ if w.title.trim().is_empty() => bail!("a post needs a title"),
        _ => {}
    }
    check_attachment(w.attachment.as_ref())?;
    if let (Fold::Page { version, .. }, Some(a)) = (&w.fold, &w.attachment)
        && (a["type"] != "page" || a["version"] != version.as_str())
    {
        bail!("a page post carries its page and that version");
    }
    if matches!(w.fold, Fold::Page { .. }) && w.attachment.is_none() {
        bail!("a page post carries its page");
    }
    Ok(())
}

/// At most one attachment, of a kind the apps know; a link is https only.
fn check_attachment(a: Option<&Value>) -> Result<()> {
    let Some(a) = a else { return Ok(()) };
    let kind = a["type"].as_str().unwrap_or_default();
    if !ATTACHMENTS.contains(&kind) {
        bail!("unknown attachment type");
    }
    let ok = match kind {
        "file" | "video" => a["file"].is_object(),
        "page" => a["file"].is_object() && a["version"].is_string(),
        "number" => a["value"].is_number(),
        _ => a["url"].as_str().is_some_and(|u| u.starts_with("https://")),
    };
    if !ok {
        bail!("the {kind} attachment is malformed");
    }
    Ok(())
}

/// A new row for `w` (no id or rev yet: the store gives those).
fn fresh(w: &PostWrite, key: &str, now: i64) -> Post {
    Post {
        id: String::new(),
        rev: 0,
        fold_key: key.to_owned(),
        agent: w.agent.clone(),
        kind: w.kind.clone(),
        title: one_line(&w.title, TITLE_MAX),
        line: one_line(&w.line, LINE_MAX),
        attachment: w.attachment.clone(),
        items: vec![],
        fact_at: w.fact_at,
        fact_id: w.fact_id.clone(),
        streak: 0,
        source_chat: w.source.chat.clone(),
        source_thread: w.source.thread.clone(),
        source_entry: w.source.entry.clone(),
        by: w.by.as_str().to_owned(),
        created_at: now,
        updated_at: now,
        liked_at: None,
        noted_at: None,
        hidden_at: None,
    }
}

/// What the stored row `old` becomes when `new`'s content replaces what it shows: its id, birth, like and memory note
/// stay, and, as the content changed, it moves to the top and a dismissed card comes back.
fn replaced(old: &Post, new: Post, now: i64) -> Post {
    Post {
        id: old.id.clone(),
        rev: old.rev,
        created_at: old.created_at,
        liked_at: old.liked_at,
        noted_at: old.noted_at,
        updated_at: now,
        hidden_at: None,
        ..new
    }
}

/// The row `w` makes of `old` (None: no row yet), or None when the fact changes nothing (a repeat, a PR already in,
/// a late fact the rules keep out): no rev is taken.
pub fn decide(old: Option<&Post>, w: &PostWrite, key: &str, now: i64) -> Result<Option<Post>> {
    check(w)?;
    Ok(match &w.fold {
        Fold::Merges { pr, title } => merges(old, w, key, now, pr, title),
        Fold::Routine { ok, .. } => routine(old, w, key, now, *ok),
        Fold::Page { version, .. } => page(old, w, key, now, version),
        Fold::Tool { .. } => tool(old, w, key, now),
    })
}

/// A set of PRs keyed by `repo#n`; the card reads "Merged N PRs today".
fn merges(old: Option<&Post>, w: &PostWrite, key: &str, now: i64, pr: &str, title: &str) -> Option<Post> {
    let item = json!({"id": pr, "title": one_line(title, TITLE_MAX)});
    let mut p = match old {
        Some(o) if o.items.iter().any(|i| i["id"] == pr) => return None,
        Some(o) => {
            let mut p = replaced(o, o.clone(), now);
            p.fact_at = o.fact_at.max(w.fact_at);
            p
        }
        None => Post { fact_id: pr.to_owned(), ..fresh(w, key, now) },
    };
    p.items.push(item);
    p.kind = "merges".into();
    let n = p.items.len();
    p.title = format!("Merged {n} PR{} today", if n == 1 { "" } else { "s" });
    let list: Vec<String> = p
        .items
        .iter()
        .map(|i| format!("{} {}", i["id"].as_str().unwrap_or_default(), i["title"].as_str().unwrap_or_default()))
        .collect();
    p.line = one_line(&list.join("; "), LINE_MAX);
    p.attachment = None;
    Some(p)
}

/// The newest run is shown; every run goes into the last ten, and the streak is counted from those in run order.
fn routine(old: Option<&Post>, w: &PostWrite, key: &str, now: i64, ok: bool) -> Option<Post> {
    let run = json!({"id": w.fact_id, "at": w.fact_at, "ok": ok});
    let Some(o) = old else {
        let mut p = fresh(w, key, now);
        p.streak = i64::from(!ok);
        p.items.push(run);
        return Some(p);
    };
    if o.items.iter().any(|i| i["id"] == w.fact_id.as_str()) {
        return None;
    }
    let mut items = o.items.clone();
    items.push(run);
    items.sort_by(|a, b| (a["at"].as_i64(), a["id"].as_str()).cmp(&(b["at"].as_i64(), b["id"].as_str())));
    items.drain(..items.len().saturating_sub(RUNS_KEPT));
    let streak = i64::try_from(items.iter().rev().take_while(|i| i["ok"] == false).count()).unwrap_or(i64::MAX);
    let newest = (w.fact_at, w.fact_id.as_str()) > (o.fact_at, o.fact_id.as_str());
    let mut p = if newest { replaced(o, fresh(w, key, now), now) } else { o.clone() };
    p.items = items;
    p.streak = streak;
    if !newest && streak != o.streak {
        // The card shows the same run, but the streak under it moved: that is news, as a changed content is.
        p.updated_at = now;
        p.hidden_at = None;
    }
    Some(p)
}

/// A page version replaces the card; the same version again, or an older file, does not. The picture of the old
/// version is gone with it (the shot is for the version shown).
fn page(old: Option<&Post>, w: &PostWrite, key: &str, now: i64, version: &str) -> Option<Post> {
    let mut new = fresh(w, key, now);
    if let Some(a) = new.attachment.as_mut() {
        a["shot"] = Value::Null;
    }
    let Some(o) = old else { return Some(new) };
    let same = o.attachment.as_ref().is_some_and(|a| a["version"] == version);
    (!same && w.fact_at >= o.fact_at).then(|| replaced(o, new, now))
}

/// A tool post replaces its earlier one of the same fold when it is not older; with no fold it is always new.
fn tool(old: Option<&Post>, w: &PostWrite, key: &str, now: i64) -> Option<Post> {
    let new = fresh(w, key, now);
    let Some(o) = old else { return Some(new) };
    (w.fact_at >= o.fact_at && w.fact_id != o.fact_id).then(|| replaced(o, new, now))
}

#[cfg(test)]
mod tests;
