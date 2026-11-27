//! The `posts` table: the owner's Feed (designs/2026-11-24-app-to-design/phase2-feed). One row per fold key.
//! `Store::put_post` is the only writer of a card's content; it runs the fold rules (`feed::fold`) and the write in one
//! transaction that also takes the shared rev. Hiding, a page's picture and the retention pass touch only their own
//! columns. Posts are the owner's: a chat of an invited person is refused at the door.

use super::model::BotConfig;
use super::{Store, next_rev, now_ms};
use crate::LockExt;
use crate::feed::{PostWrite, fold};
use anyhow::{Result, bail};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};

/// Unliked posts are kept this long (by `updated_at`), liked ones the longer.
const KEEP_MS: i64 = 30 * 24 * 3600 * 1000;
const KEEP_LIKED_MS: i64 = 180 * 24 * 3600 * 1000;

/// One row of the Feed. `fold_key`, `fact_id` and `noted_at` stay on the host; the wire shape is [`Post::json`].
#[derive(Clone, Debug, PartialEq)]
pub struct Post {
    pub id: String,
    pub rev: i64,
    pub fold_key: String,
    pub agent: String,
    pub kind: String,
    pub title: String,
    pub line: String,
    pub attachment: Option<Value>,
    pub items: Vec<Value>,
    pub fact_at: i64,
    pub fact_id: String,
    pub streak: i64,
    pub source_chat: String,
    pub source_thread: Option<String>,
    pub source_entry: Option<String>,
    pub by: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub liked_at: Option<i64>,
    pub noted_at: Option<i64>,
    pub hidden_at: Option<i64>,
}

impl Post {
    /// The wire shape (contract type `Post`).
    pub fn json(&self) -> Value {
        let mut source = json!({"chat": self.source_chat});
        if let Some(t) = &self.source_thread {
            source["thread"] = t.clone().into();
        }
        if let Some(e) = &self.source_entry {
            source["entry"] = e.clone().into();
        }
        let mut v = json!({
            "id": self.id, "rev": self.rev, "agent": self.agent, "kind": self.kind, "title": self.title,
            "line": self.line, "items": self.items, "factAt": self.fact_at, "streak": self.streak,
            "source": source, "by": self.by, "createdAt": self.created_at, "updatedAt": self.updated_at,
            "liked": self.liked_at.is_some(), "hidden": self.hidden_at.is_some(),
        });
        if let Some(a) = &self.attachment {
            v["attachment"] = a.clone();
        }
        v
    }
}

const POST_COLS: &str = "id, rev, fold_key, agent, kind, title, line, attachment, items, fact_at, fact_id, streak, \
     source_chat, source_thread, source_entry, \"by\", created_at, updated_at, liked_at, noted_at, hidden_at";

fn row_post(r: &rusqlite::Row) -> rusqlite::Result<Post> {
    let attachment: Option<String> = r.get(7)?;
    let items: String = r.get(8)?;
    Ok(Post {
        id: r.get(0)?,
        rev: r.get(1)?,
        fold_key: r.get(2)?,
        agent: r.get(3)?,
        kind: r.get(4)?,
        title: r.get(5)?,
        line: r.get(6)?,
        attachment: attachment.and_then(|a| serde_json::from_str(&a).ok()),
        items: serde_json::from_str(&items).unwrap_or_default(),
        fact_at: r.get(9)?,
        fact_id: r.get(10)?,
        streak: r.get(11)?,
        source_chat: r.get(12)?,
        source_thread: r.get(13)?,
        source_entry: r.get(14)?,
        by: r.get(15)?,
        created_at: r.get(16)?,
        updated_at: r.get(17)?,
        liked_at: r.get(18)?,
        noted_at: r.get(19)?,
        hidden_at: r.get(20)?,
    })
}

