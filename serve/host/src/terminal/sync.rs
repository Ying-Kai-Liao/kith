//! Keeps threads in step with the terminal registry: creating them, importing the transcript, saying who writes.

use super::claim;
use super::jsonl::{self, Item};
use super::registry::{self, Live, Registry};
use super::{Env, HeldByWriter, TerminalBusy};
use crate::LockExt;
use crate::agent::bot::Cmd;
use crate::agent::folder::Pick;
use crate::hub::Hub;
use crate::store::{BotConfig, EntryKind, Lane, ReadScope, lane_key, now_ms};
use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// How often the registry is looked at.
const EVERY: Duration = Duration::from_secs(3);
/// Entries kept of a transcript's start when it is first imported; older ones stay in the terminal's transcript.
const FIRST_IMPORT: usize = 400;
/// Entries one later import may bring.
const LATER_IMPORT: usize = 4000;
/// How long a take-over waits for the terminal's process to end.
const EXIT_WAIT: Duration = Duration::from_secs(5);

/// The summary keys the terminal owns, and the thread's prose summary (written by `chat::memory::summary`); entries
/// don't produce them.
const SUMMARY_KEYS: [&str; 8] =
    ["origin", "originLabel", "originBot", "liveWriter", "liveBusy", "terminal", "summary", "summaryAudience"];

/// Keeps the terminal's keys of `old` (a root's `data.thread`) in a fresh `summary` counted from entries.
pub fn carry_over(old: &Value, summary: &mut Value) {
    for key in SUMMARY_KEYS {
        if !old[key].is_null() {
            summary[key] = old[key].clone();
        }
    }
}

/// A thread: a task root in a bot's chat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Thread {
    pub bot: String,
    pub root: String,
}

impl Thread {
    fn key(&self) -> String {
        format!("{}/{}", self.bot, self.root)
    }

    fn lane(&self) -> Lane {
        Lane::in_thread(&self.bot, &self.root)
    }

    fn parse(key: &str) -> Option<Self> {
        let (bot, root) = key.split_once('/')?;
        Some(Self { bot: bot.to_owned(), root: root.to_owned() })
    }
}

fn sid_key(sid: &str) -> String {
    format!("terminal.sid.{sid}")
}
fn info_key(t: &Thread) -> String {
    format!("terminal.info.{}", t.key())
}
fn cursor_key(t: &Thread) -> String {
    format!("terminal.cursor.{}", t.key())
}
fn final_key(t: &Thread) -> String {
    format!("terminal.final.{}", t.key())
}
fn live_key(t: &Thread) -> String {
    format!("terminal.live.{}", t.key())
}
/// Set while a terminal has written to the session since the host last loaded it.
fn stale_key(t: &Thread) -> String {
    format!("terminal.stale.{}", t.key())
}

/// How far the transcript is imported, and which file that was (a replaced file starts over).
struct Cursor {
    offset: u64,
    dev: u64,
    ino: u64,
}

#[cfg(unix)]
fn file_id(path: &Path) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::metadata(path).map_or((0, 0), |m| (m.dev(), m.ino()))
}

#[cfg(not(unix))]
fn file_id(_path: &Path) -> (u64, u64) {
    (0, 0)
}

fn load_cursor(hub: &Hub, t: &Thread) -> Option<Cursor> {
    let raw = hub.store.kv_get(&cursor_key(t))?;
    let mut parts = raw.split(':');
    let offset = parts.next()?.parse().ok()?;
    let mut next = || parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let (dev, ino) = (next(), next());
    Some(Cursor { offset, dev, ino })
}

fn save_cursor(hub: &Hub, t: &Thread, offset: u64, path: &Path) {
    let (dev, ino) = file_id(path);
    if let Err(error) = hub.store.kv_set(&cursor_key(t), &format!("{offset}:{dev}:{ino}")) {
        tracing::warn!(%error, "couldn't keep a terminal thread's cursor");
    }
}

/// Looks at the registry now and then, for as long as the host lives.
pub fn start(hub: &Arc<Hub>) {
    // Unit tests build hubs of their own and never watch this computer's terminals.
    if cfg!(test) {
        return;
    }
    let hub = hub.clone();
    tokio::spawn(async move {
        let h = hub.clone();
        if let Err(error) = crate::watchdog::blocking("terminal-backfill", move || backfill_finals(&h)).await {
            tracing::warn!(%error, "final-reply repair failed");
        }
        loop {
            let h = hub.clone();
            if let Err(error) = crate::watchdog::blocking("terminal-scan", move || scan(&h)).await {
                tracing::warn!(%error, "terminal scan failed");
            }
            tokio::time::sleep(EVERY).await;
        }
    });
}

/// One pass over the registry. Idempotent: a session is a thread once, and only new transcript lines are imported.
pub fn scan(hub: &Hub) {
    let env = hub.terminal.env();
    let regs = registry::read_all(&env.state);
    let mut seen = Vec::new();
    for reg in &regs {
        match sync_session(hub, &env, reg) {
            Ok(Some(thread)) => seen.push(thread),
            Ok(None) => {}
            Err(error) => tracing::warn!(error = format!("{error:#}"), "couldn't sync a terminal session"),
        }
    }
    hub.terminal.empty.locked().retain(|sid, _| regs.iter().any(|r| r.session_id == *sid));
    // A thread whose terminal was live but whose registry file went away (or aged out) is closed. Closed threads
    // are not looked at again.
    for key in hub.store.kv_prefix("terminal.live.") {
        let Some(thread) = Thread::parse(&key["terminal.live.".len()..]) else { continue };
        if !seen.contains(&thread) && hub.store.kv_get(&key).as_deref() == Some("1") {
            update_state(hub, &env, &thread, None);
        }
    }
}

