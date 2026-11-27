//! A like on a post (phase2-feed, section 4): the post's `liked_at`, and once per post one line in the agent's
//! `memory/feedback.md` saying the owner liked it. `noted_at` is that "once": an unlike never deletes the line, and a
//! like after an unlike writes nothing more. Owner only; nobody counts likes.

use super::event;
use crate::LockExt;
use crate::hqmem::{self, Layout, Who, tidy};
use crate::hub::Hub;
use crate::store::Post;
use anyhow::{Result, anyhow};
use std::sync::{Arc, Mutex};

const FILE: &str = "feedback.md";
const INDEX: &str = "MEMORY.md";
const INDEX_LINE: &str = "- [feedback.md](feedback.md) — Feed posts Kai liked";

/// One note at a time, so the "line already there?" check and `noted_at` can't be raced by a second like.
static NOTING: Mutex<()> = Mutex::new(());

/// What became of one post's note.
#[derive(Debug, PartialEq, Eq)]
pub enum Noted {
    Written,
    /// Already in the file (a crash between the append and `noted_at`).
    Present,
    /// The post was unliked first, or noted already: nothing to write.
    Nothing,
    /// The agent has no folder or the guard refused its title: the like stays, the note is given up on.
    GivenUp(String),
}

/// Likes a post or takes the like back; the changed post goes out as an event, then the note is written.
pub fn like_post(hub: &Hub, id: &str, liked: bool) -> Result<Post> {
    like_post_in(hub, &Layout::from_env(), id, liked)
}

pub(crate) fn like_post_in(hub: &Hub, layout: &Layout, id: &str, liked: bool) -> Result<Post> {
    let post = {
        let _g = hub.emit_lock.locked();
        let (post, changed) = hub.store.set_post_liked(id, liked)?.ok_or_else(|| anyhow!("unknown post"))?;
        if changed {
            let _ = hub.events.send(event(&post));
        }
        post
    };
    // A note that fails to write is retried at the next start; the like itself is done.
    if post.noted_at.is_none()
        && let Err(error) = note(hub, layout, id)
    {
        tracing::warn!(post = id, error = format!("{error:#}"), "a liked post's memory line is not written yet");
    }
    Ok(post)
}

/// Writes the post's line if the post is liked right now and was not noted before.
pub(crate) fn note(hub: &Hub, layout: &Layout, id: &str) -> Result<Noted> {
    let _one = NOTING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    // Read again right before the append: an unlike in between is the undo, and writes nothing.
    if !hub.store.post_unnoted_like(id)? {
        return Ok(Noted::Nothing);
    }
    let Some(post) = hub.store.post(id)? else { return Ok(Noted::Nothing) };
    let done = append(layout, &post)?;
    if let Noted::GivenUp(why) = &done {
        tracing::warn!(post = id, agent = post.agent, why, "a liked post is not noted in the agent's memory");
    }
    hub.store.set_post_noted(id)?;
    Ok(done)
}

fn line_of(post: &Post) -> String {
    // The title is the agent's text: one line, whatever it holds.
    let title = post.title.split_whitespace().collect::<Vec<_>>().join(" ");
    format!("- {}: Kai liked your Feed post \"{title}\" ({}).", hqmem::ymd(crate::store::now_ms()), post.kind)
}

fn append(layout: &Layout, post: &Post) -> Result<Noted> {
    let agent = post.agent.as_str();
    let (Some(feedback), Some(index)) =
        (tidy::target_of(layout, agent, false, FILE), tidy::target_of(layout, agent, false, INDEX))
    else {
        return Ok(Noted::GivenUp(format!("{agent} has no folder")));
    };
    let conn = hqmem::open(layout)?;
    let who = Who::Liked(agent);
    let line = line_of(post);
    let old = hqmem::read(&feedback.path);
    let present = old.as_deref().is_some_and(|text| text.lines().any(|l| l.trim_end() == line));
    if !present {
        let text = with_line(old.as_deref(), &line);
        if let Err(refusal) = hqmem::write(&conn, &feedback, who, &text)? {
            return Ok(Noted::GivenUp(refusal.message().to_owned()));
        }
    }
    // The index names the file once, whenever it is first written (also after a crash that left only the file).
    let map = hqmem::read(&index.path);
    if !map.as_deref().is_some_and(|text| text.contains("(feedback.md)")) {
        let text = with_line(map.as_deref(), INDEX_LINE);
        if let Err(refusal) = hqmem::write(&conn, &index, who, &text)? {
            return Ok(Noted::GivenUp(refusal.message().to_owned()));
        }
    }
    Ok(if present { Noted::Present } else { Noted::Written })
}

fn with_line(old: Option<&str>, line: &str) -> String {
    match old {
        Some(text) if !text.is_empty() => format!("{}\n{line}\n", text.trim_end_matches('\n')),
        _ => format!("{line}\n"),
    }
}

/// At host start: likes whose line was not written (a crash, a busy history) get another try.
pub fn start(hub: &Arc<Hub>) {
    // Unit tests build hubs of their own and must never write to this computer's memory.
    if cfg!(test) {
        return;
    }
    let hub = hub.clone();
    tokio::task::spawn_blocking(move || retry_all(&hub, &Layout::from_env()));
}

pub(crate) fn retry_all(hub: &Hub, layout: &Layout) {
    let ids = hub.store.posts_to_note().unwrap_or_default();
    for id in ids {
        if let Err(error) = note(hub, layout, &id) {
            tracing::warn!(post = id, error = format!("{error:#}"), "a liked post's memory line is not written yet");
        }
    }
}

#[cfg(test)]
mod tests;
