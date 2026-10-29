//! Bot guests: cloud agents (Grok Bot and the like) with a VM of their own that join this computer with a link and ask its
//! agents over the end-to-end relay (docs/kith-bot-guests.md). Every bot belongs to one person and never has more than
//! that person, checked live. This module has the owner's side (invite, see, allow, limit, remove), the bot's own calls
//! (`bot_call`, deny by default through `roles::BOT_METHODS`), and the one cascade that ends a bot (`end_bot`).
//!
//! kith-auth is the truth about which agent a bot may use (`kith-auth bot` / `member`); the cloud only records that a
//! key accepted an invite and the host decides what that means (`remote::botsync`).

use super::devices::{Caller, Forbidden, NotAllowed, Principal, principal};
use super::people::{AUTH_WRITES, auth_bin, chat_with, hex};
use super::{roles, str_arg};
use crate::LockExt;
use crate::agent::bot::Cmd;
use crate::environment::Environment;
use crate::hub::Hub;
use crate::remote::{account, botsync, cloud, crypto};
use crate::store::{BotConversation, BotGuest, BotInvite, BotState, Entry, EntryKind, Lane, now_ms};
use anyhow::{Context, Result, anyhow, bail};
use kith_wire::cloud::{CreateInvite, path};
use kith_wire::rpc::{BotErrorCode as Code, ConversationState as State};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::ops::ControlFlow;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A bot invite's link stays good this long (same as a person's).
const INVITE_MS: i64 = 24 * 60 * 60 * 1000;
/// Live bots one owner may have (`invited`, `joining`, `pending`, `active`).
const MAX_LIVE_BOTS: usize = 10;
/// Characters in one question.
pub const MAX_TEXT: usize = 16_000;
/// Asks waiting in one conversation, running one included.
const MAX_PENDING: usize = 5;
const HOUR_MS: i64 = 60 * 60 * 1000;
/// Defaults of `bot_guests` and what a bot's owner may raise them to; the computer's owner may go higher.
const DEFAULT_RATE: i64 = 60;
const DEFAULT_CONCURRENT: i64 = 2;
/// How long a room waits for an outside bot's post on its turn (a bot's own setting, in this range).
const DEFAULT_TURN_TIMEOUT_S: i64 = 300;
const TURN_TIMEOUT_RANGE: (i64, i64) = (30, 3600);
const OWNER_MAX_RATE: i64 = 600;
const OWNER_MAX_CONCURRENT: i64 = 8;
/// Oldest conversations counted when asking how many turns a bot has running.
const RECENT_CONVERSATIONS: i64 = 50;
/// Oldest minimum client this host still talks to (`botStatus.minClient`).
const MIN_CLIENT: &str = "0.0.0";

/// A bot call that failed in a way the client acts on: the HTTP-like status and a stable word (`err.code` on the
/// channel: `pending`, `notGranted`, `rateLimited`, `busy`, `notFound`, ...).
#[derive(Debug)]
pub struct BotError {
    pub status: u16,
    /// The contract's word for it, which a bot acts on; never `Unknown`.
    pub code: Code,
    pub message: String,
}

impl BotError {
    fn err(status: u16, code: Code, message: impl Into<String>) -> anyhow::Error {
        Self { status, code, message: message.into() }.into()
    }
}

impl std::fmt::Display for BotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for BotError {}

// MARK: the owner's and members' methods

/// Who is managing bots: the computer's owner, or a member of it.
enum Actor {
    Owner,
    Member(String),
}

impl Actor {
    fn of(hub: &Hub, caller: &Caller) -> Result<Self> {
        match principal(&hub.store, caller) {
            Principal::Owner => Ok(Self::Owner),
            Principal::Member(p) => Ok(Self::Member(p)),
            _ => Err(Forbidden.into()),
        }
    }

    /// The owner person a new bot gets: None for the computer's owner.
    fn owner_person(&self) -> Option<&str> {
        match self {
            Self::Owner => None,
            Self::Member(p) => Some(p),
        }
    }

    /// May this actor change `bot`? The computer's owner any; a member their own.
    fn manages(&self, bot: &BotGuest) -> bool {
        match self {
            Self::Owner => true,
            Self::Member(p) => bot.owner_person.as_deref() == Some(p),
        }
    }

    fn owner_only(&self) -> Result<()> {
        match self {
            Self::Owner => Ok(()),
            Self::Member(_) => Err(Forbidden.into()),
        }
    }
}

pub(super) async fn call(hub: &Arc<Hub>, caller: &Caller, method: &str, b: Value) -> Result<ControlFlow<Value, Value>> {
    Ok(ControlFlow::Break(match method {
        "createBotInvite" => create_bot_invite(hub, caller, &b).await?,
        "revokeBotInvite" => revoke_bot_invite(hub, caller, str_arg(&b, "inviteId")?).await?,
        "revokeBot" => revoke_bot(hub, caller, str_arg(&b, "botId")?).await?,
        "bots" => bots(hub, caller)?,
        "myBots" => my_bots(hub, caller)?,
        "decideBot" => decide_bot(hub, caller, &b).await?,
        "setAutoAllowBots" => set_auto_allow(hub, caller, &b).await?,
        "setBotLimits" => set_bot_limits(hub, caller, &b).await?,
        _ => return Ok(ControlFlow::Continue(b)),
    }))
}

fn opt_string(b: &Value, k: &str, max: usize) -> Result<Option<String>> {
    match b[k].as_str().map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) if s.chars().count() <= max && !s.chars().any(char::is_control) => Ok(Some(s.to_owned())),
        Some(_) => Err(BotError::err(
            400,
            Code::BadRequest,
            format!("`{k}` is at most {max} characters, without control characters"),
        )),
    }
}

/// An agent id as the kernel names agents: a name that can't pass for a flag or a list.
fn valid_agent(a: &str) -> bool {
    !a.is_empty()
        && a.len() <= 64
        && a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        && !a.starts_with(['-', '.'])
}

fn provider_of(b: &Value) -> String {
    let given = b["provider"].as_str().unwrap_or_default().trim().to_lowercase();
    if ["grok", "muse", "dots", "codex", "cursor"].contains(&given.as_str()) { given } else { "other".into() }
}

fn provider_label(provider: &str) -> &'static str {
    match provider {
        "grok" => "Grok",
        "muse" => "Muse",
        "dots" => "Dots",
        "codex" => "Codex",
        "cursor" => "Cursor",
        _ => "Bot",
    }
}

/// Percent-encodes everything but unreserved characters, for the `r=` of the app link.
fn enc(s: &str) -> String {
    s.bytes().fold(String::new(), |mut out, c| {
        if c.is_ascii_alphanumeric() || b"-._~".contains(&c) {
            out.push(char::from(c));
        } else {
            let _ = write!(out, "%{c:02X}");
        }
        out
    })
}

/// The agents set up on this computer (the owner's bots of an hq agent), which a bot of the owner may be given.
fn agents_here(hub: &Hub) -> Vec<String> {
    let mut agents: Vec<String> = hub
        .store
        .bots()
        .unwrap_or_default()
        .into_iter()
        .filter(|r| !r.deleted && r.config.person.is_none())
        .filter_map(|r| r.config.hq_agent)
        .collect();
    agents.sort();
    agents.dedup();
    agents
}

/// What a failed `createBotInvite` puts back in kith-auth.
async fn undo_auth(bin: &Path, person: &str) {
    let _w = AUTH_WRITES.lock().await;
    account::run(bin, &["bot", person, "--rm"]).await.ok();
}

