//! Group chats (Grok Bot's design): several bots and the user in one conversation.
//!
//! A group is a roster row (`BotKind::Group`) with a member list and its own
//! transcript, but no agent. When a person writes, the group opens a stored
//! *conversation* (store/conversations.rs) with a turn budget and runs a *room turn*: the
//! members answer one at a time, each in its **own** session (a member's history is unified
//! across its chats), told what was said since it last spoke. Everyone answers unless the
//! new messages @-mention someone; a member with nothing to add says `(pass)`. Members the
//! person @-mentions are queued first, agents a reply @-mentions are appended (FIFO). The
//! room turn runs until the budget is spent (every member turn is charged when it is sent,
//! a pass included: C2), a round where everyone passes, a person's Stop, or 24 hours idle.
//! The queue and what is left are stored with each step, so a host restart resumes the turn
//! (the interrupted speaker first, not charged again).
//!
//! A newer message in the same lane (or Stop) moves the lane's epoch (kept in the store, so
//! it only grows), which ends a running room turn before its next speaker. A member whose
//! turn was already running finishes; its reply is stored marked `late` and wakes nobody (C3).
//!
//! Threads work the same way inside a group: the room turn runs in the thread's lane and
//! only sees the thread.

use crate::agent::bot::Cmd;
use crate::hub::Hub;
use crate::remote::push::{self, AlertKind};
use crate::store::{
    BotConfig, BotGuest, BotKind, BotState, Charge, ConvState, Conversation, Entry, EntryKind, Lane, Store,
    is_person_message, now_ms,
};
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;

/// Agent turns one person message may cause, rounds included, unless the person sets another (`send` `budget`).
pub const DEFAULT_BUDGET: i64 = 10;
/// The most a person can grant in one message.
const MAX_BUDGET: i64 = 100;
/// A conversation nobody has moved for this long ends (checked at each step and when the host resumes it).
const IDLE_MS: i64 = 24 * 60 * 60 * 1000;
/// Room lines a member is shown per turn.
const HISTORY_LINES: i64 = 24;
/// A bot's line in another member's prompt is clipped (the user's never is).
const LINE_CHARS: usize = 2000;
const PASS: &str = "(pass)";

/// One member's turn in a room, queued on its actor like any other turn.
pub struct GroupTurn {
    pub lane: Lane,
    pub prompt: String,
    /// The room turn that asked; a member skips it once a newer one started (see [`current`]).
    pub epoch: u64,
    /// The reply the room sees, or `None` for a pass.
    pub reply: oneshot::Sender<Result<Option<String>>>,
}

/// The budget a `send` asks for: its `budget`, else [`DEFAULT_BUDGET`], at least 1 and at most [`MAX_BUDGET`].
pub fn budget_of(requested: Option<i64>) -> i64 {
    requested.unwrap_or(DEFAULT_BUDGET).clamp(1, MAX_BUDGET)
}

/// Whether `epoch` is still the lane's newest (a member's reply to an older one is late and wakes nobody).
pub fn current(store: &Store, lane: &Lane, epoch: u64) -> bool {
    store.lane_epoch(lane) == epoch
}

/// Stop: ends every conversation in `group` (main chat and threads), moving their epochs, and cancels the member
/// turns in it.
pub fn stop(hub: &Hub, group: &str) {
    if let Err(error) = hub.store.stop_conversations(group) {
        tracing::warn!(group, error = format!("{error:#}"), "couldn't stop the room's conversations");
    }
    // An outside bot has no actor to cancel: its waiting turn ends as a pass.
    cancel_bot_turns(|t| t.lane.chat == group);
    let Ok(Some(row)) = hub.store.bot(group) else { return };
    for member in &row.config.members {
        let _ = hub.send_cmd(member, Cmd::CancelGroup { chat: group.to_owned() });
    }
}

/// An outside bot's turn the room is waiting on, found by the key the bot posts under.
pub struct BotTurn {
    pub bot_id: String,
    pub lane: Lane,
    conversation: String,
    epoch: u64,
    reply: oneshot::Sender<String>,
}

static BOT_TURNS: Mutex<BTreeMap<String, BotTurn>> = Mutex::new(BTreeMap::new());
/// Members an unprompted post of an outside bot @-mentioned, per conversation, for its room turn to queue.
static MENTIONS: Mutex<BTreeMap<String, Vec<String>>> = Mutex::new(BTreeMap::new());

fn turns() -> std::sync::MutexGuard<'static, BTreeMap<String, BotTurn>> {
    BOT_TURNS.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Ends the waiting turns of outside bots that `which` picks (their wait returns as a pass).
pub fn cancel_bot_turns(which: impl Fn(&BotTurn) -> bool) {
    turns().retain(|_, t| !which(t));
}

/// The bot's reply to the turn waiting under `key`, handed to the room turn; false when no such turn waits for this
/// bot, in this conversation and epoch (the reply is late, or not this bot's).
pub fn complete_bot_turn(key: &str, bot_id: &str, conversation: &str, epoch: u64, text: &str) -> bool {
    let mut all = turns();
    if !all.get(key).is_some_and(|t| t.bot_id == bot_id && t.conversation == conversation && t.epoch == epoch) {
        return false;
    }
    all.remove(key).is_some_and(|t| t.reply.send(text.to_owned()).is_ok())
}

/// Whether the room is waiting on this bot under `key` (checked before a reply is stored, so it is stored plain).
pub fn bot_turn_waits(key: &str, bot_id: &str, conversation: &str, epoch: u64) -> bool {
    turns().get(key).is_some_and(|t| t.bot_id == bot_id && t.conversation == conversation && t.epoch == epoch)
}

/// An unprompted post of an outside bot woke these members: the room turn queues them after everyone waiting.
pub fn queue_mentions(conversation: &str, members: Vec<String>) {
    if !members.is_empty() {
        MENTIONS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).entry(conversation.to_owned()).or_default().extend(members);
    }
}

/// The agent members of `group` that `text` @-mentions, in the order they are members.
pub fn mentioned_agents(hub: &Hub, group: &BotConfig, text: &str) -> Vec<BotConfig> {
    members(hub, group)
        .into_iter()
        .filter(|m| guest_member(hub, &m.id).is_none())
        .filter(|m| handles(&m.name).iter().any(|h| mentions(text, h)))
        .collect()
}

/// The ids of every member of `group` that `text` @-mentions (agents and outside bots).
pub fn mentioned_members(hub: &Hub, group: &BotConfig, text: &str) -> Vec<String> {
    members(hub, group)
        .into_iter()
        .filter(|m| handles(&m.name).iter().any(|h| mentions(text, h)))
        .map(|m| m.id)
        .collect()
}

/// An owner's own bot guest that is let in: the only outside agent a room takes (C7: it counts as its owner).
fn is_owners_active_bot(g: &BotGuest) -> bool {
    g.state == BotState::Active && g.owner_person.is_none()
}

/// The outside bot behind member `id`, while it is still the owner's own and let in.
pub fn guest_member(hub: &Hub, id: &str) -> Option<BotGuest> {
    hub.store.bot_guest(id).filter(is_owners_active_bot)
}

/// Whether bot guest `bot_id` is a member of room `room` right now (checked on every read, post and event).
pub fn is_room_member(store: &Store, room: &str, bot_id: &str) -> bool {
    let in_room = store.bot(room).ok().flatten().is_some_and(|r| {
        !r.deleted && r.config.is_group() && r.config.members.iter().any(|m| m == bot_id)
    });
    in_room && store.bot_guest(bot_id).is_some_and(|g| is_owners_active_bot(&g))
}

/// Existing, distinct agent bots (not groups, not the group itself), and the owner's own bot guests that are let in
/// (C7). Another person's bot, or one that is revoked, denied or still waiting, is refused: a room never silently
/// takes an outside agent it can't talk to.
pub fn valid_members(store: &Store, group_id: &str, ids: &[String]) -> Result<Vec<String>> {
    let mut out: Vec<String> = vec![];
    let before = store.bot(group_id)?.map(|r| r.config.members).unwrap_or_default();
    for id in ids.iter().map(|i| i.trim()) {
        if id.is_empty() || id == group_id || out.iter().any(|o| o == id) {
            continue;
        }
        if let Some(guest) = store.bot_guest(id) {
            if is_owners_active_bot(&guest) {
                out.push(id.to_owned());
            } else if before.iter().any(|m| m == id) {
                // A member that has since been revoked drops out of the room on the next edit.
            } else if guest.owner_person.is_some() {
                bail!("{} belongs to someone else. A room takes only your own bots.", guest.name);
            } else {
                bail!("{} isn't let in (it is {}), so it can't join a room.", guest.name, guest.state.as_str());
            }
            continue;
        }
        if id.starts_with("bg_") && before.iter().all(|m| m != id) {
            bail!("{id} isn't one of your bots.");
        }
        match store.bot(id)? {
            Some(row) if !row.deleted && row.config.kind == BotKind::Agent => out.push(id.to_owned()),
            Some(row) if row.config.is_group() => bail!("a group can't be a member of another group"),
            _ => {}
        }
    }
    if out.is_empty() {
        bail!("a group needs at least one bot");
    }
    Ok(out)
}

