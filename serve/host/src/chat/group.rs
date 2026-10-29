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
use crate::store::{BotConfig, BotKind, ConvState, Conversation, Entry, EntryKind, Lane, Store, is_person_message, now_ms};
use anyhow::{Result, bail};
use std::sync::Arc;
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
    let Ok(Some(row)) = hub.store.bot(group) else { return };
    for member in &row.config.members {
        let _ = hub.send_cmd(member, Cmd::CancelGroup { chat: group.to_owned() });
    }
}

/// Existing, distinct agent bots (not groups, not the group itself).
pub fn valid_members(store: &Store, group_id: &str, ids: &[String]) -> Result<Vec<String>> {
    let mut out: Vec<String> = vec![];
    for id in ids.iter().map(|i| i.trim()) {
        if id.is_empty() || id == group_id || out.iter().any(|o| o == id) {
            continue;
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
        // The speaker a restart interrupted goes first, already charged. Otherwise the next in the queue, charged now:
        // a turn that is sent costs one even if the member passes, times out or fails.
        let member_id = match conv.queue.current.clone() {
            Some(id) => id,
            None => {
                if conv.queue.waiting.is_empty() {
                    if conv.queue.round > 0 && conv.queue.spoke == 0 {
                        hub.store.end_conversation(&conv.id, ConvState::Ended)?;
                        break;
                    }
                    let since_user = since_last_user_message(hub, &lane)?;
                    let mut speakers: Vec<String> =
                        responders(&members, &since_user).into_iter().map(|m| m.id.clone()).collect();
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
                if conv.left <= 0 {
                    hub.store.end_conversation(&conv.id, ConvState::Ended)?;
                    break;
                }
                let id = conv.queue.waiting.remove(0);
                conv.left -= 1;
                conv.queue.current = Some(id.clone());
                id
            }
        };
        // Written before the turn is sent: a restart from here on resumes this speaker without charging again.
        if !hub.store.save_conversation(&conv.id, conv.left, &conv.queue)? {
            return Ok(());
        }
        let Some(member) = members.iter().find(|m| m.id == member_id) else {
            // Left the group since it was queued.
            conv.queue.current = None;
            continue;
        };
        let prompt = member_prompt(hub, &group, &members, member, &lane)?;
        let (reply, answer) = oneshot::channel();
        let turn = GroupTurn { lane: lane.clone(), prompt, epoch: conv.epoch, reply };
        let text = if hub.send_cmd(&member.id, Cmd::Group(turn)).is_err() {
            None
        } else {
            // A failed, cancelled or overdue member turn counts as a pass; its error shows in the room.
            match tokio::time::timeout(crate::chat::team::ASK_TIMEOUT, answer).await {
                Ok(Ok(Ok(Some(text)))) => Some(text),
                _ => None,
            }
        };
        conv.queue.current = None;
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
        if !hub.store.save_conversation(&conv.id, conv.left, &conv.queue)? {
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

fn members(hub: &Hub, group: &BotConfig) -> Vec<BotConfig> {
    group
        .members
        .iter()
        .filter_map(|id| hub.store.bot(id).ok().flatten())
        .filter(|r| !r.deleted)
        .map(|r| r.config)
        .collect()
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

    #[test]
    fn pass_is_recognized_loosely() {
        for t in ["(pass)", " (pass) ", "\"(pass)\"", "pass.", ""] {
            assert!(is_pass(t), "{t:?}");
        }
        assert!(!is_pass("I'll pass this to Bob"));
    }
}