fn sync_session(hub: &Hub, env: &Env, reg: &Registry) -> Result<Option<Thread>> {
    // A deleted transcript leaves its thread as it is.
    let Some(size) = transcript_size(reg) else { return Ok(None) };
    let age = now_ms() - reg.started_ms().max(modified_ms(reg));
    if age > registry::MAX_AGE_MS && !registry::liveness(reg, env.sessions.as_deref()).is_live() {
        return Ok(None);
    }
    if hub.terminal.empty.locked().get(&reg.session_id) == Some(&size) {
        return Ok(None);
    }
    let (thread, created) = if let Some(t) = mapped(hub, reg) {
        (t, false)
    } else {
        let Some(bot) = owner_bot(hub, &reg.agent) else { return Ok(None) };
        let Some(t) = create_thread(hub, env, reg, &bot)? else {
            hub.terminal.empty.locked().insert(reg.session_id.clone(), size);
            return Ok(None);
        };
        (t, true)
    };
    let stored = serde_json::to_string(reg)?;
    if hub.store.kv_get(&info_key(&thread)).as_deref() != Some(stored.as_str()) {
        hub.store.kv_set(&info_key(&thread), &stored)?;
    }
    if !created {
        repair_lane(hub, &thread, reg);
        if !host_writing(hub, &thread) {
            import(hub, &thread, reg)?;
        }
    }
    update_state(hub, env, &thread, Some(reg));
    Ok(Some(thread))
}

/// A terminal thread whose session or folder didn't get saved when it was made gets them now.
fn repair_lane(hub: &Hub, t: &Thread, reg: &Registry) {
    if hub.store.entry(&t.root).is_none_or(|r| r.data["thread"]["origin"] != "terminal") {
        return;
    }
    let key = lane_key("session", &t.bot, &t.lane());
    if hub.store.kv_get(&key).is_none_or(|s| s.is_empty()) {
        let _ = hub.store.kv_set(&key, &reg.session_id);
    }
    if Pick::load(&hub.store, &t.bot, &t.lane()).is_none() {
        let _ = Pick::save(&hub.store, &t.bot, &t.lane(), Some(&Pick::Folder(reg.cwd.clone())));
    }
}

fn transcript_size(reg: &Registry) -> Option<u64> {
    std::fs::metadata(reg.transcript()).ok().map(|m| m.len())
}

fn modified_ms(reg: &Registry) -> i64 {
    std::fs::metadata(reg.transcript())
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(0))
}

/// The owner's bot of `agent`.
fn owner_bot(hub: &Hub, agent: &str) -> Option<BotConfig> {
    let rows = hub.store.bots().ok()?;
    rows.into_iter()
        .find(|r| !r.deleted && r.config.person.is_none() && r.config.hq_agent.as_deref() == Some(agent))
        .map(|r| r.config)
}

fn host_writing(hub: &Hub, t: &Thread) -> bool {
    hub.store.bot(&t.bot).ok().flatten().is_some_and(|r| hub.working_lanes(&r.config).contains(&t.lane()))
}

/// The thread a session already belongs to: by its own id, by the session it was resumed from, or because an app
/// thread's lane runs it (a thread resumed in the terminal).
fn mapped(hub: &Hub, reg: &Registry) -> Option<Thread> {
    let exists = |t: &Thread| hub.store.entry(&t.root).is_some_and(|e| e.bot_id == t.bot && e.thread_id.is_none());
    if let Some(t) = hub.store.kv_get(&sid_key(&reg.session_id)).and_then(|k| Thread::parse(&k)).filter(exists) {
        return Some(t);
    }
    let owned = |t: &Thread| {
        hub.store
            .bot(&t.bot)
            .ok()
            .flatten()
            .is_some_and(|r| r.config.person.is_none() && r.config.hq_agent.as_deref() == Some(reg.agent.as_str()))
    };
    // `--resume` keeps the session id; a fork names the session it came from.
    if let Some(from) = reg.resumed_from.as_deref().filter(|f| *f != reg.session_id)
        && let Some(t) = hub.store.kv_get(&sid_key(from)).and_then(|k| Thread::parse(&k)).filter(exists)
    {
        adopt(hub, &t, reg, true);
        return Some(t);
    }
    let lane_sessions = hub.store.kv_prefix("lane.session.");
    for key in lane_sessions {
        if hub.store.kv_get(&key).as_deref() != Some(reg.session_id.as_str()) {
            continue;
        }
        let Some((bot, rest)) = key["lane.session.".len()..].split_once('@') else { continue };
        let Some(root) = rest.strip_prefix(&format!("{bot}/")) else { continue };
        let t = Thread { bot: bot.to_owned(), root: root.to_owned() };
        if exists(&t) && owned(&t) {
            adopt(hub, &t, reg, false);
            return Some(t);
        }
    }
    if let Some(from) = reg.resumed_from.as_deref().filter(|f| *f != reg.session_id) {
        // An app thread resumed in a terminal that forked the session.
        for key in hub.store.kv_prefix("lane.session.") {
            if hub.store.kv_get(&key).as_deref() != Some(from) {
                continue;
            }
            let Some((bot, rest)) = key["lane.session.".len()..].split_once('@') else { continue };
            let Some(root) = rest.strip_prefix(&format!("{bot}/")) else { continue };
            let t = Thread { bot: bot.to_owned(), root: root.to_owned() };
            if exists(&t) && owned(&t) {
                adopt(hub, &t, reg, true);
                return Some(t);
            }
        }
    }
    None
}

/// Makes `reg`'s session the one of an existing thread. A forked session becomes the thread's lane session, in the
/// folder it ran in, so the host carries on from where the terminal left it.
fn adopt(hub: &Hub, t: &Thread, reg: &Registry, forked: bool) {
    crate::agent::hq::tag_session(&hub.store, &reg.session_id, reg.account.as_deref());
    let result = (|| -> Result<()> {
        hub.store.kv_set(&sid_key(&reg.session_id), &t.key())?;
        if forked {
            hub.store.kv_set(&lane_key("session", &t.bot, &t.lane()), &reg.session_id)?;
            Pick::save(&hub.store, &t.bot, &t.lane(), Some(&Pick::Folder(reg.cwd.clone())))?;
            // The fork starts as a copy of what the thread already has.
            hub.store.kv_delete(&cursor_key(t))?;
        }
        hub.store.kv_set(&stale_key(t), "1")?;
        forget_loaded(hub, t);
        Ok(())
    })();
    if let Err(error) = result {
        tracing::warn!(error = format!("{error:#}"), "couldn't map a terminal session to its thread");
    }
}

