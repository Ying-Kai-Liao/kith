//! Discord: one bot in one server, ported from the bridge's `discord_chat.py`. Every agent has its own channel in
//! the `hq` category (a room whose default is that agent); any other channel is a room too, answered when an agent
//! or the bot is mentioned, or once it has a default (`/default <agent>`). A task is a thread; agents post in it
//! through the channel's webhook under their display name, and `kith-send` (`KITH_DISCORD_WEBHOOK_URL`,
//! `KITH_DISCORD_THREAD_ID`) posts there too. Each agent has a mentionable role of the same name, so `@` completes;
//! when the bot can't manage roles, a plain-text `@agent` works the same. DMs with the bot work like a room with a
//! default (the owner's: `channels.dm_default`).
//!
//! Server: `channels.discord_guild` (name or id) when the bot is in more than one. Until kith knows the owner's
//! Discord account, the owner of that named server is taken as kith's owner; with the setting unset, no one is.
//!
//! Every gateway frame (a heartbeat ack comes every ~41 s) is a sign of life; without any for [`QUIET`] the
//! supervisor restarts the connection. After each new session the adapter reads what was sent since the last
//! message it saw (at most 24 h back) and handles each message once.
//!
//! Both of Discord's addresses can be moved for tests: `KITH_DISCORD_API_BASE` and `KITH_DISCORD_GATEWAY_URL` (or the old HQ_ names)
//! (read here only, when the adapter is made).
//!
//! The library is by hand (`tokio-tungstenite` for the gateway, `reqwest` for REST): the dependency list is
//! already what a gateway needs, a gateway crate would bring a second TLS stack and its own HTTP client, and the
//! fake server in the tests can stand in for both addresses with nothing to patch.

mod api;
mod gateway;
mod message;
mod tasks;
#[cfg(test)]
mod tests;

use super::adapter::{Adapter, Alive, Ended, Fut, Incoming, Reaction, StopPress};
use super::core::Core;
use super::manager::Registry;
use crate::LockExt;
use anyhow::{Context, Result, anyhow, bail};
use api::{Api, Body, api_error, id_of, id_value, multipart, snowflake_time, time_snowflake};
use reqwest::Method;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tasks::{Guard, Tasks};
use tokio::sync::{mpsc, watch};

const API_BASE: &str = "https://discord.com/api/v10";
const GATEWAY: &str = "wss://gateway.discord.gg";
/// Without a frame for this long the connection is restarted.
const QUIET: Duration = Duration::from_secs(180);
/// New agents get their channel within this long.
const RESCAN: Duration = Duration::from_secs(60);
/// A message limit of Discord's.
const CHUNK: usize = 1900;
/// Messages older than this are not read back after a reconnect.
const CATCH_UP_HOURS: u64 = 24;
const WORKING: &str = "-# working…";
const STOPPED: &str = "-# stopped";
/// Larger attachments are not downloaded.
const MAX_FILE: u64 = 25 * 1024 * 1024;
const MANAGE_ROLES: u64 = 1 << 28;
const ADMINISTRATOR: u64 = 1 << 3;

/// Where Discord is, and how patient to be with it.
#[derive(Clone, Debug)]
pub struct Config {
    pub api_base: String,
    pub gateway_url: String,
    pub quiet: Duration,
    pub rescan: Duration,
    /// `channels.discord_guild`: used by the first profile, `discord/hq`, like `KITH_DISCORD_GUILD` was.
    pub guild: Option<String>,
}

impl Config {
    pub fn from_env(guild: Option<String>) -> Self {
        let var = |name: &str, default: &str| {
            crate::kernel::var(name)
                .and_then(|v| v.into_string().ok())
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| default.to_owned())
        };
        Self {
            api_base: var("DISCORD_API_BASE", API_BASE),
            gateway_url: var("DISCORD_GATEWAY_URL", GATEWAY),
            quiet: QUIET,
            rescan: RESCAN,
            guild,
        }
    }
}

/// Makes `discord` known to the registry: `hq-auth grant serve discord <profile>` then brings a bot up.
pub fn register(registry: &mut Registry) {
    registry.add(
        "discord",
        Arc::new(|conn, secrets, core| {
            let token = secrets.get("DISCORD_BOT_TOKEN").context("DISCORD_BOT_TOKEN is missing")?;
            let cfg = Config::from_env(core.settings.discord_guild.clone());
            let ad: Arc<dyn Adapter> = Discord::new(conn, token, core, cfg);
            Ok(ad)
        }),
    );
}

/// Agent text can't ping everyone or a role; a named user still can.
fn no_mass_pings() -> Value {
    json!({"parse": ["users"]})
}

#[derive(Clone, Debug)]
struct Chan {
    kind: u64,
    name: String,
    parent: Option<String>,
}

impl Chan {
    fn of(v: &Value) -> Option<(String, Self)> {
        Some((
            id_of(&v["id"])?,
            Self {
                kind: v["type"].as_u64()?,
                name: v["name"].as_str().unwrap_or_default().to_owned(),
                parent: id_of(&v["parent_id"]),
            },
        ))
    }

    fn is_text(&self) -> bool {
        matches!(self.kind, 0 | 5)
    }

    fn is_thread(&self) -> bool {
        matches!(self.kind, 10..=12)
    }
}

struct Guild {
    id: String,
    name: String,
    owner_id: String,
    /// The server `channels.discord_guild` names: only its owner is taken as kith's owner before linking.
    named: bool,
    manage_roles: bool,
}

#[derive(Default)]
struct Rt {
    /// The bot's own user id and application id.
    me: String,
    app: String,
    guild: Option<Guild>,
    /// Role id -> agent.
    roles: HashMap<String, String>,
    chans: HashMap<String, Chan>,
    /// Messages being handled now, so that a live one and the catch-up don't both take it.
    inflight: HashSet<u64>,
}

