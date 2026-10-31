//! Row and wire types: bot configs, lanes, entries and their kinds.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// The bot's permission mode: one of Claude Code's own, passed to the agent unchanged. Bots saved before the modes
/// were Claude's own hold `ask` (read as `default`) or `auto`.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Permission {
    #[serde(rename = "default", alias = "ask")]
    Ask,
    #[serde(rename = "acceptEdits")]
    AcceptEdits,
    #[serde(rename = "plan")]
    Plan,
    /// New bots start here.
    #[default]
    /// A stored `dontAsk` (no longer offered) reads as auto.
    #[serde(rename = "auto", alias = "dontAsk")]
    Auto,
    #[serde(rename = "bypassPermissions")]
    BypassPermissions,
}

impl Permission {
    /// The mode id the agent knows it by.
    pub fn mode_id(self) -> &'static str {
        match self {
            Self::Ask => "default",
            Self::AcceptEdits => "acceptEdits",
            Self::Plan => "plan",
            Self::Auto => "auto",
            Self::BypassPermissions => "bypassPermissions",
        }
    }
}

/// Transcript entry kinds (the `kind` column / wire field).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    User,
    Agent,
    Thought,
    Tool,
    Plan,
    Permission,
    Notice,
}

impl EntryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Agent => "agent",
            Self::Thought => "thought",
            Self::Tool => "tool",
            Self::Plan => "plan",
            Self::Permission => "permission",
            Self::Notice => "notice",
        }
    }
}

/// A roster row is an agent bot or a group chat of bots (wire values `agent` / `group`).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BotKind {
    #[default]
    Agent,
    /// Several bots and the user in one conversation; it has no agent of its own (see `group`).
    Group,
}

/// How many labels a room carries and how long each is.
pub const MAX_LABELS: usize = 5;
pub const MAX_LABEL_CHARS: usize = 24;

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[expect(clippy::struct_excessive_bools, reason = "independent user settings stored as plain JSON flags")]
pub struct BotConfig {
    pub id: String,
    #[serde(default)]
    pub kind: BotKind,
    /// A group's bots, in the order they were added.
    #[serde(default)]
    pub members: Vec<String>,
    /// A group's purpose labels (filters in the room list, not folders): lowercase, trimmed, at most
    /// [`MAX_LABELS`] of [`MAX_LABEL_CHARS`] characters. Stored in the bot's JSON, so rows from before have none.
    #[serde(default, skip_serializing_if = "Vec::is_empty", deserialize_with = "null_as_empty")]
    pub labels: Vec<String>,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_color")]
    pub avatar_color: String,
    /// Grok-Bot-style character shape: blob | pebble | squircle | tablet | wedge | hex | cloud | teardrop.
    #[serde(default = "default_shape")]
    pub avatar_shape: String,
    /// Where the avatar comes from: `auto` (a crew portrait matching the name, else the default),
    /// `crew:<name>`, `spare:<name>`, or `shape` (draw `avatar_shape` in `avatar_color`).
    #[serde(default = "default_avatar_source")]
    pub avatar_source: String,
    /// Empty for a group.
    #[serde(default)]
    pub backend: String,
    #[serde(default)]
    pub command: Option<String>,
    /// Empty for a group.
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub permission: Permission,
    #[serde(default)]
    pub model: Option<String>,
    /// The model's own settings (effort, fast…): ACP config option id → value.
    #[serde(default)]
    pub model_options: BTreeMap<String, String>,
    /// Ask the agent for its Fast mode (Claude Code, Codex): faster replies for more usage.
    #[serde(default)]
    pub fast_mode: bool,
    #[serde(default)]
    pub pinned: bool,
    /// Place in the roster, lowest first (after pinned bots). All 0 until the user drags one:
    /// ties sort by recent activity (see `Hub::reorder_bots`).
    #[serde(default)]
    pub position: i64,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub notify: Option<bool>,
    /// Connector ids (MCP servers) handed to the agent; see `market`.
    #[serde(default)]
    pub connectors: Vec<String>,
    /// Skill ids the bot is told about; see `market`.
    #[serde(default)]
    pub skills: Vec<String>,
    /// The bot gets the built-in `computer` MCP server (see `screen`).
    #[serde(default)]
    pub computer: bool,
    /// Uses the memory shared by all bots ("yours"). Unset: on for a bot in its own workspace, off for one in a
    /// project folder (see `chat::memory::shared`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_memory: Option<bool>,
    /// Keeps the harness's own memory (Claude Code's auto memory…) on beside Kith's. Off unless the user turns it
    /// on, and only where Kith can turn it off (see `agent::own_memory::kept`). A client sending back the bot as
    /// it got it sends `null` where there is no switch.
    #[serde(default, deserialize_with = "null_as_false")]
    pub harness_memory: bool,
    /// The thread rows in this bot's room show the thread's summary instead of its first message (the starting point
    /// may just be a "hi"). Off unless the owner turns it on; the apps read the summary from the row (`data.thread`).
    #[serde(default, deserialize_with = "null_as_false")]
    pub row_shows_summary: bool,
    /// Created without a name: the host names it from its first conversations (see `naming`)
    /// until the user renames it.
    #[serde(default)]
    pub auto_name: bool,
    /// Each burst of the user's chat is a task of its own (see `chat::tasks`). Unset: on. Off keeps one endless
    /// conversation in the bot's main session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tasks: Option<bool>,
    /// The hq agent this bot runs (its folder name under `hq ls`). Set: the bot starts from `hq <agent> --launch-json`
    /// with the agent's own prompt, memory, skills and servers, and none of Kith's (see `agent::hq`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hq_agent: Option<String>,
    /// The hq project the bot's places work in unless a place picked its own (see `agent::folder`). Beats `cwd`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// The invited person whose chat this is (a kith-auth person id); None is the owner's. Kept in the config JSON
    /// rather than a column: it is set once when the chat starts, no query needs it, and old rows already mean the
    /// owner. Only the host sets it (see `api::people::start_chat`); a client can't edit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub person: Option<String>,
    #[serde(default)]
    pub created_at: i64,
}

