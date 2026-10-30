//! Shared host state: store, event fan-out, bot actors, push, usage.

use crate::LockExt;
use crate::agent::bot::{self, Cmd};
use crate::agent::folder;
use crate::api::devices::Pairing;
use crate::remote::identity::Identity;
use crate::remote::push;
use crate::screen::Screen;
use crate::service;
use crate::store::{BotConfig, Entry, EntryKind, Lane, ReadScope, Store};
use crate::usage::Usage;
use anyhow::{Result, anyhow, bail};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use tokio::sync::{broadcast, watch};

mod actors;
mod folders;
pub(crate) mod hq;
mod order;
mod sync;

/// What a bot is doing right now (wire values: `idle` / `working` / `needsInput` / `error`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum BotStatus {
    #[default]
    Idle,
    Working,
    NeedsInput,
    Error,
}

#[derive(Clone, Default, Debug)]
pub struct Runtime {
    pub status: BotStatus,
    pub activity: String,
    pub started_at: Option<i64>,
    /// Where the running turn talks: its own chat, a thread, or a group.
    pub lane: Option<Lane>,
    /// The user's message that started the running turn; `None` for a turn a routine, a room or another bot
    /// started. Memory cites it as where a fact was learned.
    pub message: Option<String>,
    /// The ACP session the running turn prompts. A memory write made there is one that session already knows.
    pub session: Option<String>,
    pub fast_mode: crate::agent::config::FastModeSupport,
}

/// Reactions per message: a handful of distinct emoji.
const MAX_REACTIONS: usize = 8;

pub struct Hub {
    pub analytics: crate::analytics::Analytics,
    pub routines: crate::routines::Routines,
    pub team: crate::chat::team::Requests,
    pub calls: crate::calls::Calls,
    pub store: Store,
    pub host_id: String,
    pub identity: Identity,
    /// Loopback-only bearer token for this computer's own apps and helpers.
    pub token: String,
    pub port: u16,
    pub events: broadcast::Sender<Value>,
    /// Held across "mutate store + broadcast" so events leave in rev order.
    emit_lock: Mutex<()>,
    /// Bot id -> its actors, one per place it talks (see `actors`).
    bots: Mutex<HashMap<String, actors::BotActors>>,
    /// Actor key (see `agent::bot::actor_key`) -> what that actor is doing.
    runtime: Mutex<HashMap<String, Runtime>>,
    /// This hub, for starting actors from `&self`.
    me: Weak<Hub>,
    pub phone_clients: AtomicUsize,
    pub usage: Mutex<Usage>,
    /// Which backup subscriptions are out of usage until when (see `agent::backups`).
    pub backups: Arc<crate::agent::backups::Backups>,
    keep_awake: Mutex<service::KeepAwake>,
    /// Generation of the authorized-device table and pairing offers: bumped on every
    /// change so open channels re-check their device (and the ACL is republished).
    pub auth: watch::Sender<u64>,
    pub pairing: Mutex<Pairing>,
    /// Device key -> open channels that have exchanged at least one frame.
    pub connected: Mutex<HashMap<String, usize>>,
    /// The cloud's `/v1/host/state` was applied on the current relay connection;
    /// account devices are refused until then (§4.3).
    pub cloud_synced: AtomicBool,
    /// Bumped when the cloud says its state changed: a pull that started before it can't mark this host in sync.
    pub cloud_changes: std::sync::atomic::AtomicU64,
    pub cloud: crate::remote::cloud::Cloud,
    pub screen: Arc<Screen>,
    /// Setup terminals (installs, sign-ins).
    pub terms: Arc<crate::agent::term::Terms>,
    /// One sync of the hq bots at a time (see `hq`).
    hq_sync: tokio::sync::Mutex<()>,
    /// Terminal sessions shown as threads (see `terminal`).
    pub terminal: crate::terminal::Terminal,
    /// How many agent turns run at once, and whether memory has room for another.
    pub gate: Arc<crate::agent::gate::Gate>,
    /// Transcript entries the store refused (disk full, read-only), waiting to be written again.
    unsaved: Mutex<Unsaved>,
}

/// How many unsaved entries the host holds; the oldest is dropped (and counted) past it.
const MAX_UNSAVED: usize = 500;
/// How often unsaved entries are tried again when nothing new arrives.
const RETRY_UNSAVED: std::time::Duration = std::time::Duration::from_secs(15);

struct PendingEntry {
    id: String,
    lane: Lane,
    kind: EntryKind,
    turn: i64,
    data: Value,
    at: i64,
}