/// A group whose members are exactly `members`.
pub fn with_members(store: &Store, members: &[String]) -> Result<Option<String>> {
    let mut wanted = members.to_vec();
    wanted.sort();
    Ok(store.bots()?.into_iter().find_map(|row| {
        let mut have = row.config.members.clone();
        have.sort();
        (!row.deleted && row.config.is_group() && have == wanted).then_some(row.config.id)
    }))
}

/// A person wrote in a group lane: open a conversation there (ending the one still open) and run its room turn.
pub fn start(hub: &Arc<Hub>, lane: Lane, budget: i64, opened_by: &str) -> Result<()> {
    let conv = hub.store.open_conversation(&lane, budget, opened_by)?;
    // The older conversation's outside bot (if the room was waiting on one) is not waited for any more.
    cancel_bot_turns(|t| t.lane == lane);
    spawn(hub, conv);
    Ok(())
}

/// After a host restart: every conversation still open goes on where it stopped.
pub fn resume(hub: &Arc<Hub>) -> Result<()> {
    for conv in hub.store.open_conversations()? {
        let gone = hub.store.bot(&conv.lane.chat)?.is_none_or(|r| r.deleted);
        if gone || now_ms() - conv.last_at > IDLE_MS {
            hub.store.end_conversation(&conv.id, if gone { ConvState::Stopped } else { ConvState::Ended })?;
            continue;
        }
        spawn(hub, conv);
    }
    Ok(())
}

fn spawn(hub: &Arc<Hub>, conv: Conversation) {
    let hub = hub.clone();
    tokio::spawn(async move {
        let group = conv.lane.chat.clone();
        if let Err(error) = run(&hub, conv).await {
            tracing::warn!(group, error = format!("{error:#}"), "room turn failed");
        }
    });
}

async fn run(hub: &Arc<Hub>, mut conv: Conversation) -> Result<()> {
    let result = run_steps(hub, &mut conv).await;
    MENTIONS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&conv.id);
    result
}

async fn run_steps(hub: &Arc<Hub>, conv: &mut Conversation) -> Result<()> {
    let lane = conv.lane.clone();
    let group_id = lane.chat.clone();
    let mut last_reply: Option<(String, String, String)> = None;
    loop {
        // A newer message or Stop moved the lane's epoch (and closed this conversation).
        if !current(&hub.store, &lane, conv.epoch) {
            return Ok(());
        }
        if now_ms() - conv.last_at > IDLE_MS {
            hub.store.end_conversation(&conv.id, ConvState::Ended)?;
            break;
        }
        let Some(group) = hub.store.bot(&group_id)?.filter(|r| !r.deleted).map(|r| r.config) else {
            hub.store.end_conversation(&conv.id, ConvState::Stopped)?;
            bail!("group deleted");
        };
        let members = members(hub, &group);
        // Members an outside bot's unprompted post woke join the end of the queue.
        let woken = MENTIONS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&conv.id);
        for id in woken.into_iter().flatten() {
            if conv.queue.current.as_deref() != Some(&id) && !conv.queue.waiting.contains(&id) {
                conv.queue.waiting.push(id);
            }
        }
        // The speaker a restart interrupted goes first, already charged. Otherwise the next in the queue, charged now:
        // a turn that is sent costs one even if the member passes, times out or fails.
        let member_id = match conv.queue.current.clone() {
            Some(id) => {
                // Written before the turn is sent: a restart from here on resumes this speaker without charging again.
                if !hub.store.save_queue(&conv.id, &conv.queue)? {
                    return Ok(());
                }
                id
            }
            None => {
                if conv.queue.waiting.is_empty() {
                    if conv.queue.round > 0 && conv.queue.spoke == 0 {
                        hub.store.end_conversation(&conv.id, ConvState::Ended)?;
                        break;
                    }
                    let since_user = since_last_user_message(hub, &lane)?;
                    let mut speakers: Vec<String> =
                        responders(&members, &since_user).into_iter().map(|m| m.id.clone()).collect();
                    // An outside bot whose own post is the room's latest line has nothing new to read: waking it
                    // again would only spend its owner's Grok runs.
                    speakers.retain(|id| !spoke_last(hub, &lane, id));
                    // Each round starts one member later, so the same bot doesn't always speak first.
                    if !speakers.is_empty() {
                        let offset = conv.queue.round as usize % speakers.len();
                        speakers.rotate_left(offset);
                    }
                    conv.queue.waiting = speakers;
                    conv.queue.round += 1;
                    conv.queue.spoke = 0;
                    if conv.queue.waiting.is_empty() {
                        hub.store.end_conversation(&conv.id, ConvState::Ended)?;
                        break;
                    }
                }
                let mut next = conv.queue.clone();
                let id = next.waiting.remove(0);
                next.current = Some(id.clone());
                // An outside bot posts under a key of this turn; its wake goes out once.
                next.turn_key = guest_member(hub, &id).map(|_| uuid::Uuid::new_v4().to_string());
                next.woke = false;
                // The turn and the queue are written together, before the turn is sent.
                match hub.store.charge_turn(&conv.id, &next)? {
                    Charge::Charged(left) => {
                        conv.left = left;
                        conv.queue = next;
                    }
                    Charge::Spent => {
                        hub.store.end_conversation(&conv.id, ConvState::Ended)?;
                        break;
                    }
                    Charge::Closed => return Ok(()),
                }
                id
            }
        };
        let Some(member) = members.iter().find(|m| m.id == member_id) else {
            // Left the group since it was queued.
            conv.queue.current = None;
            continue;
        };
        let text = if let Some(guest) = guest_member(hub, &member.id) {
            bot_turn(hub, conv, &group, &guest).await?
        } else {
            let prompt = member_prompt(hub, &group, &members, member, &lane)?;
            let (reply, answer) = oneshot::channel();
            let turn = GroupTurn { lane: lane.clone(), prompt, epoch: conv.epoch, reply };
            if hub.send_cmd(&member.id, Cmd::Group(turn)).is_err() {
                None
            } else {
                // A failed, cancelled or overdue member turn counts as a pass; its error shows in the room.
                match tokio::time::timeout(crate::chat::team::ASK_TIMEOUT, answer).await {
                    Ok(Ok(Ok(Some(text)))) => Some(text),
                    _ => None,
                }
            }
        };
        conv.queue.current = None;
        conv.queue.turn_key = None;
        conv.queue.woke = false;
        // Moved while the member worked: its reply is stored late and wakes nobody.
        if !current(&hub.store, &lane, conv.epoch) {
            return Ok(());
        }
        if let Some(text) = text {
            conv.queue.spoke += 1;
            // Agents the reply @-mentions are woken after everyone already queued.
            let woken: Vec<String> = members
                .iter()
                .filter(|m| m.id != member.id && !conv.queue.waiting.contains(&m.id))
                .filter(|m| handles(&m.name).iter().any(|h| mentions(&text, h)))
                .map(|m| m.id.clone())
                .collect();
            conv.queue.waiting.extend(woken);
            last_reply = Some((member.id.clone(), member.name.clone(), text));
        }
        if !hub.store.save_queue(&conv.id, &conv.queue)? {
            return Ok(());
        }
        conv.last_at = now_ms();
    }
    if let Some((from, name, text)) = last_reply
        && let Some(group) = hub.store.bot(&group_id)?.map(|r| r.config)
    {
        push::notify(hub, &group, Some(&from), &format!("{name} in {}", group.name), &text, AlertKind::Done);
    }
    Ok(())
}

/// An outside bot's question to one agent of the room (`botAsk` with `room`, C5): one turn of that agent in the room's
/// main lane, with no conversation opened. Its reply is stored in the room like any member's and wakes nobody (it is
/// not part of a room turn). A person's newer message in the room takes priority over it.
pub fn ask_agent(hub: &Arc<Hub>, room: &str, agent_id: &str) -> Result<()> {
    let Some(group) = hub.store.bot(room)?.filter(|r| !r.deleted).map(|r| r.config) else {
        bail!("no such room");
    };
    let members = members(hub, &group);
    let Some(agent) = members.iter().find(|m| m.id == agent_id) else { bail!("{agent_id} isn't in this room") };
    let lane = Lane::main(room);
    let prompt = member_prompt(hub, &group, &members, agent, &lane)?;
    let (reply, answer) = oneshot::channel();
    let epoch = hub.store.lane_epoch(&lane);
    hub.send_cmd(agent_id, Cmd::Group(GroupTurn { lane, prompt, epoch, reply }))?;
    // The turn is skipped once nobody holds its reply, so something has to.
    tokio::spawn(async move {
        let _ = tokio::time::timeout(crate::chat::team::ASK_TIMEOUT, answer).await;
    });
    Ok(())
}

/// Whether `id` is an outside bot and the room's latest line is its own post.
fn spoke_last(hub: &Hub, lane: &Lane, id: &str) -> bool {
    guest_member(hub, id).is_some()
        && hub
            .store
            .messages_after(lane, 0, 1)
            .ok()
            .and_then(|last| last.last().map(|e| e.data["sender"]["botId"] == id))
            .unwrap_or(false)
}

