//! The bot's ACP sessions: starting its agent, new/load/fork, thread sessions.

use super::{Actor, NoticeStyle, session_owner};
use crate::agent::acp;
use crate::agent::folder;
use crate::chat::{memory, tasks};
use crate::store::{Entry, EntryKind, Lane, lane_key};
use anyhow::{Result, anyhow, bail};
use serde_json::Value;
use std::time::Duration;

impl Actor {
    /// Which session a lane's turns use: a thread of the bot's own chat has its own
    /// (by root); otherwise the actor's own (`session_id`: its main one, or a group lane's).
    pub(super) fn slot(&self, lane: &Lane) -> Option<String> {
        (lane.chat == self.cfg.id).then(|| lane.thread.clone()).flatten()
    }

    /// The place a session in `slot` belongs to: a thread of the bot's own chat, or the actor's own place.
    pub(super) fn lane_of(&self, slot: Option<&str>) -> Lane {
        slot.map_or_else(|| self.home.clone(), |root| Lane::in_thread(&self.cfg.id, root))
    }

    pub(super) fn session_for(&mut self, slot: Option<&str>) -> Option<String> {
        let Some(root) = slot else { return self.session_id.clone() };
        if let Some(sid) = self.thread_sessions.get(root) {
            return Some(sid.clone());
        }
        let sid = self.hub.store.kv_get(&lane_key("session", &self.cfg.id, &Lane::in_thread(&self.cfg.id, root)));
        let sid = sid.filter(|s| !s.is_empty())?;
        self.thread_sessions.insert(root.to_owned(), sid.clone());
        Some(sid)
    }

    pub(super) fn set_session(&mut self, slot: Option<&str>, sid: Option<&str>) {
        // A session ended for good ends on a backup account too.
        if sid.is_none() {
            self.backup_sessions.retain(|(_, s), _| s != slot.unwrap_or_default());
        }
        let Some(root) = slot else {
            self.session_id = sid.map(str::to_owned);
            let stored = if self.is_main() {
                self.hub.store.set_session(&self.cfg.id, sid)
            } else {
                self.hub
                    .store
                    .kv_set(&lane_key("session", &session_owner(&self.cfg), &self.home), sid.unwrap_or_default())
            };
            if let Err(error) = stored {
                tracing::warn!(bot = %self.cfg.id, error = format!("{error:#}"), "couldn't store the session");
            }
            return;
        };
        match sid {
            Some(s) => self.thread_sessions.insert(root.to_owned(), s.to_owned()),
            None => self.thread_sessions.remove(root),
        };
        let key = lane_key("session", &self.cfg.id, &Lane::in_thread(&self.cfg.id, root));
        if let Err(error) = self.hub.store.kv_set(&key, sid.unwrap_or_default()) {
            tracing::warn!(bot = %self.cfg.id, error = format!("{error:#}"), "couldn't store the thread session");
        }
    }

    /// Roots of the thread sessions this actor answers for: a thread's actor, its own; the bot's own chat's, those
    /// of every thread without an actor of its own (loaded or only stored). Another actor's thread is that one's.
    pub(super) fn own_thread_roots(&self) -> Vec<String> {
        if !self.is_main() {
            return self.home.thread.iter().filter(|_| self.home.chat == self.cfg.id).cloned().collect();
        }
        let prefix = lane_key("session", &self.cfg.id, &Lane::main(&self.cfg.id)) + "/";
        let mut roots: Vec<String> = self.thread_sessions.keys().cloned().collect();
        roots.extend(
            self.hub.store.kv_prefix(&prefix).into_iter().filter_map(|k| k.strip_prefix(&prefix).map(str::to_owned)),
        );
        roots.sort();
        roots.dedup();
        roots.retain(|root| !self.hub.has_place(&self.cfg.id, &Lane::in_thread(&self.cfg.id, root)));
        roots
    }

    /// What a new thread session is told first: where the thread branches off. Nothing for a task: its first
    /// message is the root itself.
    pub(super) fn thread_intro(&self, root: &str, forked: bool) -> Option<String> {
        let quote = |e: &Entry| {
            let who = if e.kind == EntryKind::User.as_str() { "the user" } else { "you" };
            format!("{who}: \"{}\"", acp::truncate(e.data["text"].as_str().unwrap_or_default(), 1500))
        };
        let Some(root_entry) = self.hub.store.entry(root) else {
            return Some(
                "[Thread] The user opened a thread off your main chat. It is a separate branch of the conversation."
                    .into(),
            );
        };
        if tasks::is_task(&root_entry) {
            return None;
        }
        if forked {
            return Some(format!(
                "[Thread] The user opened a thread on this message from your main chat — {}. This thread is its own \
                 branch: it starts from everything said so far, and nothing said here reaches the main chat. \
                 Just continue the conversation; there's no need to mention this.",
                quote(&root_entry)
            ));
        }
        // A fresh session: bring the lines leading up to the message along.
        let before = self.hub.store.history(&self.cfg.id, root_entry.seq, 12).unwrap_or_default();
        let lines: Vec<String> = before
            .iter()
            .chain(std::iter::once(&root_entry))
            .filter(|e| {
                e.kind == EntryKind::User.as_str() || (e.kind == EntryKind::Agent.as_str() && e.data["final"] == true)
            })
            .map(quote)
            .collect();
        Some(format!(
            "[Thread] The user opened a thread on the last of these messages from your main chat (oldest first):\n{}\n\
             This thread is its own branch of the conversation; nothing said here reaches the main chat. \
             Just continue the conversation; there's no need to mention this.",
            lines.join("\n")
        ))
    }