#[derive(Default)]
struct Unsaved {
    queue: std::collections::VecDeque<PendingEntry>,
    /// Entries dropped for lack of room since the host started.
    dropped: u64,
    /// The store's last write error while it is failing; `None` once a write lands.
    error: Option<String>,
}

impl Hub {
    pub fn busy(&self) -> bool {
        self.runtime.locked().values().any(|r| matches!(r.status, BotStatus::Working | BotStatus::NeedsInput))
            || self.terms.busy()
    }

    pub fn new(store: Store, host_id: String, identity: Identity, token: String, port: u16) -> Arc<Self> {
        Self::new_with_gate(store, host_id, identity, token, port, new_gate())
    }

    pub fn new_with_gate(
        store: Store,
        host_id: String,
        identity: Identity,
        token: String,
        port: u16,
        gate: Arc<crate::agent::gate::Gate>,
    ) -> Arc<Self> {
        let (events, _) = broadcast::channel(1024);
        let screen = Arc::new(Screen::new(Screen::load_enabled(&store), events.clone()));
        Arc::new_cyclic(|me| Self {
            me: me.clone(),
            analytics: crate::analytics::Analytics::load(&store),
            routines: crate::routines::Routines::default(),
            team: crate::chat::team::Requests::default(),
            calls: crate::calls::Calls::for_host(),
            store,
            host_id,
            identity,
            token,
            port,
            events,
            emit_lock: Mutex::new(()),
            bots: Mutex::default(),
            runtime: Mutex::default(),
            phone_clients: AtomicUsize::new(0),
            usage: Mutex::default(),
            backups: Arc::new(crate::agent::backups::Backups::new(service::data_dir().join("backups.json"))),
            keep_awake: Mutex::default(),
            auth: watch::Sender::new(0),
            pairing: Mutex::default(),
            connected: Mutex::default(),
            cloud_synced: AtomicBool::new(false),
            cloud_changes: std::sync::atomic::AtomicU64::new(0),
            cloud: crate::remote::cloud::Cloud::default(),
            screen,
            terms: Arc::default(),
            hq_sync: tokio::sync::Mutex::new(()),
            terminal: crate::terminal::Terminal::default(),
            gate,
            unsaved: Mutex::default(),
        })
    }

    pub fn start(self: &Arc<Self>) -> Result<()> {
        // Approval cards and queued sends from a previous run can't be honored anymore.
        self.store.expire_pending()?;
        crate::chat::memory::import_old_memory(self);
        self.routines.restore(&self.store)?;
        crate::market::turn_on_installed_once(&self.store)?;
        for row in self.store.bots()? {
            if row.deleted {
                self.routines.disable_bot(&self.store, &row.config.id)?;
            } else if !row.config.is_group() {
                self.spawn_bot(&row.config);
            }
        }
        self.spawn_unsaved_retry();
        // Room turns a restart cut off go on (their speaker first, not charged again).
        crate::chat::group::resume(self)?;
        tokio::spawn(crate::agent::gate::run(self.gate.clone(), Arc::downgrade(self)));
        crate::routines::start(self);
        crate::run_outbox::start(self);
        crate::analytics::start(self);
        hq::start(self);
        crate::terminal::start(self);
        Ok(())
    }

    // MARK: bots

    pub fn bot_name(&self, id: &str) -> String {
        self.store.bot(id).ok().flatten().map_or_else(|| "A deleted bot".into(), |b| b.config.name)
    }

    pub fn create_bot(self: &Arc<Self>, mut cfg: BotConfig) -> Result<Value> {
        cfg.id = uuid::Uuid::new_v4().to_string();
        cfg.created_at = crate::store::now_ms();
        cfg.position = self.new_bot_position()?;
        // No name yet (Grok Bot's flow): it gets one after its first few conversations.
        cfg.auto_name = !cfg.is_group() && cfg.name.trim().is_empty();
        if cfg.auto_name {
            crate::chat::naming::PLACEHOLDER.clone_into(&mut cfg.name);
        }
        if cfg.is_group() {
            cfg.members = crate::chat::group::valid_members(&self.store, &cfg.id, &cfg.members)?;
            // The same bots again: open the group they already share (Grok Bot's rule).
            if let Some(existing) = crate::chat::group::with_members(&self.store, &cfg.members)? {
                return Ok(self.bot_value(&existing).unwrap_or(Value::Null));
            }
        }
        cfg.clean_avatar_source();
        cfg.clean_project();
        validate(&cfg)?;
        crate::agent::workspace::prepare(&mut cfg)?;
        {
            let _g = self.emit_lock.locked();
            self.store.save_bot(&cfg)?;
        }
        if cfg.is_group() {
            crate::chat::group::members_changed(self, &cfg.id, &[], &cfg.members);
        } else {
            self.spawn_bot(&cfg);
        }
        self.emit_bot(&cfg.id);
        Ok(self.bot_value(&cfg.id).unwrap_or(Value::Null))
    }