/// A message being handled; it stops being in flight when this is dropped, however that happens.
struct Claim<'a>(&'a Discord, u64);

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        self.0.rt.locked().inflight.remove(&self.1);
    }
}

pub struct Discord {
    conn: String,
    cfg: Config,
    api: Api,
    core: Arc<Core>,
    alive: Alive,
    rt: Mutex<Rt>,
    /// Webhooks are made one at a time, so two posts to a new channel don't make two.
    hook_lock: tokio::sync::Mutex<()>,
    /// Message ids of working lines already edited to "stopped". The run ending and the Stop press race;
    /// "stopping…" must never land after "stopped". Held across the edit to order them.
    stopped_lines: tokio::sync::Mutex<HashSet<String>>,
    /// Set once the server is known and set up: messages wait for it.
    ready: watch::Sender<bool>,
}

impl Discord {
    pub fn new(conn: &str, token: &str, core: Arc<Core>, cfg: Config) -> Arc<Self> {
        Arc::new(Self {
            conn: conn.to_owned(),
            api: Api::new(&cfg.api_base, token),
            cfg,
            core,
            alive: Alive::default(),
            rt: Mutex::new(Rt::default()),
            hook_lock: tokio::sync::Mutex::new(()),
            stopped_lines: tokio::sync::Mutex::new(HashSet::new()),
            ready: watch::channel(false).0,
        })
    }

    fn token(&self) -> &str {
        self.api.token()
    }

    fn me(&self) -> String {
        self.rt.locked().me.clone()
    }

    fn set_me(&self, user: &str, app: Option<&str>) {
        let mut rt = self.rt.locked();
        user.clone_into(&mut rt.me);
        if let Some(app) = app {
            app.clone_into(&mut rt.app);
        }
    }

    /// A channel was made, renamed or deleted: what is cached about it is changed to match.
    fn channel_changed(&self, data: &Value, deleted: bool) {
        let Some((id, chan)) = Chan::of(data) else { return };
        let mut rt = self.rt.locked();
        if deleted {
            rt.chans.remove(&id);
        } else {
            rt.chans.insert(id, chan);
        }
    }

    fn guild_id(&self) -> Result<String> {
        self.rt.locked().guild.as_ref().map(|g| g.id.clone()).context("the server isn't known yet")
    }

    fn as_adapter(self: &Arc<Self>) -> Arc<dyn Adapter> {
        self.clone()
    }

    async fn get(&self, path: &str) -> Result<Value> {
        self.api.bot(Method::GET, path, Body::None).await
    }

    async fn send(&self, method: Method, path: &str, body: Value) -> Result<Value> {
        self.api.bot(method, path, Body::Json(body)).await
    }

    // ---- the connection ----

    /// A channel's kind, name and parent: cached, and fetched when it isn't known.
    async fn chan(&self, id: &str) -> Result<Chan> {
        if let Some(c) = self.rt.locked().chans.get(id) {
            return Ok(c.clone());
        }
        let v = self.get(&format!("/channels/{id}")).await?;
        let (_, c) = Chan::of(&v).context("a channel Discord didn't describe")?;
        self.rt.locked().chans.insert(id.to_owned(), c.clone());
        Ok(c)
    }

    /// The one server this bot works in.
    async fn pick_guild(&self) -> Result<()> {
        let guilds = self.get("/users/@me/guilds").await?;
        let all = guilds.as_array().cloned().unwrap_or_default();
        let setting = self.cfg.guild.as_deref().map(message::squash).filter(|w| !w.is_empty());
        let want = if self.conn == "discord/hq" { setting.as_deref() } else { None };
        let title = |g: &Value| g["name"].as_str().unwrap_or_default().to_owned();
        let is = |w: &str, g: &Value| w == message::squash(&title(g)) || Some(w) == id_of(&g["id"]).as_deref();
        let found: Vec<&Value> = all.iter().filter(|g| want.is_none_or(|w| is(w, g))).collect();
        let [g] = found.as_slice() else {
            let names: Vec<String> = all.iter().map(title).collect();
            bail!("expected one server, found {names:?}; set channels.discord_guild in config.json");
        };
        let id = id_of(&g["id"]).context("a server without an id")?;
        let perms = g["permissions"].as_str().and_then(|p| p.parse::<u64>().ok()).unwrap_or(0);
        let detail = self.get(&format!("/guilds/{id}")).await?;
        let owner_id = id_of(&detail["owner_id"]).unwrap_or_default();
        let named = setting.as_deref().is_some_and(|w| is(w, g));
        let manage_roles = perms & (MANAGE_ROLES | ADMINISTRATOR) != 0;
        self.rt.locked().guild = Some(Guild { id, name: title(g), owner_id, named, manage_roles });
        Ok(())
    }

    /// A session began: the server, its channels and roles, then what was missed. The first one of this
    /// connection also hands over what waited for the host and starts the rescan.
    async fn ready(self: &Arc<Self>, tasks: &Tasks, last: u64) -> Result<()> {
        self.pick_guild().await?;
        if let Err(e) = self.setup().await {
            tracing::warn!("{}: setting up the server failed: {e:#}", self.conn);
        }
        let first = !*self.ready.borrow();
        if first {
            self.ready.send_replace(true);
            self.core.resume(&self.as_adapter());
            let me = self.clone();
            tasks.spawn(async move { me.rescan().await });
        }
        let me = self.clone();
        tasks.spawn(async move { me.catch_up(last).await }); // a new session: events in between are not replayed
        Ok(())
    }

    /// Handles messages one at a time, in the order they came, once the server is known.
    async fn messages(self: Arc<Self>, mut rx: mpsc::UnboundedReceiver<Value>) {
        let mut ready = self.ready.subscribe();
        if ready.wait_for(|r| *r).await.is_err() {
            return;
        }
        while let Some(msg) = rx.recv().await {
            self.got(&msg).await;
        }
    }

    // ---- the server: channels, roles, webhooks ----

    fn topic_of(&self, agent: &str) -> String {
        self.core.description(agent).chars().take(1000).collect()
    }