/// `createBotInvite {name, provider?, agents, botId?}`: the bot (a kith-auth person that is a guest of its owner), its
/// invite, and the link and share text to hand to the bot's VM. With `botId` it makes a fresh invite for a bot the
/// caller already has (a rebuilt VM), whose new key replaces the old one when it joins.
async fn create_bot_invite(hub: &Arc<Hub>, caller: &Caller, b: &Value) -> Result<Value> {
    let actor = Actor::of(hub, caller)?;
    let base = cloud::url(&hub.store)
        .ok_or_else(|| anyhow!("The Kith cloud is off on this computer, so a bot can't be invited."))?;
    let base = base.trim_end_matches('/').to_owned();
    let bin = auth_bin().await?;
    let who = match &actor {
        Actor::Owner => Principal::Owner,
        Actor::Member(p) => Principal::Member(p.clone()),
    };
    let owner_id = account::person_of(&bin, &who).await?;

    let existing = match b["botId"].as_str().filter(|s| !s.is_empty()) {
        Some(id) => {
            let bot = hub.store.bot_guest(id).filter(|g| g.state.live() && actor.manages(g));
            Some(bot.ok_or_else(|| NotAllowed("That isn't one of your bots.".into()))?)
        }
        None => None,
    };
    let (bot, fresh) = match existing {
        Some(bot) => (bot, false),
        None => (new_bot(hub, &actor, &bin, &owner_id, b).await?, true),
    };

    let token = crypto::b64(&crypto::random::<32>());
    let proof = hex(&crypto::sha256(&[token.as_bytes()]));
    let invite_id = format!("bi_{}", hex(&crypto::random::<6>()));
    let now = now_ms();
    let invite = BotInvite {
        invite_id: invite_id.clone(),
        bot_id: bot.bot_id.clone(),
        token_hash: proof.clone(),
        created_at: now,
        expires_at: now + INVITE_MS,
        used_at: None,
        revoked_at: None,
    };
    let posted = match hub.store.put_bot_invite(&invite) {
        Ok(()) => {
            // The cloud sees sha256(proof) only, so it can neither accept nor prove the invite by itself.
            let body = CreateInvite {
                invite_id: invite_id.clone(),
                token_hash: hex(&crypto::sha256(&[proof.as_bytes()])),
                expires_at: invite.expires_at,
            };
            cloud::post(hub, path::HOST_CREATE_BOT_INVITE, &body).await.map(drop)
        }
        Err(e) => Err(e),
    };
    if let Err(e) = posted {
        hub.store.delete_bot_invite(&invite_id).ok();
        if fresh {
            hub.store.delete_bot_guest(&bot.bot_id).ok();
            undo_auth(&bin, &bot.person_id).await;
        }
        return Err(e.context("The Kith cloud didn't take the invite, so none was made"));
    }
    let actor_id = actor_id(hub, &bin, caller).await;
    hub.store.audit_add(&actor_id, caller.device_key(), "bot.invite", Some(&bot.bot_id), true);
    emit_updated(hub, &bot.bot_id);

    let (cid, host_key) = (hub.identity.computer_id(), hub.identity.sign_pub_b64());
    let link = format!("{base}/b/{cid}/{invite_id}#t={token}&k={host_key}");
    let kith_link = format!(
        "{}://bot?c={cid}&i={invite_id}&t={token}&k={host_key}&r={}",
        Environment::current().url_scheme(),
        enc(&base)
    );
    let share = share_text(&link, &bot.agents);
    Ok(json!({
        "botId": bot.bot_id, "inviteId": invite_id, "expiresAt": invite.expires_at, "link": link,
        "kithLink": kith_link, "share": share,
    }))
}

/// The text the owner pastes into their bot (docs/kith-bot-guests.md 5.2), written once here so the apps don't each
/// carry their own wording.
fn share_text(link: &str, agents: &[String]) -> String {
    let agent = agents.first().map_or("house", String::as_str);
    format!(
        "Install the Kith client, then join my Kith computer. This link works once and expires in 24 hours.\n\n  \
         curl -fsSL https://raw.githubusercontent.com/Ying-Kai-Liao/kith/main/scripts/install-guest.sh | sh\n  \
         kith join '{link}'\n\n\
         Afterwards ask my agents with:  kith ask @{agent} \"your question\"\n\
         Run `kith --help` for the rest.\n\n\
         If I put you in a Kith room, you are woken (a webhook run) when it is your turn. The wake names a room, a \
         conversation, an epoch, a turnKey and a `since` cursor. On a wake, run:\n  \
         kith room read <room> --since <since>\n  \
         kith room post <room> --conversation <conversation> --epoch <epoch> --turn-key <turnKey> \"your reply\"\n\
         Post once per wake, or post (pass) when you have nothing to add. Please allow `kith` to run when asked: \
         approvals for routine runs expire after about 30 minutes, so allow it again if a wake finds it blocked.\n"
    )
}

/// A new bot: its kith-auth person and grants, then its row. Checked against what the caller holds first.
async fn new_bot(hub: &Arc<Hub>, actor: &Actor, bin: &Path, owner_id: &str, b: &Value) -> Result<BotGuest> {
    let name = opt_string(b, "name", 40)?.ok_or_else(|| anyhow!("Give the bot a `name` (1 to 40 characters)."))?;
    let mut agents: Vec<String> =
        b["agents"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect();
    agents.sort();
    agents.dedup();
    if let Some(bad) = agents.iter().find(|a| !valid_agent(a)) {
        bail!("`{bad}` isn't an agent id.");
    }
    if hub.store.live_bot_count(actor.owner_person()) >= MAX_LIVE_BOTS {
        return Err(BotError::err(
            409,
            Code::TooMany,
            format!("At most {MAX_LIVE_BOTS} bots at a time. Remove one first."),
        ));
    }
    // A bot has at most what its owner has: a member can give only what they hold, the owner what is set up here.
    let here = agents_here(hub);
    for agent in &agents {
        let ok = match actor {
            Actor::Owner => here.contains(agent),
            Actor::Member(p) => roles::may_use(p, agent).await,
        };
        if !ok {
            return Err(NotAllowed(format!("You can't give a bot {agent}.")).into());
        }
    }
    let provider = provider_of(b);
    let bot = BotGuest {
        bot_id: format!("bg_{}", hex(&crypto::random::<6>())),
        person_id: format!("bot-{}", hex(&crypto::random::<4>())),
        owner_person: actor.owner_person().map(str::to_owned),
        name: name.clone(),
        provider: provider.clone(),
        agents,
        state: BotState::Invited,
        device_key: None,
        cloud_device: None,
        rate_per_hour: DEFAULT_RATE,
        concurrent: DEFAULT_CONCURRENT,
        turn_timeout_s: DEFAULT_TURN_TIMEOUT_S,
        wake_profile: None,
        created_at: now_ms(),
        joined_at: None,
        decided_at: None,
        decided_by: None,
        last_seen_at: None,
        revoked_at: None,
        revoked_why: None,
    };
    {
        let _w = AUTH_WRITES.lock().await;
        let (name_arg, provider_arg) = (format!("name={name}"), format!("provider={provider}"));
        account::run(bin, &["bot", &bot.person_id, "--of", owner_id, &name_arg, &provider_arg])
            .await
            .context("making the bot's person")?;
        for agent in &bot.agents {
            if let Err(e) = account::run(bin, &["member", agent, &bot.person_id, "preset=guest"]).await {
                account::run(bin, &["bot", &bot.person_id, "--rm"]).await.ok();
                return Err(e.context(format!("giving the bot {agent}")));
            }
        }
    }
    if let Err(e) = hub.store.put_bot_guest(&bot) {
        undo_auth(bin, &bot.person_id).await;
        return Err(e);
    }
    Ok(bot)
}

/// The caller's person id for the audit log; the computer's owner when kith-auth can't say.
async fn actor_id(hub: &Hub, bin: &Path, caller: &Caller) -> String {
    account::caller_person(hub, bin, caller).await.unwrap_or_else(|_| "owner".into())
}

async fn revoke_bot_invite(hub: &Arc<Hub>, caller: &Caller, invite_id: &str) -> Result<Value> {
    let actor = Actor::of(hub, caller)?;
    let invite = hub.store.bot_invite(invite_id);
    let bot = invite.as_ref().and_then(|i| hub.store.bot_guest(&i.bot_id)).filter(|g| actor.manages(g));
    let (Some(_), Some(bot)) = (invite, bot) else {
        return Err(NotAllowed("That isn't one of your bots' invites.".into()).into());
    };
    let revoked = hub.store.revoke_bot_invite(invite_id)?;
    if revoked {
        botsync::revoke_invite_in_cloud(hub, invite_id).await;
        // A bot that never joined is gone with its only invite.
        if bot.state == BotState::Invited && !hub.store.bot_invites_of(&bot.bot_id)?.iter().any(|i| i.open(now_ms())) {
            end_bot(hub, &bot.bot_id, BotState::Revoked, actor_kind(&actor), &actor_label(hub, caller).await).await;
        }
    }
    Ok(json!({"revoked": revoked}))
}

fn actor_kind(actor: &Actor) -> &'static str {
    match actor {
        Actor::Owner => "kai",
        Actor::Member(_) => "owner",
    }
}

/// `revokeBot {botId}`: the owner's own bot, or any bot for the computer's owner. Removing twice is fine.
async fn revoke_bot(hub: &Arc<Hub>, caller: &Caller, bot_id: &str) -> Result<Value> {
    let actor = Actor::of(hub, caller)?;
    let bot = hub.store.bot_guest(bot_id).filter(|g| actor.manages(g));
    let bot = bot.ok_or_else(|| NotAllowed("That isn't one of your bots.".into()))?;
    let ended = end_bot(hub, &bot.bot_id, BotState::Revoked, actor_kind(&actor), &actor_label(hub, caller).await).await;
    Ok(json!({"revoked": ended}))
}

async fn actor_label(hub: &Hub, caller: &Caller) -> String {
    match account::bin().await {
        Some(bin) => actor_id(hub, &bin, caller).await,
        None => "owner".into(),
    }
}