    /// Adds a group the host made itself (`samples`), as it is: none of `create_bot`'s checks or reuse.
    pub fn add_host_group(&self, mut cfg: BotConfig) -> Result<()> {
        if !cfg.is_group() {
            bail!("only a group");
        }
        cfg.position = self.new_bot_position()?;
        {
            let _g = self.emit_lock.locked();
            self.store.save_bot(&cfg)?;
        }
        crate::chat::group::members_changed(self, &cfg.id, &[], &cfg.members);
        self.emit_bot(&cfg.id);
        Ok(())
    }

    pub fn update_bot(&self, patch: &Value) -> Result<Value> {
        let id = patch["id"].as_str().ok_or_else(|| anyhow!("id required"))?.to_owned();
        let row = self.store.bot(&id)?.filter(|b| !b.deleted).ok_or_else(|| anyhow!("unknown bot"))?;
        let mut merged = serde_json::to_value(&row.config)?;
        for (k, v) in patch.as_object().into_iter().flatten() {
            if k != "id" && k != "createdAt" {
                merged[k] = v.clone();
            }
        }
        let mut cfg: BotConfig = serde_json::from_value(merged)?;
        // Which hq agent a bot is can't be edited, and it runs on Claude.
        cfg.hq_agent.clone_from(&row.config.hq_agent);
        // Whose chat it is can't be edited either.
        cfg.person.clone_from(&row.config.person);
        if cfg.is_hq() {
            cfg.backend.clone_from(&row.config.backend);
            cfg.command = None;
            // Model, effort, Fast mode and permission are hq's; an app's (stale) copy doesn't get stored over them.
            cfg.model.clone_from(&row.config.model);
            cfg.model_options.clone_from(&row.config.model_options);
            cfg.fast_mode = row.config.fast_mode;
            cfg.permission = row.config.permission;
        }
        if cfg.name.trim().is_empty() {
            cfg.name.clone_from(&row.config.name);
        }
        // A name the user picked ends automatic naming; the namer passes `autoName` itself.
        if cfg.name != row.config.name && patch.get("autoName").is_none() {
            cfg.auto_name = false;
        }
        // Options belong to one model; an app that doesn't know them can't clear them itself.
        if (cfg.model != row.config.model || cfg.backend != row.config.backend) && patch.get("modelOptions").is_none() {
            cfg.model_options.clear();
        }
        if cfg.kind != row.config.kind {
            bail!("a bot can't become a group, or a group a bot");
        }
        if cfg.is_group() {
            cfg.members = crate::chat::group::valid_members(&self.store, &cfg.id, &cfg.members)?;
        }
        cfg.clean_avatar_source();
        cfg.clean_project();
        validate(&cfg)?;
        crate::agent::workspace::prepare(&mut cfg)?;
        crate::agent::own_memory::settle(&mut cfg, &row.config, patch);
        let default_moved = !cfg.is_group() && folder::default_of(&cfg) != folder::default_of(&row.config);
        {
            let _g = self.emit_lock.locked();
            self.store.save_bot(&cfg)?;
        }
        // Places without a folder of their own follow the bot's: their stored sessions end before the actors learn.
        if default_moved {
            self.reset_inheriting_stored(&cfg);
        }
        if cfg.is_group() {
            crate::chat::group::members_changed(self, &id, &row.config.members, &cfg.members);
            self.close_group_places(&id, &cfg.members);
            // An outside bot taken out of the room is no longer waited for.
            crate::chat::group::cancel_bot_turns(|t| t.lane.chat == id && !cfg.members.contains(&t.bot_id));
        } else {
            if crate::chat::memory::shared(&cfg) != crate::chat::memory::shared(&row.config) {
                crate::chat::memory::changed(self, Some(&id));
            }
            self.send_cmd(&id, Cmd::Reconfigure(Box::new(cfg)))?;
        }
        self.emit_bot(&id);
        Ok(self.bot_value(&id).unwrap_or(Value::Null))
    }