    /// Makes sure the agent runs and `slot`'s session (main, or a thread's) is live;
    /// sets `turn_session`. A thread without one forks the main session when it can.
    pub(super) async fn ensure_session(&mut self, slot: Option<&str>) -> Result<()> {
        self.cfg.refuse_native()?;
        // A room never runs a turn of its own; every other row that passed the refusal is an hq agent.
        if !self.is_hq() {
            bail!("{} has no kith agent behind it", self.cfg.name);
        }
        self.ensure_hq_session(slot).await
    }

    /// A session request the agent doesn't answer in time stops the agent: a hung one
    /// would otherwise hold the bot, and its Stop, forever.
    pub(super) async fn session_request(&mut self, method: &str, params: Value) -> Result<Value> {
        let acp = self.conn.as_ref().ok_or_else(|| anyhow!("agent not running"))?.acp.clone();
        if let Ok(result) = tokio::time::timeout(SESSION_TIMEOUT, acp.request(method, params)).await {
            return result;
        }
        if let Some(c) = self.conn.take() {
            c.acp.kill().await;
        }
        let message = format!("the agent didn't answer {method} within {}s", SESSION_TIMEOUT.as_secs());
        Err(acp::Failure::error(acp::FailureKind::Timeout, message))
    }

    /// The folder of `lane` changed (or its bot's default did, and it follows it): the session there ends and its
    /// agent restarts in the new folder on the next turn. The stored session was dropped by the caller, so nothing
    /// loads the old one; a turn still running there stops first, and one that was cut off by a restart of the host
    /// doesn't resume into the new folder.
    pub(super) async fn reset_place(&mut self, lane: &Lane) {
        if *lane == self.home {
            self.reset_own().await;
            return;
        }
        // Another place's actor: the bot's own chat holds the sessions of threads without an actor of their own, and
        // may be running a turn there (a routine run in a thread).
        if self.turn.is_some() && self.lane == *lane {
            self.cancel_turn().await;
            if let Some(c) = self.conn.take() {
                c.acp.kill().await;
            }
        }
        // Cleared in the store too: a turn already starting its session would write the old id back.
        if lane.chat == self.cfg.id
            && let Some(root) = &lane.thread
        {
            self.set_session(Some(root), None);
        }
    }

    async fn reset_own(&mut self) {
        let slot = self.slot(&self.home);
        let had_session = self.session_for(slot.as_deref()).is_some();
        if self.turn.is_some() {
            self.cancel_turn().await;
        }
        if let Some(c) = self.conn.take() {
            c.acp.kill().await;
        }
        self.set_session(slot.as_deref(), None);
        self.thread_intro = None;
        if self.is_main() {
            let _ = self.tidier.send(memory::TidyEvent::Flush);
        }
        if had_session {
            self.notice("The working folder changed — the agent starts with a fresh context.", NoticeStyle::Divider);
        }
    }

    /// The bot's default folder changed: its places without a folder of their own start afresh in the new one.
    pub(super) async fn reset_inheriting(&mut self) {
        let inherits = |this: &Self, lane: &Lane| folder::Pick::load(&this.hub.store, &this.cfg.id, lane).is_none();
        if inherits(self, &self.home) {
            self.reset_own().await;
        }
        // A turn this actor runs for another place (a routine in a thread) follows that place's folder.
        let running = self.lane.clone();
        if self.turn.is_some() && running != self.home && inherits(self, &running) {
            self.reset_place(&running).await;
        }
        if self.is_main() {
            for root in self.own_thread_roots() {
                if inherits(self, &Lane::in_thread(&self.cfg.id, &root)) {
                    self.set_session(Some(&root), None);
                }
            }
        }
    }

    /// A terminal wrote to the session of `lane`'s thread: what this actor cached of it is stale, so the next turn
    /// loads the stored session again. A turn running there is left alone.
    pub(super) fn forget_loaded(&mut self, lane: &Lane) {
        let Some(root) = lane.thread.as_deref().filter(|_| lane.chat == self.cfg.id) else { return };
        if self.turn.is_some() && self.lane == *lane {
            return;
        }
        let stored = self.hub.store.kv_get(&lane_key("session", &self.cfg.id, lane)).filter(|s| !s.is_empty());
        let cached = self.thread_sessions.remove(root);
        if let Some(conn) = self.conn.as_mut() {
            for sid in cached.into_iter().chain(stored) {
                conn.loaded.remove(&sid);
            }
        }
        // Writer mode: the core's row for the dropped session goes on the next tick.
        self.writer_forgot();
    }

    /// Drops the session of the actor's own place so its next turn starts a fresh one.
    pub(super) fn forget_session(&mut self) {
        let slot = self.slot(&self.home);
        let old = self.session_for(slot.as_deref());
        self.set_session(slot.as_deref(), None);
        // Writer mode: a session the process no longer serves has no row (the process's idle release closes it later).
        if let (Some(old), Some(conn)) = (old, self.conn.as_mut().filter(|c| c.writer.is_some())) {
            conn.loaded.remove(&old);
            self.writer_forgot();
        }
    }
}

/// How long `session/new`, `session/load` and `session/fork` may take.
const SESSION_TIMEOUT: Duration = Duration::from_secs(180);