fn limits_of(g: &BotGuest) -> Value {
    json!({
        "ratePerHour": g.rate_per_hour, "concurrent": g.concurrent,
        "turnTimeoutS": g.turn_timeout_s, "wakeProfile": g.wake_profile,
    })
}

/// A kith-auth profile name as `kith send-to --profile` takes it: a plain word or path, never a flag.
fn valid_profile(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 64
        && p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
        && !p.starts_with(['-', '.', '/'])
        && !p.contains("..")
}

/// A bot as the apps get it. `full` (the computer's owner) adds its person id and conversations.
fn view(hub: &Hub, g: &BotGuest, full: bool) -> Value {
    let owner_name = g.owner_person.as_deref().and_then(|p| hub.store.person(p)).map(|p| p.name);
    let connected = g.device_key.as_deref().is_some_and(|k| hub.connected.locked().contains_key(k));
    let mut v = json!({
        "botId": g.bot_id, "name": g.name, "provider": g.provider, "state": g.state.as_str(),
        "ownerPerson": g.owner_person, "ownerName": owner_name, "agents": g.agents,
        "limits": limits_of(g), "connected": connected,
        "createdAt": g.created_at, "joinedAt": g.joined_at, "decidedAt": g.decided_at, "decidedBy": g.decided_by,
        "lastSeenAt": g.last_seen_at, "revokedAt": g.revoked_at, "revokedWhy": g.revoked_why,
    });
    if full {
        v["personId"] = g.person_id.clone().into();
        let convs = hub.store.bot_conversations_of(&g.bot_id, 100).unwrap_or_default();
        v["conversations"] = convs
            .iter()
            .map(|c| {
                json!({"conversation": c.conversation_id, "agent": c.agent, "chatBotId": c.chat_bot_id,
                       "threadRoot": c.thread_root, "originLabel": c.origin_label, "lastAt": c.last_at})
            })
            .collect();
    }
    v
}

/// `bots {}` (the computer's owner): every bot with its owner, newest first.
fn bots(hub: &Arc<Hub>, caller: &Caller) -> Result<Value> {
    Actor::of(hub, caller)?.owner_only()?;
    let mut all = hub.store.bot_guests()?;
    all.reverse();
    Ok(json!({"bots": all.iter().map(|g| view(hub, g, true)).collect::<Vec<_>>()}))
}

/// `myBots {}`: the caller's own bots, ended ones too (so they see why).
fn my_bots(hub: &Arc<Hub>, caller: &Caller) -> Result<Value> {
    let actor = Actor::of(hub, caller)?;
    let mut mine: Vec<BotGuest> = hub
        .store
        .bot_guests()?
        .into_iter()
        .filter(|g| match &actor {
            Actor::Owner => g.owner_person.is_none(),
            Actor::Member(p) => g.owner_person.as_deref() == Some(p),
        })
        .collect();
    mine.reverse();
    Ok(json!({"bots": mine.iter().map(|g| view(hub, g, false)).collect::<Vec<_>>()}))
}

/// `decideBot {botId, decision: "allow" | "deny", alwaysForOwner?}` (the computer's owner). Answering twice is fine.
async fn decide_bot(hub: &Arc<Hub>, caller: &Caller, b: &Value) -> Result<Value> {
    Actor::of(hub, caller)?.owner_only()?;
    let id = str_arg(b, "botId")?;
    let bot = hub.store.bot_guest(id).ok_or_else(|| anyhow!("unknown bot"))?;
    let actor = actor_label(hub, caller).await;
    match str_arg(b, "decision")? {
        "allow" => {
            match bot.state {
                BotState::Pending => {
                    if hub.store.decide_bot_guest(id, BotState::Active, "kai", now_ms())? {
                        hub.store.audit_add(&actor, caller.device_key(), "bot.approve", Some(id), true);
                    }
                }
                BotState::Active => {}
                _ => bail!("That bot isn't waiting for an answer."),
            }
            if b["alwaysForOwner"] == true
                && let Some(owner) = &bot.owner_person
            {
                set_auto(hub, owner, true, &actor, caller.device_key())?;
            }
        }
        "deny" => match bot.state {
            BotState::Pending => {
                end_bot(hub, id, BotState::Denied, "denied", &actor).await;
            }
            BotState::Denied => {}
            _ => bail!("That bot isn't waiting for an answer. Remove it instead."),
        },
        _ => bail!("`decision` is `allow` or `deny`"),
    }
    emit_updated(hub, id);
    Ok(json!({"bot": hub.store.bot_guest(id).map(|g| view(hub, &g, true))}))
}

/// Turns the per-person toggle on or off; turning it on lets the person's waiting bots in.
fn set_auto(hub: &Hub, person: &str, on: bool, actor: &str, device: &str) -> Result<()> {
    if !hub.store.set_auto_bots(person, on)? {
        bail!("`{person}` isn't someone you invited");
    }
    if on {
        let waiting = hub.store.bot_guests()?.into_iter();
        for g in waiting.filter(|g| g.state == BotState::Pending && g.owner_person.as_deref() == Some(person)) {
            if hub.store.decide_bot_guest(&g.bot_id, BotState::Active, "auto", now_ms())? {
                hub.store.audit_add(actor, device, "bot.approve", Some(&g.bot_id), true);
                emit_updated(hub, &g.bot_id);
            }
        }
    }
    Ok(())
}

/// `setAutoAllowBots {personId, on}` (the computer's owner).
async fn set_auto_allow(hub: &Arc<Hub>, caller: &Caller, b: &Value) -> Result<Value> {
    Actor::of(hub, caller)?.owner_only()?;
    let (person, on) = (str_arg(b, "personId")?, b["on"].as_bool().ok_or_else(|| anyhow!("`on` is required"))?);
    set_auto(hub, person, on, &actor_label(hub, caller).await, caller.device_key())?;
    Ok(json!({"person": hub.store.person(person)}))
}

/// `setBotLimits {botId, ratePerHour?, concurrent?}`: a bot's owner can lower them (back to the defaults at most), the
/// computer's owner can raise them.
async fn set_bot_limits(hub: &Arc<Hub>, caller: &Caller, b: &Value) -> Result<Value> {
    let actor = Actor::of(hub, caller)?;
    let id = str_arg(b, "botId")?;
    let bot = hub.store.bot_guest(id).filter(|g| g.state.live() && actor.manages(g));
    let bot = bot.ok_or_else(|| NotAllowed("That isn't one of your bots.".into()))?;
    let (max_rate, max_concurrent) = match actor {
        Actor::Owner => (OWNER_MAX_RATE, OWNER_MAX_CONCURRENT),
        Actor::Member(_) => (DEFAULT_RATE, DEFAULT_CONCURRENT),
    };
    let pick = |k: &str, now: i64, max: i64| -> Result<i64> {
        match b[k].as_i64() {
            None if b[k].is_null() => Ok(now),
            Some(n) if (1..=max).contains(&n) => Ok(n),
            _ => bail!("`{k}` is a number from 1 to {max}"),
        }
    };
    let rate = pick("ratePerHour", bot.rate_per_hour, max_rate)?;
    let concurrent = pick("concurrent", bot.concurrent, max_concurrent)?;
    // A room waits this long for the bot's post; out of range is clamped, not refused.
    let timeout = match b["turnTimeoutS"].as_i64() {
        None if b["turnTimeoutS"].is_null() => bot.turn_timeout_s,
        Some(n) => n.clamp(TURN_TIMEOUT_RANGE.0, TURN_TIMEOUT_RANGE.1),
        None => bail!("`turnTimeoutS` is a number of seconds"),
    };
    // The wake goes out through a key on this Mac, so only the computer's owner picks which profile it uses.
    let wake = match b["wakeProfile"].as_str() {
        None if b["wakeProfile"].is_null() => bot.wake_profile.clone(),
        Some(_) if matches!(actor, Actor::Member(_)) => return Err(Forbidden.into()),
        Some("") => None,
        Some(p) if valid_profile(p.trim()) => Some(p.trim().to_owned()),
        _ => bail!("`wakeProfile` is the name of a kith-auth profile (letters, digits, - _ . /), or empty for none"),
    };
    hub.store.set_bot_limits(id, rate, concurrent)?;
    hub.store.set_bot_room_settings(id, timeout, wake.as_deref())?;
    hub.store.audit_add(&actor_label(hub, caller).await, caller.device_key(), "bot.limits", Some(id), true);
    emit_updated(hub, id);
    Ok(json!({"bot": hub.store.bot_guest(id).map(|g| view(hub, &g, false))}))
}

// MARK: ending a bot

/// Tells the apps a bot changed (the computer's owner and the bot's own owner hear of it).
pub(crate) fn emit_updated(hub: &Hub, bot_id: &str) {
    if let Some(g) = hub.store.bot_guest(bot_id) {
        let _ = hub.events.send(json!({"type": "botUpdated", "bot": view(hub, &g, false)}));
    }
}