    pub fn delete_bot(&self, id: &str) -> Result<()> {
        self.routines.disable_bot(&self.store, id)?;
        self.team.cancel_bot(id);
        crate::chat::group::stop(self, id);
        self.store.drop_room(id)?;
        self.remove_bot_actors(id);
        {
            let _g = self.emit_lock.locked();
            let rev = self.store.delete_bot(id)?;
            let _ =
                self.events.send(json!({"type": "bot", "rev": rev, "bot": {"id": id, "deleted": true, "rev": rev}}));
        }
        // Its shared files are unreachable once the bot is deleted.
        let files = crate::chat::files::root(id);
        let bot = id.to_owned();
        tokio::task::spawn_blocking(move || {
            crate::chat::memory::files::remove_bot(&bot);
            if let Err(error) = std::fs::remove_dir_all(&files)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(error = %error, "couldn't remove a deleted bot's shared files");
            }
        });
        // A deleted bot leaves its groups; what it said there stays.
        for row in self.store.bots()? {
            let mut group = row.config;
            if !row.deleted && group.is_group() && group.members.iter().any(|m| m == id) {
                group.members.retain(|m| m != id);
                {
                    let _g = self.emit_lock.locked();
                    self.store.save_bot(&group)?;
                }
                self.emit_bot(&group.id);
            }
        }
        Ok(())
    }

    /// Marks what the user has seen as read. Nothing unread there is a no-op, so clients
    /// may call it whenever a chat or thread is on screen.
    pub fn mark_read(&self, id: &str, scope: ReadScope) -> Result<()> {
        let Some(row) = self.store.bot(id)? else { bail!("unknown bot") };
        if let ReadScope::Thread(root) = scope
            && !self.store.entry(root).is_some_and(|r| r.bot_id == id && r.thread_id.is_none())
        {
            bail!("unknown thread");
        }
        // A thread gets its own read mark the first time, or the main chat's would cover it.
        let first = matches!(scope, ReadScope::Thread(root) if self.store.kv_get(&format!("read.{root}")).is_none());
        if !first && self.store.unread(id, row.read_rev, scope) == 0 {
            return Ok(());
        }
        let roots = match scope {
            ReadScope::All => self.store.unread_roots(id),
            ReadScope::Thread(root) => vec![root.to_owned()],
            ReadScope::Chat => Vec::new(),
        };
        {
            let _g = self.emit_lock.locked();
            match scope {
                ReadScope::All => self.store.mark_all_read(id)?,
                ReadScope::Chat => self.store.mark_read(id)?,
                ReadScope::Thread(root) => self.store.mark_thread_read(id, root)?,
            };
            // Before the summaries' later revs: a client resuming after them must not miss the row.
            if let Some(v) = self.bot_value(id) {
                let _ = self.events.send(json!({"type": "bot", "rev": v["rev"], "bot": v}));
            }
        }
        for root in roots {
            self.refresh_thread_summary(id, &root);
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn bot_value_for_test(&self, id: &str) -> Value {
        self.bot_value(id).unwrap_or(Value::Null)
    }

    fn bot_value(&self, id: &str) -> Option<Value> {
        self.store.bot(id).ok().flatten().map(|r| self.bot_json(&r))
    }

    /// Bumps the bot's rev and broadcasts its current state.
    pub fn emit_bot(&self, id: &str) {
        let _g = self.emit_lock.locked();
        if self.store.touch_bot(id).is_ok()
            && let Some(v) = self.bot_value(id)
        {
            let _ = self.events.send(json!({"type": "bot", "rev": v["rev"], "bot": v}));
        }
    }

    /// Updates one actor's runtime (`key`: see `agent::bot::actor_key`) and tells clients about its bot.
    pub fn set_runtime(&self, key: &str, f: impl FnOnce(&mut Runtime)) {
        let (before, after) = {
            let mut map = self.runtime.locked();
            let rt = map.entry(key.to_owned()).or_default();
            let before = rt.clone();
            f(rt);
            (before, rt.clone())
        };
        let id = bot::bot_of(key);
        if before.status != after.status
            || before.activity != after.activity
            || before.lane != after.lane
            || before.fast_mode != after.fast_mode
        {
            self.emit_bot(id);
            // A group shows what its busy member is doing.
            for lane in [&before.lane, &after.lane].into_iter().flatten() {
                if lane.chat != id {
                    self.emit_bot(&lane.chat);
                }
            }
            self.update_keep_awake();
        }
        if before.status != after.status {
            push::live_activity_update(self, id, &self.bot_runtime(id));
            // A group's Live Activity follows its busy member.
            for lane in [&before.lane, &after.lane].into_iter().flatten().filter(|l| l.chat != id) {
                if let Ok(Some(group)) = self.store.bot(&lane.chat) {
                    push::live_activity_update(self, &lane.chat, &self.group_runtime(&group.config));
                }
            }
        }
    }

    /// One actor's runtime (`key`: a bot's id is its own chat's actor); [`Self::bot_runtime`] is the whole bot's.
    pub fn runtime(&self, key: &str) -> Runtime {
        self.runtime.locked().get(key).cloned().unwrap_or_default()
    }

    pub(crate) fn update_keep_awake(&self) {
        let busy = self.runtime.locked().values().any(|r| r.status == BotStatus::Working);
        self.keep_awake.locked().set(busy || self.routines.keeps_awake());
    }

    // MARK: entries

    /// Persists and broadcasts a new transcript entry. A write the store refuses (disk full, read-only) is logged,
    /// kept in memory (bounded) and retried in order (see [`Self::retry_unsaved`]); it yields `None` and the
    /// conversation goes on. Held entries are lost if the host restarts first: there is no disk to keep them on.
    pub fn add_entry(&self, lane: &Lane, kind: EntryKind, turn: i64, data: &Value) -> Option<Entry> {
        let (added, mut landed) = {
            let _g = self.emit_lock.locked();
            // Older held entries go first, so a thread's lines stay in order.
            let landed = self.flush_unsaved();
            let pending = PendingEntry {
                id: uuid::Uuid::new_v4().to_string(),
                lane: lane.clone(),
                kind,
                turn,
                data: data.clone(),
                at: crate::store::now_ms(),
            };
            let added = if self.unsaved.locked().queue.is_empty() {
                match self.write_pending(&pending) {
                    Ok(e) => Some(e),
                    Err(error) => {
                        self.hold(pending, &error);
                        None
                    }
                }
            } else {
                // Still failing: queue behind the others rather than jump ahead of them.
                tracing::warn!(chat = pending.lane.chat, "store still can't write; holding transcript entry");
                self.hold_queued(pending);
                None
            };
            (added, landed)
        };
        landed.extend(added.clone());
        for e in &landed {
            self.update_thread_summary(e);
        }
        added
    }

    /// Writes and broadcasts one entry, under `emit_lock`. Clears the store flag when the write lands.
    fn write_pending(&self, p: &PendingEntry) -> Result<Entry> {
        // An entry stored by an attempt that reported an error comes back as it is: no duplicate row.
        let (e, _) = self.store.insert_entry_as(&p.id, &p.lane, p.kind, p.turn, &p.data, p.at)?;
        self.set_store_error(None);
        let _ = self.events.send(json!({"type": "entry", "rev": e.rev, "entry": e}));
        Ok(e)
    }

    /// Whether the store refused a write (as opposed to a rule such as "the chat was deleted").
    fn is_store_error(error: &anyhow::Error) -> bool {
        error.downcast_ref::<rusqlite::Error>().is_some()
    }

    /// Keeps an entry the store refused for a retry, unless it was refused for good.
    fn hold(&self, p: PendingEntry, error: &anyhow::Error) {
        tracing::error!(
            error = format!("{error:#}"),
            chat = p.lane.chat,
            kind = p.kind.as_str(),
            "couldn't save transcript entry"
        );
        if Self::is_store_error(error) {
            self.set_store_error(Some(format!("{error:#}")));
            self.hold_queued(p);
        }
    }

    fn hold_queued(&self, p: PendingEntry) {
        let mut u = self.unsaved.locked();
        if u.queue.len() >= MAX_UNSAVED {
            u.queue.pop_front();
            u.dropped += 1;
            tracing::warn!(dropped = u.dropped, "unsaved transcript entries over the limit; dropped the oldest");
        }
        u.queue.push_back(p);
    }

    /// Tries the held entries oldest first, under `emit_lock`; stops at the first the store still refuses.
    /// Entries that landed are returned for their threads' summaries.
    fn flush_unsaved(&self) -> Vec<Entry> {
        let mut landed = Vec::new();
        loop {
            let Some(next) = self.unsaved.locked().queue.pop_front() else { break };
            match self.write_pending(&next) {
                Ok(e) => landed.push(e),
                Err(error) if Self::is_store_error(&error) => {
                    self.set_store_error(Some(format!("{error:#}")));
                    self.unsaved.locked().queue.push_front(next);
                    break;
                }
                // Its chat was deleted meanwhile: nothing to write it to.
                Err(error) => tracing::warn!(error = format!("{error:#}"), "dropping an unsaved transcript entry"),
            }
        }
        landed
    }

    /// Writes held entries again; the host calls it periodically, and every new entry does the same.
    pub fn retry_unsaved(&self) {
        if self.unsaved.locked().queue.is_empty() {
            return;
        }
        let landed = {
            let _g = self.emit_lock.locked();
            self.flush_unsaved()
        };
        for e in &landed {
            self.update_thread_summary(e);
        }
    }

    fn spawn_unsaved_retry(self: &Arc<Self>) {
        let me = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(RETRY_UNSAVED).await;
                let Some(hub) = me.upgrade() else { break };
                let _ = tokio::task::spawn_blocking(move || hub.retry_unsaved()).await;
            }
        });
    }

    /// Sets or clears the store-can't-write flag and tells connected apps when it changes.
    fn set_store_error(&self, error: Option<String>) {
        let changed = {
            let mut u = self.unsaved.locked();
            let changed = u.error.is_some() != error.is_some();
            u.error = error;
            changed
        };
        if changed {
            let _ = self.events.send(json!({"type": "storeHealth", "store": self.store_health(false)}));
        }
    }

    /// The store's health, for `hello` and the `storeHealth` event: whether it takes writes, how many entries wait
    /// to be saved and how many were dropped. `detail` adds the error text (which can name this computer's paths).
    pub fn store_health(&self, detail: bool) -> Value {
        let u = self.unsaved.locked();
        let mut v = json!({"writable": u.error.is_none(), "unsaved": u.queue.len(), "dropped": u.dropped});
        if detail {
            v["error"] = json!(u.error);
        }
        v
    }

    /// [`Self::add_entry`] stamped `at` (ms), without refreshing the thread's summary: the caller imports many
    /// lines and refreshes it once ([`Self::refresh_thread_summary`]).
    pub fn add_entry_at(&self, lane: &Lane, kind: EntryKind, turn: i64, data: &Value, at: i64) -> Option<Entry> {
        let _g = self.emit_lock.locked();
        match self.store.insert_entry_at(lane, kind, turn, data, at) {
            Ok(e) => {
                let _ = self.events.send(json!({"type": "entry", "rev": e.rev, "entry": e}));
                Some(e)
            }
            Err(error) => {
                tracing::error!(error = format!("{error:#}"), chat = lane.chat, "couldn't save imported entry");
                None
            }
        }
    }

    pub fn add_entry_once(&self, id: &str, lane: &Lane, kind: EntryKind, data: &Value) -> Result<(Entry, bool)> {
        let (entry, added) = {
            let _guard = self.emit_lock.locked();
            let (entry, added) = self.store.insert_entry_once(id, lane, kind, data)?;
            if added {
                let _ = self.events.send(json!({"type":"entry","rev":entry.rev,"entry":entry}));
            }
            (entry, added)
        };
        if added {
            self.update_thread_summary(&entry);
        }
        Ok((entry, added))
    }

    /// Updates and broadcasts an entry; failures are logged here, like [`Self::add_entry`].
    pub fn set_entry(&self, id: &str, data: &Value) -> Option<Entry> {
        let updated = {
            let _g = self.emit_lock.locked();
            match self.store.update_entry(id, data) {
                Ok(e) => {
                    if let Some(e) = &e {
                        let _ = self.events.send(json!({"type": "entry", "rev": e.rev, "entry": e}));
                    }
                    e
                }
                Err(error) => {
                    tracing::error!(error = format!("{error:#}"), entry = id, "couldn't update transcript entry");
                    None
                }
            }
        };
        if let Some(e) = &updated {
            self.update_thread_summary(e);
        }
        updated
    }

    /// Toggles the user's `emoji` reaction on a chat message (`data.reactions`, oldest first).
    /// A message keeps `reactions: []` once reacted to, so it never turns unread again.
    pub fn react(&self, id: &str, emoji: &str) -> Result<Entry> {
        let emoji = emoji.trim();
        if emoji.is_empty() || emoji.len() > 32 || emoji.chars().any(|c| c.is_alphanumeric() || c.is_whitespace()) {
            bail!("`emoji` must be one emoji");
        }
        let mut e = self.store.entry(id).ok_or_else(|| anyhow!("unknown message"))?;
        let visible =
            e.kind == EntryKind::User.as_str() || (e.kind == EntryKind::Agent.as_str() && e.data["final"] == true);
        if !visible {
            bail!("only chat messages take reactions");
        }
        let mut reactions: Vec<String> = serde_json::from_value(e.data["reactions"].clone()).unwrap_or_default();
        if let Some(i) = reactions.iter().position(|r| r == emoji) {
            reactions.remove(i);
        } else if reactions.len() >= MAX_REACTIONS {
            bail!("a message takes at most {MAX_REACTIONS} reactions");
        } else {
            reactions.push(emoji.to_owned());
        }
        e.data["reactions"] = json!(reactions);
        self.set_entry(id, &e.data).ok_or_else(|| anyhow!("couldn't save the reaction"))
    }

    /// A message in a thread refreshes the summary on its root (`data.thread`: reply count,
    /// newest reply, who replied, unread replies), which the main chat shows under it.
    fn update_thread_summary(&self, e: &Entry) {
        let Some(root) = &e.thread_id else { return };
        let visible =
            e.kind == EntryKind::User.as_str() || (e.kind == EntryKind::Agent.as_str() && e.data["final"] == true);
        if visible {
            self.refresh_thread_summary(&e.bot_id, root);
        }
    }

    /// Says how the task rooted at `lane`'s thread stands (`data.thread.status`), for clients to show. Threads that
    /// aren't host-driven tasks (a terminal's, a bot guest's) are left alone: clients derive their status.
    pub(crate) fn set_task_status(&self, lane: &Lane, state: crate::chat::tasks::TaskState) {
        let Some(root) = &lane.thread else { return };
        let Some(mut root_entry) = self.store.entry(root) else { return };
        if !crate::chat::tasks::is_task(&root_entry) || root_entry.data["thread"]["origin"] == "terminal" {
            return;
        }
        match self.store.thread_summary(&lane.chat, root) {
            Ok(mut summary) => {
                crate::terminal::carry_over(&root_entry.data["thread"], &mut summary);
                summary["status"] = json!(state);
                if root_entry.data["thread"] != summary {
                    root_entry.data["thread"] = summary;
                    self.set_entry(root, &root_entry.data);
                }
            }
            Err(error) => tracing::warn!(error = format!("{error:#}"), root, "couldn't set the task status"),
        }
    }

    pub(crate) fn refresh_thread_summary(&self, chat: &str, root: &str) {
        let Some(mut root_entry) = self.store.entry(root) else { return };
        match self.store.thread_summary(chat, root) {
            Ok(mut summary) => {
                // What the terminal says about the thread (see `terminal`) isn't counted from entries.
                crate::terminal::carry_over(&root_entry.data["thread"], &mut summary);
                // The host's say on how the task stands (see `chat::tasks::TaskState`) isn't counted from entries either.
                if !root_entry.data["thread"]["status"].is_null() {
                    summary["status"] = root_entry.data["thread"]["status"].clone();
                }
                if root_entry.data["thread"] != summary {
                    root_entry.data["thread"] = summary;
                    self.set_entry(root, &root_entry.data);
                }
            }
            Err(error) => tracing::warn!(error = format!("{error:#}"), root, "couldn't summarize thread"),
        }
    }

    pub fn set_usage(&self, usage: Usage) {
        let _ = self.events.send(json!({"type": "usage", "usage": usage}));
        *self.usage.locked() = usage;
    }

    // MARK: devices

    /// Tells open channels (and the ACL publisher) that devices or pairing offers changed.
    pub fn auth_changed(&self) {
        self.auth.send_modify(|g| *g += 1);
    }

    /// Removes an authorized device: its tickets go with it and its open channels close.
    pub fn revoke_device(&self, key: &str) -> Result<bool> {
        let removed = self.store.remove_device(key)?;
        if removed {
            tracing::info!(device = key, "device revoked");
            self.auth_changed();
        }
        Ok(removed)
    }

    pub fn phone_connected(&self) -> bool {
        self.phone_clients.load(Ordering::Relaxed) > 0
    }
}