/// How long the host lets one wake command run (it retries a 429 itself, for up to two minutes of waiting).
const WAKE_LIMIT: Duration = Duration::from_secs(180);

/// An outside bot's turn: tells it (a `turn` event, and a wake through its webhook when it has a wake profile), then
/// waits for its post under this turn's key for the bot's own timeout. No post by then, no wake profile reachable or a
/// wake the webhook refused is a pass; the turn was charged when it was sent.
async fn bot_turn(hub: &Arc<Hub>, conv: &mut Conversation, group: &BotConfig, guest: &BotGuest) -> Result<Option<String>> {
    let Some(key) = conv.queue.turn_key.clone() else {
        return Ok(None);
    };
    let (reply, answer) = oneshot::channel();
    turns().insert(
        key.clone(),
        BotTurn { bot_id: guest.bot_id.clone(), lane: conv.lane.clone(), conversation: conv.id.clone(), epoch: conv.epoch, reply },
    );
    // However this ends, nothing waits under the key afterwards.
    struct Forget(String);
    impl Drop for Forget {
        fn drop(&mut self) {
            turns().remove(&self.0);
        }
    }
    let _forget = Forget(key.clone());
    let _ = hub.events.send(json!({
        "type": "turn", "botId": guest.bot_id, "room": conv.lane.chat, "thread": conv.lane.thread,
        "conversation": conv.id, "epoch": conv.epoch, "turnKey": key,
    }));
    if !conv.queue.woke {
        // Written before the wake goes out: a restart waits again but never wakes the bot twice.
        conv.queue.woke = true;
        if !hub.store.save_queue(&conv.id, &conv.queue)? {
            return Ok(None);
        }
        if let Some(profile) = guest.wake_profile.as_deref() {
            let since = hub.store.last_rev_of_bot_guest(&conv.lane.chat, &guest.bot_id);
            let body = wake_body(group, conv, &key, since);
            if !wake(profile, &body).await {
                return Ok(None);
            }
        }
    }
    let wait = Duration::from_secs(u64::try_from(guest.turn_timeout_s).unwrap_or(300));
    Ok(match tokio::time::timeout(wait, answer).await {
        Ok(Ok(text)) if !is_pass(&text) => Some(text),
        _ => None,
    })
}

/// What the wake carries: where to post and a cursor to read from, in words a bot with a shell can act on. Never a key,
/// and no text of the room itself: the bot reads that with `kith room read`.
fn wake_body(group: &BotConfig, conv: &Conversation, key: &str, since: i64) -> Value {
    let (room, id, epoch) = (&conv.lane.chat, &conv.id, conv.epoch);
    json!({
        "room": room, "conversation": id, "epoch": epoch, "turnKey": key, "since": since,
        "text": format!(
            "It is your turn in the Kith room \"{}\". Read what you missed: kith room read {room} --since {since}\n\
             Then reply once: kith room post {room} --conversation {id} --epoch {epoch} --turn-key {key} \"your reply\"\n\
             If you have nothing to add, post (pass). Run `kith room read` and `kith room post` only; this wake is for this turn alone.",
            group.name
        ),
    })
}

/// The wake command (`kith-send-to`, beside kith-auth), tests point at a stub.
#[cfg(test)]
pub(crate) static TEST_SEND_TO: Mutex<Option<std::path::PathBuf>> = Mutex::new(None);

async fn send_to_bin() -> Option<std::path::PathBuf> {
    #[cfg(test)]
    if let Some(b) = TEST_SEND_TO.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone() {
        return Some(b);
    }
    let auth = crate::remote::account::bin().await?;
    Some(auth.with_file_name("kith-send-to")).filter(|p| p.is_file())
}

/// Pushes `body` through `kith send-to grok --profile <profile>`: the command holds the webhook URL and key (kith-auth
/// hands them to curl, never to an argument or a log), and the body goes in on stdin. True when it says the run started.
async fn wake(profile: &str, body: &Value) -> bool {
    use tokio::io::AsyncWriteExt;
    let Some(bin) = send_to_bin().await else {
        tracing::warn!("no kith-send-to beside kith-auth; the outside bot isn't woken");
        return false;
    };
    let spawned = tokio::process::Command::new(&bin)
        .args(["grok", "--body", "-", "--profile", profile, "--json"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn();
    let Ok(mut child) = spawned else { return false };
    if let Some(mut stdin) = child.stdin.take() {
        if stdin.write_all(body.to_string().as_bytes()).await.is_err() {
            return false;
        }
        // Closing it is what lets the command read the body to its end.
        drop(stdin);
    }
    let Ok(Ok(out)) = tokio::time::timeout(WAKE_LIMIT, child.wait_with_output()).await else { return false };
    let said = String::from_utf8_lossy(&out.stdout);
    let ok = out.status.success()
        && said.lines().rev().find_map(|l| serde_json::from_str::<Value>(l).ok()).is_some_and(|v| v["ok"] == true);
    if !ok {
        tracing::info!(status = ?out.status.code(), "the wake wasn't taken; the outside bot's turn passes");
    }
    ok
}

fn members(hub: &Hub, group: &BotConfig) -> Vec<BotConfig> {
    group
        .members
        .iter()
        .filter_map(|id| {
            // An outside bot is a member while it is the owner's and let in; it has a name and nothing else of a bot.
            if let Some(guest) = hub.store.bot_guest(id) {
                return is_owners_active_bot(&guest).then(|| outside_member(&guest));
            }
            hub.store.bot(id).ok().flatten().filter(|r| !r.deleted).map(|r| r.config)
        })
        .collect()
}

/// A bot guest as a room member: its name and a line saying it is outside, shown to the other members.
fn outside_member(guest: &BotGuest) -> BotConfig {
    let mut cfg: BotConfig = serde_json::from_value(json!({
        "id": guest.bot_id, "name": guest.name, "backend": "claude", "cwd": "/tmp",
        "description": "an outside agent (reads and posts through its own link)",
    }))
    .expect("a bare bot config");
    cfg.hidden = true;
    cfg
}

/// The lane's messages since the user last wrote there, that message included.
fn since_last_user_message(hub: &Hub, lane: &Lane) -> Result<Vec<Entry>> {
    let recent = hub.store.messages_after(lane, 0, HISTORY_LINES)?;
    let start = recent.iter().rposition(is_person_message).unwrap_or(0);
    Ok(recent[start..].to_vec())
}

/// Who answers: the members @-mentioned since the user's last message, or everyone
/// when nobody (or `@all` / `@everyone`) is.
fn responders<'a>(members: &'a [BotConfig], messages: &[Entry]) -> Vec<&'a BotConfig> {
    let texts: Vec<&str> = messages.iter().filter_map(|e| e.data["text"].as_str()).collect();
    let everyone = texts.iter().any(|t| mentions(t, "all") || mentions(t, "everyone"));
    let named: Vec<&BotConfig> =
        members.iter().filter(|m| texts.iter().any(|t| handles(&m.name).iter().any(|h| mentions(t, h)))).collect();
    if everyone || named.is_empty() { members.iter().collect() } else { named }
}

/// Ways to @-mention a bot: its full name, without spaces, or its first word (lowercased).
fn handles(name: &str) -> Vec<String> {
    let lower = name.trim().to_lowercase();
    let mut out = vec![lower.clone(), lower.split_whitespace().collect::<String>()];
    if let Some(first) = lower.split_whitespace().next() {
        out.push(first.to_owned());
    }
    out.retain(|h| !h.is_empty());
    out.dedup();
    out
}