/// The chats (`bots` rows) a bot guest talks in, one per agent.
fn chats_of(hub: &Hub, person: &str) -> Vec<(String, Option<String>)> {
    hub.store
        .bots()
        .unwrap_or_default()
        .into_iter()
        .filter(|r| !r.deleted && r.config.person.as_deref() == Some(person))
        .map(|r| (r.config.id, r.config.hq_agent))
        .collect()
}

/// The one place a bot ends (docs/kith-bot-guests.md 7.5), whatever the trigger: the owner, the computer's owner, the
/// bot itself, its owner's person revoked, a denied request, the cloud, a join that never finished. `final_state` is
/// `Revoked`, or `Denied` for a request answered no. Running the steps again is harmless: only the first caller
/// gets past the row. Returns whether this call ended it.
pub(crate) async fn end_bot(hub: &Hub, bot_id: &str, final_state: BotState, why: &str, actor: &str) -> bool {
    let Some(bot) = hub.store.bot_guest(bot_id) else { return false };
    // First, so every in-flight check fails from here on.
    match hub.store.end_bot_guest(bot_id, final_state, why, now_ms()) {
        Ok(true) => {}
        Ok(false) => return false,
        Err(e) => {
            tracing::warn!(error = format!("{e:#}"), bot = bot_id, "couldn't end a bot");
            return false;
        }
    }
    // A room waiting on this bot's post stops waiting: its turn ends as a pass.
    crate::chat::group::cancel_bot_turns(|t| t.bot_id == bot.bot_id);
    // What the bot had asked its owner for ends with it (A7).
    crate::cards::withdraw_person(hub, &bot.person_id, None);
    if let Some(key) = &bot.device_key
        && let Err(e) = hub.revoke_device(key)
    {
        tracing::warn!(error = format!("{e:#}"), bot = bot_id, "couldn't remove a bot's device");
    }
    // Running and queued turns stop, with the pending cards; what was written so far stays, marked ended.
    for (chat, _) in chats_of(hub, &bot.person_id) {
        if let Err(e) = hub.send_cmd(&chat, Cmd::Stop) {
            tracing::warn!(error = format!("{e:#}"), bot = bot_id, "couldn't stop a bot's turn");
        }
    }
    // Then whatever still runs as the bot: processes a restarted host no longer knows, found by their claims.
    sweep_claims(hub, vec![bot.person_id.clone()], false).await;
    for c in hub.store.bot_conversations_of(bot_id, 1000).unwrap_or_default() {
        let lane = Lane::in_thread(&c.chat_bot_id, &c.thread_root);
        let data = json!({"text": "This bot's access to this computer ended.", "style": "info", "ended": why});
        hub.add_entry(&lane, EntryKind::Notice, hub.store.max_turn(&c.chat_bot_id), &data);
    }
    // kith-auth: the person and, with it, its memberships. Its memory folder stays (a rejoin gets it back).
    if let Some(bin) = account::bin().await {
        let _w = AUTH_WRITES.lock().await;
        if let Err(e) = account::run(&bin, &["bot", &bot.person_id, "--rm"]).await {
            tracing::warn!(error = format!("{e:#}"), bot = bot_id, "couldn't remove a bot's person from kith-auth");
        }
    }
    if let Some(cloud_device) = &bot.cloud_device {
        botsync::revoke_device_in_cloud(hub, cloud_device).await;
    }
    for invite in hub.store.bot_invites_of(bot_id).unwrap_or_default().iter().filter(|i| i.used_at.is_none()) {
        if hub.store.revoke_bot_invite(&invite.invite_id).unwrap_or(false) {
            botsync::revoke_invite_in_cloud(hub, &invite.invite_id).await;
        }
    }
    let method = if final_state == BotState::Denied { "bot.deny" } else { "bot.revoke" };
    hub.store.audit_add(actor, "", method, Some(bot_id), true);
    emit_updated(hub, bot_id);
    tracing::info!(bot = bot_id, why, "a bot guest ended");
    true
}

/// Ends every live bot of `person` (their person was revoked, or their last membership is gone), and any process still
/// running with `person`'s own profile. Callers pass invited people only.
pub(crate) async fn revoke_owned_by(hub: &Hub, person: &str, why: &str) {
    for g in hub.store.bot_guests().unwrap_or_default() {
        if g.state.live() && g.owner_person.as_deref() == Some(person) {
            end_bot(hub, &g.bot_id, BotState::Revoked, why, "host").await;
        }
    }
    sweep_claims(hub, vec![person.to_owned()], false).await;
}

/// Kills the holders of every writer claim that names one of `persons`, and what they started (see
/// `terminal::claim::sweep`): the runs a restarted host lost track of. Never the owner's (their claims name nobody).
/// With `only_denied` (access narrowed, not removed) a holder survives while its person may still use its agent; a claim
/// that names no agent (from before claims did) cannot be judged and goes.
pub(crate) async fn sweep_claims(hub: &Hub, persons: Vec<String>, only_denied: bool) {
    let state = hub.terminal.env().state;
    let mut allowed: std::collections::BTreeSet<(String, String)> = std::collections::BTreeSet::new();
    if only_denied {
        let (st, ps) = (state.clone(), persons.clone());
        let held =
            tokio::task::spawn_blocking(move || crate::terminal::claim::holders_of(&st, &ps)).await.unwrap_or_default();
        for (person, agent) in held.into_iter().filter_map(|c| Some((c.person?, c.agent?))) {
            if roles::may_use(&person, &agent).await {
                allowed.insert((person, agent));
            }
        }
    }
    let ended = tokio::task::spawn_blocking(move || {
        let keep = |c: &crate::terminal::claim::Claim| {
            only_denied && c.person.clone().zip(c.agent.clone()).is_some_and(|pa| allowed.contains(&pa))
        };
        crate::terminal::claim::sweep(&state, &persons, &keep, Duration::from_secs(2))
    })
    .await
    .unwrap_or_default();
    if !ended.is_empty() {
        tracing::info!(pids = ?ended, "ended runs left over after access was removed");
    }
}

/// After `person`'s access changed: a bot's turn on an agent its owner no longer holds stops now, so a long turn
/// doesn't outlive the permission. (`may_use` already refuses the bot's next call.)
pub(crate) async fn enforce(hub: &Hub, person: &str) {
    for g in hub.store.bot_guests().unwrap_or_default() {
        if !g.state.live() || g.owner_person.as_deref() != Some(person) {
            continue;
        }
        for (chat, agent) in chats_of(hub, &g.person_id) {
            let Some(agent) = agent else { continue };
            if !roles::may_use(&g.person_id, &agent).await
                && let Err(e) = hub.send_cmd(&chat, Cmd::Stop)
            {
                tracing::warn!(error = format!("{e:#}"), bot = g.bot_id, "couldn't stop a bot's turn");
            }
        }
    }
}

// MARK: the bot's own calls

/// A call from a bot guest's key: the allow-list, every call audited, the method itself, answers shaped for a bot.
pub(super) async fn bot_call(
    hub: &Arc<Hub>,
    caller: &Caller,
    who: Principal,
    method: &str,
    b: Value,
    permitted: Result<(), Forbidden>,
) -> Result<Value> {
    let (bot_id, active) = match who {
        Principal::Bot { bot_id, .. } => (bot_id, true),
        Principal::BotPending(bot_id) => (bot_id, false),
        _ => return Err(Forbidden.into()),
    };
    let Some(bot) = hub.store.bot_guest(&bot_id) else { return Err(Forbidden.into()) };
    let verdict = match permitted {
        Err(e) => Err(e.into()),
        Ok(()) if !roles::bot_may(method) => {
            Err(NotAllowed("This isn't available to bots on this computer.".into()).into())
        }
        Ok(()) if !active && !roles::BOT_PENDING_METHODS.contains(&method) => {
            Err(BotError::err(403, Code::Pending, "This bot is waiting for its owner's allow."))
        }
        Ok(()) => Ok(()),
    };
    let target = roles::target_of(&b);
    let outcome = match verdict {
        Ok(()) => bot_method(hub, caller, &bot, active, method, &b).await,
        Err(e) => Err(e),
    };
    hub.store.touch_bot_guest(&bot.bot_id);
    // A sent ask audits itself, and a retry of one isn't a new ask and isn't logged; the proof counts only when it
    // checks out.
    let ask_ok = matches!(method, "botAsk" | "botPost") && outcome.is_ok();
    if !ask_ok {
        hub.store.audit_add(&bot.person_id, caller.device_key(), method, target.as_deref(), outcome.is_ok());
    }
    let mut shaped = outcome?;
    if method == "hello" {
        shaped = roles::shape(hub, &bot.person_id, method, shaped);
        if shaped.is_object() {
            shaped["grantedAgents"] = roles::granted_agents(hub, &bot.person_id).await.into();
        }
    }
    #[cfg(test)]
    super::answers_as_the_contract_says(if active { "bot" } else { "botPending" }, method, &shaped);
    Ok(shaped)
}