impl Store {
    /// Folds one fact into the Feed: the only writer of a post's content (I1). One transaction: the chat is checked
    /// (I2: never a chat of an invited person), the row of the fold key is read, the rules decide, the row is written
    /// with a fresh rev. `None` is a fact that changes nothing (a repeat, a PR already in, an older file): no rev.
    pub fn put_post(&self, w: &PostWrite) -> Result<Option<Post>> {
        let c = self.db.locked();
        let tx = c.unchecked_transaction()?;
        let chat: Option<(String, bool)> = tx
            .query_row("SELECT config, deleted FROM bots WHERE id = ?1", [&w.source.chat], |r| {
                Ok((r.get(0)?, r.get::<_, i64>(1)? != 0))
            })
            .optional()?;
        let Some((config, false)) = chat else { bail!("a post needs a chat that is there") };
        match serde_json::from_str::<BotConfig>(&config) {
            Ok(cfg) if cfg.person.is_none() => {}
            _ => bail!("a post never names the chat of an invited person"),
        }
        let key = fold::key(w);
        let old =
            tx.query_row(&format!("SELECT {POST_COLS} FROM posts WHERE fold_key = ?1"), [&key], row_post).optional()?;
        let Some(mut p) = fold::decide(old.as_ref(), w, &key, now_ms())? else { return Ok(None) };
        p.rev = next_rev(&tx)?;
        let (attachment, items) = (
            p.attachment.as_ref().map(Value::to_string),
            serde_json::to_string(&p.items).unwrap_or_else(|_| "[]".into()),
        );
        if old.is_some() {
            tx.execute(
                "UPDATE posts SET rev = ?2, kind = ?3, title = ?4, line = ?5, attachment = ?6, items = ?7, fact_at = ?8,
                    fact_id = ?9, streak = ?10, source_chat = ?11, source_thread = ?12, source_entry = ?13,
                    \"by\" = ?14, updated_at = ?15, hidden_at = ?16 WHERE id = ?1",
                params![
                    p.id, p.rev, p.kind, p.title, p.line, attachment, items, p.fact_at, p.fact_id, p.streak,
                    p.source_chat, p.source_thread, p.source_entry, p.by, p.updated_at, p.hidden_at
                ],
            )?;
        } else {
            p.id = format!("p-{}", uuid::Uuid::new_v4().simple());
            tx.execute(
                &format!(
                    "INSERT INTO posts({POST_COLS}) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                        ?16, ?17, ?18, ?19, ?20, ?21)"
                ),
                params![
                    p.id, p.rev, p.fold_key, p.agent, p.kind, p.title, p.line, attachment, items, p.fact_at, p.fact_id,
                    p.streak, p.source_chat, p.source_thread, p.source_entry, p.by, p.created_at, p.updated_at,
                    p.liked_at, p.noted_at, p.hidden_at
                ],
            )?;
        }
        tx.commit()?;
        Ok(Some(p))
    }

    #[allow(dead_code, reason = "the likes package (F4) reads a post by id; tests use it now")]
    pub fn post(&self, id: &str) -> Result<Option<Post>> {
        let c = self.db.locked();
        Ok(c.query_row(&format!("SELECT {POST_COLS} FROM posts WHERE id = ?1"), [id], row_post).optional()?)
    }

    /// Posts that are not hidden, newest `updated_at` first, older than `before`: at most `limit`.
    pub fn posts_page(&self, before: Option<i64>, limit: i64) -> Result<Vec<Post>> {
        let c = self.db.locked();
        let mut st = c.prepare(&format!(
            "SELECT {POST_COLS} FROM posts WHERE hidden_at IS NULL AND updated_at < ?1 ORDER BY updated_at DESC, rev DESC LIMIT ?2"
        ))?;
        Ok(st.query_map(params![before.unwrap_or(i64::MAX), limit], row_post)?.collect::<rusqlite::Result<_>>()?)
    }

