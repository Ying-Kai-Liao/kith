//! The host's own posts (designs/2026-11-24-app-to-design/phase2-feed section 2.2): a routine's result, a cron job's
//! failure or text, a page an agent shared. Each writer calls [`host_post`] right after the entry it already writes, and
//! only when that write added the entry, so a retry makes no second post (and the fold takes a repeat of the fact id as
//! a no-op anyway). A post is never worth failing the write it follows: whatever goes wrong here is logged.

use super::{By, Fold, PostSource, PostWrite, fold, put_post};
use crate::chat::files::SharedFile;
use crate::hub::Hub;
use crate::store::now_ms;
use serde_json::{Value, json};

/// What the host knows of one fact. `chat` is the chat whose entry carries it.
pub struct HostFact {
    pub chat: String,
    /// The `meta.person` of the item that carried the fact: an invitee's is never shown.
    pub person: Option<String>,
    /// The agent to name when the chat has none (a group takes the routine's).
    pub agent: Option<String>,
    pub entry: Option<String>,
    pub fold: Fold,
    pub kind: String,
    pub title: String,
    pub line: String,
    pub attachment: Option<Value>,
    pub fact_at: i64,
    pub fact_id: String,
}

/// Posts the fact to the owner's Feed, or says why not in the log. Never fails the caller.
pub fn host_post(hub: &Hub, fact: HostFact) {
    let chat = fact.chat.clone();
    let Some(w) = write_of(hub, fact) else {
        tracing::debug!(chat, "feed: a host fact is not the owner's, or has no agent: no post");
        return;
    };
    if let Err(error) = put_post(hub, &w) {
        tracing::warn!(%error, chat, "feed: couldn't post a host fact");
    }
}

/// The write for a fact, or `None` when it must not be shown: an invitee's chat or item, a chat that is gone, no agent.
fn write_of(hub: &Hub, f: HostFact) -> Option<PostWrite> {
    if f.person.is_some() {
        return None;
    }
    let config = hub.store.bot(&f.chat).ok().flatten().filter(|b| !b.deleted)?.config;
    if config.person.is_some() {
        return None;
    }
    let agent = config.hq_agent.as_ref().map(ToString::to_string).or(f.agent).filter(|a| !a.trim().is_empty())?;
    Some(PostWrite {
        fold: f.fold,
        agent,
        kind: f.kind,
        title: f.title,
        line: f.line,
        attachment: f.attachment,
        source: PostSource { chat: f.chat, thread: None, entry: f.entry },
        by: By::Host,
        fact_at: f.fact_at,
        fact_id: f.fact_id,
    })
}

/// A routine run's fact (`key` is the routine's id, or `job:<job>`). `ok` is whether it worked; the kind says which.
pub struct RunFact<'a> {
    pub chat: &'a str,
    pub person: Option<&'a str>,
    pub agent: Option<&'a str>,
    pub entry: Option<&'a str>,
    pub key: String,
    pub name: &'a str,
    pub text: &'a str,
    pub ok: bool,
    /// What a good run attached, and the kind that says so (`file`, `number`, else plain).
    pub attachment: Option<Value>,
    pub kind: Option<&'a str>,
    pub fact_at: i64,
    pub fact_id: String,
}

pub fn run_post(hub: &Hub, r: RunFact<'_>) {
    let kind = if r.ok { r.kind.unwrap_or("plain") } else { "failing" };
    host_post(
        hub,
        HostFact {
            chat: r.chat.to_owned(),
            person: r.person.map(str::to_owned),
            agent: r.agent.map(str::to_owned),
            entry: r.entry.map(str::to_owned),
            fold: Fold::Routine { key: r.key, ok: r.ok },
            kind: kind.to_owned(),
            title: fold::one_line(r.name, 80),
            line: fold::one_line(r.text, 200),
            attachment: r.ok.then_some(r.attachment).flatten(),
            fact_at: r.fact_at,
            fact_id: r.fact_id,
        },
    );
}

/// Whether a shared file is a page: only html is, every other `send_file` stays a chat message.
fn is_page(name: &str) -> bool {
    std::path::Path::new(name)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("html") || ext.eq_ignore_ascii_case("htm"))
}

/// A post for each html file just committed to a chat. `shot` stays null: the shooter fills it in later.
pub fn page_posts(
    hub: &Hub,
    chat: &str,
    person: Option<&str>,
    agent: Option<&str>,
    entry: &str,
    files: &[&SharedFile],
) {
    for file in files.iter().filter(|f| is_page(&f.name)) {
        host_post(
            hub,
            HostFact {
                chat: chat.to_owned(),
                person: person.map(str::to_owned),
                agent: agent.map(str::to_owned),
                entry: Some(entry.to_owned()),
                fold: Fold::Page { name: file.name.clone(), version: file.sha256.clone() },
                kind: "page".into(),
                title: fold::one_line(&file.name, 80),
                line: String::new(),
                attachment: Some(json!({"type": "page", "file": file, "shot": null, "version": file.sha256})),
                fact_at: now_ms(),
                fact_id: file.sha256.clone(),
            },
        );
    }
}

#[cfg(test)]
mod tests;