/// What `read_items` found.
struct Read {
    /// Items in order, the oldest dropped past the cap.
    items: Vec<Item>,
    dropped: usize,
    /// The first thing the user said, and where among all items.
    first_user: Option<(usize, Item)>,
    /// The offset after the last whole line read.
    end: u64,
}

/// Reads `path` from `from`, keeping at most `keep` items (the newest).
fn read_items(path: &Path, from: u64, since: i64, keep: usize) -> std::io::Result<Read> {
    let mut file = std::fs::File::open(path)?;
    file.seek(SeekFrom::Start(from))?;
    let mut out = Read { items: Vec::new(), dropped: 0, first_user: None, end: from };
    let mut kept: VecDeque<Item> = VecDeque::new();
    let (mut carry, mut buf, mut index) = (Vec::new(), vec![0u8; 4 << 20], 0usize);
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        carry.extend_from_slice(&buf[..n]);
        let parsed = jsonl::parse(&carry, since);
        carry.drain(..parsed.consumed);
        out.end += parsed.consumed as u64;
        for item in parsed.items {
            if out.first_user.is_none() && matches!(item, Item::User { .. }) {
                out.first_user = Some((index, item.clone()));
            }
            index += 1;
            kept.push_back(item);
            if kept.len() > keep {
                kept.pop_front();
                out.dropped += 1;
            }
        }
    }
    out.items = kept.into();
    Ok(out)
}

/// A new thread for a session with something the user said; None while it has none yet.
fn create_thread(hub: &Hub, env: &Env, reg: &Registry, bot: &BotConfig) -> Result<Option<Thread>> {
    let Ok(read) = read_items(reg.transcript(), 0, 0, FIRST_IMPORT) else { return Ok(None) };
    let Some((first_index, Item::User { text, at })) = read.first_user.clone() else { return Ok(None) };
    let live = registry::liveness(reg, env.sessions.as_deref()).is_live();
    let turn = hub.store.max_turn(&bot.id) + 1;
    let data = json!({
        "text": cap_text(&text),
        "task": true,
        "status": "sent",
        "clientNonce": "",
        "attachments": [],
        "thread": thread_value(reg, "terminal", live, false),
    });
    let root = hub
        .add_entry_at(&Lane::main(&bot.id), EntryKind::User, turn, &data, at)
        .ok_or_else(|| anyhow!("couldn't save the thread"))?;
    let thread = Thread { bot: bot.id.clone(), root: root.id.clone() };
    // The session's claim on the thread first: without it the next scan would make a second thread. If it can't be
    // kept, this one is withdrawn.
    if let Err(error) = (0..3).try_for_each(|_| hub.store.kv_set(&sid_key(&reg.session_id), &thread.key())) {
        let mut withdrawn = root.data.clone();
        withdrawn["status"] = "cancelled".into();
        withdrawn["task"] = false.into();
        hub.set_entry(&root.id, &withdrawn);
        return Err(error);
    }
    // Loaded only under the login that made it (see `agent::hq::Launch::may_load`).
    crate::agent::hq::tag_session(&hub.store, &reg.session_id, reg.account.as_deref());
    // The rest is repaired by a later scan if it fails (see `repair_lane`).
    let saved = hub
        .store
        .kv_set(&lane_key("session", &thread.bot, &thread.lane()), &reg.session_id)
        .and_then(|()| Pick::save(&hub.store, &thread.bot, &thread.lane(), Some(&Pick::Folder(reg.cwd.clone()))));
    if let Err(error) = saved {
        tracing::warn!(error = format!("{error:#}"), "couldn't save a terminal thread's session");
    }
    hub.store.kv_set(&info_key(&thread), &serde_json::to_string(reg)?)?;
    hub.store.kv_set(&live_key(&thread), if live { "1" } else { "0" })?;
    if live {
        hub.store.kv_set(&stale_key(&thread), "1")?;
    }
    // A one-shot says who asked in the card's label, not in a divider about a terminal nobody sat in.
    if !reg.one_shot() {
        writer_notice(hub, &thread, "startedInTerminal", "Started in the terminal", at);
    }
    let mut items = read.items;
    let mut dropped = read.dropped > 0;
    if first_index >= read.dropped {
        items.remove(first_index - read.dropped);
    } else {
        dropped = true;
    }
    if dropped {
        notice(hub, &thread, "Earlier messages are in the terminal transcript.", at);
    }
    apply(hub, &thread, &items, live);
    save_cursor(hub, &thread, read.end, reg.transcript());
    done_importing(hub, &thread, reg);
    Ok(Some(thread))
}

/// The root's `data.thread` of a new terminal thread.
fn thread_value(reg: &Registry, origin: &str, live: bool, busy: bool) -> Value {
    // One-shots keep `origin: terminal`: the apps already read such a card as read-only, continued from the terminal.
    let mut value = json!({
        "count": 0, "lastAt": 0, "authors": [], "unread": 0,
        "origin": origin,
        "liveWriter": if live { json!("terminal") } else { Value::Null },
        "liveBusy": busy,
        "terminal": {"sessionId": reg.session_id, "cwd": reg.cwd},
    });
    if let Some(label) = reg.label.as_deref().filter(|_| reg.one_shot()) {
        value["originLabel"] = label.into();
    }
    value
}

