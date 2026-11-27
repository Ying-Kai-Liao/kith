//! Who prompted a user-side entry: the host stamps `data.prompter = {"kind": K, "name"?: string}` on every
//! `EntryKind::User` entry it writes. The kind comes from the authenticated caller or the code path that writes the
//! entry, never from client JSON. Entries written before this have none: readers treat that as unknown.
//!
//! Kinds: `app` (the owner, from an app), `terminal` (the owner, in a terminal session the host imports), `manager`
//! (another Claude session driving the agent through the CLI), `schedule` (a schedule fired it; name = its id),
//! `agent` (another kith agent; name = its id) and `person` (an invited person or bot guest; name = their id).
//! Only the owner's views carry it: the member and bot-guest shapers drop it ([`strip`]).

use serde_json::{Value, json};

const KEY: &str = "prompter";
/// The longest name kept.
const NAME_MAX: usize = 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    App,
    Terminal,
    Manager,
    Schedule,
    Agent,
    Person,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Terminal => "terminal",
            Self::Manager => "manager",
            Self::Schedule => "schedule",
            Self::Agent => "agent",
            Self::Person => "person",
        }
    }
}

/// The `prompter` value: `name` is left out when empty.
pub fn value(kind: Kind, name: Option<&str>) -> Value {
    let mut v = json!({"kind": kind.as_str()});
    if let Some(name) = name.map(str::trim).filter(|n| !n.is_empty()) {
        v["name"] = name.chars().take(NAME_MAX).collect::<String>().into();
    }
    v
}

/// Sets `data.prompter`, replacing whatever was there (a client never gets to say who prompted).
pub fn stamp(data: &mut Value, kind: Kind, name: Option<&str>) {
    data[KEY] = value(kind, name);
}

/// Who prompted the user lines of a terminal session the host imports, from how the kernel registered it: a terminal
/// is the owner at the keyboard; a one-shot says who started it in its label (`threads::run_label` in the kernel).
pub fn of_session(kind: &str, label: Option<&str>) -> Value {
    let label = label.unwrap_or_default();
    match kind {
        // "asked by <agent>" (the caller's KITH_AGENT), or "asked from a terminal": a session with no agent name, such as
        // a flow manager's, driving the agent through the CLI.
        "ask" => match label.strip_prefix("asked by ") {
            Some(who) => value(Kind::Agent, Some(who)),
            None => value(Kind::Manager, None),
        },
        "schedule" => value(Kind::Schedule, label.strip_prefix("scheduled: ")),
        "helper" => value(Kind::Agent, label.strip_prefix("helper of ")),
        _ => value(Kind::Terminal, None),
    }
}

/// Drops `data.prompter` from an entry as JSON, for what an invited person or a bot guest is shown.
pub fn strip(entry: &mut Value) {
    if let Some(data) = entry.get_mut("data").and_then(Value::as_object_mut) {
        data.remove(KEY);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use of_session as reg;

    #[test]
    fn the_session_decides_who_prompted() {
        assert_eq!(reg("terminal", None), json!({"kind": "terminal"}));
        assert_eq!(reg("ask", Some("asked by architect")), json!({"kind": "agent", "name": "architect"}));
        assert_eq!(reg("ask", Some("asked from a terminal")), json!({"kind": "manager"}));
        assert_eq!(reg("schedule", Some("scheduled: nightly")), json!({"kind": "schedule", "name": "nightly"}));
        assert_eq!(reg("schedule", Some("scheduled")), json!({"kind": "schedule"}));
        assert_eq!(reg("helper", Some("helper of coder")), json!({"kind": "agent", "name": "coder"}));
    }

    #[test]
    fn stamp_overwrites_what_a_client_sent_and_strip_removes_it() {
        let mut data = json!({"text": "hi", "prompter": {"kind": "schedule", "name": "x"}});
        stamp(&mut data, Kind::App, None);
        assert_eq!(data["prompter"], json!({"kind": "app"}));
        let mut entry = json!({"id": "e", "data": data});
        strip(&mut entry);
        assert!(entry["data"].get("prompter").is_none());
        assert_eq!(entry["data"]["text"], "hi");
    }
}