/// The gate with this host's settings. Unit tests never wait on a real machine's memory or slots.
fn new_gate() -> Arc<crate::agent::gate::Gate> {
    use crate::agent::gate::{Gate, Settings, memory};
    if cfg!(test) {
        return Gate::new(Settings::unlimited(), Box::new(memory::System));
    }
    Gate::new(Settings::load(), Box::new(memory::System))
}

fn validate(cfg: &BotConfig) -> Result<()> {
    if cfg.name.trim().is_empty() {
        bail!("name is required");
    }
    if cfg.is_group() {
        return Ok(());
    }
    if let Some(project) = &cfg.project
        && !folder::valid_project_id(project)
    {
        bail!("`{project}` isn't a project id (lowercase words joined by dashes)");
    }
    if !cfg.cwd.is_empty() && !std::path::Path::new(&cfg.cwd).is_dir() {
        bail!("workspace folder does not exist: {}", cfg.cwd);
    }
    if cfg.command.as_deref().map(str::trim).unwrap_or_default().is_empty()
        && !crate::agent::backends::is_known(&cfg.backend)
    {
        bail!("unknown backend {}", cfg.backend);
    }
    Ok(())
}

#[cfg(test)]
mod unsaved_tests {
    use super::*;

    const BREAK: &str =
        "CREATE TRIGGER full BEFORE INSERT ON entries BEGIN SELECT RAISE(FAIL, 'database or disk is full'); END;";
    const MEND: &str = "DROP TRIGGER full;";