/// Imports what the transcript gained since the cursor. A thread that has none yet (an app thread a terminal
/// resumed), or whose file was replaced or cut short, reads the lines newer than the thread's own newest entry.
/// One import of a thread at a time: the scan, a send and a take-over all come here.
pub(super) fn import(hub: &Hub, t: &Thread, reg: &Registry) -> Result<()> {
    let lock = hub.terminal.thread_lock(&t.key());
    let _one = lock.locked();
    import_locked(hub, t, reg)
}

/// `import` for a caller that holds the thread's lock already.
fn import_locked(hub: &Hub, t: &Thread, reg: &Registry) -> Result<()> {
    let Some(size) = transcript_size(reg) else { return Ok(()) };
    let live = registry::liveness(reg, hub.terminal.env().sessions.as_deref()).is_live();
    let (dev, ino) = file_id(reg.transcript());
    let (from, since, keep) = match load_cursor(hub, t) {
        Some(c) if (c.dev != 0 && (c.dev, c.ino) != (dev, ino)) || c.offset > size => {
            (0, newest_at(hub, t), FIRST_IMPORT)
        }
        Some(c) if c.offset == size => return Ok(()),
        Some(c) => (c.offset, 0, LATER_IMPORT),
        None => (0, newest_at(hub, t), FIRST_IMPORT),
    };
    let read = read_items(reg.transcript(), from, since, keep)?;
    if read.dropped > 0 {
        notice(hub, t, "Earlier messages are in the terminal transcript.", now_ms());
    }
    apply(hub, t, &read.items, live);
    save_cursor(hub, t, read.end, reg.transcript());
    done_importing(hub, t, reg);
    // A terminal that ran and closed between two scans was never seen live: the session still changed under the cache.
    if !read.items.is_empty() {
        forget_loaded(hub, t);
    }
    Ok(())
}

/// When the thread last changed (ms).
fn newest_at(hub: &Hub, t: &Thread) -> i64 {
    let last = hub.store.thread(&t.bot, &t.root, 1).ok().and_then(|v| v.last().map(|e| e.updated_at));
    last.or_else(|| hub.store.entry(&t.root).map(|e| e.created_at)).unwrap_or(0)
}

fn done_importing(hub: &Hub, t: &Thread, reg: &Registry) {
    hub.refresh_thread_summary(&t.bot, &t.root);
    // A one-shot nobody watched is new to the room: it stays unread. A terminal was read where it ran.
    if reg.one_shot() {
        return;
    }
    if let Err(error) = hub.mark_read(&t.bot, ReadScope::Thread(&t.root)) {
        tracing::warn!(%error, "couldn't mark a terminal thread read");
    }
}

fn cap_text(text: &str) -> String {
    text.chars().take(8000).collect()
}

fn notice(hub: &Hub, t: &Thread, text: &str, at: i64) {
    let turn = hub.store.max_turn(&t.bot);
    hub.add_entry_at(&t.lane(), EntryKind::Notice, turn, &json!({"text": text, "style": "divider"}), at);
}

/// A notice for the apps' time-separator: `writerChange` says who started or stopped writing.
fn writer_notice(hub: &Hub, t: &Thread, change: &str, text: &str, at: i64) {
    let turn = hub.store.max_turn(&t.bot);
    let data = json!({"text": text, "style": "divider", "writerChange": change, "at": at});
    hub.add_entry_at(&t.lane(), EntryKind::Notice, turn, &data, at);
}

/// Adds `items` to the thread as entries. `live`: a tool without an outcome yet is still running.
fn apply(hub: &Hub, t: &Thread, items: &[Item], live: bool) {
    let mut tools: Option<HashMap<String, String>> = None;
    let mut final_id = hub.store.kv_get(&final_key(t)).filter(|s| !s.is_empty());
    let lane = t.lane();
    // A turn's last reply is its final one, as in app turns: only a later reply moves it, never a thought or a
    // tool call, and a new user message starts a turn without touching the last one's.
    let unfinal = |final_id: &mut Option<String>| {
        if let Some(id) = final_id.take()
            && let Some(mut e) = hub.store.entry(&id)
        {
            e.data["final"] = false.into();
            hub.set_entry(&id, &e.data);
        }
    };
    for (i, item) in items.iter().enumerate() {
        let turn = hub.store.max_turn(&t.bot);
        match item {
            Item::User { text, at } => {
                let data = json!({"text": cap_text(text), "status": "sent", "clientNonce": "", "attachments": []});
                hub.add_entry_at(&lane, EntryKind::User, turn + 1, &data, *at);
                final_id = None;
            }
            Item::Agent { text, at } => {
                unfinal(&mut final_id);
                let data = json!({"text": text, "final": true});
                final_id = hub.add_entry_at(&lane, EntryKind::Agent, turn, &data, *at).map(|e| e.id);
            }
            Item::Thought { text, at } => {
                hub.add_entry_at(&lane, EntryKind::Thought, turn, &json!({"text": text}), *at);
            }
            Item::ToolUse { id, title, kind, at } => {
                let answered = items[i + 1..].iter().any(|x| matches!(x, Item::ToolResult { id: r, .. } if r == id));
                let status = if answered || !live { "completed" } else { "pending" };
                let data = json!({
                    "toolCallId": id, "title": title, "toolKind": kind, "status": status,
                    "output": "", "diffs": [], "locations": null,
                });
                if let Some(e) = hub.add_entry_at(&lane, EntryKind::Tool, turn, &data, *at)
                    && let Some(map) = tools.as_mut()
                {
                    map.insert(id.clone(), e.id);
                }
            }
            Item::ToolResult { id, output, failed } => {
                let map = tools.get_or_insert_with(|| tool_entries(hub, t));
                let Some(mut e) = map.get(id).and_then(|e| hub.store.entry(e)) else { continue };
                e.data["status"] = if *failed { "failed" } else { "completed" }.into();
                e.data["output"] = output.clone().into();
                hub.set_entry(&e.id, &e.data);
            }
        }
    }
    let stored = final_id.unwrap_or_default();
    if let Err(error) = hub.store.kv_set(&final_key(t), &stored) {
        tracing::warn!(%error, "couldn't keep the final reply of a terminal thread");
    }
}