/// `@handle` in `text`, not followed by more of a Latin word (`@al` isn't `@alice`).
/// CJK names are often followed straight by the message, so only ASCII letters end a match early.
fn mentions(text: &str, handle: &str) -> bool {
    let lower = text.to_lowercase();
    let needle = format!("@{handle}");
    lower
        .match_indices(&needle)
        .any(|(i, _)| lower[i + needle.len()..].chars().next().is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_')))
}

fn member_prompt(hub: &Hub, group: &BotConfig, members: &[BotConfig], me: &BotConfig, lane: &Lane) -> Result<String> {
    let peers: Vec<&BotConfig> = members.iter().filter(|m| m.id != me.id).collect();
    let peer_names = if peers.is_empty() {
        "just the user".to_owned()
    } else {
        peers.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")
    };
    let about = group.description.trim();
    let room = if about.is_empty() { format!("\"{}\"", group.name) } else { format!("\"{}\" — {about}", group.name) };
    let mut out =
        vec![format!("You are {}, one participant in a group chat ({room}) with the user and {peer_names}.", me.name)];
    if !peers.is_empty() {
        out.push("Other participants in the room:".into());
        for p in &peers {
            let about =
                if p.description.trim().is_empty() { String::new() } else { format!(" ({})", p.description.trim()) };
            out.push(format!("- {}{about}", p.name));
        }
    }
    out.push(format!(
        "You have your full toolkit here. Do the work first, then answer: your final reply of this turn is \
         the one message the room sees. Keep it short and conversational. @-mention another participant only \
         when you need them to act. If you have nothing new worth adding, reply exactly \"{PASS}\". Never \
         reveal private one-on-one context."
    ));
    out.push(String::new());

    let seen = hub.store.last_spoke(lane, &me.id);
    let mut lines: Vec<String> = vec![];
    let mut header = format!("[Group chat: \"{}\" - with {peer_names}]", group.name);
    if let Some(root) = lane.thread.as_deref().and_then(|r| hub.store.entry(r)) {
        header = format!("[Group chat: \"{}\", in a thread - with {peer_names}]", group.name);
        if seen == 0 {
            lines.push(format!("(thread started on) {}", line(hub, group, &root, me)));
        }
    }
    for e in hub.store.messages_after(lane, seen, HISTORY_LINES)? {
        lines.push(line(hub, group, &e, me));
    }
    out.push(header);
    if lines.is_empty() {
        out.push("No new messages in the room since your last turn.".into());
    } else {
        out.push("New messages in the room (oldest first):".into());
        out.extend(lines);
    }
    out.push(String::new());
    out.push(format!(
        "It's your turn, {}. Reply to the room if you have something worth adding, or reply exactly \"{PASS}\" if you don't.",
        me.name
    ));
    Ok(out.join("\n"))
}

fn line(hub: &Hub, group: &BotConfig, e: &Entry, me: &BotConfig) -> String {
    let text = e.data["text"].as_str().unwrap_or_default();
    if let Some(bot) = e.data["sender"]["botId"].as_str().filter(|_| e.kind == EntryKind::User.as_str()) {
        // A bot guest's ask is its own voice in the room, not the user's.
        let name = e.data["sender"]["name"].as_str().filter(|n| !n.is_empty()).unwrap_or(bot);
        return format!("{name} (bot guest): {text}");
    }
    if e.kind == EntryKind::User.as_str() {
        let root = crate::chat::uploads::root(group);
        let files: Vec<_> = e.data["attachments"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|a| crate::chat::uploads::path_of(&root, a))
            .collect();
        let files = if files.is_empty() { String::new() } else { crate::chat::uploads::prompt_suffix(&files) };
        return format!("User: {text}{files}");
    }
    let author = e.data["author"].as_str().unwrap_or_default();
    let text = crate::agent::acp::truncate(text, LINE_CHARS);
    if author == me.id { format!("{} (you): {text}", me.name) } else { format!("{}: {text}", hub.bot_name(author)) }
}

/// A member's final text is a pass (the room doesn't see it).
pub fn is_pass(text: &str) -> bool {
    let t = text.trim().trim_matches(|c: char| c == '"' || c == '.' || c == '*').to_lowercase();
    t.is_empty() || t == PASS || t == "pass"
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bot(id: &str, name: &str) -> BotConfig {
        serde_json::from_value(json!({"id": id, "name": name, "backend": "claude", "cwd": "/tmp"})).expect("valid bot")
    }

    fn said(text: &str) -> Entry {
        Entry {
            id: "e".into(),
            seq: 1,
            bot_id: "g".into(),
            thread_id: None,
            rev: 1,
            kind: "user".into(),
            turn: 1,
            data: json!({"text": text}),
            created_at: 0,
            updated_at: 0,
        }
    }

    #[tokio::test]
    async fn a_bot_guests_entry_does_not_reset_since_the_last_user_message() {
        let room = Room::new();
        let main = Lane::main(&room.group);
        let hub = &room.hub;
        let user = hub.add_entry(&main, EntryKind::User, 1, &json!({"text": "hello"})).unwrap();
        hub.add_entry(&main, EntryKind::Agent, 2, &json!({"text": "hi", "author": "a"})).unwrap();
        let guest = json!({"text": "psst", "sender": {"botId": "bg1", "name": "Grok"}});
        let ask = hub.add_entry(&main, EntryKind::User, 3, &guest).unwrap();
        let since = since_last_user_message(hub, &main).unwrap();
        assert_eq!(since.first().map(|e| e.id.clone()), Some(user.id), "the guest's ask is not the user's message");
        assert_eq!(
            line(hub, &hub.store.bot(&room.group).unwrap().unwrap().config, &ask, &bot("a", "Alice")),
            "Grok (bot guest): psst"
        );
        room.shutdown().await;
    }

    #[test]
    fn mentions_pick_responders_and_no_mention_means_everyone() {
        let members = [bot("a", "Alice Chen"), bot("b", "Bob"), bot("c", "尼采課")];
        let ids = |m: &[BotConfig], text: &str| -> Vec<String> {
            responders(m, &[said(text)]).into_iter().map(|b| b.id.clone()).collect()
        };
        assert_eq!(ids(&members, "hi all"), ["a", "b", "c"]);
        assert_eq!(ids(&members, "@bob can you look"), ["b"]);
        assert_eq!(ids(&members, "@alicechen and @Alice"), ["a"]);
        assert_eq!(ids(&members, "@尼采課幫我整理"), ["c"], "CJK names can run into the message");
        assert_eq!(ids(&members, "@bobby?"), ["a", "b", "c"], "a longer word isn't a mention");
        assert_eq!(ids(&members, "@all go"), ["a", "b", "c"]);
    }

    use crate::api::devices::Caller;
    use crate::hub::BotStatus;
    use std::path::PathBuf;
    use std::time::Duration;

    /// A host with agent bots `a` and `b` (the scripted fixture agent) and a group of both.
    struct Room {
        hub: Arc<Hub>,
        dir: PathBuf,
        group: String,
    }

    impl Room {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("kith-group-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let store = Store::open(&dir.join("test.db")).unwrap();
            let agent = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/team_agent.py");
            for id in ["a", "b"] {
                let cwd = dir.join(id);
                std::fs::create_dir_all(&cwd).unwrap();
                let cfg: BotConfig = serde_json::from_value(json!({
                    "id": id, "name": id, "backend": "fixture", "cwd": cwd, "tasks": false,
                    "command": crate::shell::python(&agent), "notify": false,
                }))
                .unwrap();
                store.save_bot(&cfg).unwrap();
            }
            let hub = Hub::new(
                store,
                "test-host".into(),
                crate::remote::identity::Identity::load_or_create(&dir).unwrap(),
                "test-token".into(),
                19222,
            );
            hub.start().unwrap();
            let group = hub
                .create_bot(
                    serde_json::from_value(
                        json!({"id": "", "kind": "group", "name": "Crew", "description": "Ship the release", "members": ["a", "b", "a"]}),
                    )
                    .unwrap(),
                )
                .unwrap();
            let group = group["id"].as_str().unwrap().to_owned();
            Self { hub, dir, group }
        }

        async fn send(&self, text: &str, thread: Option<&str>) -> Entry {
            let sent = crate::api::dispatch(
                &self.hub,
                &Caller::Local,
                "send",
                json!({"botId": self.group, "text": text, "threadId": thread}),
            )
            .await
            .unwrap();
            serde_json::from_value::<serde_json::Value>(sent["entry"].clone())
                .ok()
                .and_then(|e| self.hub.store.entry(e["id"].as_str()?))
                .unwrap()
        }

        fn replies(&self, lane: &Lane) -> Vec<(String, String)> {
            self.hub
                .store
                .messages_after(lane, 0, 100)
                .unwrap()
                .into_iter()
                .filter(|e| e.kind == "agent")
                .map(|e| (e.data["author"].as_str().unwrap().to_owned(), e.data["text"].as_str().unwrap().to_owned()))
                .collect()
        }

        async fn settle(&self) {
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    let idle = ["a", "b"].iter().all(|b| self.hub.bot_runtime(b).status == BotStatus::Idle);
                    if idle && self.hub.bots_json(0).unwrap().iter().all(|b| b["status"] == "idle") {
                        break;
                    }
                }
            })
            .await
            .expect("room settles");
        }

        async fn shutdown(self) {
            self.hub.shutdown().await;
            let _ = std::fs::remove_dir_all(self.dir);
        }
    }

    #[tokio::test]
    async fn room_turns_follow_mentions_and_end_when_everyone_passes() {
        let room = Room::new();
        let row = room.hub.store.bot(&room.group).unwrap().unwrap();
        assert_eq!(row.config.members, ["a", "b"], "members are deduplicated");
        let again = room
            .hub
            .create_bot(
                serde_json::from_value(json!({"id": "", "kind": "group", "name": "Dup", "members": ["b", "a"]}))
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(again["id"], room.group, "the same bots reopen their group");

        let main = Lane::main(&room.group);
        room.send("hello everyone", None).await;
        room.settle().await;
        // `a` answers the user; `b` then sees a bot spoke last and passes; round two is all passes.
        assert_eq!(room.replies(&main), [("a".to_owned(), "reply: group".to_owned())]);

        room.send("@b your turn", None).await;
        room.settle().await;
        assert_eq!(room.replies(&main).last().unwrap().0, "b", "only the mentioned bot answers");
        assert_eq!(room.replies(&main).len(), 2);
        let listed = room.hub.bot_value_for_test(&room.group);
        assert_eq!(listed["lastMessage"], "b: reply: group");
        let prompts = std::fs::read_to_string(room.dir.join("a/prompts.jsonl")).unwrap();
        assert!(
            prompts.contains("Ship the release) with the user"),
            "members are told what the room is for: {prompts}"
        );
        // The members' own chats stay private: the room's entries live in the group.
        assert!(room.hub.store.messages_after(&Lane::main("a"), 0, 100).unwrap().is_empty());
        room.shutdown().await;
    }

    #[tokio::test]
    async fn threads_stay_out_of_the_main_chat_and_summarize_on_their_root() {
        let room = Room::new();
        let first = room.send("@a start here", None).await;
        room.settle().await;
        room.send("@a in the thread", Some(&first.id)).await;
        room.settle().await;
        let thread = Lane::in_thread(&room.group, &first.id);
        assert_eq!(room.replies(&thread).len(), 1);
        assert_eq!(room.replies(&Lane::main(&room.group)).len(), 1, "the thread's reply isn't in the main chat");
        let summary = room.hub.store.entry(&first.id).unwrap().data["thread"].clone();
        assert_eq!(summary["count"], 2);
        assert_eq!(summary["authors"], json!(["user", "a"]));

        // Threads are read on their own (Slack's): reading the main chat leaves the thread's reply unread.
        let unread = || room.hub.bot_value_for_test(&room.group)["unread"].as_i64().unwrap();
        assert_eq!(summary["unread"], 1);
        assert_eq!(unread(), 2, "the main chat's reply and the thread's");
        let mark = |b: serde_json::Value| crate::api::dispatch(&room.hub, &Caller::Local, "markRead", b);
        mark(json!({"botId": room.group})).await.unwrap();
        assert_eq!(unread(), 1);
        mark(json!({"botId": room.group, "threadId": first.id})).await.unwrap();
        assert_eq!(unread(), 0);
        assert_eq!(room.hub.store.entry(&first.id).unwrap().data["thread"]["unread"], 0);
        assert!(mark(json!({"botId": "a", "threadId": first.id})).await.is_err(), "a thread of another chat");

        // Reactions toggle, and reacting to a read message doesn't make it unread.
        let reply = room.hub.store.messages_after(&Lane::main(&room.group), 0, 100).unwrap();
        let reply = reply.iter().find(|e| e.kind == "agent" && e.data["final"] == true).unwrap();
        let react = |emoji: &str| {
            crate::api::dispatch(&room.hub, &Caller::Local, "react", json!({"entryId": reply.id, "emoji": emoji}))
        };
        react("👍").await.unwrap();
        let reacted = react("✅").await.unwrap();
        assert_eq!(reacted["entry"]["data"]["reactions"], json!(["👍", "✅"]));
        assert_eq!(unread(), 0);
        let undone = react("👍").await.unwrap();
        assert_eq!(undone["entry"]["data"]["reactions"], json!(["✅"]));
        assert!(react("hi").await.is_err(), "only emoji");

        // A thread in a bot's own chat gets a session of its own, told where it branched off.
        let dm = crate::api::dispatch(&room.hub, &Caller::Local, "send", json!({"botId": "b", "text": "main"}))
            .await
            .unwrap();
        room.settle().await;
        let dm_root = dm["entry"]["id"].as_str().unwrap().to_owned();
        crate::api::dispatch(
            &room.hub,
            &Caller::Local,
            "send",
            json!({"botId": "b", "text": "aside", "threadId": dm_root}),
        )
        .await
        .unwrap();
        room.settle().await;
        let prompts = std::fs::read_to_string(room.dir.join("b/prompts.jsonl")).unwrap();
        assert!(prompts.lines().last().unwrap().contains("[Thread]"), "{prompts}");
        let replies = room.hub.store.thread("b", &dm_root, 10).unwrap();
        assert_eq!(replies.iter().filter(|e| e.data["final"] == true).count(), 1);
        assert!(
            crate::api::dispatch(
                &room.hub,
                &Caller::Local,
                "send",
                json!({"botId": "b", "text": "x", "threadId": replies[0].id})
            )
            .await
            .is_err(),
            "threads are flat"
        );
        room.shutdown().await;
    }

    #[tokio::test]
    async fn stop_ends_the_room_turn_before_the_next_speaker() {
        let room = Room::new();
        room.send("BLOCK everyone look", None).await;
        let hub = room.hub.clone();
        tokio::time::timeout(Duration::from_secs(10), async {
            while hub.bot_runtime("a").status != BotStatus::Working {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("a starts");
        crate::api::dispatch(&room.hub, &Caller::Local, "stop", json!({"botId": room.group})).await.unwrap();
        room.settle().await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(!room.dir.join("b/prompts.jsonl").exists(), "b never got a turn after Stop");
        assert!(room.replies(&Lane::main(&room.group)).iter().all(|(_, t)| t != "reply: group"));
        let conv = &room.conversations()[0];
        assert_eq!((conv.state, conv.epoch), (ConvState::Stopped, 2), "stop ends the conversation and moves the epoch");
        assert_eq!(room.hub.store.lane_epoch(&Lane::main(&room.group)), 2);
        room.shutdown().await;
    }

    impl Room {
        /// Member turns the fixture agents ran (one prompt line each).
        fn member_turns(&self) -> usize {
            ["a", "b"]
                .iter()
                .map(|id| std::fs::read_to_string(self.dir.join(id).join("prompts.jsonl")).map_or(0, |p| p.lines().count()))
                .sum()
        }

        fn conversations(&self) -> Vec<Conversation> {
            self.hub.store.conversations_of(&self.group).unwrap()
        }

        /// Waits until the lane's newest conversation is no longer open.
        async fn closed(&self) -> Conversation {
            tokio::time::timeout(Duration::from_secs(20), async {
                loop {
                    if let Some(c) = self.conversations().last().filter(|c| c.state != ConvState::Open) {
                        return c.clone();
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            })
            .await
            .expect("the conversation closes")
        }
    }

    #[tokio::test]
    async fn a_budget_of_three_stops_two_chatty_agents_at_three_member_turns() {
        let room = Room::new();
        crate::api::dispatch(
            &room.hub,
            &Caller::Local,
            "send",
            json!({"botId": room.group, "text": "CHATTY @all go", "budget": 3}),
        )
        .await
        .unwrap();
        let conv = room.closed().await;
        room.settle().await;
        assert_eq!((conv.budget, conv.left, conv.state), (3, 0, ConvState::Ended));
        assert_eq!(room.member_turns(), 3, "rounds count against the budget too");
        // A member with nothing new to answer passes, and the pass was charged too.
        assert!(room.replies(&Lane::main(&room.group)).len() <= 3);
        room.shutdown().await;
    }

    #[tokio::test]
    async fn a_plain_message_gets_the_default_budget_and_a_new_epoch_each_time() {
        let room = Room::new();
        room.send("hello everyone", None).await;
        let first = room.closed().await;
        assert_eq!((first.budget, first.epoch, first.opened_by.as_str()), (DEFAULT_BUDGET, 1, "owner"));
        assert!(first.left < first.budget, "passes are charged too");
        room.settle().await;
        room.send("again", None).await;
        let convs = room.conversations();
        assert_eq!(convs.iter().map(|c| c.epoch).collect::<Vec<_>>(), [1, 2], "the epoch only grows");
        assert_eq!(budget_of(Some(0)), 1);
        assert_eq!(budget_of(Some(5000)), MAX_BUDGET);
        room.shutdown().await;
    }

    #[tokio::test]
    async fn a_restart_resumes_the_open_conversation_without_charging_its_speaker_again() {
        let room = Room::new();
        let main = Lane::main(&room.group);
        // What a host that died after sending `a`'s turn (3 granted, 1 charged) left behind.
        room.hub.add_entry(&main, EntryKind::User, 1, &json!({"text": "CHATTY @all go", "status": "sent"})).unwrap();
        let conv = room.hub.store.open_conversation(&main, 3, "owner").unwrap();
        let queue = crate::store::RoomQueue {
            waiting: vec!["b".into()],
            current: Some("a".into()),
            round: 1,
            spoke: 0,
            ..Default::default()
        };
        assert!(room.hub.store.save_conversation(&conv.id, 2, &queue).unwrap());
        assert_eq!(room.member_turns(), 0);

        resume(&room.hub).unwrap();
        let done = room.closed().await;
        room.settle().await;
        assert_eq!((done.id.as_str(), done.left, done.state), (conv.id.as_str(), 0, ConvState::Ended));
        assert_eq!(room.member_turns(), 3, "the granted 3 turns were each run once, the interrupted one included");
        let first = std::fs::read_to_string(room.dir.join("a/prompts.jsonl")).unwrap();
        assert_eq!(first.lines().count(), 1, "the interrupted speaker went first");
        room.shutdown().await;
    }

    #[tokio::test]
    async fn resume_closes_a_conversation_nobody_moved_for_a_day() {
        let room = Room::new();
        let main = Lane::main(&room.group);
        let conv = room.hub.store.open_conversation(&main, 3, "owner").unwrap();
        // An idle conversation from before the restart.
        room.hub.store.set_last_at_for_test(&conv.id, now_ms() - IDLE_MS - 1000);
        resume(&room.hub).unwrap();
        assert_eq!(room.conversations()[0].state, ConvState::Ended);
        assert_eq!(room.member_turns(), 0);
        room.shutdown().await;
    }

    #[tokio::test]
    async fn after_stop_a_slow_members_reply_is_stored_late_and_wakes_nobody() {
        let room = Room::new();
        let main = Lane::main(&room.group);
        room.send("@a BLOCK take your time", None).await;
        let hub = room.hub.clone();
        tokio::time::timeout(Duration::from_secs(10), async {
            while hub.bot_runtime("a").status != BotStatus::Working {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("a starts");
        // The turn is already running; a newer person message moves the lane on, then Stop ends that one too.
        room.send("@a hold on", None).await;
        let epoch_after_new_message = room.hub.store.lane_epoch(&main);
        assert_eq!(epoch_after_new_message, 2);
        std::fs::write(room.dir.join("a/release"), "").unwrap();
        room.settle().await;
        let replies: Vec<_> = room
            .hub
            .store
            .messages_after(&main, 0, 100)
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == "agent" && e.data["final"] == true)
            .collect();
        let late: Vec<_> = replies.iter().filter(|e| e.data["late"] == true).collect();
        assert_eq!(late.len(), 1, "the slow reply to the older message is marked late: {replies:?}");
        assert!(late[0].data["text"].as_str().unwrap().starts_with("reply:"));
        let convs = room.conversations();
        assert_eq!(convs[0].state, ConvState::Ended, "the older conversation was ended by the newer message");
        assert_eq!(room.hub.store.lane_epoch(&main), 2);
        assert!(!room.dir.join("b/prompts.jsonl").exists(), "the late reply woke nobody");

        crate::api::dispatch(&room.hub, &Caller::Local, "stop", json!({"botId": room.group})).await.unwrap();
        let after_stop = room.hub.store.lane_epoch(&main);
        assert!(after_stop >= 2, "stop never moves the epoch back");
        room.shutdown().await;
    }


    // MARK: an outside bot as a member (conversations plan PR 3)

    use crate::store::{Device, DeviceSource, Scope};

    /// One test at a time sets the wake command, since it is process-wide.
    static WAKE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    struct WakeStub {
        dir: PathBuf,
        _hold: tokio::sync::MutexGuard<'static, ()>,
    }

    impl WakeStub {
        /// A `kith-send-to` that records its arguments and body, and answers `answer` with exit code `code`.
        async fn new(dir: &std::path::Path, answer: &str, code: i32) -> Self {
            let hold = WAKE_LOCK.lock().await;
            let dir = dir.join("wake");
            std::fs::create_dir_all(&dir).unwrap();
            let script = dir.join("kith-send-to");
            std::fs::write(
                &script,
                format!(
                    "#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{d}/args'\ncat > '{d}/body.json'\necho '{answer}'\nexit {code}\n",
                    d = dir.display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
            *TEST_SEND_TO.lock().unwrap() = Some(script);
            Self { dir, _hold: hold }
        }

        fn args(&self) -> Vec<String> {
            std::fs::read_to_string(self.dir.join("args")).map_or(vec![], |a| a.lines().map(str::to_owned).collect())
        }

        fn body(&self) -> Value {
            serde_json::from_str(&std::fs::read_to_string(self.dir.join("body.json")).unwrap()).unwrap()
        }
    }

    impl Drop for WakeStub {
        fn drop(&mut self) {
            *TEST_SEND_TO.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        }
    }

    fn guest(id: &str, owner: Option<&str>, state: BotState, timeout: i64, wake: Option<&str>) -> BotGuest {
        BotGuest {
            bot_id: id.into(),
            person_id: format!("bot-{id}"),
            owner_person: owner.map(str::to_owned),
            name: "Grok".into(),
            provider: "grok".into(),
            agents: vec![],
            state,
            device_key: Some(format!("key-{id}")),
            cloud_device: None,
            rate_per_hour: 60,
            concurrent: 2,
            turn_timeout_s: timeout,
            wake_profile: wake.map(str::to_owned),
            created_at: 1,
            joined_at: Some(1),
            decided_at: Some(1),
            decided_by: Some("auto".into()),
            last_seen_at: None,
            revoked_at: None,
            revoked_why: None,
        }
    }

    impl Room {
        /// The owner's bot `bg1` ("Grok"), let in and holding a key, as a third member. Returns what it calls with.
        fn join_bot(&self, timeout: i64, wake: Option<&str>) -> Caller {
            let g = guest("bg1", None, BotState::Active, timeout, wake);
            self.hub.store.put_bot_guest(&g).unwrap();
            let key = g.device_key.clone().unwrap();
            self.hub
                .store
                .put_device(&Device {
                    key: key.clone(),
                    name: "Rex".into(),
                    platform: "bot".into(),
                    source: DeviceSource::Bot,
                    grant_id: Some("bot:bd1".into()),
                    scopes: vec![Scope::Control],
                    lease_until: None,
                    created_at: 1,
                    last_seen_at: None,
                    person_id: None,
                })
                .unwrap();
            self.hub.store.set_device_proven(&key).unwrap();
            self.hub.cloud_synced.store(true, std::sync::atomic::Ordering::Release);
            self.hub.update_bot(&json!({"id": self.group, "members": ["a", "b", "bg1"]})).unwrap();
            Caller::Device { key, scopes: vec![Scope::Control] }
        }

        async fn bot_post(
            &self,
            bot: &Caller,
            conv: &str,
            epoch: u64,
            key: Option<&str>,
            text: &str,
            nonce: &str,
        ) -> Result<Value> {
            crate::api::dispatch(
                &self.hub,
                bot,
                "botPost",
                json!({"room": self.group, "conversation": conv, "epoch": epoch, "turnKey": key, "text": text,
                       "clientNonce": nonce}),
            )
            .await
        }

        fn bot_entries(&self) -> Vec<Entry> {
            self.hub
                .store
                .messages_after(&Lane::main(&self.group), 0, 100)
                .unwrap()
                .into_iter()
                .filter(|e| e.data["sender"]["botId"] == "bg1")
                .collect()
        }
    }

    /// The next `turn` event on the hub.
    async fn next_turn(rx: &mut tokio::sync::broadcast::Receiver<Value>) -> Value {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if let Ok(v) = rx.recv().await
                    && v["type"] == "turn"
                {
                    return v;
                }
            }
        })
        .await
        .expect("a turn event")
    }

    fn code_of(e: &anyhow::Error) -> Option<&'static str> {
        crate::api::error_code(e)
    }

    #[tokio::test]
    async fn the_bots_post_under_its_turn_key_is_its_reply_and_a_repeat_shows_once() {
        let room = Room::new();
        let bot = room.join_bot(300, None);
        let mut events = room.hub.events.subscribe();
        room.send("@Grok what do you think?", None).await;
        let turn = next_turn(&mut events).await;
        assert_eq!((turn["botId"].as_str(), turn["room"].as_str()), (Some("bg1"), Some(room.group.as_str())));
        let (conv, epoch, key) =
            (turn["conversation"].as_str().unwrap(), turn["epoch"].as_u64().unwrap(), turn["turnKey"].as_str().unwrap());
        // Charged when sent: one of ten is gone before the bot says anything.
        assert_eq!(room.conversations()[0].left, DEFAULT_BUDGET - 1);

        let first = room.bot_post(&bot, conv, epoch, Some(key), "I think yes", "n1").await.unwrap();
        assert_eq!((first["replied"].as_bool(), first["late"].as_bool()), (Some(true), Some(false)));
        let again = room.bot_post(&bot, conv, epoch, Some(key), "I think yes", "n1").await.unwrap();
        assert_eq!((&again["entryId"], &again["duplicate"]), (&first["entryId"], &json!(true)));
        assert_eq!(room.bot_entries().len(), 1, "a post sent twice shows once");
        let other = room.bot_post(&bot, conv, epoch, Some(key), "something else", "n1").await.unwrap_err();
        assert_eq!(code_of(&other), Some("nonceReused"));
        assert_eq!(room.bot_entries().len(), 1);
        let stored = &room.bot_entries()[0];
        assert_eq!(stored.kind, "user");
        assert!(!is_person_message(stored), "a bot's post is never a person's message");
        assert_eq!(stored.data["pasted"][0], "I think yes");
        room.settle().await;
        // The reply completed the turn at once: nothing waited out the 300 s.
        assert_ne!(room.conversations()[0].state, ConvState::Open);
        room.shutdown().await;
    }

    #[tokio::test]
    async fn a_budget_of_four_gives_exactly_four_member_turns_across_agents_and_the_outside_bot() {
        let room = Room::new();
        let _bot = room.join_bot(300, None);
        let mut events = room.hub.events.subscribe();
        // The outside bot answers each of its turns as soon as the room tells it.
        let (hub, group) = (room.hub.clone(), room.group.clone());
        let caller = Caller::Device { key: "key-bg1".into(), scopes: vec![Scope::Control] };
        let answering = tokio::spawn(async move {
            let mut turns = 0;
            while let Ok(v) = events.recv().await {
                if v["type"] != "turn" {
                    continue;
                }
                turns += 1;
                let key = v["turnKey"].as_str().unwrap().to_owned();
                let body = json!({"room": group, "conversation": v["conversation"], "epoch": v["epoch"],
                    "turnKey": key, "text": "CHATTY from outside", "clientNonce": key});
                crate::api::dispatch(&hub, &caller, "botPost", body).await.unwrap();
                if turns == 2 {
                    break;
                }
            }
            turns
        });
        crate::api::dispatch(
            &room.hub,
            &Caller::Local,
            "send",
            json!({"botId": room.group, "text": "CHATTY @all go", "budget": 4}),
        )
        .await
        .unwrap();
        let conv = room.closed().await;
        room.settle().await;
        answering.abort();
        let bot_turns = room.bot_entries().len();
        assert_eq!((conv.budget, conv.left, conv.state), (4, 0, ConvState::Ended));
        assert_eq!(room.member_turns() + bot_turns, 4, "agents ran {} and the bot {bot_turns}", room.member_turns());
        assert!(bot_turns >= 1, "the outside bot had a turn");
        room.shutdown().await;
    }

    #[tokio::test]
    async fn without_a_wake_profile_the_bots_turn_passes_after_its_timeout() {
        let room = Room::new();
        let _bot = room.join_bot(1, None);
        let mut events = room.hub.events.subscribe();
        room.send("@Grok are you there?", None).await;
        next_turn(&mut events).await;
        let conv = room.closed().await;
        room.settle().await;
        assert_eq!(conv.state, ConvState::Ended);
        assert_eq!(conv.left, DEFAULT_BUDGET - 1, "the turn was charged, the pass wasn't free");
        assert!(room.bot_entries().is_empty());
        room.shutdown().await;
    }

    #[tokio::test]
    async fn the_wake_runs_send_to_with_the_turns_ids_and_a_refused_wake_is_a_pass_at_once() {
        let room = Room::new();
        let stub = WakeStub::new(&room.dir, r#"{"ok":true,"status":200,"message":"sent"}"#, 0).await;
        let bot = room.join_bot(300, Some("grok/kai"));
        let mut events = room.hub.events.subscribe();
        room.send("@Grok pineapple-secret", None).await;
        let turn = next_turn(&mut events).await;
        let key = turn["turnKey"].as_str().unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while stub.args().is_empty() {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("the wake ran");
        assert_eq!(stub.args(), ["grok", "--body", "-", "--profile", "grok/kai", "--json"]);
        let body = stub.body();
        assert_eq!(
            (body["room"].as_str(), body["conversation"].as_str(), body["epoch"].as_u64(), body["turnKey"].as_str()),
            (Some(room.group.as_str()), turn["conversation"].as_str(), turn["epoch"].as_u64(), Some(key))
        );
        assert!(body["since"].is_i64() && body["text"].as_str().unwrap().contains("kith room read"));
        let said = body.to_string();
        assert!(!said.contains("pineapple"), "the wake carries no text of the room: {said}");
        // The bot answers and the turn completes.
        let ok = room.bot_post(&bot, turn["conversation"].as_str().unwrap(), 1, Some(key), "here", "w1").await.unwrap();
        assert_eq!(ok["replied"], true);
        room.closed().await;
        room.settle().await;
        drop(stub);

        // A webhook that doesn't start a run: the turn is a pass at once, not after the 300 s.
        let refused = WakeStub::new(&room.dir, r#"{"ok":false,"status":400,"message":"paused"}"#, 1).await;
        room.send("@Grok again", None).await;
        let started = std::time::Instant::now();
        next_turn(&mut events).await;
        room.closed().await;
        assert!(started.elapsed() < Duration::from_secs(20), "a refused wake didn't wait for the timeout");
        assert!(!refused.args().is_empty());
        room.settle().await;
        room.shutdown().await;
    }

    #[tokio::test]
    async fn a_post_after_stop_is_late_and_starts_nothing() {
        let room = Room::new();
        let bot = room.join_bot(300, None);
        let mut events = room.hub.events.subscribe();
        room.send("@Grok go", None).await;
        let turn = next_turn(&mut events).await;
        let (conv, key) = (turn["conversation"].as_str().unwrap(), turn["turnKey"].as_str().unwrap());
        crate::api::dispatch(&room.hub, &Caller::Local, "stop", json!({"botId": room.group})).await.unwrap();
        room.settle().await;
        let late = room.bot_post(&bot, conv, 1, Some(key), "too late @a see this", "n1").await.unwrap();
        assert_eq!((late["late"].as_bool(), late["replied"].as_bool()), (Some(true), Some(false)));
        assert_eq!(room.bot_entries()[0].data["late"], true);
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(room.member_turns(), 0, "a late post woke nobody");
        assert_eq!(room.conversations()[0].state, ConvState::Stopped);
        room.shutdown().await;
    }

    #[tokio::test]
    async fn an_unprompted_post_is_charged_and_wakes_only_who_it_mentions_and_with_no_open_conversation_starts_nothing() {
        let room = Room::new();
        let bot = room.join_bot(300, None);
        let main = Lane::main(&room.group);
        // A conversation that is over: the post is kept, and nothing starts.
        let old = room.hub.store.open_conversation(&main, 5, "owner").unwrap();
        room.hub.store.end_conversation(&old.id, ConvState::Ended).unwrap();
        let post = room.bot_post(&bot, &old.id, old.epoch, None, "@a look at this", "u1").await.unwrap();
        assert_eq!(post["late"], true);
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(room.member_turns(), 0);
        assert_eq!(room.hub.store.conversation(&old.id).unwrap().left, 5, "a late post costs nothing");

        // An open conversation with budget: the post costs one turn and queues the member it names.
        let mut events = room.hub.events.subscribe();
        room.send("@Grok start", None).await;
        let turn = next_turn(&mut events).await;
        let (conv, epoch, key) =
            (turn["conversation"].as_str().unwrap(), turn["epoch"].as_u64().unwrap(), turn["turnKey"].as_str().unwrap());
        let left = room.hub.store.conversation(conv).unwrap().left;
        let speak = room.bot_post(&bot, conv, epoch, None, "@a CHATTY please look", "u2").await.unwrap();
        assert_eq!((speak["late"].as_bool(), speak["replied"].as_bool()), (Some(false), Some(false)));
        assert_eq!(room.hub.store.conversation(conv).unwrap().left, left - 1, "an unprompted post is charged");
        // The bot's own turn is still waiting; completing it lets the room turn queue `a`.
        room.bot_post(&bot, conv, epoch, Some(key), "(pass)", "u3").await.unwrap();
        tokio::time::timeout(Duration::from_secs(15), async {
            while !room.dir.join("a/prompts.jsonl").exists() {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("the member the post named got a turn");
        assert!(!room.dir.join("b/prompts.jsonl").exists(), "only the named member was woken");
        // A post never tops up: nothing here raised `budget`.
        room.settle().await;
        assert_eq!(room.hub.store.conversation(conv).unwrap().budget, DEFAULT_BUDGET);
        room.shutdown().await;
    }

    #[tokio::test]
    async fn two_posts_for_one_turn_key_the_first_completes_it_and_the_second_is_late() {
        let room = Room::new();
        let bot = room.join_bot(300, None);
        let mut events = room.hub.events.subscribe();
        room.send("@Grok go", None).await;
        let turn = next_turn(&mut events).await;
        let (conv, key) = (turn["conversation"].as_str().unwrap(), turn["turnKey"].as_str().unwrap());
        let one = room.bot_post(&bot, conv, 1, Some(key), "first", "p1").await.unwrap();
        let two = room.bot_post(&bot, conv, 1, Some(key), "second", "p2").await.unwrap();
        assert_eq!((one["replied"].as_bool(), two["replied"].as_bool(), two["late"].as_bool()), (Some(true), Some(false), Some(true)));
        room.settle().await;
        room.shutdown().await;
    }

    #[tokio::test]
    async fn only_the_owners_own_let_in_bot_can_join_a_room() {
        let room = Room::new();
        let group_members = |ids: &[&str]| json!({"id": room.group, "members": ids});
        room.hub.store.put_bot_guest(&guest("bg_sam", Some("sam"), BotState::Active, 300, None)).unwrap();
        room.hub.store.put_bot_guest(&guest("bg_wait", None, BotState::Pending, 300, None)).unwrap();
        room.hub.store.put_bot_guest(&guest("bg_gone", None, BotState::Revoked, 300, None)).unwrap();
        for (id, why) in [
            ("bg_sam", "belongs to someone else"),
            ("bg_wait", "isn't let in"),
            ("bg_gone", "isn't let in"),
            ("bg_nobody", "isn't one of your bots"),
        ] {
            let e = room.hub.update_bot(&group_members(&["a", id])).unwrap_err();
            assert!(format!("{e:#}").contains(why), "{id}: {e:#}");
        }
        assert_eq!(room.hub.store.bot(&room.group).unwrap().unwrap().config.members, ["a", "b"], "nothing changed");
        room.hub.store.put_bot_guest(&guest("bg_mine", None, BotState::Active, 300, None)).unwrap();
        room.hub.update_bot(&group_members(&["a", "bg_mine"])).unwrap();
        assert_eq!(room.hub.store.bot(&room.group).unwrap().unwrap().config.members, ["a", "bg_mine"]);
        // Revoked later, it drops out on the next edit instead of blocking it.
        room.hub.store.end_bot_guest("bg_mine", BotState::Revoked, "kai", 5).unwrap();
        room.hub.update_bot(&group_members(&["a", "b", "bg_mine"])).unwrap();
        assert_eq!(room.hub.store.bot(&room.group).unwrap().unwrap().config.members, ["a", "b"]);
        room.shutdown().await;
    }

    #[tokio::test]
    async fn a_bot_gets_room_events_only_while_it_is_a_let_in_member() {
        let room = Room::new();
        let _bot = room.join_bot(300, None);
        let main = Lane::main(&room.group);
        let said = room.hub.add_entry(&main, EntryKind::User, 1, &json!({"text": "hello room"})).unwrap();
        let agent = room
            .hub
            .add_entry(&main, EntryKind::Agent, 1, &json!({"text": "hi", "author": "a", "final": true}))
            .unwrap();
        let hidden = room.hub.add_entry(&main, EntryKind::Thought, 1, &json!({"text": "thinking", "author": "a"})).unwrap();
        let event = |e: &Entry| json!({"type": "entry", "rev": e.rev, "entry": e});
        let seen = |e: &Entry| crate::api::roles::event_for_bot(&room.hub.store, "bg1", event(e));
        let first = seen(&said).expect("a member sees the room");
        assert_eq!((first["room"].as_str(), first["entry"]["text"].as_str()), (Some(room.group.as_str()), Some("hello room")));
        assert_eq!(seen(&agent).unwrap()["entry"]["author"], "a");
        assert!(seen(&hidden).is_none(), "a thought is not shown, as in any bot thread");
        // A room it is not in stays dark, even to a member of another.
        let other = room.hub.create_bot(serde_json::from_value(json!({"id": "", "kind": "group", "name": "Other", "members": ["a"]})).unwrap()).unwrap();
        let elsewhere = room.hub.add_entry(&Lane::main(other["id"].as_str().unwrap()), EntryKind::User, 1, &json!({"text": "secret"})).unwrap();
        assert!(seen(&elsewhere).is_none(), "no text from a room the bot isn't in");

        // Out of the group: events stop at once.
        room.hub.update_bot(&json!({"id": room.group, "members": ["a", "b"]})).unwrap();
        assert!(seen(&said).is_none());
        room.hub.update_bot(&json!({"id": room.group, "members": ["a", "bg1"]})).unwrap();
        assert!(seen(&said).is_some());
        // Revoked: nothing more, and a turn event too.
        crate::api::botguests::end_bot(&room.hub, "bg1", BotState::Revoked, "kai", "kai").await;
        assert!(seen(&said).is_none() && seen(&agent).is_none(), "a revoked bot gets no more room events");
        let turn = json!({"type": "turn", "botId": "bg1", "room": room.group, "conversation": "c", "epoch": 1, "turnKey": "k"});
        assert!(crate::api::roles::event_for_bot(&room.hub.store, "bg1", turn).is_none());
        room.shutdown().await;
    }

    #[tokio::test]
    async fn a_bot_that_is_not_in_the_room_gets_a_404_and_reads_only_its_own_room() {
        let room = Room::new();
        let bot = room.join_bot(300, None);
        let main = Lane::main(&room.group);
        room.hub.add_entry(&main, EntryKind::User, 1, &json!({"text": "first"})).unwrap();
        room.hub.add_entry(&main, EntryKind::User, 1, &json!({"text": "second"})).unwrap();
        let read = crate::api::dispatch(&room.hub, &bot, "botRoomRead", json!({"room": room.group})).await.unwrap();
        assert_eq!(read["entries"].as_array().unwrap().len(), 2);
        let cursor = read["cursor"].as_i64().unwrap();
        room.hub.add_entry(&main, EntryKind::User, 1, &json!({"text": "third"})).unwrap();
        let next =
            crate::api::dispatch(&room.hub, &bot, "botRoomRead", json!({"room": room.group, "since": cursor})).await.unwrap();
        assert_eq!(next["entries"][0]["text"], "third");
        assert_eq!(next["entries"].as_array().unwrap().len(), 1);

        let other = room.hub.create_bot(serde_json::from_value(json!({"id": "", "kind": "group", "name": "Other", "members": ["a"]})).unwrap()).unwrap();
        let other = other["id"].as_str().unwrap();
        let denied = crate::api::dispatch(&room.hub, &bot, "botRoomRead", json!({"room": other})).await.unwrap_err();
        assert_eq!(code_of(&denied), Some("notFound"));
        let post = crate::api::dispatch(
            &room.hub,
            &bot,
            "botPost",
            json!({"room": other, "conversation": "c", "epoch": 1, "text": "hi", "clientNonce": "x"}),
        )
        .await
        .unwrap_err();
        assert_eq!(code_of(&post), Some("notFound"));
        room.shutdown().await;
    }

    #[tokio::test]
    async fn an_outside_bots_ask_is_one_turn_of_the_agent_it_names_and_opens_nothing() {
        let room = Room::new();
        let bot = room.join_bot(300, None);
        let ask = |text: &str, nonce: &str| {
            crate::api::dispatch(
                &room.hub,
                &bot,
                "botAsk",
                json!({"room": room.group, "text": text, "clientNonce": nonce}),
            )
        };
        let nobody = ask("what do you think?", "r0").await.unwrap_err();
        assert_eq!(code_of(&nobody), Some("badRequest"), "the agent is named with @");
        let asked = ask("@a CHATTY what do you think?", "r1").await.unwrap();
        assert_eq!(asked["room"], room.group.as_str());
        room.settle().await;
        assert_eq!(room.member_turns(), 1, "one turn of the agent it named");
        assert!(room.conversations().is_empty(), "no conversation opened");
        assert!(room.replies(&Lane::main(&room.group)).iter().any(|(a, t)| a == "a" && t == "CHATTY ok"));
        assert!(!room.dir.join("b/prompts.jsonl").exists(), "the reply woke nobody");
        let again = ask("@a CHATTY what do you think?", "r1").await.unwrap();
        assert_eq!(again["duplicate"], true);
        assert_eq!(room.member_turns(), 1);
        let reused = ask("@a something else", "r1").await.unwrap_err();
        assert_eq!(code_of(&reused), Some("nonceReused"));
        room.shutdown().await;
    }

    #[tokio::test]
    async fn an_ask_in_an_open_conversation_is_charged_to_its_budget() {
        let room = Room::new();
        let bot = room.join_bot(300, None);
        let main = Lane::main(&room.group);
        // A conversation another message opened; its room turn is blocked on `a` while the bot asks.
        room.send("@a BLOCK hold", None).await;
        let hub = room.hub.clone();
        tokio::time::timeout(Duration::from_secs(10), async {
            while hub.bot_runtime("a").status != BotStatus::Working {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("a starts");
        let left = room.hub.store.open_conversation_of(&main).unwrap().left;
        crate::api::dispatch(
            &room.hub,
            &bot,
            "botAsk",
            json!({"room": room.group, "text": "@b CHATTY hello", "clientNonce": "q1"}),
        )
        .await
        .unwrap();
        assert_eq!(room.hub.store.open_conversation_of(&main).unwrap().left, left - 1, "every door's turns are charged");
        std::fs::write(room.dir.join("a/release"), "").unwrap();
        std::fs::write(room.dir.join("b/release"), "").unwrap();
        room.settle().await;
        room.shutdown().await;
    }

    #[tokio::test]
    async fn a_restart_with_the_bot_as_the_speaker_waits_again_without_waking_or_charging_it_twice() {
        let room = Room::new();
        let stub = WakeStub::new(&room.dir, r#"{"ok":true,"status":200}"#, 0).await;
        let bot = room.join_bot(300, Some("grok/kai"));
        let main = Lane::main(&room.group);
        room.hub.add_entry(&main, EntryKind::User, 1, &json!({"text": "@Grok go", "status": "sent"})).unwrap();
        let conv = room.hub.store.open_conversation(&main, 3, "owner").unwrap();
        let queue = crate::store::RoomQueue {
            waiting: vec![],
            current: Some("bg1".into()),
            round: 1,
            spoke: 0,
            turn_key: Some("key-before".into()),
            woke: true,
        };
        assert!(room.hub.store.save_conversation(&conv.id, 2, &queue).unwrap());
        let mut events = room.hub.events.subscribe();
        resume(&room.hub).unwrap();
        let turn = next_turn(&mut events).await;
        assert_eq!(turn["turnKey"], "key-before", "the same turn, told again");
        let done = room.bot_post(&bot, &conv.id, conv.epoch, Some("key-before"), "back", "r1").await.unwrap();
        assert_eq!(done["replied"], true);
        room.closed().await;
        room.settle().await;
        assert!(stub.args().is_empty(), "the wake already went out before the restart");
        let after = room.hub.store.conversation(&conv.id).unwrap();
        assert_eq!(after.left, 2, "the interrupted turn was charged once, before the restart: {after:?}");
        room.shutdown().await;
    }

    #[test]
    fn pass_is_recognized_loosely() {
        for t in ["(pass)", " (pass) ", "\"(pass)\"", "pass.", ""] {
            assert!(is_pass(t), "{t:?}");
        }
        assert!(!is_pass("I'll pass this to Bob"));
    }
}
