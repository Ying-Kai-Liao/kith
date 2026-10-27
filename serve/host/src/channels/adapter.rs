//! What the core and a platform adapter pass each other.
//!
//! # Writing an adapter (Discord, Telegram, Slack)
//!
//! An adapter is one connection: one bot on one platform, named `<platform>/<profile>` (`telegram/hq`). It
//! implements [`Adapter`], is built by a constructor registered in `channels::registry` (the constructor gets the
//! connection name, its [`Secrets`] and the [`Core`]), and is run by the supervisor: [`Adapter::start`] connects
//! and receives until the connection drops (return or error; the supervisor starts it again after a pause), and
//! must call [`Alive::touch`] whenever it hears from the platform so that [`Adapter::quiet`] can notice a stuck one.
//!
//! The adapter turns each message into an [`Incoming`] and calls [`Core::handle`]; the core decides who answers
//! and calls the adapter back to post, show the working line, ask and react. An adapter never decides who may
//! speak to which agent, never keeps a queue and never starts a run. Also call, as the Python bridge's adapters
//! did: [`Core::resume`] once connected (messages that waited when the host last stopped, live agents' answers),
//! [`Core::picked`] when a "Who should take this?" button is pressed, [`Core::stop_pressed`] for the Stop button,
//! and [`Core::link_reply`] / [`Core::replied_thread`] on platforms without threads. The connection's own state
//! (offsets, webhooks) lives in [`Core::conn_state`] / [`Core::update_conn`]. Secrets never go into logs or
//! `Debug` output: [`Secrets`] prints nothing.

use super::core::Core;
use anyhow::Result;
use futures::future::BoxFuture;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// One message, as the core needs it.
#[derive(Clone, Debug, Default)]
pub struct Incoming {
    /// The sender's platform user id (numeric, or Slack's U0123ABC).
    pub uid: String,
    /// With the bot's own mention taken out and agent mentions as `@agent`.
    pub text: String,
    /// `<connection>:<chat>`.
    pub room: String,
    /// Where the message is, for the adapter (`open_thread`, `ask`); JSON-safe.
    pub origin: Value,
    /// The thread key when it came inside a thread (ours or not).
    pub thread: Option<String>,
    /// A direct message with the bot.
    pub private: bool,
    /// The bot or an agent was mentioned, or the bot's message replied to.
    pub addressed: bool,
    /// The agent whose own room this is (Discord's agent channels).
    pub room_agent: Option<String>,
    /// The owner of the server `channels.discord_guild` names (Discord), taken as hq's owner until hq knows their
    /// account. Any other server's owner is not.
    pub admin: bool,
    /// The text of a message replied to that isn't hq's.
    pub quoted: String,
    /// Local paths of the files sent with it.
    pub files: Vec<String>,
    /// The adapter's own handle on the message (for reactions).
    pub reference: Value,
    /// When it was sent (unix seconds), so one that waited for the host is marked late.
    pub sent: Option<f64>,
}

/// What a reaction on a message says about what the core did with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reaction {
    /// The message stopped the running work.
    Stop,
    /// The message corrected the running work, which starts again.
    Redo,
}

/// How a turn ended, as `status_end` is told.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ended {
    Done,
    Stopped,
    Interrupted,
}

impl Ended {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Stopped => "stopped",
            Self::Interrupted => "interrupted",
        }
    }
}

/// What the Stop button answers with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopPress {
    Stopping,
    Finished,
    NotYours,
}

impl StopPress {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stopping => "stopping",
            Self::Finished => "finished",
            Self::NotYours => "not yours",
        }
    }
}

/// A connection's secrets by variable name (`TELEGRAM_BOT_TOKEN`). Debug prints only the names.
#[derive(Clone, Default)]
pub struct Secrets(pub BTreeMap<String, String>);

impl Secrets {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }
}

impl std::fmt::Debug for Secrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.0.keys()).finish()
    }
}

/// When the platform last answered. [`Adapter::start`] touches it; the supervisor restarts a connection that
/// has been silent for longer than [`Adapter::quiet`].
#[derive(Debug)]
pub struct Alive(AtomicU64);