    /// Whether the server lacks a channel or role for an agent.
    fn lacks(&self, channels: &[Value]) -> bool {
        let agents = self.core.agents();
        let rt = self.rt.locked();
        let missing_channel = agents
            .iter()
            .any(|a| !channels.iter().any(|c| Chan::of(c).is_some_and(|(_, c)| c.is_text() && c.name == *a)));
        let manage = rt.guild.as_ref().is_some_and(|g| g.manage_roles);
        let missing_role = manage && agents.iter().any(|a| !rt.roles.values().any(|r| r == a));
        missing_channel || missing_role
    }

    async fn setup(&self) -> Result<()> {
        let gid = self.guild_id()?;
        let channels = self.get(&format!("/guilds/{gid}/channels")).await?;
        let channels = channels.as_array().cloned().unwrap_or_default();
        {
            let mut rt = self.rt.locked();
            for c in &channels {
                if let Some((id, c)) = Chan::of(c) {
                    rt.chans.insert(id, c);
                }
            }
        }
        let category = match channels.iter().find(|c| c["type"] == 4 && (c["name"] == "kith" || c["name"] == "hq")) /* "hq": made before the rename */ {
            Some(c) => id_of(&c["id"]),
            None => id_of(
                &self.send(Method::POST, &format!("/guilds/{gid}/channels"), json!({"name": "kith", "type": 4})).await?["id"],
            ),
        };
        let agents = self.core.agents();
        for agent in &agents {
            let topic = self.topic_of(agent);
            let existing =
                channels.iter().find(|c| c["name"] == agent.as_str() && matches!(c["type"].as_u64(), Some(0 | 5)));
            let id = if let Some(c) = existing {
                id_of(&c["id"]).unwrap_or_default()
            } else {
                let made = self
                    .send(
                        Method::POST,
                        &format!("/guilds/{gid}/channels"),
                        json!({"name": agent, "type": 0, "parent_id": category, "topic": topic}),
                    )
                    .await?;
                if let Some((id, c)) = Chan::of(&made) {
                    self.rt.locked().chans.insert(id, c);
                }
                id_of(&made["id"]).context("a channel without an id")?
            };
            if let Err(e) = self.hook(&id).await {
                tracing::warn!("{}: no webhook for #{agent}: {e:#}", self.conn);
            }
        }
        let manage = self.rt.locked().guild.as_ref().is_some_and(|g| g.manage_roles);
        if manage {
            let roles = self.get(&format!("/guilds/{gid}/roles")).await?;
            for agent in &agents {
                let role = match roles.as_array().and_then(|r| r.iter().find(|r| r["name"] == agent.as_str())) {
                    Some(r) => r.clone(),
                    None => {
                        self.send(
                            Method::POST,
                            &format!("/guilds/{gid}/roles"),
                            json!({"name": agent, "mentionable": true}),
                        )
                        .await?
                    }
                };
                if let Some(id) = id_of(&role["id"]) {
                    self.rt.locked().roles.insert(id, agent.clone());
                }
            }
        } else if self.core.conn_state(&self.conn).get("roles").is_none() {
            self.core.update_conn(&self.conn, |cs| cs["roles"] = json!("none"));
            tracing::info!(
                "{}: no Manage Roles permission, so no @agent roles; plain @agent works the same",
                self.conn
            );
        }
        let name = self.rt.locked().guild.as_ref().map(|g| g.name.clone()).unwrap_or_default();
        tracing::info!("{}: ready in {name}: {}", self.conn, agents.join(", "));
        Ok(())
    }

    /// New agents (from the builder or hq-new) get their channel within a minute, no restart needed.
    async fn rescan(self: Arc<Self>) {
        loop {
            tokio::time::sleep(self.cfg.rescan).await;
            let check = async {
                let gid = self.guild_id()?;
                let channels = self.get(&format!("/guilds/{gid}/channels")).await?;
                let channels = channels.as_array().cloned().unwrap_or_default();
                if self.lacks(&channels) {
                    self.setup().await?;
                }
                anyhow::Ok(())
            };
            if let Err(e) = check.await {
                tracing::warn!("{}: rescan failed: {e:#}", self.conn); // never takes the connection down
            }
        }
    }

    fn saved_hook(&self, channel: &str) -> Option<String> {
        self.core.conn_state(&self.conn)["hooks"][channel].as_str().filter(|u| !u.is_empty()).map(str::to_owned)
    }

    /// The channel's webhook URL, made once: agents post through it under their own names.
    async fn hook(&self, channel: &str) -> Result<String> {
        if let Some(url) = self.saved_hook(channel) {
            return Ok(url);
        }
        let _one_at_a_time = self.hook_lock.lock().await;
        if let Some(url) = self.saved_hook(channel) {
            return Ok(url);
        }
        let listed = self.get(&format!("/channels/{channel}/webhooks")).await?;
        let mine = listed
            .as_array()
            .and_then(|l| l.iter().find(|w| (w["name"] == "kith" || w["name"] == "hq") && w["token"].is_string()))
            .cloned();
        let hook = match mine {
            Some(h) => h,
            None => self.send(Method::POST, &format!("/channels/{channel}/webhooks"), json!({"name": "kith"})).await?,
        };
        let (Some(id), Some(token)) = (id_of(&hook["id"]), hook["token"].as_str()) else {
            bail!("Discord made a webhook without a token");
        };
        // unversioned, like the bridge's, so a rollback to discord.py reads it
        let root = self.api.base().strip_suffix("/v10").unwrap_or(self.api.base());
        let url = format!("{root}/webhooks/{id}/{token}");
        self.core.update_conn(&self.conn, |cs| {
            if !cs["hooks"].is_object() {
                cs["hooks"] = json!({});
            }
            cs["hooks"][channel] = json!(url);
        });
        Ok(url)
    }