async fn bot_method(
    hub: &Arc<Hub>,
    caller: &Caller,
    bot: &BotGuest,
    active: bool,
    method: &str,
    b: &Value,
) -> Result<Value> {
    match method {
        "hello" => super::route(hub, caller, "hello", b.clone()).await,
        "botStatus" => status(hub, bot, active).await,
        "proveBotInvite" => prove(hub, caller, bot, b).await,
        "botAsk" => ask(hub, caller, bot, b).await,
        "botPost" => post(hub, caller, bot, b).await,
        "botRoomRead" => room_read(hub, bot, b).await,
        "botThread" => thread(hub, bot, b).await,
        "botConversations" => conversations(hub, bot, b),
        "botStop" => stop(hub, bot, b),
        "botLeave" => leave(hub, bot).await,
        _ => Err(Forbidden.into()),
    }
}

/// The owner's name as the bot's owner is known to people: their name, or the computer owner's from kith-auth.
async fn owner_label(hub: &Hub, bot: &BotGuest) -> String {
    if let Some(p) = bot.owner_person.as_deref() {
        return hub.store.person(p).map_or_else(|| p.to_owned(), |p| p.name);
    }
    match bot.agents.first() {
        Some(agent) => roles::owner_name(agent).await.unwrap_or_else(|| "the owner".into()),
        None => "the owner".into(),
    }
}

/// `botStatus {}`: the bot's own state, owner and agents. The agents are the live answer, so an owner who lost one
/// shows it gone at once; a bot that isn't in yet sees none.
async fn status(hub: &Hub, bot: &BotGuest, active: bool) -> Result<Value> {
    let agents = if active { roles::granted_agents(hub, &bot.person_id).await } else { vec![] };
    Ok(json!({
        "botId": bot.bot_id, "name": bot.name, "state": bot.state.as_str(),
        "owner": {"name": owner_label(hub, bot).await}, "agents": agents,
        "limits": limits_of(bot),
        "version": env!("CARGO_PKG_VERSION"), "minClient": MIN_CLIENT,
    }))
}

/// Wrong tries a key may make at `proveBotInvite` per window.
const PROOF_TRIES: usize = 5;
const PROOF_WINDOW: Duration = Duration::from_secs(10 * 60);
static PROVE_ATTEMPTS: std::sync::Mutex<BTreeMap<String, Vec<Instant>>> = std::sync::Mutex::new(BTreeMap::new());

/// `proveBotInvite {inviteId, token}`: the bot's new key shows the invite token from its link, which the cloud never
/// saw. Until it does the key can call nothing else, so a cloud that made up a bot device still reaches no agent.
/// Then the state is decided (docs/kith-bot-guests.md 7.4). Proving twice is fine.
async fn prove(hub: &Arc<Hub>, caller: &Caller, bot: &BotGuest, b: &Value) -> Result<Value> {
    let key = caller.device_key().to_owned();
    // Already in: a repeat (kith join run twice) answers where the bot stands.
    if hub.store.device_proven(&key) {
        return Ok(json!({"state": bot.state.as_str()}));
    }
    {
        let mut tries = PROVE_ATTEMPTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mine = tries.entry(key.clone()).or_default();
        mine.retain(|t| t.elapsed() < PROOF_WINDOW);
        if mine.len() >= PROOF_TRIES {
            return Err(NotAllowed("Too many tries. Wait a few minutes.".into()).into());
        }
        mine.push(Instant::now());
    }
    let (id, token) = (str_arg(b, "inviteId")?, str_arg(b, "token")?);
    let stored = hub.store.bot_invite(id).filter(|i| i.bot_id == bot.bot_id && i.open(now_ms()));
    let given = crypto::sha256(&[hex(&crypto::sha256(&[token.as_bytes()])).as_bytes()]);
    let ok = stored.as_ref().is_some_and(|i| crypto::ct_eq(&crypto::sha256(&[i.token_hash.as_bytes()]), &given));
    if !ok {
        return Err(NotAllowed("That isn't this bot's invite token.".into()).into());
    }
    if !hub.store.use_bot_invite(id, now_ms())? {
        return Err(NotAllowed("That invite was already used.".into()).into());
    }
    hub.store.set_device_proven(&key)?;
    PROVE_ATTEMPTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&key);
    // A bot already allowed (a re-keyed one) stays allowed; the computer owner's own bots and the bots of a person
    // they trust are in at once; the rest wait for the owner's answer.
    let owner_auto = match &bot.owner_person {
        None => true,
        Some(p) => hub.store.person(p).is_some_and(|p| p.auto_bots),
    };
    let (state, by) = if bot.state == BotState::Active || owner_auto {
        (
            BotState::Active,
            if bot.state == BotState::Active { bot.decided_by.as_deref().unwrap_or("auto") } else { "auto" },
        )
    } else {
        (BotState::Pending, "")
    };
    hub.store.set_bot_proven(&bot.bot_id, state, by, now_ms())?;
    let label = owner_label(hub, bot).await;
    hub.store.audit_add(&bot.person_id, &key, "bot.prove", Some(&bot.bot_id), true);
    if state == BotState::Pending {
        // To the computer's owner only: a DecisionCard with Allow, Deny and "Always allow <owner>'s bots".
        let _ = hub.events.send(json!({"type": "botRequest", "bot": {
            "botId": bot.bot_id, "name": bot.name, "provider": bot.provider, "ownerPerson": bot.owner_person,
            "ownerName": label, "agents": bot.agents, "createdAt": bot.created_at,
        }}));
    }
    emit_updated(hub, &bot.bot_id);
    Ok(json!({"state": state.as_str(), "botId": bot.bot_id, "owner": label, "agents": bot.agents}))
}

/// The ask the bot already sent with this nonce: the same answer, nothing sent twice.
fn replay(hub: &Hub, bot: &BotGuest, e: &Entry) -> Result<Value> {
    let conv = e
        .thread_id
        .as_deref()
        .and_then(|root| hub.store.bot_conversation_of_thread(&e.bot_id, root))
        .filter(|c| c.bot_id == bot.bot_id)
        .ok_or_else(|| BotError::err(404, Code::NotFound, "No such conversation."))?;
    let (state, queued) = conv_state(hub, &conv);
    Ok(json!({
        "conversation": conv.conversation_id, "entryId": e.id, "turn": e.turn, "seq": e.rev,
        "state": state, "queued": queued, "duplicate": true,
    }))
}

/// Whether `e`, the ask this bot already sent under a nonce, is the one now being sent again: the same trimmed text, to
/// the same agent, in the same conversation. A conversation named by id or `ref` must be the earlier one; an ask that
/// named neither made a new conversation then and may do again.
fn is_retry(
    hub: &Hub,
    e: &Entry,
    text: &str,
    agent: &str,
    known: Option<&BotConversation>,
    external: Option<&str>,
) -> bool {
    if e.data["text"].as_str() != Some(text) {
        return false;
    }
    let earlier = e.thread_id.as_deref().and_then(|root| hub.store.bot_conversation_of_thread(&e.bot_id, root));
    let Some(earlier) = earlier else {
        // Its conversation is gone (ended): same text still replays, to the 404 it always got.
        return true;
    };
    earlier.agent == agent
        && known.is_none_or(|c| c.conversation_id == earlier.conversation_id)
        && external.is_none_or(|r| earlier.external_ref.as_deref() == Some(r))
}

/// Where a conversation stands: `running`, `waitingApproval`, `queued`, `stopped` or `idle`, and how many asks wait
/// behind the running one.
fn conv_state(hub: &Hub, conv: &BotConversation) -> (State, usize) {
    let entries = hub.store.thread(&conv.chat_bot_id, &conv.thread_root, 500).unwrap_or_default();
    let users: Vec<&Entry> = entries.iter().filter(|e| e.kind == EntryKind::User.as_str()).collect();
    let queued = users.iter().filter(|e| e.data["status"] == "queued").count();
    let lane = Lane::in_thread(&conv.chat_bot_id, &conv.thread_root);
    let working = hub
        .store
        .bot(&conv.chat_bot_id)
        .ok()
        .flatten()
        .is_some_and(|row| hub.working_lanes(&row.config).contains(&lane));
    let waiting = entries.iter().any(|e| e.kind == EntryKind::Permission.as_str() && e.data["status"] == "pending");
    let state = if working && waiting {
        State::WaitingApproval
    } else if working {
        State::Running
    } else if queued > 0 {
        State::Queued
    } else if users.last().is_some_and(|e| e.data["status"] == "cancelled") {
        State::Stopped
    } else {
        State::Idle
    };
    (state, queued)
}

/// The chat row of `person` and `agent` when there already is one.
fn chat_row_of(hub: &Hub, person: &str, agent: &str) -> Option<String> {
    chats_of(hub, person).into_iter().find(|(_, a)| a.as_deref() == Some(agent)).map(|(id, _)| id)
}