impl BotConfig {
    /// An avatar source the apps can't read becomes `auto`; image names are theirs to resolve
    /// (an unknown one falls back to `auto` there too).
    pub fn clean_avatar_source(&mut self) {
        let ok = match self.avatar_source.split_once(':') {
            None => matches!(self.avatar_source.as_str(), "auto" | "shape"),
            Some((kind, name)) => {
                matches!(kind, "crew" | "spare")
                    && !name.is_empty()
                    && name.len() <= 32
                    && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            }
        };
        if !ok {
            self.avatar_source = default_avatar_source();
        }
    }

    /// Normalizes `labels` (lowercase, trimmed, no blanks or repeats) and refuses what a room list couldn't show.
    pub fn clean_labels(&mut self) -> anyhow::Result<()> {
        let mut clean: Vec<String> = Vec::new();
        for l in &self.labels {
            let l = l.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
            if l.is_empty() || clean.contains(&l) {
                continue;
            }
            if l.chars().count() > MAX_LABEL_CHARS {
                anyhow::bail!("a label is at most {MAX_LABEL_CHARS} characters");
            }
            clean.push(l);
        }
        if clean.len() > MAX_LABELS {
            anyhow::bail!("a room has at most {MAX_LABELS} labels");
        }
        if !clean.is_empty() && !self.is_group() {
            anyhow::bail!("only a room has labels");
        }
        self.labels = clean;
        Ok(())
    }

    /// An empty default project is none.
    pub fn clean_project(&mut self) {
        self.project = self.project.take().map(|p| p.trim().to_owned()).filter(|p| !p.is_empty());
    }

    /// Whether the bot is an hq agent.
    pub fn is_hq(&self) -> bool {
        self.hq_agent.is_some()
    }

    pub fn is_group(&self) -> bool {
        self.kind == BotKind::Group
    }

    /// Whether the user's messages to this bot are tasks (a group's never are: it's one room).
    pub fn uses_tasks(&self) -> bool {
        !self.is_group() && self.tasks != Some(false)
    }
}

fn null_as_empty<'de, D: serde::Deserializer<'de>>(value: D) -> Result<Vec<String>, D::Error> {
    Ok(Option::<Vec<String>>::deserialize(value)?.unwrap_or_default())
}

fn null_as_false<'de, D: serde::Deserializer<'de>>(value: D) -> Result<bool, D::Error> {
    Ok(Option::<bool>::deserialize(value)?.unwrap_or(false))
}

fn default_color() -> String {
    "blue".into()
}
fn default_shape() -> String {
    "blob".into()
}
fn default_avatar_source() -> String {
    "auto".into()
}

/// kv key for per-bot state in one lane (`what`: `session`, `seen`).
pub fn lane_key(what: &str, bot_id: &str, lane: &Lane) -> String {
    format!("lane.{what}.{bot_id}@{}", lane.key())
}

/// What the user has read (`markRead`): the main chat, one thread, or all of a chat.
#[derive(Clone, Copy, Debug)]
pub enum ReadScope<'a> {
    All,
    Chat,
    Thread(&'a str),
}