    /// A call through the channel's webhook into a thread. A webhook someone deleted is made again, once; a
    /// thread that was archived is opened again, once.
    async fn hook_call(
        &self,
        channel: &str,
        thread: &str,
        method: Method,
        suffix: &str,
        body: Body,
        wait: bool,
    ) -> Result<Value> {
        let (mut recreated, mut unarchived) = (false, false);
        loop {
            let url = self.hook(channel).await?;
            let mut body = body.clone();
            if method == Method::POST
                && let Body::Json(v) = &mut body
            {
                v["allowed_mentions"] = no_mass_pings();
            }
            let full = format!("{url}{suffix}?thread_id={thread}{}", if wait { "&wait=true" } else { "" });
            let err = match self.api.call(method.clone(), &full, body, false).await {
                Ok(v) => return Ok(v),
                Err(e) => e,
            };
            match api_error(&err).map(|a| a.code) {
                Some(10015) if !recreated => {
                    recreated = true;
                    tracing::info!("{}: the webhook of a channel is gone; making another", self.conn);
                    self.core.update_conn(&self.conn, |cs| {
                        if let Some(h) = cs["hooks"].as_object_mut() {
                            h.remove(channel);
                        }
                    });
                }
                Some(50083) if !unarchived => {
                    unarchived = true;
                    let _ = self.send(Method::PATCH, &format!("/channels/{thread}"), json!({"archived": false})).await;
                }
                _ => return Err(err),
            }
        }
    }

    // ---- messages in ----

    /// A message, live or read back after a reconnect: each is handled once.
    async fn got(self: &Arc<Self>, msg: &Value) {
        if msg["author"]["bot"] == true || !msg["webhook_id"].is_null() {
            return;
        }
        let Some(id) = msg["id"].as_str().and_then(|i| i.parse::<u64>().ok()) else { return };
        if !matches!(msg["type"].as_u64().unwrap_or(0), 0 | 19) {
            return; // joins, pins, "thread created": not something said to hq
        }
        let Some(claim) = self.claim(id) else { return };
        // Marked before the core gets it, like the bridge did: a crash in between loses the message rather than
        // running a task twice. The claim stays until the end so the catch-up can't take it meanwhile.
        self.mark_seen(id);
        match self.incoming(msg).await {
            Ok(Some(m)) => {
                if let Err(e) = self.core.handle(&self.as_adapter(), &m).await {
                    tracing::warn!("{}: message failed: {e:#}", self.conn); // one bad message must not stop the bot
                }
            }
            Ok(None) => {}
            Err(e) => tracing::warn!("{}: message failed: {e:#}", self.conn),
        }
        drop(claim);
    }