    fn hub() -> Arc<Hub> {
        let dir = std::env::temp_dir().join(format!("kith-unsaved-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(std::path::Path::new(":memory:")).unwrap();
        let cfg: BotConfig =
            serde_json::from_value(json!({"id": "a", "name": "A", "backend": "claude", "cwd": dir.to_string_lossy()}))
                .unwrap();
        store.save_bot(&cfg).unwrap();
        Hub::new(store, "t".into(), Identity::load_or_create(&dir).unwrap(), "t".into(), 0)
    }

    fn texts(hub: &Hub) -> Vec<String> {
        let mut all = hub.store.history("a", i64::MAX, 1000).unwrap();
        all.sort_by_key(|e| e.seq);
        all.iter().map(|e| e.data["text"].as_str().unwrap().to_owned()).collect()
    }

    #[test]
    fn a_full_disk_holds_entries_in_order_and_they_land_once() {
        let hub = hub();
        let lane = Lane::main("a");
        assert!(hub.add_entry(&lane, EntryKind::User, 1, &json!({"text": "one"})).is_some());
        assert_eq!(hub.store_health(true)["writable"], true);

        hub.store.exec_for_test(BREAK);
        assert!(hub.add_entry(&lane, EntryKind::User, 1, &json!({"text": "two"})).is_none());
        let health = hub.store_health(true);
        assert_eq!((health["writable"].clone(), health["unsaved"].clone()), (json!(false), json!(1)));
        assert!(health["error"].as_str().unwrap().contains("disk is full"));
        // A later entry queues behind the held one.
        assert!(hub.add_entry(&lane, EntryKind::Agent, 1, &json!({"text": "three"})).is_none());
        assert_eq!(hub.store_health(false)["unsaved"], 2);
        hub.retry_unsaved();
        assert_eq!(texts(&hub), ["one"], "nothing lands while the disk is full");

        hub.store.exec_for_test(MEND);
        hub.retry_unsaved();
        assert_eq!(texts(&hub), ["one", "two", "three"]);
        let health = hub.store_health(true);
        assert_eq!((health["writable"].clone(), health["unsaved"].clone()), (json!(true), json!(0)));
        hub.retry_unsaved();
        assert_eq!(texts(&hub).len(), 3, "a second retry adds nothing");
    }

    #[test]
    fn a_new_entry_after_recovery_flushes_the_held_ones_first() {
        let hub = hub();
        let lane = Lane::main("a");
        hub.store.exec_for_test(BREAK);
        hub.add_entry(&lane, EntryKind::User, 1, &json!({"text": "held"}));
        hub.store.exec_for_test(MEND);
        assert!(hub.add_entry(&lane, EntryKind::User, 1, &json!({"text": "new"})).is_some());
        assert_eq!(texts(&hub), ["held", "new"]);
        assert_eq!(hub.store_health(false)["writable"], true);
    }

    #[test]
    fn an_entry_that_already_landed_is_not_written_twice() {
        let hub = hub();
        let lane = Lane::main("a");
        let p = PendingEntry {
            id: "same".into(),
            lane: lane.clone(),
            kind: EntryKind::User,
            turn: 1,
            data: json!({"text": "x"}),
            at: 1,
        };
        hub.hold_queued(p);
        hub.store.insert_entry_as("same", &lane, EntryKind::User, 1, &json!({"text": "x"}), 1).unwrap();
        hub.retry_unsaved();
        assert_eq!(texts(&hub), ["x"]);
    }

    #[test]
    fn the_held_entries_are_bounded_and_the_dropped_are_counted() {
        let hub = hub();
        let lane = Lane::main("a");
        hub.store.exec_for_test(BREAK);
        for i in 0..MAX_UNSAVED + 3 {
            hub.add_entry(&lane, EntryKind::User, 1, &json!({"text": i.to_string()}));
        }
        let health = hub.store_health(false);
        assert_eq!((health["unsaved"].clone(), health["dropped"].clone()), (json!(MAX_UNSAVED), json!(3)));
        hub.store.exec_for_test(MEND);
        hub.retry_unsaved();
        let t = texts(&hub);
        assert_eq!((t.len(), t[0].as_str()), (MAX_UNSAVED, "3"), "the oldest were dropped");
    }

    #[test]
    fn a_deleted_chat_is_not_held() {
        let hub = hub();
        hub.store.delete_bot("a").unwrap();
        assert!(hub.add_entry(&Lane::main("a"), EntryKind::User, 1, &json!({"text": "x"})).is_none());
        assert_eq!(hub.store_health(false)["unsaved"], 0);
        assert_eq!(hub.store_health(false)["writable"], true);
    }
}