/// How long a bot ask waits for this host to catch up with the cloud before it is refused (retryable).
#[cfg(not(test))]
const SYNC_WAIT: Duration = Duration::from_secs(10);
#[cfg(test)]
const SYNC_WAIT: Duration = Duration::from_millis(300);
/// One ask at a time between "has this been sent already" and "send it", so a retry racing its original can't send
/// twice.
static ASK_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// `botAsk {agent?, text, conversation?, ref?, speaker?, clientNonce, noQueue?}` (docs/kith-bot-guests.md 5.8): a message
/// into a conversation between this bot and one agent, made on first use. The question runs as the bot's own person
/// (guest, at most its owner's access, checked now), in a thread of the bot's chat with that agent.
async fn ask(hub: &Arc<Hub>, caller: &Caller, bot: &BotGuest, b: &Value) -> Result<Value> {
    let bad = |m: &str| BotError::err(400, Code::BadRequest, m);
    let text = b["text"].as_str().map(str::trim).filter(|t| !t.is_empty()).ok_or_else(|| bad("`text` is required"))?;
    if text.chars().count() > MAX_TEXT {
        return Err(BotError::err(400, Code::TooLong, format!("A question is at most {MAX_TEXT} characters.")));
    }
    let nonce = b["clientNonce"].as_str().filter(|n| (1..=128).contains(&n.chars().count()));
    let nonce = nonce.ok_or_else(|| bad("`clientNonce` (1 to 128 characters) is required"))?;
    let external = opt_string(b, "ref", 128)?;
    let speaker = opt_string(b, "speaker", 128)?;
    let room = match &b["room"] {
        Value::Null => None,
        Value::String(r) if !r.is_empty() => Some(r.as_str()),
        _ => return Err(bad("`room` is a room id")),
    };
    let no_queue = b["noQueue"] == true;
    // The cloud may have just recorded a revoke of the bot's owner: no turn starts until this host has applied it.
    if !crate::remote::sync_lease::wait(hub, SYNC_WAIT).await {
        return Err(BotError::err(
            503,
            Code::Syncing,
            "This computer is catching up with the Kith cloud. Try again in a moment.",
        ));
    }
    let _one = ASK_LOCK.lock().await;
    if let Some(room) = room {
        if b["conversation"].as_str().is_some_and(|c| !c.is_empty()) || external.is_some() {
            return Err(bad("A room ask has no conversation or ref: it is one turn of an agent in the room."));
        }
        return ask_room(hub, caller, bot, room, text, nonce, speaker).await;
    }

    // Which conversation. Another bot's id is a 404, never a 403: no existence leak.
    let known = match b["conversation"].as_str().filter(|c| !c.is_empty()) {
        Some(id) => Some(
            hub.store
                .bot_conversation_by_id(id)
                .filter(|c| c.bot_id == bot.bot_id)
                .ok_or_else(|| BotError::err(404, Code::NotFound, "No such conversation."))?,
        ),
        None => external.as_deref().and_then(|r| hub.store.bot_conversation_by_ref(&bot.bot_id, r)),
    };
    let named = b["agent"].as_str().filter(|a| !a.is_empty());
    let agent = match (&known, named) {
        (Some(c), Some(a)) if c.agent != a => return Err(bad("That conversation is with another agent.")),
        (Some(c), _) => c.agent.clone(),
        (None, Some(a)) if valid_agent(a) => a.to_owned(),
        (None, _) => return Err(bad("`agent` is required for a new conversation")),
    };
    // At most its owner's, now: an owner who lost the agent has taken it from the bot too.
    if !roles::may_use(&bot.person_id, &agent).await {
        return Err(BotError::err(403, Code::NotGranted, format!("This bot may not use {agent}.")));
    }
    // A retry of an ask already sent answers the same and sends nothing; the same nonce for anything else is refused
    // (C4). The nonce is the bot's own, across its agents and conversations.
    let chat = known.as_ref().map(|c| c.chat_bot_id.clone()).or_else(|| chat_row_of(hub, &bot.person_id, &agent));
    if let Some(e) = hub.store.find_bot_guest_nonce(&bot.bot_id, nonce) {
        if !is_retry(hub, &e, text, &agent, known.as_ref(), external.as_deref()) {
            return Err(BotError::err(
                409,
                Code::NonceReused,
                "This clientNonce was already used for a different message.",
            ));
        }
        return replay(hub, bot, &e);
    }
    let now = now_ms();
    if hub.store.audit_count_since(&bot.person_id, "botAsk", now - HOUR_MS) >= bot.rate_per_hour {
        return Err(BotError::err(
            429,
            Code::RateLimited,
            format!("This bot has asked {} times in the last hour. Try again in a few minutes.", bot.rate_per_hour),
        ));
    }
    let (state, queued) = known.as_ref().map_or((State::Idle, 0), |c| conv_state(hub, c));
    let busy = !matches!(state, State::Idle | State::Stopped);
    if busy {
        let pending = queued + usize::from(state != State::Queued);
        if no_queue {
            return Err(BotError::err(409, Code::Busy, "A turn of this conversation is still running."));
        }
        if pending >= MAX_PENDING {
            return Err(BotError::err(429, Code::Busy, "This conversation already has five asks waiting."));
        }
    } else {
        let mut running = 0;
        for c in hub.store.bot_conversations_of(&bot.bot_id, RECENT_CONVERSATIONS)? {
            running += i64::from(!matches!(conv_state(hub, &c).0, State::Idle | State::Stopped | State::Queued));
        }
        if running >= bot.concurrent {
            return Err(BotError::err(
                429,
                Code::Busy,
                format!("This bot already has {} turns running.", bot.concurrent),
            ));
        }
    }
    // One writer per thread: a terminal or a take-over holding the conversation's thread refuses, it doesn't queue (the
    // queue above is for the bot's own turns). A new conversation has no thread yet. `liveWriter` is display only.
    if let Some(c) = &known {
        crate::terminal::bot_gate(hub, &c.chat_bot_id, &c.thread_root).await.map_err(|_| {
            BotError::err(409, Code::HeldByOtherWriter, "Something else is writing to this conversation right now.")
        })?;
    }

    let chat = match chat {
        Some(c) => c,
        None => {
            chat_with(hub, &bot.person_id, &agent).await?["id"].as_str().context("a chat without an id")?.to_owned()
        }
    };
    let owner = owner_label(hub, bot).await;
    let (conv, first) = match known {
        Some(c) => (c, false),
        None => (open_conversation(hub, bot, &chat, &agent, external, &owner)?, true),
    };
    let lane = Lane::in_thread(&chat, &conv.thread_root);
    let turn = hub.store.max_turn(&chat) + 1;
    let sender = json!({
        "botId": bot.bot_id, "name": bot.name, "provider": bot.provider,
        "owner": bot.owner_person, "speaker": speaker,
    });
    // `pasted`: memory never takes a bot's text as its owner's own words.
    let data = json!({"text": text, "clientNonce": nonce, "status": "queued", "attachments": [], "pasted": [text], "sender": sender});
    let entry =
        hub.add_entry(&lane, EntryKind::User, turn, &data).ok_or_else(|| anyhow!("couldn't save the message"))?;
    hub.store.touch_bot_conversation(&conv.conversation_id, speaker.as_deref())?;
    hub.store.audit_add(&bot.person_id, caller.device_key(), "botAsk", Some(&conv.conversation_id), true);
    let prompt = crate::chat::prompt::of_bot_guest(first, provider_label(&bot.provider), &bot.name, &owner, text);
    hub.send_cmd(&chat, Cmd::Send { lane, entry_id: entry.id.clone(), text: prompt, route: false })?;
    // The claim itself is taken when the turn starts (`claim_for_turn`, `Kind::Bot`) and released when it ends.
    set_writer(hub, &chat, &conv.thread_root, Some("bot"));
    Ok(json!({
        "conversation": conv.conversation_id, "entryId": entry.id, "turn": turn, "seq": entry.rev,
        "state": if busy { State::Queued } else { State::Running }, "queued": queued + usize::from(busy && state != State::Queued),
    }))
}

