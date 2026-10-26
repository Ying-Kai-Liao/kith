//! What a bot sees of a conversation (section 5.8, `BotEntry`) and how a turn's reply is assembled from it. Entries come
//! from two places, the events subscription and `botThread` catch-ups, and are keyed by `id`: an entry that grows is
//! replaced in place, and nothing is ever counted twice.

use kith_wire::rpc::{self, BotEntryKind as Kind};
use serde_json::Value;

use crate::exit::CliError;

/// The contract's `BotEntry` (serve/contract/host/contract.json).
pub type BotEntry = rpc::BotEntry;
/// The contract's `BotThread`: an unknown `state` reads as `running`, an unknown entry `kind` as `Unknown`.
pub type Thread = rpc::BotThread;

/// A `botThread` answer. One this version can't read is a version mismatch, not an empty thread.
pub fn parse_thread(v: &Value) -> Result<Thread, CliError> {
    serde_json::from_value(v.clone()).map_err(|_| CliError::version())
}

/// The entry inside an `entry` event, if that is what the event is. The conversation filter is the caller's.
pub fn entry_of_event(ev: &Value) -> Option<(Option<String>, BotEntry)> {
    if ev["type"] != "entry" {
        return None;
    }
    let body = if ev["entry"].is_object() { &ev["entry"] } else { ev };
    let entry: BotEntry = serde_json::from_value(body.clone()).ok()?;
    let conversation = ev["conversation"].as_str().or(Some(entry.conversation.as_str())).map(str::to_string);
    Some((conversation, entry))
}

/// One asked turn's answer, as far as it has arrived.
#[derive(Debug)]
pub struct Reply {
    pub turn: i64,
    agents: Vec<BotEntry>,
    pub waiting: Option<String>,
    waiting_seq: i64,
    pub failed: Option<String>,
    pub last_seq: i64,
}

impl Reply {
    pub fn new(turn: i64, since: i64) -> Self {
        Self { turn, agents: vec![], waiting: None, waiting_seq: 0, failed: None, last_seq: since }
    }

    /// Returns true when the entry changed what we know.
    pub fn apply(&mut self, e: &BotEntry) -> bool {
        self.last_seq = self.last_seq.max(e.seq);
        match e.kind {
            Kind::Agent if e.turn == self.turn => {
                if self.waiting.is_some() && e.seq > self.waiting_seq {
                    self.waiting = None;
                }
                if let Some(old) = self.agents.iter_mut().find(|a| a.id == e.id) {
                    if old == e {
                        return false;
                    }
                    *old = e.clone();
                } else {
                    self.agents.push(e.clone());
                    self.agents.sort_by_key(|a| a.seq);
                }
                true
            }
            Kind::Waiting if e.turn == self.turn => {
                let changed = self.waiting.as_deref() != Some(e.text.as_str());
                self.waiting = Some(e.text.clone());
                self.waiting_seq = e.seq;
                changed
            }
            // A notice that ends the turn badly keeps its text (an origin line is not an error and never ends it).
            Kind::Notice if e.turn == self.turn && (e.r#final == Some(true) || e.error == Some(true)) => {
                let changed = self.failed.as_deref() != Some(e.text.as_str());
                self.failed = Some(e.text.clone());
                changed
            }
            // User entries, other turns, and kinds this version doesn't know are not part of this reply.
            _ => false,
        }
    }

    /// The reply so far: the turn's agent entries, in order.
    pub fn text(&self) -> String {
        self.agents.iter().map(|a| a.text.trim_end()).filter(|t| !t.is_empty()).collect::<Vec<_>>().join("\n\n")
    }