fn epoch() -> Instant {
    static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

#[allow(clippy::cast_possible_truncation)]
fn now_ms() -> u64 {
    epoch().elapsed().as_millis() as u64
}

impl Default for Alive {
    fn default() -> Self {
        Self(AtomicU64::new(now_ms()))
    }
}

impl Alive {
    pub fn touch(&self) {
        self.0.store(now_ms(), Ordering::Relaxed);
    }

    pub fn silent_for(&self) -> Duration {
        Duration::from_millis(now_ms().saturating_sub(self.0.load(Ordering::Relaxed)))
    }
}

pub type Fut<'a, T> = BoxFuture<'a, T>;

/// One connection (a bot on a platform). The core never touches the platform; the adapter never touches the queue.
pub trait Adapter: Send + Sync {
    /// `discord`, `telegram`, `slack`: what `hq-auth who` is asked about.
    fn platform(&self) -> &str;
    /// The connection name, `<platform>/<profile>`; rooms and threads are keyed `<conn>:<…>`.
    fn conn(&self) -> &str;
    /// Added to every task: where the answer goes and how to send files.
    #[allow(clippy::unnecessary_literal_bound)] // adapters return text of their own
    fn footer(&self) -> &str {
        ""
    }
    /// A short line under a reply that says something about it (a backup account answered): italics, which all
    /// the platforms read.
    fn note_line(&self, text: &str) -> String {
        format!("_{text}_")
    }
    /// Seconds without a sign of life after which the connection is restarted.
    fn quiet(&self) -> Option<Duration> {
        None
    }
    fn alive(&self) -> &Alive;

    /// Connects and receives; returns or fails when the connection drops. It gets its own `Arc` so that it can
    /// hand itself to [`Core::handle`] and the like (they take `&Arc<dyn Adapter>`): `let me: Arc<dyn Adapter> = self;`.
    fn start(self: Arc<Self>) -> Fut<'static, Result<()>>;
    /// A short note from hq itself, answering `m`.
    fn reply<'a>(&'a self, m: &'a Incoming, text: &'a str) -> Fut<'a, Result<()>>;
    /// Where a new task's messages go: (thread key, conv). The core keeps conv (JSON) with the thread and adds `key`.
    fn open_thread<'a>(&'a self, m: &'a Incoming, agent: &'a str, title: &'a str) -> Fut<'a, Result<(String, Value)>>;
    /// A new message in a thread: where the answer to it goes.
    fn follow(&self, conv: &Value, _m: &Incoming) -> Value {
        conv.clone()
    }
    /// The agent's answer (Markdown); `ctx_pct`: how full its context is, in percent, when worth showing.
    fn post<'a>(&'a self, conv: &'a Value, agent: &'a str, text: &'a str, ctx_pct: Option<f64>) -> Fut<'a, Result<()>>;
    fn send_files<'a>(
        &'a self,
        conv: &'a Value,
        agent: &'a str,
        files: &'a [String],
        caption: &'a str,
    ) -> Fut<'a, Result<()>>;
    /// The small "working…" line with a Stop button (`wid` names the work for [`Core::stop_pressed`]) -> a handle.
    fn status<'a>(&'a self, _conv: &'a Value, _agent: &'a str, _wid: &'a str) -> Fut<'a, Result<Value>> {
        Box::pin(async { Ok(Value::Null) })
    }
    /// Changes the working line (queued count, restarting).
    fn status_text<'a>(&'a self, _handle: &'a Value, _text: &'a str) -> Fut<'a, ()> {
        Box::pin(async {})
    }
    /// Done: remove the line; stopped: say so. `handle` is null when no line was shown.
    fn status_end<'a>(&'a self, _conv: &'a Value, _agent: &'a str, _handle: &'a Value, _ended: Ended) -> Fut<'a, ()> {
        Box::pin(async {})
    }
    fn react<'a>(&'a self, _m: &'a Incoming, _what: Reaction) -> Fut<'a, ()> {
        Box::pin(async {})
    }
    /// "Who should take this?" with a button per agent; a press goes to [`Core::picked`] with `pid`.
    fn ask<'a>(&'a self, m: &'a Incoming, pid: &'a str, agents: &'a [String]) -> Fut<'a, Result<()>>;
    /// Shows that the agent is working, until this future is dropped.
    fn typing<'a>(&'a self, _conv: &'a Value) -> Fut<'a, ()> {
        Box::pin(std::future::pending())
    }
    /// Extra environment for the run (Discord: the thread's webhook).
    fn env(&self, _conv: &Value, _agent: &str) -> Vec<(String, String)> {
        Vec::new()
    }
}

/// Builds the adapter of one connection.
pub type Constructor = Arc<dyn Fn(&str, Secrets, Arc<Core>) -> Result<Arc<dyn Adapter>> + Send + Sync>;
