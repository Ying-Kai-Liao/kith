//! The owner's Feed (designs/2026-11-24-app-to-design/phase2-feed): cards about what the agents did. `put_post` is the
//! one door every writer goes through; the rules that fold a new fact into an old card are in `fold`, and the row
//! itself is written by `Store::put_post`, in one transaction that also takes the shared rev. Owner only: no post
//! names an invitee's chat, and no post event reaches a member or a bot (`api::roles`).

pub mod fold;

use crate::LockExt;
use crate::hub::Hub;
use crate::store::Post;
use anyhow::{Result, anyhow};
use serde_json::{Value, json};

/// Who wrote the post: an agent through its tool, or the host from a fact it already knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code, reason = "the tool (F2) and the host writers (F3) are the callers")]
pub enum By {
    Agent,
    Host,
}

impl By {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Host => "host",
        }
    }
}

/// How a fact folds into what is already shown (section 3 of the design). The key of the row comes from this and the
/// agent, never from a caller, so an agent can't fold into another agent's card.
#[derive(Clone, Debug)]
#[allow(dead_code, reason = "the tool (F2) and the host writers (F3) are the callers")]
pub enum Fold {
    /// A merged PR (`repo#n`), set into the day's card of the merging agent.
    Merges { pr: String, title: String },
    /// A run of a routine: `key` is its id, or `job:<job>`. `ok` is whether the run worked.
    Routine { key: String, ok: bool },
    /// A page an agent shared: `name` is the file name, `version` the sha256 of the html.
    Page { name: String, version: String },
    /// An agent's own post. With `fold` it replaces its earlier post of that name; without, it is a new card.
    Tool { fold: Option<String> },
}

/// The chat a post points back at: "open in chat" and the reply box go there.
#[derive(Clone, Debug)]
pub struct PostSource {
    pub chat: String,
    pub thread: Option<String>,
    pub entry: Option<String>,
}

/// One fact to show. `title` and `line` of a merges post are worked out by the fold from the PRs.
#[derive(Clone, Debug)]
pub struct PostWrite {
    pub fold: Fold,
    pub agent: String,
    pub kind: String,
    pub title: String,
    pub line: String,
    pub attachment: Option<Value>,
    pub source: PostSource,
    pub by: By,
    /// When the fact happened (merge time, run end, file time): the fold's order, never "now".
    pub fact_at: i64,
    /// The fact's own id (run id, `repo#n`, file sha): the tie-break and what makes a repeat a no-op.
    pub fact_id: String,
}

/// The event a changed post goes out as: live to every stream, of which only the owner's gets it (`api::roles`).
pub fn event(p: &Post) -> Value {
    json!({"type": "post", "rev": p.rev, "post": p.json()})
}

/// Folds one fact into the Feed and tells the owner's apps when a card changed. `None` is a repeat or a late fact the
/// rules keep out: no rev, no event. Under `emit_lock`, so events leave in rev order.
#[allow(dead_code, reason = "the tool (F2) and the host writers (F3) are the callers")]
pub fn put_post(hub: &Hub, w: &PostWrite) -> Result<Option<Post>> {
    let _g = hub.emit_lock.locked();
    let post = hub.store.put_post(w)?;
    if let Some(p) = &post {
        let _ = hub.events.send(event(p));
    }
    Ok(post)
}

/// Hides a post, or brings it back. Quiet when it already is as asked.
pub fn hide_post(hub: &Hub, id: &str, hidden: bool) -> Result<Post> {
    let _g = hub.emit_lock.locked();
    let (post, changed) = hub.store.set_post_hidden(id, hidden)?.ok_or_else(|| anyhow!("unknown post"))?;
    if changed {
        let _ = hub.events.send(event(&post));
    }
    Ok(post)
}

/// A page's picture, for the version it was taken of. Dropped when the page has moved on (I5).
#[allow(dead_code, reason = "the page shooter (F6) is the caller")]
pub fn put_shot(hub: &Hub, id: &str, version: &str, shot: Option<Value>) -> Result<Option<Post>> {
    let _g = hub.emit_lock.locked();
    let post = hub.store.put_post_shot(id, version, shot)?;
    if let Some(p) = &post {
        let _ = hub.events.send(event(p));
    }
    Ok(post)
}

#[cfg(test)]
mod tests;
