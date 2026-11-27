//! The owner's Feed: `feed`, `hidePost` and `stories` (designs/2026-11-24-app-to-design/phase2-feed, sections 6 and 9).
//! All of them are the owner's alone: none is on the member or bot lists, so `guard` and `bot_call` never reach this
//! file. `likePost` and `feedRoute` are in the contract and answer "not yet" until their packages fill them in.

use super::str_arg;
use crate::hub::Hub;
use crate::store::{ReadScope, now_ms};
use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use std::ops::ControlFlow;

/// The Feed's page: most posts one call answers, and how many it answers when asked for no number.
const MAX_PAGE: i64 = 50;
const DEFAULT_PAGE: i64 = 30;
/// A ring above the Feed lasts this long after the agent's last entry.
const STORY_MS: i64 = 3 * 3600 * 1000;

pub(super) fn call(hub: &Hub, method: &str, b: Value) -> Result<ControlFlow<Value, Value>> {
    Ok(ControlFlow::Break(match method {
        "feed" => feed(hub, &b)?,
        "hidePost" => hide(hub, &b)?,
        "stories" => stories(hub)?,
        // Their packages fill these in; an app asks and gets this until then.
        "likePost" | "feedRoute" => bail!("this host doesn't answer {method} yet"),
        _ => return Ok(ControlFlow::Continue(b)),
    }))
}

fn feed(hub: &Hub, b: &Value) -> Result<Value> {
    let limit = b["limit"].as_i64().unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE);
    // One more than asked for says whether another page follows.
    let mut posts = hub.store.posts_page(b["before"].as_i64(), limit + 1)?;
    let more = i64::try_from(posts.len()).is_ok_and(|n| n > limit);
    posts.truncate(usize::try_from(limit).unwrap_or(0));
    let next = more.then(|| posts.last().map(|p| p.updated_at)).flatten();
    let mut out = json!({"posts": posts.iter().map(crate::store::Post::json).collect::<Vec<_>>()});
    if let Some(next) = next {
        out["next"] = next.into();
    }
    Ok(out)
}

fn hide(hub: &Hub, b: &Value) -> Result<Value> {
    let hidden = b["hidden"].as_bool().ok_or_else(|| anyhow!("`hidden` is required"))?;
    let post = crate::feed::hide_post(hub, str_arg(b, "id")?, hidden)?;
    Ok(json!({"post": post.json()}))
}

/// Chats the owner sees whose agent said something in the last three hours: computed from the entries, no record.
/// An invitee's chat is never one of them; a group room is, since the owner sees rooms whole.
fn stories(hub: &Hub) -> Result<Value> {
    let mut out = vec![];
    for (chat, last_at, last_entry) in hub.store.story_rows(now_ms() - STORY_MS)? {
        let Some(row) = hub.store.bot(&chat)?.filter(|r| !r.deleted && r.config.person.is_none()) else { continue };
        let mut story = json!({
            "chat": chat, "lastAt": last_at, "lastEntryId": last_entry,
            "unread": hub.store.unread(&chat, row.read_rev, ReadScope::All),
        });
        if let Some(agent) = &row.config.hq_agent {
            story["agent"] = agent.as_str().into();
        }
        out.push(story);
    }
    Ok(json!({"stories": out}))
}

#[cfg(test)]
mod tests;