/// One-off repair (idempotent, runs once at start): terminal threads imported before a turn's last reply stayed
/// final lost the flag on every reply that a later tool call or thought followed. Marks each turn's last agent
/// entry final again and recomputes the summaries. Only threads the terminal imported are looked at.
pub fn backfill_finals(hub: &Hub) {
    const DONE: &str = "terminal.finals.v2";
    if hub.store.kv_get(DONE).is_some() {
        return;
    }
    for key in hub.store.kv_prefix("terminal.info.") {
        let Some(t) = Thread::parse(&key["terminal.info.".len()..]) else { continue };
        if hub.store.entry(&t.root).is_none_or(|r| r.data["thread"]["origin"] != "terminal") {
            continue;
        }
        let lock = hub.terminal.thread_lock(&t.key());
        let _one = lock.locked();
        let entries = hub.store.thread(&t.bot, &t.root, i64::MAX).unwrap_or_default();
        let mut changed = false;
        let mut last: Option<&crate::store::Entry> = None;
        let mut fix = |e: Option<&crate::store::Entry>| {
            if let Some(e) = e.filter(|e| e.data["final"] != true) {
                let mut data = e.data.clone();
                data["final"] = true.into();
                hub.set_entry(&e.id, &data);
                changed = true;
            }
        };
        for e in &entries {
            match e.kind.as_str() {
                "user" => fix(last.take()),
                "agent" => last = Some(e),
                _ => {}
            }
        }
        fix(last);
        if changed {
            hub.refresh_thread_summary(&t.bot, &t.root);
        }
        // The importer's remembered reply must be the newest turn's last one.
        if let Err(error) = hub.store.kv_set(&final_key(&t), last.map_or("", |e| e.id.as_str())) {
            tracing::warn!(%error, "couldn't keep the final reply of a terminal thread");
        }
    }
    if let Err(error) = hub.store.kv_set(DONE, "1") {
        tracing::warn!(%error, "couldn't mark the final-reply repair done");
    }
}

/// The thread's tool entries by the tool call they show.
fn tool_entries(hub: &Hub, t: &Thread) -> HashMap<String, String> {
    hub.store
        .thread(&t.bot, &t.root, 600)
        .unwrap_or_default()
        .into_iter()
        .filter(|e| e.kind == EntryKind::Tool.as_str())
        .filter_map(|e| Some((e.data["toolCallId"].as_str()?.to_owned(), e.id)))
        .collect()
}

/// Brings the root's writer fields and the writer-change notices in line with the terminal. `reg` None: the
/// session's registry file is gone. The live flag changes under the thread's lock, so one pass alone says so.
fn update_state(hub: &Hub, env: &Env, t: &Thread, reg: Option<&Registry>) {
    // A take-over says what happened itself.
    if hub.terminal.taking.locked().contains(&t.key()) {
        return;
    }
    let lock = hub.terminal.thread_lock(&t.key());
    let _one = lock.locked();
    let reg = reg.cloned().or_else(|| info(hub, t));
    let live = reg.as_ref().map_or(Live::Closed, |r| registry::liveness(r, env.sessions.as_deref()));
    let was_live = hub.store.kv_get(&live_key(t)).as_deref() == Some("1");
    if live.is_live() != was_live {
        let quiet = reg.as_ref().is_some_and(Registry::one_shot);
        if hub.store.kv_set(&live_key(t), if live.is_live() { "1" } else { "0" }).is_ok() && !quiet {
            let (change, text) = if live.is_live() {
                ("openedInTerminal", "Opened in the terminal")
            } else {
                ("terminalClosed", "Terminal closed")
            };
            writer_notice(hub, t, change, text, now_ms());
        }
        hub.refresh_thread_summary(&t.bot, &t.root);
    }
    if live.is_live() && hub.store.kv_get(&stale_key(t)).is_none() {
        // The terminal now writes to the session: what the host's actor holds of it will be out of date.
        if hub.store.kv_set(&stale_key(t), "1").is_ok() {
            forget_loaded(hub, t);
        }
    }
    set_writer(hub, t, reg.as_ref(), live, host_writing(hub, t));
}

/// Tells the thread's actor to load its session again on the next turn.
fn forget_loaded(hub: &Hub, t: &Thread) {
    #[cfg(test)]
    hub.terminal.forgets.locked().push(t.key());
    if let Err(error) = hub.send_cmd(&t.bot, Cmd::ForgetLoaded { lane: t.lane() }) {
        tracing::debug!(%error, "no actor to forget a terminal thread's session");
    }
}

fn info(hub: &Hub, t: &Thread) -> Option<Registry> {
    registry::parse(&hub.store.kv_get(&info_key(t))?)
}

/// Writes `liveWriter`, `liveBusy` and `terminal` into the root's summary when they changed.
fn set_writer(hub: &Hub, t: &Thread, reg: Option<&Registry>, live: Live, host: bool) {
    let Some(mut root) = hub.store.entry(&t.root) else { return };
    let writer = match (live.is_live(), host) {
        (true, _) => json!("terminal"),
        (false, true) => json!("app"),
        (false, false) => Value::Null,
    };
    let busy = live == Live::Busy || (!live.is_live() && host);
    let mut summary = root.data["thread"].clone();
    if !summary.is_object() {
        summary = json!({});
    }
    summary["liveWriter"] = writer;
    summary["liveBusy"] = busy.into();
    if let Some(r) = reg {
        summary["terminal"] = json!({"sessionId": r.session_id, "cwd": r.cwd});
    }
    if root.data["thread"] != summary {
        root.data["thread"] = summary;
        hub.set_entry(&t.root, &root.data);
    }
}