/// `botAsk` with a `room` (C5): the bot's question becomes one turn of the agent it @-mentions in a room it is a member
/// of. No conversation opens, and the agent's reply wakes nobody. When the room has an open conversation the turn is
/// taken from its budget (C2); with none it costs the bot its ask rate only.
async fn ask_room(
    hub: &Arc<Hub>,
    caller: &Caller,
    bot: &BotGuest,
    room: &str,
    text: &str,
    nonce: &str,
    speaker: Option<String>,
) -> Result<Value> {
    let missing = || BotError::err(404, Code::NotFound, "No such room.");
    if !crate::chat::group::is_room_member(&hub.store, room, &bot.bot_id) {
        return Err(missing());
    }
    let group = hub.store.bot(room)?.filter(|r| !r.deleted).map(|r| r.config).ok_or_else(missing)?;
    if let Some(e) = hub.store.find_bot_guest_nonce(&bot.bot_id, nonce) {
        if e.bot_id != room || e.data["text"].as_str() != Some(text) {
            return Err(BotError::err(
                409,
                Code::NonceReused,
                "This clientNonce was already used for a different message.",
            ));
        }
        return Ok(json!({
            "conversation": "", "entryId": e.id, "turn": e.turn, "seq": e.rev, "state": State::Idle, "queued": 0,
            "duplicate": true, "room": room,
        }));
    }
    let now = now_ms();
    if hub.store.audit_count_since(&bot.person_id, "botAsk", now - HOUR_MS) >= bot.rate_per_hour {
        return Err(BotError::err(
            429,
            Code::RateLimited,
            format!("This bot has asked {} times in the last hour. Try again in a few minutes.", bot.rate_per_hour),
        ));
    }
    let agents = crate::chat::group::mentioned_agents(hub, &group, text);
    let Some(agent) = agents.first() else {
        let names: Vec<String> =
            group.members.iter().filter_map(|m| hub.store.bot(m).ok().flatten()).map(|r| r.config.name).collect();
        return Err(BotError::err(
            400,
            Code::BadRequest,
            format!("Name the agent to ask with @ ({}).", names.join(", ")),
        ));
    };
    // Like any ask: at most what its owner holds of the agent.
    if let Some(hq) = &agent.hq_agent
        && !roles::may_use(&bot.person_id, hq).await
    {
        return Err(BotError::err(403, Code::NotGranted, format!("This bot may not use {}.", agent.name)));
    }
    let lane = Lane::main(room);
    if hub.store.open_conversation_of(&lane).is_some() && hub.store.charge_lane(&lane)?.is_none() {
        return Err(BotError::err(409, Code::Busy, "This room's turn budget is spent."));
    }
    let sender = json!({
        "botId": bot.bot_id, "name": bot.name, "provider": bot.provider,
        "owner": bot.owner_person, "speaker": speaker,
    });
    let data = json!({"text": text, "clientNonce": nonce, "attachments": [], "pasted": [text], "sender": sender});
    let turn = hub.store.max_turn(room);
    let entry =
        hub.add_entry(&lane, EntryKind::User, turn, &data).ok_or_else(|| anyhow!("couldn't save the message"))?;
    hub.store.audit_add(&bot.person_id, caller.device_key(), "botAsk", Some(room), true);
    crate::chat::group::ask_agent(hub, room, &agent.id)?;
    Ok(json!({
        "conversation": "", "entryId": entry.id, "turn": turn, "seq": entry.rev, "state": State::Running, "queued": 0,
        "room": room,
    }))
}

/// `botPost {room, conversation, epoch, turnKey?, text, clientNonce}`: the bot says something in a room it is a member
/// of. It is stored once per nonce (the same text again replays, other text is `nonceReused`, C4). With the turnKey of
/// the turn the room waits on it for, it is the member's reply and completes that turn. With none it is an unprompted
/// post: charged one turn, and it wakes only the members it @-mentions, only while its conversation is open with budget
/// left. Anything else (a conversation that ended or moved on, a turnKey nobody waits on any more) is kept and marked
/// late, wakes nobody, and costs nothing (C3). A post never opens or tops up a budget (C1).
async fn post(hub: &Arc<Hub>, caller: &Caller, bot: &BotGuest, b: &Value) -> Result<Value> {
    let bad = |m: &str| BotError::err(400, Code::BadRequest, m);
    let text = b["text"].as_str().map(str::trim).filter(|t| !t.is_empty()).ok_or_else(|| bad("`text` is required"))?;
    if text.chars().count() > MAX_TEXT {
        return Err(BotError::err(400, Code::TooLong, format!("A post is at most {MAX_TEXT} characters.")));
    }
    let nonce = b["clientNonce"].as_str().filter(|n| (1..=128).contains(&n.chars().count()));
    let nonce = nonce.ok_or_else(|| bad("`clientNonce` (1 to 128 characters) is required"))?;
    let room = b["room"].as_str().filter(|r| !r.is_empty()).ok_or_else(|| bad("`room` is required"))?;
    let conversation = b["conversation"].as_str().filter(|c| !c.is_empty());
    let conversation = conversation.ok_or_else(|| bad("`conversation` is required"))?;
    let epoch = b["epoch"].as_u64().ok_or_else(|| bad("`epoch` is required"))?;
    let turn_key = b["turnKey"].as_str().filter(|k| !k.is_empty());
    if !crate::remote::sync_lease::wait(hub, SYNC_WAIT).await {
        return Err(BotError::err(
            503,
            Code::Syncing,
            "This computer is catching up with the Kith cloud. Try again in a moment.",
        ));
    }
    let _one = ASK_LOCK.lock().await;
    // Not a member, or no such conversation in that room: a 404, never a hint that the room exists.
    let missing = || BotError::err(404, Code::NotFound, "No such room.");
    if !crate::chat::group::is_room_member(&hub.store, room, &bot.bot_id) {
        return Err(missing());
    }
    let conv = hub.store.conversation(conversation).filter(|c| c.lane.chat == room).ok_or_else(missing)?;
    if let Some(e) = hub.store.find_bot_guest_nonce(&bot.bot_id, nonce) {
        if e.bot_id != room || e.data["text"].as_str() != Some(text) {
            return Err(BotError::err(
                409,
                Code::NonceReused,
                "This clientNonce was already used for a different message.",
            ));
        }
        return Ok(posted(&e, true));
    }
    if hub.store.audit_count_since(&bot.person_id, "botPost", now_ms() - HOUR_MS) >= bot.rate_per_hour {
        return Err(BotError::err(
            429,
            Code::RateLimited,
            format!("This bot has posted {} times in the last hour. Try again in a few minutes.", bot.rate_per_hour),
        ));
    }
    let lane = conv.lane.clone();
    let live = epoch == conv.epoch && crate::chat::group::current(&hub.store, &lane, conv.epoch);
    let live = live && hub.store.conversation(&conv.id).is_some_and(|c| c.state == crate::store::ConvState::Open);
    let replies = live
        && turn_key.is_some_and(|k| crate::chat::group::bot_turn_waits(k, &bot.bot_id, &conv.id, conv.epoch));
    let mut charged = false;
    let mut late = !live;
    if live && !replies {
        // Without a key it is the bot speaking up: one turn off the budget, if there is one. With a key nobody waits on
        // any more (answered already, or timed out) it is late.
        if turn_key.is_some() {
            late = true;
        } else if hub.store.charge_lane(&lane)?.is_some() {
            charged = true;
        } else {
            late = true;
        }
    }
    let sender = json!({"botId": bot.bot_id, "name": bot.name, "provider": bot.provider, "owner": bot.owner_person});
    // `pasted`: memory never takes a bot's text as its owner's own words.
    let mut data = json!({
        "text": text, "clientNonce": nonce, "attachments": [], "pasted": [text], "sender": sender,
        "conversation": conv.id, "epoch": epoch, "replied": replies,
    });
    if let Some(k) = turn_key {
        data["turnKey"] = k.into();
    }
    if late {
        data["late"] = true.into();
    }
    let entry = hub
        .add_entry(&lane, EntryKind::User, hub.store.max_turn(room), &data)
        .ok_or_else(|| anyhow!("couldn't save the post"))?;
    hub.store.audit_add(&bot.person_id, caller.device_key(), "botPost", Some(&conv.id), true);
    if replies {
        // The room turn that waits reads the text from here; it @-mentions on from there.
        crate::chat::group::complete_bot_turn(turn_key.unwrap_or_default(), &bot.bot_id, &conv.id, conv.epoch, text);
    } else if charged
        && let Some(group) = hub.store.bot(room)?.map(|r| r.config)
    {
        let woken = crate::chat::group::mentioned_members(hub, &group, text);
        crate::chat::group::queue_mentions(&conv.id, woken.into_iter().filter(|m| *m != bot.bot_id).collect());
    }
    Ok(posted(&entry, false))
}

/// What `botPost` answers for a stored post.
fn posted(e: &Entry, duplicate: bool) -> Value {
    let mut v = json!({
        "entryId": e.id, "seq": e.rev, "turn": e.turn, "room": e.bot_id,
        "conversation": e.data["conversation"], "replied": e.data["replied"] == true,
        "late": e.data["late"] == true,
    });
    if duplicate {
        v["duplicate"] = true.into();
    }
    v
}

