//! The core, the same on every platform: people, access, rooms, threads and sessions. The queue and the runs are
//! in `queue.rs` and `run.rs`; this file is what happens to a message until it reaches the queue.
//!
//! Who answers a message: `@agent` (or `/agent`) at its start, or a mention anywhere; else, in a thread, the agent
//! that answered last; else the room's default (an agent's own Discord channel defaults to that agent); else, when
//! the bot was addressed, the bot asks "who should take this?" and never guesses. The first agent in a thread owns
//! the task; one mentioned later answers in its own session there.

use super::adapter::{Adapter, Incoming};
use super::hq::{self, Access, Agents, Paths, Who};
use super::judge::Judge;
use super::settings::Settings;
use super::state::{Imported, State};
use super::text::{self, short};
use crate::LockExt;
use crate::agent::backups::Backups;
use crate::agent::gate::Gate;
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// A message older than this when it arrives was sent while the host was away.
const LATE_SECONDS: f64 = 120.0;
/// How long "who should take this?" waits for a button.
const ASK_EXPIRES: f64 = 86400.0;
/// What a revoked person is told when they write.
const REMOVED: &str = "Your access to kith on this computer was removed.";
/// What someone is told when their message waited `sync_hold` and this computer is still out of sync with the cloud.
const CATCHING_UP: &str = "Kith on this computer is catching up with your other computers. Send it again in a minute.";

pub fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

/// What the core is built from.
pub struct Deps {
    pub paths: Paths,
    pub settings: Settings,
    pub gate: Arc<Gate>,
    pub judge: Judge,
    /// The host's record of which backup accounts are out of usage (the one the bots' turns use).
    pub backups: Arc<Backups>,
}

/// The turn a thread's agent is running (or about to run, or between turns).
#[allow(clippy::struct_excessive_bools)] // independent flags the worker and the router set and clear
#[derive(Default)]
pub(super) struct Busy {
    /// The prompt of the current turn.
    pub task: String,
    /// The messages of the current turn, so a correction can run them again.
    pub batch: Vec<String>,
    pub pid: Option<u32>,
    pub exited: bool,
    pub ended: Option<super::adapter::Ended>,
    /// The last message looked unfinished: give the rest time before starting.
    pub hold: bool,
    /// The next batch continues this same work (it was queued while the run waited for Jev).
    pub cont: bool,
    /// The next turn restarts corrected work.
    pub redo: bool,
    /// The adapter's working line.
    pub status: Value,
    /// The turn waits for the gate: its batch is still in the inbox, so new messages join it.
    pub waiting: bool,
    /// Wakes a turn that waits for the gate when it is stopped.
    pub cancel: Arc<tokio::sync::Notify>,
}

impl Busy {
    pub fn running(&self) -> bool {
        self.pid.is_some() && !self.exited
    }

    /// Something Stop can end: a turn waiting for the gate, running, or about to start.
    pub fn stoppable(&self) -> bool {
        // Batch taken, process not started yet: run() kills it at once.
        self.waiting || self.running() || (self.pid.is_none() && !self.task.is_empty())
    }
}

pub(super) struct Inner {
    pub st: State,
    pub path: std::path::PathBuf,
    /// Work key (`<thread>#<agent>`) -> the running turn.
    pub busy: HashMap<String, Busy>,
    /// Work key -> the last user messages, for the router.
    pub history: HashMap<String, Vec<String>>,
    /// Short id -> work key, for Stop buttons.
    pub works: HashMap<String, String>,
}

impl Inner {
    pub fn save(&mut self) {
        if let Err(e) = self.st.save(&self.path) {
            tracing::error!(error = %e, "channels: can't write the state");
        }
    }
}

pub struct Core {
    pub(super) paths: Paths,
    pub settings: Settings,
    pub(super) agents: Agents,
    pub(super) gate: Arc<Gate>,
    pub(super) judge: Judge,
    pub(super) backups: Arc<Backups>,
    pub(super) inner: Mutex<Inner>,
    /// The running adapter of each connection (set when it resumes), so a card's wake can start a worker on it.
    pub(super) ads: Mutex<HashMap<String, Arc<dyn Adapter>>>,
    /// Wake notes handed to a thread (see `wake`) -> the cards they answer, until the turn that carries them has run.
    pub(super) wakes: Mutex<Vec<(String, Vec<String>)>>,
    hinted: Mutex<HashSet<String>>,
    /// What a run gets to ring its person with, given its agent and person (set by the host, see `calls::run_env`).
    pub(super) call_env: std::sync::OnceLock<CallEnv>,
}