/// A host turn on `lane` ended: what it wrote to the transcript is already in the thread, so the cursor moves to the
/// end of the file.
pub fn host_turn_ended(hub: &Hub, lane: &Lane) {
    // A bot guest's conversation is free again (it is not a terminal thread; this does nothing else for it).
    crate::api::botguests::turn_ended(hub, lane);
    let Some(root) = &lane.thread else { return };
    let t = Thread { bot: lane.chat.clone(), root: root.clone() };
    let Some(reg) = info(hub, &t) else { return };
    let lock = hub.terminal.thread_lock(&t.key());
    let _one = lock.locked();
    if let Some(size) = transcript_size(&reg) {
        save_cursor(hub, &t, size, reg.transcript());
    }
}

/// A message to the thread `root` when it is a terminal thread: refused while another writer holds it. A closed
/// terminal's last lines are imported first, and the thread's actor loads the session again if the terminal wrote
/// to it, so the host carries on from the whole conversation.
#[cfg(test)]
pub async fn before_send(hub: &Arc<Hub>, bot: &str, root: &str) -> Result<()> {
    send_gate(hub, bot, root, false).await.map(|_| ())
}

/// What a send into a thread does now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendGate {
    /// Nobody else writes: the turn starts now.
    Free,
    /// A terminal (or a take-over) holds the thread: the message waits as a wake until it lets go. Only said when the
    /// caller `may_wait`.
    Waits,
}

/// [`before_send`] for a caller whose message may wait for a terminal instead of being refused (the owner's own app
/// chat: the wake runs as the person the session serves, rule of 2026-10-26). A bot guest's writer is never waited for.
pub async fn send_gate(hub: &Arc<Hub>, bot: &str, root: &str, may_wait: bool) -> Result<SendGate> {
    let t = Thread { bot: bot.to_owned(), root: root.to_owned() };
    let hub = hub.clone();
    // File and process reads: off the runtime.
    tokio::task::spawn_blocking(move || send_gate_blocking(&hub, &t, may_wait)).await?
}

fn send_gate_blocking(hub: &Hub, t: &Thread, may_wait: bool) -> Result<SendGate> {
    // The same lock `resolve` (hostBusy) and a turn's start take: a claim, a resume and a start see each other in order.
    let lock = hub.terminal.thread_lock(&t.key());
    let _one = lock.locked();
    match writer(hub, t) {
        Some(Writer::Terminal) if may_wait => return Ok(SendGate::Waits),
        Some(w) => return Err(HeldByWriter { code: w.code() }.into()),
        None => {}
    }
    sync_one(hub, t);
    if hub.store.kv_get(&stale_key(t)).is_some() && hub.store.kv_delete(&stale_key(t)).is_ok() {
        forget_loaded(hub, t);
    }
    Ok(SendGate::Free)
}

/// Who holds a thread, as far as a send into it is concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Writer {
    /// A terminal, a take-over, or a take-over under way.
    Terminal,
    /// A bot guest's turn (`Kind::Bot` claim, taken by `agent/bot/turn.rs` with the agent process's pid).
    Bot,
}

impl Writer {
    fn code(self) -> &'static str {
        match self {
            Self::Terminal => "liveInTerminal",
            Self::Bot => "heldByOtherWriter",
        }
    }
}

/// Who holds the thread, judged on what is on disk now: a claim, a live registry file of the lane's session or of the
/// last scanned one, or a live process. `info` alone lags a scan behind.
fn writer(hub: &Hub, t: &Thread) -> Option<Writer> {
    let env = hub.terminal.env();
    if hub.terminal.taking.locked().contains(&t.key()) {
        return Some(Writer::Terminal);
    }
    let lane_sid = hub.store.kv_get(&lane_key("session", &t.bot, &t.lane())).filter(|s| !s.is_empty());
    let known = info(hub, t);
    let sids = lane_sid.into_iter().chain(known.iter().map(|r| r.session_id.clone()));
    for sid in sids {
        if !registry::safe_id(&sid) {
            continue;
        }
        if let Some(c) = claim::holder(&env.state, &sid) {
            return Some(if c.kind == claim::Kind::Bot { Writer::Bot } else { Writer::Terminal });
        }
        if let Some(reg) = registry::read_one(&env.state, &sid)
            && registry::liveness(&reg, env.sessions.as_deref()).is_live()
        {
            return Some(Writer::Terminal);
        }
    }
    known.filter(|reg| registry::liveness(reg, env.sessions.as_deref()).is_live()).map(|_| Writer::Terminal)
}

/// A bot guest asks into its conversation's thread (`botAsk`): refused, as `heldByOtherWriter`, while a writer other than
/// the bot's own turns holds it (a terminal, a take-over). A `Kind::Bot` claim is the bot's own running turn: its ask
/// waits behind it in the thread's lane. Same lock as `before_send` and the start of a turn.
pub async fn bot_gate(hub: &Arc<Hub>, chat: &str, root: &str) -> Result<(), HeldByWriter> {
    let (hub, t) = (hub.clone(), Thread { bot: chat.to_owned(), root: root.to_owned() });
    tokio::task::spawn_blocking(move || {
        let lock = hub.terminal.thread_lock(&t.key());
        let _one = lock.locked();
        match writer(&hub, &t) {
            Some(Writer::Terminal) => Err(HeldByWriter { code: "heldByOtherWriter" }),
            Some(Writer::Bot) | None => Ok(()),
        }
    })
    .await
    .unwrap_or(Err(HeldByWriter { code: "heldByOtherWriter" }))
}

/// Brings this one thread in step with the registry now, with its lock held: a fork becomes the lane's session
/// (`mapped` adopts it), the file's last lines are imported, and the cache is forgotten when any were.
fn sync_one(hub: &Hub, t: &Thread) {
    let env = hub.terminal.env();
    for reg in registry::read_all(&env.state) {
        // `mapped` adopts a fork (the lane's session becomes the fork); only the lane's current session is imported.
        if mapped(hub, &reg).as_ref() != Some(t) {
            continue;
        }
        let lane_sid = hub.store.kv_get(&lane_key("session", &t.bot, &t.lane()));
        if lane_sid.as_deref() != Some(reg.session_id.as_str()) {
            continue;
        }
        if let Ok(stored) = serde_json::to_string(&reg)
            && hub.store.kv_get(&info_key(t)).as_deref() != Some(stored.as_str())
        {
            let _ = hub.store.kv_set(&info_key(t), &stored);
        }
        if let Err(error) = import_locked(hub, t, &reg) {
            tracing::warn!(error = format!("{error:#}"), "couldn't catch up a terminal thread before a send");
        }
    }
}