/// `botRoomRead {room, since?, limit?}`: what a member bot may read of a room, after the cursor `since` (the `rev` of
/// the last entry it saw; its first read gets the newest few). Each entry is shaped as for a bot (no thoughts, tool
/// calls, plans or attachments), with the room, the lane's conversation and epoch, and who spoke.
async fn room_read(hub: &Hub, bot: &BotGuest, b: &Value) -> Result<Value> {
    let room = b["room"].as_str().filter(|r| !r.is_empty());
    let missing = || BotError::err(404, Code::NotFound, "No such room.");
    let room = room.filter(|r| crate::chat::group::is_room_member(&hub.store, r, &bot.bot_id)).ok_or_else(missing)?;
    // Like reading a thread: only in sync with the cloud, so a bot revoked there reads nothing more.
    if !crate::remote::sync_lease::wait(hub, SYNC_WAIT).await {
        return Err(BotError::err(
            503,
            Code::Syncing,
            "This computer is catching up with the Kith cloud. Try again in a moment.",
        ));
    }
    let since = b["since"].as_i64().unwrap_or(0).max(0);
    let limit = b["limit"].as_i64().unwrap_or(50).clamp(1, 200);
    let rows = hub.store.chat_entries_since(room, since, limit)?;
    let waits_on = match &bot.owner_person {
        Some(p) => hub.store.person(p).map_or_else(|| "the owner".to_owned(), |p| p.name),
        None => "the owner".to_owned(),
    };
    let entries: Vec<Value> = rows.iter().filter_map(|e| roles::room_entry(&hub.store, &json!(e), &waits_on)).collect();
    let cursor = rows.iter().map(|e| e.rev).max().unwrap_or(0).max(since);
    let newest = hub.store.newest_conversation(&Lane::main(room));
    Ok(json!({
        "room": room, "conversation": newest.as_ref().map(|c| c.id.clone()), "epoch": newest.as_ref().map(|c| c.epoch),
        "entries": entries, "cursor": cursor,
    }))
}

/// A new conversation: its root line in the chat's main lane (the thread) and the mapping row. The root carries the
/// origin tags the apps show; the thread-origin package moves them onto the thread itself (docs section 6.2).
fn open_conversation(
    hub: &Hub,
    bot: &BotGuest,
    chat: &str,
    agent: &str,
    external_ref: Option<String>,
    owner: &str,
) -> Result<BotConversation> {
    let id = format!("cv_{}", crypto::b64(&crypto::random::<12>()));
    let label = format!("from {} · {owner}'s {}", provider_label(&bot.provider), bot.name);
    // The thread's origin keys live in the root's `thread` summary, where the terminal threads keep theirs and where a
    // recount of the summary carries them over (`terminal::carry_over`).
    let root = json!({
        "text": label, "style": "info", "conversation": id,
        "thread": {
            "origin": "bot", "originLabel": label, "liveWriter": null, "liveBusy": false,
            "originBot": {"provider": bot.provider, "owner": bot.owner_person, "name": bot.name},
        },
    });
    let root = hub
        .add_entry(&Lane::main(chat), EntryKind::Notice, hub.store.max_turn(chat), &root)
        .ok_or_else(|| anyhow!("couldn't start the conversation"))?;
    let now = now_ms();
    let conv = BotConversation {
        conversation_id: id,
        bot_id: bot.bot_id.clone(),
        agent: agent.to_owned(),
        chat_bot_id: chat.to_owned(),
        thread_root: root.id,
        external_ref,
        room_id: None,
        speaker: None,
        origin_label: label,
        created_at: now,
        last_at: now,
    };
    hub.store.put_bot_conversation(&conv)?;
    Ok(conv)
}

/// The root's `thread` summary of a conversation.
fn thread_summary(hub: &Hub, chat: &str, root: &str) -> Option<(Entry, Value)> {
    let e = hub.store.entry(root).filter(|e| e.bot_id == chat)?;
    let summary = e.data["thread"].clone();
    Some((e, if summary.is_object() { summary } else { json!({}) }))
}

/// Marks who writes to a conversation's thread (`bot`, `app` or nobody) in its root summary, for the apps' origin tag.
fn set_writer(hub: &Hub, chat: &str, root: &str, writer: Option<&str>) {
    let Some((mut e, mut summary)) = thread_summary(hub, chat, root) else { return };
    summary["liveWriter"] = writer.map_or(Value::Null, Value::from);
    summary["liveBusy"] = writer.is_some().into();
    if e.data["thread"] != summary {
        e.data["thread"] = summary;
        hub.set_entry(root, &e.data);
    }
}

/// A host turn on `lane` ended: a bot conversation's thread is free again unless more asks wait in it.
pub(crate) fn turn_ended(hub: &Hub, lane: &Lane) {
    let Some(root) = &lane.thread else { return };
    let Some(conv) = hub.store.bot_conversation_of_thread(&lane.chat, root) else { return };
    let waiting = hub
        .store
        .thread(&lane.chat, root, 500)
        .unwrap_or_default()
        .iter()
        .any(|e| e.kind == EntryKind::User.as_str() && e.data["status"] == "queued");
    set_writer(hub, &conv.chat_bot_id, root, waiting.then_some("bot"));
}

fn own_conversation(hub: &Hub, bot: &BotGuest, b: &Value) -> Result<BotConversation> {
    let id = b["conversation"].as_str().filter(|c| !c.is_empty());
    id.and_then(|id| hub.store.bot_conversation_by_id(id))
        .filter(|c| c.bot_id == bot.bot_id)
        .ok_or_else(|| BotError::err(404, Code::NotFound, "No such conversation."))
}

/// `botThread {conversation, afterSeq?, limit?}`: what a bot may read of its own conversation (docs 5.8).
async fn thread(hub: &Hub, bot: &BotGuest, b: &Value) -> Result<Value> {
    let conv = own_conversation(hub, bot, b)?;
    // Reading the thread is how a bot gets its answers: like an ask, only in sync with the cloud and while its owner
    // may still use the agent, so a turn that outlived a revoke delivers nothing (R5).
    if !crate::remote::sync_lease::wait(hub, SYNC_WAIT).await {
        return Err(BotError::err(
            503,
            Code::Syncing,
            "This computer is catching up with the Kith cloud. Try again in a moment.",
        ));
    }
    if !roles::may_use(&bot.person_id, &conv.agent).await {
        return Err(BotError::err(403, Code::NotGranted, format!("This bot may not use {}.", conv.agent)));
    }
    let after = b["afterSeq"].as_i64().unwrap_or(0);
    let limit = b["limit"].as_i64().unwrap_or(200).clamp(1, 500);
    let all = hub.store.thread(&conv.chat_bot_id, &conv.thread_root, 500)?;
    let waits_on = match &bot.owner_person {
        Some(p) => hub.store.person(p).map_or_else(|| "the owner".to_owned(), |p| p.name),
        None => "the owner".to_owned(),
    };
    let shape = |e: &Entry| roles::bot_entry(&json!(e), &conv.conversation_id, &waits_on);
    let entries: Vec<Value> =
        all.iter().filter(|e| e.rev > after).filter_map(shape).take(usize::try_from(limit).unwrap_or(200)).collect();
    // Done: the last ask has its final answer, or a terminal notice, or was taken back.
    let last = all.iter().rev().find(|e| e.kind == EntryKind::User.as_str());
    let done = last.is_none_or(|u| {
        u.data["status"] == "cancelled"
            || all.iter().any(|e| {
                e.turn == u.turn
                    && ((e.kind == EntryKind::Agent.as_str() && e.data["final"] == true)
                        || (e.kind == EntryKind::Notice.as_str() && e.data["style"] == "error"))
            })
    });
    let (state, _) = conv_state(hub, &conv);
    Ok(json!({
        "conversation": conv.conversation_id, "entries": entries, "done": done,
        "turn": last.map(|u| u.turn), "state": state,
    }))
}

/// `botConversations {limit?}`: its own conversations, newest first.
fn conversations(hub: &Hub, bot: &BotGuest, b: &Value) -> Result<Value> {
    let limit = b["limit"].as_i64().unwrap_or(50).clamp(1, 200);
    let list: Vec<Value> = hub
        .store
        .bot_conversations_of(&bot.bot_id, limit)?
        .iter()
        .map(|c| {
            json!({"conversation": c.conversation_id, "agent": c.agent, "ref": c.external_ref,
                   "originLabel": c.origin_label, "lastAt": c.last_at, "state": conv_state(hub, c).0})
        })
        .collect();
    Ok(json!({"conversations": list}))
}

/// `botStop {conversation}`: stops the running turn of its own conversation (and what waits behind it).
fn stop(hub: &Hub, bot: &BotGuest, b: &Value) -> Result<Value> {
    let conv = own_conversation(hub, bot, b)?;
    hub.stop_place(&conv.chat_bot_id, &Lane::in_thread(&conv.chat_bot_id, &conv.thread_root))?;
    Ok(json!({}))
}

/// `botLeave {}`: the bot retires itself, like its owner removing it.
async fn leave(hub: &Hub, bot: &BotGuest) -> Result<Value> {
    end_bot(hub, &bot.bot_id, BotState::Revoked, "bot", &bot.person_id).await;
    Ok(json!({}))
}