pub type CallEnv = Box<dyn Fn(&str, Option<&str>) -> Vec<(String, String)> + Send + Sync>;

impl Core {
    pub fn new(deps: Deps) -> Arc<Self> {
        let path = deps.paths.data.join("state.json");
        let st = State::load(&path);
        Arc::new(Self {
            agents: Agents::new(deps.paths.agent_roots.clone()),
            paths: deps.paths,
            settings: deps.settings,
            gate: deps.gate,
            judge: deps.judge,
            backups: deps.backups,
            inner: Mutex::new(Inner { st, path, busy: HashMap::new(), history: HashMap::new(), works: HashMap::new() }),
            ads: Mutex::new(HashMap::new()),
            wakes: Mutex::new(Vec::new()),
            hinted: Mutex::new(HashSet::new()),
            call_env: std::sync::OnceLock::new(),
        })
    }

    pub(super) fn with<R>(&self, f: impl FnOnce(&mut Inner) -> R) -> R {
        f(&mut self.inner.locked())
    }

    // ---- helpers for adapters ----

    /// The agents on this computer.
    pub fn agents(&self) -> Vec<String> {
        self.agents.list()
    }

    /// The name an agent posts under; anything that isn't an agent stays as it is.
    pub fn display_name(&self, agent: &str) -> String {
        self.agents.display_name(agent)
    }

    pub fn description(&self, agent: &str) -> String {
        self.agents.description(agent)
    }

    /// Telegram commands: lowercase letters, digits and `_`.
    pub fn cmd_name(agent: &str) -> String {
        hq::cmd_name(agent)
    }

    pub fn find_agent(&self, word: &str) -> Option<String> {
        self.agents.find(word)
    }

    /// Pieces of at most `n` characters, cut at line ends, with a code fence cut in two closed and reopened.
    pub fn chunks(text: &str, n: usize) -> Vec<String> {
        text::chunks(text, n)
    }

    /// Where a connection's downloaded attachments go: one folder per connection.
    pub fn attachments_dir(&self, conn: &str) -> std::path::PathBuf {
        self.attachments_root().join(conn.replace('/', "_"))
    }

    /// All connections' attachments: what the daily prune sweeps.
    pub fn attachments_root(&self) -> std::path::PathBuf {
        self.paths.data.join("attachments")
    }

    /// Whether a thread of this key exists.
    pub fn has_thread(&self, key: &str) -> bool {
        self.with(|i| i.st.threads.contains_key(key))
    }

    /// The Claude sessions behind a work key (`<thread>#<agent>`), for ending what they delegated.
    pub fn thread_sessions(&self, wk: &str) -> Vec<String> {
        self.with(|i| i.st.sessions_of(wk))
    }

    /// A room's default agent: None when the room has none set (an agent's own channel then defaults to it).
    pub fn room_default(&self, room: &str) -> Option<String> {
        self.with(|i| i.st.rooms.get(room)?.get("default")?.as_str().map(str::to_owned))
    }

    /// The rooms of a connection: (room key `<conn>:<chat>`, its settings).
    pub fn rooms(&self, conn: &str) -> Vec<(String, Value)> {
        let prefix = format!("{conn}:");
        self.with(|i| {
            i.st.rooms.iter().filter(|(k, _)| k.starts_with(&prefix)).map(|(k, v)| (k.clone(), v.clone())).collect()
        })
    }

    /// The connection's own state (offsets, webhooks).
    pub fn conn_state(&self, conn: &str) -> Value {
        self.with(|i| i.st.conns.get(conn).cloned().unwrap_or_else(|| json!({})))
    }

    /// Changes the connection's own state and saves it.
    pub fn update_conn<R>(&self, conn: &str, f: impl FnOnce(&mut Value) -> R) -> R {
        self.with(|i| {
            let r = f(i.st.conns.entry(conn).or_insert_with(|| json!({})));
            i.save();
            r
        })
    }

    /// A message of a thread (Telegram has no threads: a reply to one of these continues it).
    pub fn link_reply(&self, conn: &str, chat: &str, msg_id: &str, key: &str) {
        self.with(|i| {
            i.st.replies.insert(format!("{conn}:{chat}:{msg_id}"), json!(key));
        });
    }