/// A turn of the host is about to start in `lane`: refused while a terminal holds the thread. `begin` marks the turn
/// running, inside the thread's lock, so `resolve` (which reads the lanes at work under the same lock) sees it. File and
/// process reads: off the runtime.
pub async fn start_gate(
    hub: &Arc<Hub>,
    lane: &Lane,
    begin: impl FnOnce(&Hub) + Send + 'static,
) -> Result<(), HeldByWriter> {
    let (hub, lane) = (hub.clone(), lane.clone());
    tokio::task::spawn_blocking(move || {
        let Some(root) = &lane.thread else {
            begin(&hub);
            return Ok(());
        };
        let t = Thread { bot: lane.chat.clone(), root: root.clone() };
        let lock = hub.terminal.thread_lock(&t.key());
        let _one = lock.locked();
        // A bot's claim is a turn of the host's own, which the thread's lane queue already orders; only others refuse.
        if writer(&hub, &t) == Some(Writer::Terminal) {
            let bot = hub.store.bot_conversation_of_thread(&lane.chat, root).is_some();
            return Err(HeldByWriter { code: if bot { "heldByOtherWriter" } else { "liveInTerminal" } });
        }
        begin(&hub);
        Ok(())
    })
    .await
    .unwrap_or(Err(HeldByWriter { code: "liveInTerminal" }))
}

/// A send accepted but not started yet: its user entry still says `queued`.
fn send_queued(hub: &Hub, t: &Thread) -> bool {
    let queued = |e: &crate::store::Entry| e.kind == EntryKind::User.as_str() && e.data["status"] == "queued";
    // The root is the task's first message: its own turn may still be waiting.
    hub.store.entry(&t.root).is_some_and(|e| queued(&e))
        || hub.store.thread(&t.bot, &t.root, 50).unwrap_or_default().iter().any(queued)
}

/// What the kernel needs to resume a thread: `{botId, threadId, agent, sessionId, cwd, hostBusy}` for a thread id or
/// a session id.
pub async fn resolve(hub: &Hub, which: &str) -> Value {
    let unknown = || json!({"error": "unknown thread"});
    let entry = hub.store.entry(which);
    let thread = match entry {
        Some(e) => Some(Thread { bot: e.bot_id.clone(), root: e.thread_id.clone().unwrap_or(e.id.clone()) }),
        None => hub.store.kv_get(&sid_key(which)).and_then(|k| Thread::parse(&k)).or_else(|| lane_owner(hub, which)),
    };
    let Some(t) = thread else { return resolve_main_lane(hub, which).await.unwrap_or_else(unknown) };
    let Some(row) = hub.store.bot(&t.bot).ok().flatten().filter(|r| !r.deleted && r.config.person.is_none()) else {
        return unknown();
    };
    let root_ok = hub.store.entry(&t.root).is_some_and(|e| e.bot_id == t.bot && e.thread_id.is_none());
    let session = hub.store.kv_get(&lane_key("session", &t.bot, &t.lane())).filter(|s| !s.is_empty());
    let (true, Some(session), Some(agent)) = (root_ok, session, row.config.hq_agent.clone()) else {
        return unknown();
    };
    let cwd = crate::agent::folder::resolve(&hub.store, &row.config, &t.lane()).await.cwd;
    // Read under the thread's lock, after the kernel took its claim: a turn that starts later sees the claim, one that
    // started earlier is counted here (running, or still queued).
    let busy = {
        let lock = hub.terminal.thread_lock(&t.key());
        let _one = lock.locked();
        hub.working_lanes(&row.config).contains(&t.lane())
            || send_queued(hub, &t)
            || hub.terminal.taking.locked().contains(&t.key())
    };
    // `claims`: this host takes writer claims for its turns; the kernel treats an answer without it as an older host's.
    json!({
        "botId": t.bot, "threadId": t.root, "agent": agent, "sessionId": session, "cwd": cwd,
        "hostBusy": busy, "claims": 1,
    })
}

/// The answer for a session that is a bot's main chat lane: the kernel claims it like a thread's, and `hostBusy` says
/// whether the app is answering in it right now (a turn there holds a writer claim too, see `claim_for_turn`).
async fn resolve_main_lane(hub: &Hub, sid: &str) -> Option<Value> {
    let bot = hub.store.kv_prefix("lane.session.").into_iter().find_map(|key| {
        (hub.store.kv_get(&key).as_deref() == Some(sid)).then_some(())?;
        let (bot, chat) = key["lane.session.".len()..].split_once('@')?;
        (bot == chat).then(|| bot.to_owned())
    })?;
    let row = hub.store.bot(&bot).ok().flatten().filter(|r| !r.deleted && r.config.person.is_none())?;
    let agent = row.config.hq_agent.clone()?;
    let lane = Lane::main(&bot);
    let cwd = crate::agent::folder::resolve(&hub.store, &row.config, &lane).await.cwd;
    let busy = hub.working_lanes(&row.config).contains(&lane);
    Some(
        json!({"botId": bot, "threadId": null, "agent": agent, "sessionId": sid, "cwd": cwd, "hostBusy": busy, "claims": 1}),
    )
}

/// The thread whose lane runs session `sid`.
fn lane_owner(hub: &Hub, sid: &str) -> Option<Thread> {
    hub.store.kv_prefix("lane.session.").into_iter().find_map(|key| {
        (hub.store.kv_get(&key).as_deref() == Some(sid)).then_some(())?;
        let (bot, rest) = key["lane.session.".len()..].split_once('@')?;
        let root = rest.strip_prefix(&format!("{bot}/"))?;
        Some(Thread { bot: bot.to_owned(), root: root.to_owned() })
    })
}