    /// Takes a message to handle: None when it is being handled or was before.
    fn claim(&self, id: u64) -> Option<Claim<'_>> {
        if !self.rt.locked().inflight.insert(id) {
            return None;
        }
        let claim = Claim(self, id);
        let seen = self.core.conn_state(&self.conn)["seen"]
            .as_array()
            .is_some_and(|s| s.iter().any(|v| v.as_u64() == Some(id)));
        (!seen).then_some(claim)
    }

    /// Remembers the message (the last 500) and moves where a catch-up starts.
    fn mark_seen(&self, id: u64) {
        self.core.update_conn(&self.conn, |cs| {
            let Some(o) = cs.as_object_mut() else { return };
            let mut seen: Vec<Value> = o.get("seen").and_then(Value::as_array).cloned().unwrap_or_default();
            let from = seen.len().saturating_sub(499);
            seen = seen.split_off(from);
            seen.push(json!(id));
            let last = o.get("last").and_then(Value::as_u64).unwrap_or(0).max(id);
            o.insert("seen".into(), Value::Array(seen));
            o.insert("last".into(), json!(last));
        });
    }

    /// Messages sent since the last one seen (at most 24 h back), oldest first. Without a last one (the first
    /// run) there is nothing to catch up on.
    async fn catch_up(self: Arc<Self>, last: u64) {
        if let Err(e) = self.catch_up_inner(last).await {
            tracing::warn!("{}: catch-up failed: {e:#}", self.conn);
        }
    }

    async fn catch_up_inner(self: &Arc<Self>, last: u64) -> Result<()> {
        if last == 0 {
            return Ok(());
        }
        let now_ms = u64::try_from(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis())?;
        let after = last.max(time_snowflake(now_ms.saturating_sub(CATCH_UP_HOURS * 3_600_000)));
        let gid = self.guild_id()?;
        let channels = self.get(&format!("/guilds/{gid}/channels")).await?;
        let mut where_: Vec<(String, bool)> = channels
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Chan::of)
            .filter(|(_, c)| c.is_text())
            .map(|(id, _)| (id, true))
            .collect();
        match self.get(&format!("/guilds/{gid}/threads/active")).await {
            Ok(t) => {
                for th in t["threads"].as_array().into_iter().flatten() {
                    if let Some((id, c)) = Chan::of(th) {
                        self.rt.locked().chans.insert(id.clone(), c);
                        where_.push((id, true));
                    }
                }
            }
            Err(e) => tracing::warn!("{}: catch-up: no threads ({e:#})", self.conn),
        }
        let dm_prefix = format!("{}:dm:", self.conn);
        for (room, _) in self.core.rooms(&self.conn) {
            if let Some(id) = room.strip_prefix(&dm_prefix) {
                where_.push((id.to_owned(), false));
            }
        }
        let mut missed: Vec<(u64, Value)> = Vec::new();
        for (channel, in_guild) in where_ {
            for mut m in self.history(&channel, after).await {
                if in_guild {
                    m["guild_id"] = json!(gid);
                }
                if let Some(id) = m["id"].as_str().and_then(|i| i.parse::<u64>().ok()) {
                    missed.push((id, m));
                }
            }
        }
        missed.sort_by_key(|(id, _)| *id);
        let seen: Vec<u64> = self.core.conn_state(&self.conn)["seen"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_u64)
            .collect();
        missed.retain(|(id, m)| m["author"]["bot"] != true && m["webhook_id"].is_null() && !seen.contains(id));
        if !missed.is_empty() {
            tracing::info!("{}: catching up on {} message(s) sent while away", self.conn, missed.len());
        }
        for (_, m) in missed {
            self.got(&m).await;
        }
        Ok(())
    }

    /// The messages of a channel after `after`, a few pages of them; a channel the bot can't read has none.
    async fn history(&self, channel: &str, mut after: u64) -> Vec<Value> {
        let mut out = Vec::new();
        for _ in 0..5 {
            let Ok(page) = self.get(&format!("/channels/{channel}/messages?after={after}&limit=100")).await else {
                break;
            };
            let page = page.as_array().cloned().unwrap_or_default();
            let newest = page.iter().filter_map(|m| m["id"].as_str()?.parse::<u64>().ok()).max();
            let full = page.len() >= 100;
            out.extend(page);
            match newest {
                Some(n) if full => after = n,
                _ => break,
            }
        }
        out
    }

    async fn incoming(&self, msg: &Value) -> Result<Option<Incoming>> {
        let (me, roles, owner, our_guild) = {
            let rt = self.rt.locked();
            (
                rt.me.clone(),
                rt.roles.clone(),
                rt.guild.as_ref().filter(|g| g.named).map(|g| g.owner_id.clone()), // any other server's owner is no one
                rt.guild.as_ref().map(|g| g.id.clone()),
            )
        };
        let (cid, mid) = (id_of(&msg["channel_id"]).context("no channel")?, id_of(&msg["id"]).context("no id")?);
        let uid = id_of(&msg["author"]["id"]).context("no author")?;
        let (text, mut addressed) = message::strip_bot(msg["content"].as_str().unwrap_or(""), &me);
        let (text, by_role) = message::agent_roles(&text, &roles);
        addressed |= by_role;
        let agents = self.core.agents();
        if agents.iter().any(|a| text.contains(&format!("@{a}"))) {
            addressed = true;
        }
        let replied = &msg["referenced_message"];
        let mut quoted = String::new();
        if replied.is_object() {
            if !replied["webhook_id"].is_null() || id_of(&replied["author"]["id"]).as_deref() == Some(me.as_str()) {
                addressed = true;
            } else {
                quoted = replied["content"].as_str().unwrap_or("").trim().to_owned();
            }
        }
        let ref_id = id_of(&msg["message_reference"]["message_id"]);
        let ch = self.chan(&cid).await?;
        let conn = &self.conn;
        let (room, thread, private, room_agent, origin);
        if msg["guild_id"].is_null() {
            if ch.kind != 1 {
                return Ok(None);
            }
            room = format!("{conn}:dm:{cid}");
            thread = ref_id.as_deref().and_then(|r| self.core.replied_thread(conn, &cid, r));
            (private, room_agent) = (true, None);
            origin = json!({"dm": id_value(&cid), "message": id_value(&mid)});
        } else {
            if our_guild.is_some() && id_of(&msg["guild_id"]) != our_guild {
                return Ok(None); // another server this bot is in
            }
            if ch.is_thread()
                && let Some(parent) = ch.parent.clone()
            {
                let parent_name = self.chan(&parent).await?.name;
                room = format!("{conn}:{parent}");
                let key = format!("{conn}:{cid}");
                room_agent = agents.contains(&parent_name).then_some(parent_name);
                origin = json!({"thread": id_value(&cid), "channel": id_value(&parent), "message": id_value(&mid)});
                if !self.core.has_thread(&key) && !addressed {
                    return Ok(None); // someone else's thread, not talking to hq
                }
                thread = Some(key);
            } else if ch.is_text() {
                room = format!("{conn}:{cid}");
                thread = None;
                room_agent = agents.contains(&ch.name).then(|| ch.name.clone());
                origin = json!({"channel": id_value(&cid), "message": id_value(&mid)});
            } else {
                return Ok(None);
            }
            private = false;
        }
        let ours = addressed
            || private
            || room_agent.is_some()
            || thread.as_ref().is_some_and(|t| self.core.has_thread(t))
            || self.core.room_default(&room).is_some();
        let mut files = Vec::new();
        let mut text = text;
        let has_files = msg["attachments"].as_array().is_some_and(|a| !a.is_empty());
        let admin = owner.as_deref() == Some(uid.as_str());
        if ours && has_files && self.core.knows_sender("discord", &uid, admin).await {
            // only for messages hq may act on, from people hq knows
            for a in msg["attachments"].as_array().into_iter().flatten() {
                match self.download(&mid, files.len(), a).await {
                    Ok(Some(p)) => files.push(p),
                    Ok(None) => {
                        let name = a["filename"].as_str().unwrap_or("file");
                        format!("{text}\n(file too big: {name})").trim().clone_into(&mut text);
                    }
                    Err(e) => tracing::warn!("{}: an attachment wasn't saved: {e:#}", self.conn),
                }
            }
        }
        let id = mid.parse::<u64>().unwrap_or(0);
        Ok(Some(Incoming {
            admin,
            uid,
            text,
            room,
            origin,
            thread,
            private,
            addressed: addressed || private,
            room_agent,
            quoted,
            files,
            reference: json!({"channel": id_value(&cid), "message": id_value(&mid)}),
            sent: (id > 0).then(|| snowflake_time(id)),
        }))
    }

    /// Saves an attachment under the connection's folder; None when it is over [`MAX_FILE`].
    async fn download(&self, mid: &str, n: usize, attachment: &Value) -> Result<Option<String>> {
        use futures::StreamExt as _;
        use tokio::io::AsyncWriteExt as _;
        let url = attachment["url"].as_str().context("an attachment without a URL")?;
        if attachment["size"].as_u64().is_some_and(|s| s > MAX_FILE) {
            return Ok(None);
        }
        let name = message::safe_name(attachment["filename"].as_str().unwrap_or(""), "file");
        let folder = self.core.attachments_dir(&self.conn).join(mid);
        tokio::fs::create_dir_all(&folder).await?;
        let resp = crate::http().get(url).send().await.map_err(|e| anyhow!("{}", e.without_url()))?;
        if !resp.status().is_success() {
            bail!("the attachment answered {}", resp.status());
        }
        // Two files with one name in a message keep both: the later ones get their number in front.
        let mut path = folder.join(&name);
        if tokio::fs::try_exists(&path).await? {
            path = folder.join(format!("{n}-{name}"));
        }
        let mut file = tokio::fs::File::create(&path).await?;
        let (mut size, mut stream) = (0u64, resp.bytes_stream());
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| anyhow!("{}", e.without_url()))?;
            size += chunk.len() as u64;
            if size > MAX_FILE {
                drop(file);
                let _ = tokio::fs::remove_file(&path).await;
                return Ok(None);
            }
            file.write_all(&chunk).await?;
        }
        file.flush().await?;
        Ok(Some(path.to_string_lossy().into_owned()))
    }

    // ---- buttons ----

    /// A button pressed: Stop on a working line, or one of "Who should take this?". Answered at once (Discord
    /// gives three seconds) and the message edited when the core has decided.
    async fn interaction(self: &Arc<Self>, it: &Value) {
        if it["type"] != 3 {
            return;
        }
        let (Some(id), Some(token)) = (id_of(&it["id"]), it["token"].as_str()) else { return };
        let cid = it["data"]["custom_id"].as_str().unwrap_or("");
        let uid = id_of(&it["member"]["user"]["id"]).or_else(|| id_of(&it["user"]["id"])).unwrap_or_default();
        let app = id_of(&it["application_id"]).unwrap_or_else(|| self.rt.locked().app.clone());
        let me = self.as_adapter();
        let (content, private) =
            if let Some(wid) = cid.strip_prefix("kith-stop:").or_else(|| cid.strip_prefix("hq-stop:")) {
                self.defer(&id, token).await;
                match self.core.stop_pressed(&me, wid, &uid).await {
                    StopPress::NotYours => ("Only the person who started this can stop it.", true),
                    StopPress::Finished => ("-# finished", false),
                    StopPress::Stopping => ("-# stopping…", false),
                }
            } else if let Some(rest) = cid.strip_prefix("kith-pick:").or_else(|| cid.strip_prefix("hq-pick:")) {
                self.defer(&id, token).await;
                let mut parts = rest.splitn(2, ':');
                let (pid, agent) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
                match self.core.picked(&me, pid, agent, &uid).await {
                    Ok(true) => {
                        self.finish_press(&app, token, &format!("→ {agent}"), false).await;
                        return;
                    }
                    Ok(false) => ("That question is someone else's, or already answered.", true),
                    Err(e) => {
                        tracing::warn!("{}: picking an agent failed: {e:#}", self.conn);
                        ("That didn't work; try again.", true)
                    }
                }
            } else {
                return;
            };
        if content == "-# stopping…" {
            let stopped = self.stopped_lines.lock().await;
            if !id_of(&it["message"]["id"]).is_some_and(|m| stopped.contains(&m)) {
                self.finish_press(&app, token, content, private).await;
            }
            return;
        }
        self.finish_press(&app, token, content, private).await;
    }

    /// "Got it": the message stays as it is until the answer is ready.
    async fn defer(&self, id: &str, token: &str) {
        let url = self.api.url(&format!("/interactions/{id}/{token}/callback"));
        if let Err(e) = self.api.call(Method::POST, &url, Body::Json(json!({"type": 6})), false).await {
            tracing::warn!("{}: a button press wasn't acknowledged: {e:#}", self.conn);
        }
    }

    /// The pressed message becomes `content` without buttons; an answer only the presser sees is sent apart.
    async fn finish_press(&self, app: &str, token: &str, content: &str, private: bool) {
        let hook = self.api.url(&format!("/webhooks/{app}/{token}"));
        let sent = if private {
            self.api.call(Method::POST, &hook, Body::Json(json!({"content": content, "flags": 64})), false).await
        } else {
            let url = format!("{hook}/messages/@original");
            self.api.call(Method::PATCH, &url, Body::Json(json!({"content": content, "components": []})), false).await
        };
        if let Err(e) = sent {
            tracing::warn!("{}: a button press wasn't answered: {e:#}", self.conn);
        }
    }

    // ---- messages out ----

    /// The channel (or thread, or DM) a conversation lives in.
    fn place(conv: &Value) -> Option<String> {
        id_of(&conv["thread"]).or_else(|| id_of(&conv["dm"])).or_else(|| id_of(&conv["channel"]))
    }

    /// (channel, message) of the message `m` was made from.
    fn loc(m: &Incoming) -> Option<(String, String)> {
        Some((Self::place(&m.origin)?, id_of(&m.origin["message"])?))
    }

    async fn say(&self, channel: &str, mut body: Value) -> Result<Value> {
        body["allowed_mentions"] = no_mass_pings();
        self.send(Method::POST, &format!("/channels/{channel}/messages"), body).await
    }

    async fn reply_to(&self, m: &Incoming, body: &Value) -> Result<()> {
        let mut body = body.clone();
        if let Some((channel, message)) = Self::loc(m) {
            body["message_reference"] = json!({"message_id": message, "fail_if_not_exists": false});
            self.say(&channel, body).await?;
        } else {
            bail!("no place to answer in");
        }
        Ok(())
    }

    async fn post_(&self, conv: &Value, agent: &str, text: &str, ctx: Option<f64>) -> Result<()> {
        let text = ctx.map_or_else(|| text.to_owned(), |c| format!("{text}\n-# context {c:.0}%"));
        let name = self.core.display_name(agent);
        if let Some(dm) = id_of(&conv["dm"]) {
            for (i, part) in Core::chunks(&text, CHUNK).into_iter().enumerate() {
                let content = if i == 0 { format!("**{name}**\n{part}") } else { part };
                let sent = self.say(&dm, json!({"content": content})).await?;
                if let (Some(mid), Some(key)) = (id_of(&sent["id"]), conv["key"].as_str()) {
                    self.core.link_reply(&self.conn, &dm, &mid, key);
                }
            }
            return Ok(());
        }
        let (channel, thread) = Self::room_of(conv)?;
        for part in Core::chunks(&text, CHUNK) {
            let body = Body::Json(json!({"content": part, "username": name}));
            self.hook_call(&channel, &thread, Method::POST, "", body, true).await?;
        }
        Ok(())
    }

    /// (channel, thread) of a conversation in a server.
    fn room_of(conv: &Value) -> Result<(String, String)> {
        let channel = id_of(&conv["channel"]).context("a conversation without a channel")?;
        let thread = id_of(&conv["thread"]).context("a conversation without a thread")?;
        Ok((channel, thread))
    }

    fn stop_button(wid: &str) -> Value {
        // custom_id is handled in the gateway's interactions: the button survives restarts
        json!([{"type": 1, "components": [{"type": 2, "style": 2, "label": "Stop", "custom_id": format!("kith-stop:{wid}")}]}])
    }

    /// Edits (or deletes) a working line made by [`Adapter::status`].
    async fn edit_line(&self, handle: &Value, body: Value, delete: bool) -> Result<()> {
        let (Some(channel), Some(id)) = (id_of(&handle["channel"]), id_of(&handle["id"])) else { return Ok(()) };
        let method = if delete { Method::DELETE } else { Method::PATCH };
        if let Some(thread) = id_of(&handle["thread"]) {
            let body = if delete { Body::None } else { Body::Json(body) };
            self.hook_call(&channel, &thread, method, &format!("/messages/{id}"), body, false).await?;
        } else {
            let body = if delete { Body::None } else { Body::Json(body) };
            self.api.bot(method, &format!("/channels/{channel}/messages/{id}"), body).await?;
        }
        Ok(())
    }

    async fn files_(&self, conv: &Value, agent: &str, files: &[String], caption: &str) -> Result<()> {
        let mut loaded = Vec::new();
        for f in files.iter().filter(|f| std::path::Path::new(f).is_file()).take(10) {
            let name =
                std::path::Path::new(f).file_name().map_or_else(|| "file".into(), |n| n.to_string_lossy().into_owned());
            loaded.push((
                name,
                tokio::fs::read(f)
                    .await
                    .with_context(|| format!("reading {name_of}", name_of = f.rsplit('/').next().unwrap_or(f)))?,
            ));
        }
        let name = self.core.display_name(agent);
        if let Some(dm) = id_of(&conv["dm"]) {
            let content = format!("**{name}** {caption}").trim().to_owned();
            self.api
                .bot(
                    Method::POST,
                    &format!("/channels/{dm}/messages"),
                    multipart(&json!({"content": content, "allowed_mentions": no_mass_pings()}), &loaded),
                )
                .await?;
            return Ok(());
        }
        let (channel, thread) = Self::room_of(conv)?;
        let mut payload = json!({"username": name, "allowed_mentions": no_mass_pings()});
        if !caption.is_empty() {
            payload["content"] = json!(caption);
        }
        self.hook_call(&channel, &thread, Method::POST, "", multipart(&payload, &loaded), true).await?;
        Ok(())
    }
}