    pub fn replied_thread(&self, conn: &str, chat: &str, msg_id: &str) -> Option<String> {
        self.with(|i| i.st.replies.get(&format!("{conn}:{chat}:{msg_id}")).and_then(|v| v.as_str().map(str::to_owned)))
    }

    /// The first time a connection starts here, its threads, sessions, rooms and queued messages come over from
    /// the bridge's state (read only: the bridge keeps its file for the connections it still runs).
    pub fn import_bridge(&self, conn: &str) -> Option<Imported> {
        if self.with(|i| i.st.conns.contains_key(conn)) {
            return None;
        }
        let bridge =
            std::fs::read(self.paths.bridge_state()).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok());
        let imported = self.with(|i| {
            let n = i.st.import_connection(&bridge.unwrap_or(Value::Null), conn);
            i.save();
            n
        });
        tracing::info!(
            conn,
            rooms = imported.rooms,
            threads = imported.threads,
            replies = imported.replies,
            inbox = imported.inbox,
            pending = imported.pending,
            "channels: imported a connection's state from the bridge"
        );
        Some(imported)
    }

    // ---- people ----

    /// The sender -> (person id, who) or (None, who) when hq doesn't know them.
    async fn person(&self, ad: &Arc<dyn Adapter>, m: &Incoming) -> (Option<String>, Who) {
        let mut w = hq::who(&self.paths, ad.platform(), &m.uid).await;
        if w.person.is_none()
            && m.admin
            && let Some(owner) = w.owner_id.clone()
            && !w.owner_linked
        {
            // The server's owner, until hq knows their account on this platform.
            if self.hinted.locked().insert(ad.conn().to_owned()) {
                tracing::info!(
                    "{}: taking the server owner {} as {owner} (kith auth handle {owner} {} {} to say so)",
                    ad.conn(),
                    m.uid,
                    ad.platform(),
                    m.uid
                );
            }
            w.person = Some(owner);
            w.owner = true;
        }
        (w.person.clone(), w)
    }

    /// Whether hq knows the sender (the check `handle` makes before anything else): an adapter downloads a
    /// message's files only for these, so a stranger can't make this Mac fetch anything.
    pub async fn knows_sender(&self, platform: &str, uid: &str, admin: bool) -> bool {
        let w = hq::who(&self.paths, platform, uid).await;
        w.person.is_some() || (admin && w.owner_id.is_some() && !w.owner_linked)
    }

    async fn usable(&self, person: &str) -> Vec<String> {
        let checks = self.agents.list().into_iter().map(|a| async move {
            let allowed = hq::resolve(&self.paths, person, &a).await.allowed;
            (a, allowed)
        });
        futures::future::join_all(checks).await.into_iter().filter_map(|(a, ok)| ok.then_some(a)).collect()
    }

    /// Whether the sender is the person the thread belongs to (also under an id they had before a rename).
    pub(super) fn owns(th: &Value, w: &Who) -> bool {
        match th.get("person").and_then(Value::as_str).filter(|p| !p.is_empty()) {
            Some(p) => w.is(p),
            None => w.owner,
        }
    }

    // ---- a message ----

    pub async fn handle(self: &Arc<Self>, ad: &Arc<dyn Adapter>, m: &Incoming) -> Result<()> {
        let text = m.text.as_str();
        let (name, arg) = text::command(text).unwrap_or_default();
        if name == "link"
            && !arg.is_empty()
            && arg.len() <= 12
            && arg.len() >= 4
            && arg.chars().all(|c| c.is_ascii_alphanumeric())
        {
            // A one-time code from hq-auth invite.
            let linked = hq::link(&self.paths, ad.platform(), &m.uid, &arg).await;
            tracing::info!("{} link {}: {}", ad.conn(), m.uid, if linked.is_ok() { "ok" } else { "refused" });
            let said = match linked {
                Ok(person) => format!("Linked: you are {person} in kith."),
                Err(why) if why.is_empty() => "That code did not work.".into(),
                Err(why) => why,
            };
            return ad.reply(m, &said).await;
        }
        let (person, w) = self.person(ad, m).await;
        if person.is_some() && w.held && !w.owner && !w.revoked {
            // A revoke made on another computer may not have reached this one yet (revoke-person R8): the message
            // waits for the sync in the background, as the adapter's loop can't.
            let (core, ad, m) = (self.clone(), ad.clone(), m.clone());
            tokio::spawn(async move { core.held(&ad, &m).await });
            return Ok(());
        }
        self.take(ad, m, person, &w).await
    }

    /// Takes the message once this computer is in sync with the Kith cloud again (`who` is asked every `sync_poll`);
    /// still out of sync after `sync_hold`, the sender is asked to send it again and nothing runs.
    async fn held(self: &Arc<Self>, ad: &Arc<dyn Adapter>, m: &Incoming) {
        let end = Instant::now() + self.settings.sync_hold;
        while Instant::now() < end {
            tokio::time::sleep(self.settings.sync_poll).await;
            let (person, w) = self.person(ad, m).await;
            if !w.held {
                if let Err(e) = self.take(ad, m, person, &w).await {
                    tracing::warn!("{}: message failed: {e:#}", ad.conn());
                }
                return;
            }
        }
        tracing::info!("{}: held a message from {}: still out of sync with the Kith cloud", ad.conn(), m.uid);
        let known = m.thread.as_ref().is_some_and(|t| self.has_thread(t));
        if (m.private || m.addressed || known)
            && let Err(e) = ad.reply(m, CATCHING_UP).await
        {
            tracing::warn!("{}: message failed: {e:#}", ad.conn());
        }
    }

    /// A message whose sender `who` answered for in sync with the Kith cloud (or for the owner): one not linked, revoked
    /// or in someone else's thread is answered so; the rest goes on to an agent.
    async fn take(
        self: &Arc<Self>,
        ad: &Arc<dyn Adapter>,
        m: &Incoming,
        person: Option<String>,
        w: &Who,
    ) -> Result<()> {
        let text = m.text.as_str();
        let (name, arg) = text::command(text).unwrap_or_default();
        let key = m.thread.clone().filter(|t| self.with(|i| i.st.threads.contains_key(t)));
        let th = key.as_ref().and_then(|k| self.with(|i| i.st.threads.get(k).cloned()));
        let Some(person) = person else {
            if m.private || m.addressed || th.is_some() {
                ad.reply(
                    m,
                    "This account is not linked to kith yet. Ask the owner for a code, then send /link CODE here.",
                )
                .await?;
            }
            return Ok(());
        };
        if w.revoked {
            // Their handle is kept so they hear why; nothing else happens.
            if m.private || m.addressed || th.is_some() {
                ad.reply(m, REMOVED).await?;
            }
            return Ok(());
        }
        if let Some(th) = &th
            && !Self::owns(th, w)
        {
            // Its sessions hold what the agents saw for them.
            let owner = th.get("person").and_then(Value::as_str).filter(|p| !p.is_empty()).unwrap_or("the owner");
            return ad.reply(m, &format!("This thread is {owner}'s. Start your own outside it.")).await;
        }
        let default = self.with(|i| {
            let dm_default = self.settings.dm_default.clone();
            let room = i.st.rooms.entry(m.room.as_str()).or_insert_with(|| json!({}));
            if m.private && room.get("default").is_none() {
                // A new DM: the owner's starts with the dm_default setting.
                room["default"] = json!(if w.owner { dm_default } else { String::new() });
                i.save();
            }
            let room = &i.st.rooms[m.room.as_str()];
            let set = room.get("default").and_then(Value::as_str);
            set.map_or_else(|| m.room_agent.clone().unwrap_or_default(), str::to_owned)
        });
        match name.as_str() {
            "start" | "whoami" | "help" => {
                let mut said = format!("You are {person} in kith.");
                if !default.is_empty() {
                    let _ = write!(said, " Messages without @agent here go to {default}.");
                }
                said += " Write @agent to pick one; /agents lists yours, /default sets this room's.";
                return ad.reply(m, &said).await;
            }
            "agents" => {
                let mine = self.usable(&person).await;
                let list = if mine.is_empty() { "none yet".to_owned() } else { mine.join(", ") };
                return ad
                    .reply(m, &format!("Yours: {list}\nWrite @agent, or answer in a thread to go on with it."))
                    .await;
            }
            "default" => return self.set_default(ad, m, &person, &arg, &default).await,
            "stop" => {
                let wk = key
                    .as_ref()
                    .zip(th.as_ref())
                    .map(|(k, t)| format!("{k}#{}", t.get("last").and_then(Value::as_str).unwrap_or("")));
                let active = wk.as_ref().filter(|wk| {
                    self.with(|i| i.busy.contains_key(*wk) || i.st.inbox.get(*wk).is_some_and(|q| !q.is_empty()))
                });
                return match active {
                    Some(wk) => self.stop(ad, wk, Some(m)).await,
                    None => ad.reply(m, "Nothing is running here.").await,
                };
            }
            _ => {}
        }
        let mut task = text.to_owned();
        let leading = text::lead(text).and_then(|(word, rest)| Some((self.agents.find(&word)?, rest)));
        let agent = if let Some((a, rest)) = leading {
            if !rest.trim().is_empty() {
                rest.trim().clone_into(&mut task);
            }
            Some(a)
        } else {
            text::mentions(text).iter().find_map(|x| self.agents.find(x))
        };
        if !m.quoted.is_empty() {
            task = format!("(Replying to: \"{}\")\n\n{task}", text::first_chars(&m.quoted, 1500));
        }
        if !m.files.is_empty() {
            let _ = write!(task, "\n\nAttached files: {}", m.files.join(", "));
        }
        if let Some(sent) = m.sent.filter(|s| now() - s > LATE_SECONDS) {
            task = text::late_note(sent) + &task;
        }
        let last = th.as_ref().and_then(|t| t.get("last")).and_then(Value::as_str).filter(|l| !l.is_empty());
        let agent = agent.or_else(|| last.map(str::to_owned)).or_else(|| Some(default).filter(|d| !d.is_empty()));
        let Some(agent) = agent else {
            if !(m.addressed || m.private) {
                return Ok(()); // talk in a room with no default that isn't for hq
            }
            let mine = self.usable(&person).await;
            if mine.is_empty() {
                return ad.reply(m, "You have no agents yet. Ask the owner to add you.").await;
            }
            let pid = short(&format!("{}:{}:{}", m.room, m.origin, now()));
            self.with(|i| {
                i.st.pending.insert(
                    pid.clone(),
                    json!({"task": task, "text": text, "person": person, "room": m.room, "origin": m.origin,
                           "thread": key, "private": m.private, "at": now()}),
                );
                i.st.pending.retain(|_, v| v.get("at").and_then(Value::as_f64).unwrap_or(0.0) >= now() - ASK_EXPIRES);
                i.save();
            });
            return ad.ask(m, &pid, &mine).await;
        };
        self.begin(ad, m, &person, &agent, task, key).await
    }

    async fn set_default(
        &self,
        ad: &Arc<dyn Adapter>,
        m: &Incoming,
        person: &str,
        arg: &str,
        default: &str,
    ) -> Result<()> {
        if arg.is_empty() {
            let said = if default.is_empty() {
                "No default here: I ask who should take each message. /default <agent> sets one.".to_owned()
            } else {
                format!("Messages without @agent here go to {default}.")
            };
            return ad.reply(m, &said).await;
        }
        let new = if arg == "none" || arg == "-" {
            String::new()
        } else {
            let agent = self.agents.find(arg);
            let allowed = match &agent {
                Some(a) => hq::resolve(&self.paths, person, a).await.allowed,
                None => false,
            };
            match agent.filter(|_| allowed) {
                Some(a) => a,
                None => return ad.reply(m, &format!("No agent of yours called '{arg}'. /agents lists them.")).await,
            }
        };
        self.with(|i| {
            i.st.rooms.entry(m.room.as_str()).or_insert_with(|| json!({}))["default"] = json!(new);
            i.save();
        });
        let said = if new.is_empty() {
            "No default: I'll ask who should take each message.".to_owned()
        } else {
            format!("Messages without @agent here now go to {new}.")
        };
        ad.reply(m, &said).await
    }

    /// A button under "Who should take this?" -> false when it isn't this person's question (or it's gone).
    pub async fn picked(self: &Arc<Self>, ad: &Arc<dyn Adapter>, pid: &str, agent: &str, uid: &str) -> Result<bool> {
        let w = hq::who(&self.paths, ad.platform(), uid).await;
        let text = |p: &Value, k: &str| p.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
        // Looking and taking are one step: of two presses at once, only one gets the question.
        let claimed = self.with(|i| {
            if !i.st.pending.get(pid).is_some_and(|p| w.is(&text(p, "person"))) {
                return None;
            }
            let pend = i.st.pending.remove(pid);
            i.save();
            pend
        });
        let Some(pend) = claimed else { return Ok(false) };
        let thread = pend.get("thread").and_then(Value::as_str).map(str::to_owned);
        let m = Incoming {
            uid: uid.to_owned(),
            text: text(&pend, "text"),
            room: text(&pend, "room"),
            origin: pend.get("origin").cloned().unwrap_or(Value::Null),
            thread: thread.clone(),
            private: pend.get("private").and_then(Value::as_bool).unwrap_or(false),
            addressed: true,
            ..Incoming::default()
        };
        let person = w.person.clone().unwrap_or_default();
        self.begin(ad, &m, &person, agent, text(&pend, "task"), thread).await?;
        Ok(true)
    }

    /// `kith-auth resolve` for person and agent, answered in sync with the Kith cloud (`hq::resolve_in_sync`, waiting
    /// up to `sync_hold`).
    pub(super) async fn access(&self, person: &str, agent: &str) -> Access {
        hq::resolve_in_sync(&self.paths, person, agent, self.settings.sync_hold, self.settings.sync_poll).await
    }

    async fn begin(
        self: &Arc<Self>,
        ad: &Arc<dyn Adapter>,
        m: &Incoming,
        person: &str,
        agent: &str,
        task: String,
        key: Option<String>,
    ) -> Result<()> {
        let access: Access = hq::resolve(&self.paths, person, agent).await;
        if !access.allowed {
            return ad
                .reply(m, &format!("{}. Ask the owner to add you (kith auth member {agent} {person}).", access.why))
                .await;
        }
        let known = key.filter(|k| self.with(|i| i.st.threads.contains_key(k)));
        let key = if let Some(key) = known {
            let conv = self.with(|i| i.st.threads[&key].get("conv").cloned().unwrap_or(Value::Null));
            let conv = ad.follow(&conv, m);
            self.with(|i| i.st.threads[&key]["conv"] = conv);
            key
        } else {
            let (key, mut conv) = ad.open_thread(m, agent, &text::title(&m.text)).await?;
            conv["key"] = json!(key);
            self.with(|i| {
                let th =
                    i.st.threads
                        .entry(key.as_str())
                        .or_insert_with(|| json!({"person": person, "owner": agent, "room": m.room, "sessions": {}}));
                th["conv"] = conv;
            });
            key
        };
        self.with(|i| {
            if let Some(mut th) = i.st.threads.shift_remove(&key) {
                th["last"] = json!(agent);
                // Whether the chat is a one-to-one, for the room a run is told it is in (see `run::run_room`).
                th["private"] = json!(m.private);
                th["at"] = json!(now());
                i.st.threads.insert(key.clone(), th); // newest last, for pruning
            }
            i.save();
        });
        self.deliver(ad, &key, agent, m, task).await;
        Ok(())
    }

    pub(super) fn conv_of(&self, key: &str) -> Value {
        self.with(|i| i.st.threads.get(key).and_then(|t| t.get("conv").cloned()).unwrap_or(Value::Null))
    }

    /// A message from `agent` to `person` (`None` is the owner) outside any turn: posted in their newest one-to-one
    /// thread with that agent on a connection that is up, never in a room. Whether it was posted.
    pub async fn tell(&self, person: Option<&str>, agent: &str, text: &str) -> bool {
        let person = match person {
            Some(p) => p.to_owned(),
            None => match hq::who(&self.paths, "kith", "-").await.owner_id {
                Some(owner) => owner,
                None => return false,
            },
        };
        let ads = self.ads.locked().clone();
        let found = self.with(|i| {
            i.st.threads.iter().rev().find_map(|(key, th)| {
                let theirs = th.get("person").and_then(Value::as_str) == Some(person.as_str())
                    && th.get("private").and_then(Value::as_bool) == Some(true)
                    && (th.get("last").and_then(Value::as_str) == Some(agent) || th["sessions"].get(agent).is_some());
                let ad = ads.iter().find(|(conn, _)| key.starts_with(&format!("{conn}:")))?.1;
                theirs.then(|| (ad.clone(), th.get("conv").cloned().unwrap_or(Value::Null)))
            })
        });
        let Some((ad, conv)) = found else { return false };
        if !self.access(&person, agent).await.allowed {
            return false;
        }
        if let Err(e) = ad.post(&conv, agent, text, None).await {
            tracing::warn!("{}: a message to {person} failed: {e:#}", ad.conn());
            return false;
        }
        true
    }

    /// Forgets a connection's running adapter when its grant is revoked.
    pub fn forget(&self, conn: &str) {
        self.ads.locked().remove(conn);
    }
}