/// Ends the terminal that holds the thread so the host can write: refused while the terminal works, a no-op when it
/// is already closed. Returns the thread's new writer.
pub async fn take_over(hub: &Arc<Hub>, bot: &str, root: &str) -> Result<Value> {
    let t = Thread { bot: bot.to_owned(), root: root.to_owned() };
    let row = hub.store.bot(bot)?.filter(|r| !r.deleted && r.config.person.is_none());
    anyhow::ensure!(row.is_some() && info(hub, &t).is_some(), "unknown thread");
    if !hub.terminal.taking.locked().insert(t.key()) {
        // A second tap while the first runs.
        return Ok(json!({"liveWriter": null, "tookOver": false}));
    }
    // Dropped with the request, the key must still go, or the thread refuses every send until a restart.
    let _taking = TakingGuard { hub: hub.clone(), key: t.key() };
    let taken = take_over_inner(hub, &t).await?;
    if taken["tookOver"] == true {
        // Messages that waited for the terminal go out now, as the next turn: the queue is their only way in.
        crate::cards::resume::kick();
    }
    Ok(taken)
}

struct TakingGuard {
    hub: Arc<Hub>,
    key: String,
}

impl Drop for TakingGuard {
    fn drop(&mut self) {
        self.hub.terminal.taking.locked().remove(&self.key);
    }
}

async fn take_over_inner(hub: &Arc<Hub>, t: &Thread) -> Result<Value> {
    let env = hub.terminal.env();
    let reg = info(hub, t).ok_or_else(|| anyhow!("unknown thread"))?;
    match registry::liveness(&reg, env.sessions.as_deref()) {
        Live::Closed => return Ok(json!({"liveWriter": null, "tookOver": false})),
        Live::Busy => return Err(TerminalBusy("terminalBusy: The terminal is working. Wait for it to finish.").into()),
        Live::Idle => {}
    }
    // Held from here to the end: nobody resumes or sends into the session while it is being ended and imported. The
    // terminal's own claim stays its own; this one is the host's, named for the take-over.
    let sid = reg.session_id.clone();
    if claim::take(&env.state, &sid, claim::Kind::App, std::process::id(), true)?.is_err() {
        // A second take-over of the same session is already running.
        return Ok(json!({"liveWriter": null, "tookOver": false}));
    }
    let _claim = ReleaseOnDrop { state: env.state.clone(), sid };
    // The terminal reads this to tell its user where the conversation went.
    let taken = registry::dir(&env.state).join(format!("{}.taken", reg.session_id));
    write_private(&taken, &chrono::Utc::now().to_rfc3339())?;
    // Re-checked just before the signal: the pid may have changed hands, or the terminal may have taken a prompt
    // (`liveness` reads the session file's status and checks the process' start time). What is left is the time between
    // this read and `kill(2)`: microseconds, and macOS has no way to signal "this process, if still idle".
    let idle = registry::liveness(&reg, env.sessions.as_deref()) == Live::Idle;
    // The intent lands before the signal (F12 rule 8): the terminal that goes away was taken over, it did not crash.
    if idle {
        super::intent::write(&env.state, &reg.session_id, "taken over in the app", reg.pid);
    }
    if !idle || !registry::terminate(reg.pid) {
        let _ = std::fs::remove_file(super::intent::path(&env.state, &reg.session_id));
        let _ = std::fs::remove_file(&taken);
        return Err(TerminalBusy("terminalBusy: The terminal changed while taking over. Try again.").into());
    }
    let deadline = tokio::time::Instant::now() + EXIT_WAIT;
    while registry::pid_runs(reg.pid) {
        if tokio::time::Instant::now() >= deadline {
            let _ = std::fs::remove_file(&taken);
            return Err(TerminalBusy("terminalBusy: The terminal didn't close. Close it yourself.").into());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // The terminal's own `run` gives its claim back right after claude exits.
    let grace = tokio::time::Instant::now() + Duration::from_secs(2);
    while claim::read(&claim::path(&env.state, &reg.session_id, false)).is_some_and(|c| c.is_live())
        && tokio::time::Instant::now() < grace
    {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // The terminal's last lines, before the host writes after them.
    let (h, thread, r) = (hub.clone(), t.clone(), reg);
    tokio::task::spawn_blocking(move || {
        if let Err(error) = import(&h, &thread, &r) {
            tracing::warn!(error = format!("{error:#}"), "couldn't import a terminal's last lines");
        }
        // Said here, so the scan doesn't also say the terminal closed; only the pass that flips the flag says it.
        let lock = h.terminal.thread_lock(&thread.key());
        let _one = lock.locked();
        if h.store.kv_get(&live_key(&thread)).as_deref() == Some("1") && h.store.kv_set(&live_key(&thread), "0").is_ok()
        {
            writer_notice(&h, &thread, "tookOver", "Taken over in the app", now_ms());
        }
        // The terminal wrote to the session: the actor loads it again.
        if h.store.kv_delete(&stale_key(&thread)).is_ok() {
            forget_loaded(&h, &thread);
        }
        set_writer(&h, &thread, Some(&r), Live::Closed, false);
    })
    .await?;
    Ok(json!({"liveWriter": null, "tookOver": true}))
}

/// Gives the host's take-over claim back on every way out of `take_over_inner`.
struct ReleaseOnDrop {
    state: std::path::PathBuf,
    sid: String,
}

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        claim::release(&self.state, &self.sid, std::process::id(), true);
    }
}

fn write_private(path: &Path, text: &str) -> Result<()> {
    use std::io::Write as _;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt as _;
    let tmp = path.with_extension("taken.tmp");
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    opts.mode(0o600);
    opts.open(&tmp)?.write_all(text.as_bytes())?;
    std::fs::rename(tmp, path)?;
    Ok(())
}