    pub fn has_final(&self) -> bool {
        self.agents.iter().any(|a| a.r#final == Some(true))
    }
}

/// How much of `now` to print after `printed` was already printed: the new suffix when the text only grew.
/// `None` when the text was rewritten, which a terminal can't take back; the caller prints it once at the end.
pub fn suffix<'a>(printed: &str, now: &'a str) -> Option<&'a str> {
    now.strip_prefix(printed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kith_wire::rpc::ConversationState;
    use serde_json::json;

    fn entry(id: &str, seq: i64, turn: i64, kind: Kind, text: &str, fin: bool) -> BotEntry {
        BotEntry {
            id: id.into(),
            seq,
            turn,
            conversation: "cv_1".into(),
            kind,
            text: text.into(),
            r#final: (kind == Kind::Agent).then_some(fin),
            error: (kind == Kind::Notice && fin).then_some(true),
            ..Default::default()
        }
    }

    #[test]
    fn an_entry_that_grows_is_replaced_not_repeated() {
        let mut r = Reply::new(7, 10);
        assert!(r.apply(&entry("a1", 11, 7, Kind::Agent, "The list", false)));
        assert!(r.apply(&entry("a1", 11, 7, Kind::Agent, "The list has milk", false)));
        assert!(
            !r.apply(&entry("a1", 11, 7, Kind::Agent, "The list has milk", false)),
            "same entry again changes nothing"
        );
        assert_eq!(r.text(), "The list has milk");
        assert!(!r.has_final());
        assert!(r.apply(&entry("a1", 11, 7, Kind::Agent, "The list has milk.", true)));
        assert!(r.has_final());
    }

    #[test]
    fn other_turns_and_user_entries_are_ignored() {
        let mut r = Reply::new(7, 0);
        assert!(!r.apply(&entry("a0", 3, 6, Kind::Agent, "old answer", true)));
        assert!(!r.apply(&entry("u1", 4, 7, Kind::User, "question", true)));
        assert_eq!(r.text(), "");
        assert_eq!(r.last_seq, 4);
    }

    #[test]
    fn two_agent_entries_of_a_turn_are_joined_in_seq_order() {
        let mut r = Reply::new(2, 0);
        r.apply(&entry("b", 6, 2, Kind::Agent, "second", true));
        r.apply(&entry("a", 5, 2, Kind::Agent, "first", false));
        assert_eq!(r.text(), "first\n\nsecond");
    }

    #[test]
    fn waiting_is_open_until_the_agent_goes_on() {
        let mut r = Reply::new(2, 0);
        assert!(r.apply(&entry("w", 5, 2, Kind::Waiting, "Waiting for Sam to approve a step", false)));
        assert_eq!(r.waiting.as_deref(), Some("Waiting for Sam to approve a step"));
        assert!(!r.apply(&entry("w", 5, 2, Kind::Waiting, "Waiting for Sam to approve a step", false)), "told once");
        r.apply(&entry("a", 6, 2, Kind::Agent, "done", true));
        assert_eq!(r.waiting, None);
    }

    #[test]
    fn only_a_final_notice_of_this_turn_fails_it() {
        let mut r = Reply::new(2, 0);
        let origin = entry("n0", 1, 2, Kind::Notice, "from Grok", false);
        assert!(!r.apply(&origin));
        assert_eq!(r.failed, None);
        assert!(r.apply(&entry("n1", 9, 2, Kind::Notice, "The agent crashed", true)));
        assert_eq!(r.failed.as_deref(), Some("The agent crashed"));
    }

    #[test]
    fn events_parse_both_shapes() {
        let nested = json!({"type": "entry", "conversation": "cv_1", "entry": {"id": "x", "seq": 4, "turn": 1, "at": 9, "conversation": "cv_1", "kind": "agent", "text": "hi", "final": true}});
        let (c, e) = entry_of_event(&nested).unwrap();
        assert_eq!(c.as_deref(), Some("cv_1"));
        assert!(e.r#final == Some(true) && e.text == "hi");
        let flat = json!({"type": "entry", "id": "x", "seq": 4, "turn": 1, "at": 9, "conversation": "cv_2", "kind": "agent", "text": "hi"});
        let (c, e) = entry_of_event(&flat).unwrap();
        assert_eq!((c.as_deref(), e.id.as_str()), (Some("cv_2"), "x"));
        let new_kind = json!({"type": "entry", "id": "y", "seq": 5, "turn": 1, "at": 9, "conversation": "cv_1", "kind": "summary", "text": "…"});
        assert_eq!(entry_of_event(&new_kind).unwrap().1.kind, Kind::Unknown, "a kind this version doesn't know");
        assert!(entry_of_event(&json!({"type": "bot", "status": "running"})).is_none());
    }

    #[test]
    fn thread_parses_done_and_state() {
        let entry = json!({"id": "a", "seq": 1, "turn": 1, "at": 9, "conversation": "cv_1", "kind": "agent", "text": "x", "final": true});
        let t = parse_thread(
            &json!({"conversation": "cv_1", "entries": [entry], "done": true, "turn": 1, "state": "idle"}),
        )
        .unwrap();
        assert_eq!(t.entries.len(), 1);
        assert!(t.done);
        assert_eq!(t.state, ConversationState::Idle);
        let newer = json!({"conversation": "cv_1", "entries": [], "done": false, "state": "paused"});
        assert_eq!(
            parse_thread(&newer).unwrap().state,
            ConversationState::Running,
            "a state this version doesn't know"
        );
        let broken = json!({"entries": [entry, 5], "done": true, "state": "idle"});
        assert_eq!(parse_thread(&broken).unwrap_err().exit, crate::exit::Exit::Version);
    }

    #[test]
    fn stream_prints_only_what_is_new() {
        assert_eq!(suffix("The list", "The list has milk"), Some(" has milk"));
        assert_eq!(suffix("", "abc"), Some("abc"));
        assert_eq!(suffix("abc", "abc"), Some(""));
        assert_eq!(suffix("abc", "xyz"), None);
    }
}