/// Where an entry lives: a bot's or group's chat (`bot_id`), and optionally a thread in it,
/// named by its root entry. Every lane a bot talks in has its own ACP session.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Lane {
    pub chat: String,
    pub thread: Option<String>,
}

impl Lane {
    pub fn main(chat: &str) -> Self {
        Self { chat: chat.to_owned(), thread: None }
    }

    pub fn in_thread(chat: &str, root: &str) -> Self {
        Self { chat: chat.to_owned(), thread: Some(root.to_owned()) }
    }

    /// Stable text form for kv keys: `chat` or `chat/root`.
    pub fn key(&self) -> String {
        match &self.thread {
            Some(t) => format!("{}/{t}", self.chat),
            None => self.chat.clone(),
        }
    }
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub id: String,
    pub seq: i64,
    pub bot_id: String,
    /// The thread's root entry; `None` in the main chat.
    pub thread_id: Option<String>,
    pub rev: i64,
    pub kind: String,
    pub turn: i64,
    pub data: Value,
    pub created_at: i64,
    pub updated_at: i64,
}

/// The one test for "a person wrote this" (rule C1 of the agent-conversations plan): an entry of kind `user` without a
/// bot guest as sender. The owner's own message, an invited person's (`data.person`) and a host-written one
/// (`data.from == "host"`) all count; a bot's ask (`data.sender.botId`) and every non-user entry do not. Anything that
/// opens a conversation's budget, or resets "since the last user message", asks this instead of comparing the kind.
pub fn is_person_message(e: &Entry) -> bool {
    e.kind == EntryKind::User.as_str() && e.data["sender"]["botId"].is_null()
}

pub struct LastMessage {
    pub text: String,
    pub at: i64,
    /// The bot that wrote it; `None` when the user did.
    pub author: Option<String>,
}

pub struct BotRow {
    pub config: BotConfig,
    pub rev: i64,
    pub deleted: bool,
    pub session_id: Option<String>,
    pub read_rev: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(kind: &str, data: Value) -> Entry {
        Entry {
            id: "e".into(),
            seq: 1,
            bot_id: "b".into(),
            thread_id: None,
            rev: 1,
            kind: kind.into(),
            turn: 1,
            data,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn only_a_user_entry_without_a_bot_sender_is_a_person_message() {
        let person = |data: Value| is_person_message(&entry("user", data));
        assert!(person(serde_json::json!({"text": "owner"})));
        assert!(person(serde_json::json!({"text": "invited", "person": "sam"})));
        assert!(person(serde_json::json!({"text": "host", "from": "host"})));
        assert!(!person(serde_json::json!({"text": "bot", "sender": {"botId": "bg1", "name": "Grok"}})));
        assert!(!is_person_message(&entry("agent", serde_json::json!({"text": "reply"}))));
    }

    #[test]
    fn permission_wire_values_round_trip() {
        let p: Permission = serde_json::from_str("\"auto\"").unwrap();
        assert_eq!(p, Permission::Auto);
        assert_eq!(serde_json::to_string(&Permission::Ask).unwrap(), "\"default\"");
        assert_eq!(serde_json::from_str::<Permission>("\"ask\"").unwrap(), Permission::Ask, "old bots read as default");
        assert_eq!(
            serde_json::from_str::<Permission>("\"dontAsk\"").unwrap(),
            Permission::Auto,
            "a stored dontAsk reads as auto"
        );
        for mode in ["default", "acceptEdits", "plan", "auto", "bypassPermissions"] {
            let p: Permission = serde_json::from_str(&format!("\"{mode}\"")).unwrap();
            assert_eq!(p.mode_id(), mode);
            assert_eq!(serde_json::to_string(&p).unwrap(), format!("\"{mode}\""));
        }
        assert!(serde_json::from_str::<Permission>("\"yolo\"").is_err(), "unknown modes are rejected at the boundary");
    }

    #[test]
    fn avatar_source_defaults_to_auto_and_cleans_unknowns() {
        let mut bot: BotConfig = serde_json::from_value(serde_json::json!({"id": "b", "name": "Seo"})).unwrap();
        assert_eq!(bot.avatar_source, "auto", "bots saved before the field existed read as auto");
        for ok in ["auto", "shape", "crew:builder", "spare:gradcap"] {
            bot.avatar_source = ok.into();
            bot.clean_avatar_source();
            assert_eq!(bot.avatar_source, ok);
        }
        for bad in ["", "Crew:builder", "crew:", "spare:../x", "dotted", "crew:a:b"] {
            bot.avatar_source = bad.into();
            bot.clean_avatar_source();
            assert_eq!(bot.avatar_source, "auto", "{bad}");
        }
    }
}