impl Adapter for Discord {
    fn platform(&self) -> &'static str {
        "discord"
    }

    fn conn(&self) -> &str {
        &self.conn
    }

    fn footer(&self) -> &'static str {
        "\n\n(From Discord: this thread is your task. Your final answer is posted here automatically; use kith-send \
         for images and files. Helpers you call post status with `kith-send --as <their name>`.)"
    }

    fn quiet(&self) -> Option<Duration> {
        Some(self.cfg.quiet)
    }

    fn alive(&self) -> &Alive {
        &self.alive
    }

    fn start(self: Arc<Self>) -> Fut<'static, Result<()>> {
        Box::pin(async move {
            self.ready.send_replace(false);
            {
                let mut rt = self.rt.locked();
                rt.guild = None;
                rt.roles.clear();
                rt.chans.clear();
                rt.inflight.clear();
            }
            let tasks = Tasks::default();
            let _ends_with_us = Guard(tasks.clone());
            let (messages, rx) = mpsc::unbounded_channel();
            let (fatal_tx, fatal_rx) = mpsc::unbounded_channel();
            tasks.spawn(self.clone().messages(rx));
            gateway::run(self, gateway::Wiring { tasks, messages, fatal_tx, fatal_rx }).await
        })
    }

    fn reply<'a>(&'a self, m: &'a Incoming, text: &'a str) -> Fut<'a, Result<()>> {
        Box::pin(async move { self.reply_to(m, &json!({"content": text})).await })
    }

    fn open_thread<'a>(&'a self, m: &'a Incoming, _agent: &'a str, title: &'a str) -> Fut<'a, Result<(String, Value)>> {
        Box::pin(async move {
            let o = &m.origin;
            let conn = &self.conn;
            if let Some(thread) = id_of(&o["thread"]) {
                // a thread hq didn't start: the task lives in it as it is
                if let Some(channel) = id_of(&o["channel"]) {
                    let _ = self.hook(&channel).await; // so that kith-send works here from the first run
                }
                return Ok((format!("{conn}:{thread}"), json!({"thread": o["thread"], "channel": o["channel"]})));
            }
            if let Some(dm) = id_of(&o["dm"]) {
                // no threads in a DM: a reply to the agent's message continues the task
                let message = id_of(&o["message"]).context("a message without an id")?;
                return Ok((format!("{conn}:dm:{dm}:{message}"), json!({"dm": o["dm"]})));
            }
            let channel = id_of(&o["channel"]).context("a message without a channel")?;
            let message = id_of(&o["message"]).context("a message without an id")?;
            let name: String = title.chars().take(100).collect();
            let name = if name.trim().is_empty() { "task".to_owned() } else { name };
            let th = self
                .send(
                    Method::POST,
                    &format!("/channels/{channel}/messages/{message}/threads"),
                    json!({"name": name, "auto_archive_duration": 1440}),
                )
                .await?;
            let thread = id_of(&th["id"]).context("Discord made a thread without an id")?;
            self.rt.locked().chans.insert(
                thread.clone(),
                Chan { kind: th["type"].as_u64().unwrap_or(11), name, parent: Some(channel.clone()) },
            );
            if let Err(e) = self.hook(&channel).await {
                tracing::warn!("{conn}: no webhook for the channel: {e:#}");
            }
            Ok((format!("{conn}:{thread}"), json!({"thread": id_value(&thread), "channel": o["channel"]})))
        })
    }

    fn post<'a>(&'a self, conv: &'a Value, agent: &'a str, text: &'a str, ctx_pct: Option<f64>) -> Fut<'a, Result<()>> {
        Box::pin(self.post_(conv, agent, text, ctx_pct))
    }

    fn send_files<'a>(
        &'a self,
        conv: &'a Value,
        agent: &'a str,
        files: &'a [String],
        caption: &'a str,
    ) -> Fut<'a, Result<()>> {
        Box::pin(self.files_(conv, agent, files, caption))
    }

    fn status<'a>(&'a self, conv: &'a Value, agent: &'a str, wid: &'a str) -> Fut<'a, Result<Value>> {
        Box::pin(async move {
            let body = json!({"content": WORKING, "components": Self::stop_button(wid)});
            if let Some(dm) = id_of(&conv["dm"]) {
                let sent = self.say(&dm, body).await?;
                return Ok(json!({"channel": id_value(&dm), "id": sent["id"]}));
            }
            // from the agent itself, as small grey text; the webhook is the bot's, so its button works
            let (channel, thread) = Self::room_of(conv)?;
            let mut body = body;
            body["username"] = json!(self.core.display_name(agent));
            let sent = self.hook_call(&channel, &thread, Method::POST, "", Body::Json(body), true).await?;
            Ok(json!({"channel": id_value(&channel), "thread": id_value(&thread), "id": sent["id"]}))
        })
    }

    fn status_text<'a>(&'a self, handle: &'a Value, text: &'a str) -> Fut<'a, ()> {
        Box::pin(async move {
            let mut body = json!({"content": format!("-# {text}")});
            if text.contains("restarting") {
                body["components"] = json!([]);
            }
            if let Err(e) = self.edit_line(handle, body, false).await {
                tracing::debug!("{}: the working line wasn't edited: {e:#}", self.conn); // cosmetic
            }
        })
    }

    fn status_end<'a>(&'a self, conv: &'a Value, agent: &'a str, handle: &'a Value, ended: Ended) -> Fut<'a, ()> {
        Box::pin(async move {
            let shown = !handle.is_null();
            let done = if ended == Ended::Stopped && shown {
                let mut stopped = self.stopped_lines.lock().await;
                if stopped.len() > 1024 {
                    stopped.clear();
                }
                stopped.extend(id_of(&handle["id"]));
                self.edit_line(handle, json!({"content": STOPPED, "components": []}), false).await
            } else if ended == Ended::Stopped {
                self.post_(conv, agent, STOPPED, None).await
            } else if shown {
                self.edit_line(handle, Value::Null, true).await
            } else {
                Ok(())
            };
            if let Err(e) = done {
                tracing::warn!("{}: the working line wasn't closed: {e:#}", self.conn);
            }
        })
    }

    fn react<'a>(&'a self, m: &'a Incoming, what: Reaction) -> Fut<'a, ()> {
        Box::pin(async move {
            let Some((channel, message)) = Self::loc(m) else { return };
            let emoji = match what {
                Reaction::Stop => "%F0%9F%9B%91",
                Reaction::Redo => "%F0%9F%94%81",
            };
            let path = format!("/channels/{channel}/messages/{message}/reactions/{emoji}/@me");
            if let Err(e) = self.api.bot(Method::PUT, &path, Body::None).await {
                tracing::debug!("{}: no reaction: {e:#}", self.conn); // cosmetic
            }
        })
    }

    fn ask<'a>(&'a self, m: &'a Incoming, pid: &'a str, agents: &'a [String]) -> Fut<'a, Result<()>> {
        Box::pin(async move {
            let buttons: Vec<Value> = agents
                .iter()
                .take(25)
                .map(|a| json!({"type": 2, "style": 2, "label": a.chars().take(80).collect::<String>(), "custom_id": format!("kith-pick:{pid}:{a}")}))
                .collect();
            let rows: Vec<Value> = buttons.chunks(5).map(|row| json!({"type": 1, "components": row})).collect();
            self.reply_to(m, &json!({"content": "Who should take this?", "components": rows})).await
        })
    }

    fn typing<'a>(&'a self, conv: &'a Value) -> Fut<'a, ()> {
        Box::pin(async move {
            let Some(place) = Self::place(conv) else { return std::future::pending().await };
            loop {
                let _ = self.api.bot(Method::POST, &format!("/channels/{place}/typing"), Body::None).await;
                tokio::time::sleep(Duration::from_secs(8)).await;
            }
        })
    }

    fn env(&self, conv: &Value, _agent: &str) -> Vec<(String, String)> {
        if !conv["dm"].is_null() {
            return Vec::new(); // no webhooks in a DM: kith-send goes through the outbox
        }
        let (Some(channel), Some(thread)) = (id_of(&conv["channel"]), id_of(&conv["thread"])) else {
            return Vec::new();
        };
        match self.saved_hook(&channel) {
            Some(url) => {
                let mut env = Vec::new();
                env.extend(crate::kernel::both("DISCORD_WEBHOOK_URL", &url));
                env.extend(crate::kernel::both("DISCORD_THREAD_ID", &thread));
                env
            }
            None => Vec::new(),
        }
    }
}
