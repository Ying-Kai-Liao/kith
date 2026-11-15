//! The bot's ACP sessions: starting its agent, new/load/fork, thread sessions.

use super::fast_mode::Listing;
use super::launch::{claude_meta, launch_commands, select_model, start_agent, system_prompt};
use super::{Actor, Conn, Instructions, NoticeStyle, session_owner};
use crate::agent::acp::{self, Incoming};
use crate::agent::{folder, own_memory};
use crate::chat::{context, memory, tasks};
use crate::store::{Entry, EntryKind, Lane, lane_key};
use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

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

    /// Whether the thread on `root` is a task (see `chat::tasks`): its session starts fresh, with nothing from the
    /// chat before it.
    pub(super) fn is_task(&self, root: &str) -> bool {
        self.hub.store.entry(root).is_some_and(|e| tasks::is_task(&e))
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

    /// What every session request carries: the folder and MCP servers, and the memory folders for an agent that
    /// takes folders besides its own.
    fn session_params(&self, cwd: &str, servers: &Value, session: Option<&str>) -> Value {
        let mut params = json!({"cwd": cwd, "mcpServers": servers});
        if let Some(sid) = session {
            params["sessionId"] = sid.into();
        }
        if self.conn.as_ref().is_some_and(|c| c.sessions.additional_directories) {
            params["additionalDirectories"] = json!(memory::files::folders(&self.cfg));
        }
        params
    }

    /// ACP connectors, team collaboration, and optional computer control.
    pub(super) fn mcp_servers(&self) -> Result<Value> {
        let Ok(exe) = std::env::current_exe() else {
            tracing::warn!("can't locate kith-serve for MCP servers");
            return Ok(json!([]));
        };
        let port = self.hub.port;
        let mut servers: Vec<Value> =
            (if self.cfg.connectors.is_empty() { Vec::new() } else { crate::market::connectors(&self.hub.store)? })
                .iter()
                .filter(|c| self.cfg.connectors.contains(&c.id))
                .filter_map(|c| c.acp(&exe, port))
                .collect();
        let composio = crate::market::composio::enabled_for(&self.hub.store, &self.cfg.connectors)?;
        let builtin = [
            Some("connectors"),
            Some("chat"),
            Some("team"),
            Some("memory"),
            Some("routines"),
            self.cfg.computer.then_some("computer"),
            composio.then_some("composio"),
        ];
        for name in builtin.into_iter().flatten() {
            let env = if name == "memory" {
                json!([{"name":"KITH_SERVE_MEMORY_LANE", "value":self.slot(&self.lane).unwrap_or_default()}])
            } else {
                json!([])
            };
            // `--bot` names this actor, so tool calls reach the turn of the place they came from.
            servers.push(json!({
                "name": name, "command": exe,
                "args": ["mcp", name, "--bot", self.rt, "--port", port.to_string()],
                "env": env,
            }));
        }
        Ok(Value::Array(servers))
    }

    /// Makes sure the agent runs and `slot`'s session (main, or a thread's) is live;
    /// sets `turn_session`. A thread without one forks the main session when it can.
    pub(super) async fn ensure_session(&mut self, slot: Option<&str>) -> Result<()> {
        if self.is_hq() {
            return self.ensure_hq_session(slot).await;
        }
        // Writer mode: a native bot's process runs under a resident stub too and claims as `bot:<id>` (B10).
        let resident = self.writer_align().await?;
        if self.cfg.connectors.is_empty() {
            // The key may sit in the envelope alone (saved after M2-7), so ask the vault, not serve.db.
            crate::market::vault::unlock_if_sealed(&self.hub, crate::agent::auth::env_slot(&self.cfg.backend)).await?;
        } else {
            crate::market::vault::unlock(self.hub.clone()).await?;
        }
        // Where this place works: its own pick, the bot's default project, or the bot's folder.
        let target = folder::resolve(&self.hub.store, &self.cfg, &self.lane_of(slot)).await;
        let live =
            self.session_for(slot).zip(self.conn.as_ref()).is_some_and(|(sid, conn)| conn.loaded.contains_key(&sid));
        if let Some(notice) = target.notice.as_deref().filter(|_| !live) {
            self.notice(notice, NoticeStyle::Divider);
        }
        let cwd = target.cwd;
        // An agent that can't resume keeps its session; its next new session gets the new tools.
        if std::mem::take(&mut self.tools_changed)
            && self.conn.as_ref().is_some_and(|c| c.sessions.load)
            && let Some(c) = self.conn.take()
        {
            c.acp.kill().await;
        }
        if self.conn.is_none() {
            let (hub, rt) = (self.hub.clone(), self.rt.clone());
            let mut candidates = launch_commands(&self.cfg, move |msg: &str| {
                let msg = msg.to_owned();
                hub.set_runtime(&rt, |r| r.activity = msg);
            })
            .await?;
            self.hub.set_runtime(&self.rt, |r| r.activity = "Starting agent…".into());
            // Keys saved from an "environment variable" sign-in.
            let mut env = crate::agent::auth::env(&self.hub.store, &self.cfg.backend)?;
            own_memory::apply(&self.cfg, &mut env, &mut candidates).await;
            if self.cfg.backend == "codex" && !env.iter().any(|(k, _)| k == "CODEX_CONFIG") {
                // Codex's `/compact` takes no instructions: the keep-list is its config's `compact_prompt`.
                let config = json!({"compact_prompt": kith_mem::compaction::KEEP});
                env.push(("CODEX_CONFIG".into(), config.to_string()));
            }
            let mut last_err = None;
            for (i, command) in candidates.iter().enumerate() {
                let fallback_left = i + 1 < candidates.len();
                // A local CLI too old for ACP just hangs; don't make the user wait long before the fallback.
                let budget = Duration::from_secs(if fallback_left { 20 } else { 180 });
                let started = if resident {
                    self.start_resident(command, &cwd, &env, budget, &self.cfg.backend).await
                } else {
                    start_agent(command, &cwd, &env, budget).await
                };
                match started {
                    Ok(conn) => {
                        self.conn = Some(conn);
                        break;
                    }
                    Err(e) => {
                        tracing::info!(command, error = format!("{e:#}"), "agent failed to start");
                        last_err = Some(e);
                    }
                }
            }
            if self.conn.is_none() {
                return Err(last_err.unwrap_or_else(|| anyhow!("agent failed to start")));
            }
        }
        let servers = self.mcp_servers()?;
        let instructions = self.conn.as_ref().map_or(Instructions::FirstMessage, Conn::instructions);
        let mut forked = false;
        // A thread forks the bot's main session as it is now; its own chat's actor may have moved it on. A task
        // starts fresh.
        if let Some(root) = slot
            && !self.is_task(root)
            && self.session_for(slot).is_none()
            // A thread in another folder starts fresh: the main session's conversation is about its own.
            && folder::effective(&self.hub.store, &self.cfg, &self.lane_of(slot)).pick
                == folder::effective(&self.hub.store, &self.cfg, &Lane::main(&self.cfg.id)).pick
            && let Some(main) = self.hub.store.bot(&self.cfg.id)?.and_then(|b| b.session_id)
            && self.conn.as_ref().is_some_and(|c| c.sessions.fork)
        {
            self.hub.set_runtime(&self.rt, |r| r.activity = "Opening the thread…".into());
            let mut params = self.session_params(&cwd, &servers, Some(&main));
            // The fork keeps the instructions its main session runs with, so it shares its cached prompt.
            let parent = self.snapshot(&main).await?;
            match instructions {
                Instructions::ClaudeMeta => params["_meta"] = claude_meta(&self.cfg, &parent.system),
                Instructions::SystemPrompt => params["systemPrompt"] = system_prompt(&parent.system),
                Instructions::FirstMessage => {}
            }
            let res = self.session_request("session/fork", params).await.ok();
            if let Some(res) = res
                && let Some(sid) = res["sessionId"].as_str().map(str::to_owned)
                && let Some(conn) = self.conn.as_mut()
            {
                conn.loaded.insert(sid.clone(), Instant::now());
                self.set_session(slot, Some(&sid));
                self.note_options(&sid, &res, Listing::Session);
                self.set_bot_mode(&sid, &res).await;
                context::adopt(&self.hub.store, &self.cfg, &sid, parent.system.clone(), parent.memory.clone())?;
                memory::opened(&self.hub.store, &self.cfg.id, &sid, &parent.memory)?;
                if instructions == Instructions::ClaudeMeta {
                    self.applied_system.insert(sid, parent.system);
                }
                forked = true;
            } else {
                tracing::warn!(bot = %self.cfg.id, "session/fork failed; the thread starts fresh");
            }
            if forked {
                self.thread_intro = self.thread_intro(root, true);
            }
        }
        // Claude gets the frozen instructions as its system prompt. When a compaction
        // re-rendered them, loading the session again swaps the prompt in (the adapter
        // rebuilds its query and resumes the same conversation).
        let current = self.session_for(slot);
        let snapshot = match (&current, instructions) {
            (Some(sid), Instructions::ClaudeMeta) => Some(self.snapshot(sid).await?),
            _ => None,
        };
        let Some(conn) = self.conn.as_ref() else { bail!("agent not running") };
        if let Some(sid) = current {
            let prompt_current =
                snapshot.as_ref().is_none_or(|s| self.applied_system.get(&sid).is_some_and(|t| *t == s.system));
            if conn.loaded.contains_key(&sid) && prompt_current {
                self.turn_session = Some(sid);
                return Ok(());
            }
            if !conn.sessions.load {
                self.notice(
                    "This agent can't resume sessions after a restart — starting a fresh one.",
                    NoticeStyle::Divider,
                );
                self.set_session(slot, None);
                return Box::pin(self.ensure_session(slot)).await;
            }
            self.hub.set_runtime(&self.rt, |r| r.activity = "Resuming session…".into());
            let mut params = self.session_params(&cwd, &servers, Some(&sid));
            if let Some(s) = &snapshot {
                params["_meta"] = claude_meta(&self.cfg, &s.system);
            }
            if instructions == Instructions::SystemPrompt {
                params["systemPrompt"] = system_prompt(&self.snapshot(&sid).await?.system);
            }
            // Writer mode: the process's row for the session, before the load names it.
            self.warm_session(&sid).await?;
            let res = self.session_request("session/load", params).await;
            let Some(conn) = self.conn.as_mut() else { return res.map(drop) };
            // The agent replays history as session/update before answering; drop it.
            while let Ok(inc) = conn.rx.try_recv() {
                if let Incoming::Request { id, .. } = inc {
                    let _ = conn.acp.respond_error(id, -32601, "not supported").await;
                }
            }
            match res {
                Ok(res) => {
                    let loaded = res.clone();
                    // A resumed session can come back on the agent's last-used model (Cursor
                    // keeps one for all its sessions), so the bot's own is set again.
                    let res = if res.get("configOptions").is_some() || res.get("models").is_some() {
                        let acp = conn.acp.clone();
                        let selected = select_model(&acp, &res, &sid, &self.cfg).await;
                        self.drop_if_silent(selected)?
                    } else {
                        res
                    };
                    let Some(conn) = self.conn.as_mut() else { bail!("agent not running") };
                    conn.loaded.insert(sid.clone(), Instant::now());
                    self.note_options(&sid, &res, Listing::Session);
                    self.set_bot_mode(&sid, &loaded).await;
                    if let Some(s) = snapshot {
                        // Its system prompt now names the bot and holds memory as rendered.
                        context::mark_announced(&self.hub.store, &self.cfg.id, &s, s.system_identity.clone())?;
                        memory::opened(&self.hub.store, &self.cfg.id, &sid, &s.memory)?;
                        self.applied_system.insert(sid.clone(), s.system);
                    }
                    self.turn_session = Some(sid);
                    return Ok(());
                }
                Err(e) => {
                    tracing::warn!(error = format!("{e:#}"), "session/load failed; starting fresh");
                    self.thread_intro = None;
                    self.notice("Couldn't resume the previous session — starting a fresh one.", NoticeStyle::Divider);
                }
            }
        }
        let (revision, system) = {
            let (hub, cfg, room) = (self.hub.clone(), self.cfg.clone(), self.memory_room());
            tokio::task::spawn_blocking(move || {
                (memory::current_revision(&hub.store, &cfg.id), context::render(&hub.store, &cfg, room.as_deref()))
            })
            .await?
        };
        let mut params = self.session_params(&cwd, &servers, None);
        match instructions {
            Instructions::ClaudeMeta => params["_meta"] = claude_meta(&self.cfg, &system),
            Instructions::SystemPrompt => params["systemPrompt"] = system_prompt(&system),
            Instructions::FirstMessage => {}
        }
        let res = self.session_request("session/new", params).await?;
        let Some(conn) = self.conn.as_mut() else { bail!("agent not running") };
        let sid = res["sessionId"].as_str().ok_or_else(|| anyhow!("agent returned no sessionId"))?.to_owned();
        conn.loaded.insert(sid.clone(), Instant::now());
        let acp = conn.acp.clone();
        let selected = select_model(&acp, &res, &sid, &self.cfg).await;
        let options = self.drop_if_silent(selected)?;
        self.note_options(&sid, &options, Listing::Session);
        self.set_bot_mode(&sid, &res).await;
        self.set_session(slot, Some(&sid));
        self.session_fresh = true;
        self.turn_session = Some(sid.clone());
        if let Some(root) = slot {
            self.thread_intro = self.thread_intro(root, false);
        }
        context::adopt(&self.hub.store, &self.cfg, &sid, system.clone(), revision)?;
        if instructions == Instructions::ClaudeMeta {
            self.applied_system.insert(sid, system);
        }
        Ok(())
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

    /// A model switch the agent never answered stopped its process (see `timed_request`): forget the connection, so
    /// the next turn starts a fresh agent instead of writing to a dead one.
    fn drop_if_silent<T>(&mut self, result: Result<T>) -> Result<T> {
        if let Err(e) = &result
            && super::launch::timed_out(e)
        {
            self.conn = None;
        }
        result
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
        self.handoff = None;
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