    /// What a stream that last saw `since` is missing, oldest rev first, at most the newest `cap` of them. A fresh
    /// client (`since` 0) gets the newest `cap` that are not hidden; a returning one also hears of a post hidden since,
    /// so it can take it off its screen.
    pub fn posts_since(&self, since: i64, cap: i64) -> Result<Vec<Post>> {
        let c = self.db.locked();
        let hidden = if since == 0 { "AND hidden_at IS NULL" } else { "" };
        let mut st = c.prepare(&format!(
            "SELECT * FROM (SELECT {POST_COLS} FROM posts WHERE rev > ?1 {hidden} ORDER BY rev DESC LIMIT ?2) ORDER BY rev"
        ))?;
        Ok(st.query_map(params![since, cap], row_post)?.collect::<rusqlite::Result<_>>()?)
    }

    /// Hides a post, or brings it back. The post after, and whether anything changed (a repeat takes no rev). `None`
    /// for an id that is not there.
    pub fn set_post_hidden(&self, id: &str, hidden: bool) -> Result<Option<(Post, bool)>> {
        let c = self.db.locked();
        let tx = c.unchecked_transaction()?;
        let Some(mut p) =
            tx.query_row(&format!("SELECT {POST_COLS} FROM posts WHERE id = ?1"), [id], row_post).optional()?
        else {
            return Ok(None);
        };
        if p.hidden_at.is_some() == hidden {
            return Ok(Some((p, false)));
        }
        p.hidden_at = hidden.then(now_ms);
        p.rev = next_rev(&tx)?;
        tx.execute("UPDATE posts SET rev = ?2, hidden_at = ?3 WHERE id = ?1", params![id, p.rev, p.hidden_at])?;
        tx.commit()?;
        Ok(Some((p, true)))
    }

    /// A page's picture, only for the version the post shows now (I5): a picture of an older version is dropped, as is
    /// one for a post that is no page. The card keeps its place in the Feed.
    pub fn put_post_shot(&self, id: &str, version: &str, shot: Option<Value>) -> Result<Option<Post>> {
        let c = self.db.locked();
        let tx = c.unchecked_transaction()?;
        let Some(mut p) =
            tx.query_row(&format!("SELECT {POST_COLS} FROM posts WHERE id = ?1"), [id], row_post).optional()?
        else {
            return Ok(None);
        };
        let Some(a) = p.attachment.as_mut().filter(|a| a["type"] == "page" && a["version"] == version) else {
            return Ok(None);
        };
        a["shot"] = shot.unwrap_or(Value::Null);
        p.rev = next_rev(&tx)?;
        tx.execute(
            "UPDATE posts SET rev = ?2, attachment = ?3 WHERE id = ?1",
            params![id, p.rev, p.attachment.as_ref().map(Value::to_string)],
        )?;
        tx.commit()?;
        Ok(Some(p))
    }

    /// The daily retention pass: posts not touched for 30 days go, unless liked; liked ones go after 180. Returns how
    /// many. No event: an app drops old cards on its own.
    #[allow(dead_code, reason = "the host has no daily pass yet; the follow-up wires this in")]
    pub fn posts_purge(&self, now: i64) -> Result<usize> {
        let c = self.db.locked();
        Ok(c.execute(
            "DELETE FROM posts WHERE (liked_at IS NULL AND updated_at < ?1) OR (liked_at IS NOT NULL AND updated_at < ?2)",
            params![now - KEEP_MS, now - KEEP_LIKED_MS],
        )?)
    }

    /// Per chat with an agent entry newer than `since`: the chat, when its newest one was written, and its id.
    /// Which chats the owner sees is the caller's to decide.
    pub fn story_rows(&self, since: i64) -> Result<Vec<(String, i64, String)>> {
        let c = self.db.locked();
        // Newest by created_at, then rev (the store's monotonic counter): entries of one millisecond are common.
        let mut st = c.prepare(
            "SELECT bot_id, created_at, id FROM (
                 SELECT bot_id, created_at, id, rev,
                        ROW_NUMBER() OVER (PARTITION BY bot_id ORDER BY created_at DESC, rev DESC) AS n
                 FROM entries WHERE kind = 'agent' AND created_at > ?1)
             WHERE n = 1 ORDER BY created_at DESC, rev DESC",
        )?;
        Ok(st.query_map([since], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
mod tests;
