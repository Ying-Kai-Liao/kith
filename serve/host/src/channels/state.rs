//! `<data dir>/channels/state.json`: the bridge's v2 state schema, so that a connection's threads, sessions and
//! rooms can move over from the bridge's file when it is cut over.
//!
//! Tables, all keyed by the connection first (`<conn>:<chat>…`): `conns` (per-connection state: offsets, webhooks),
//! `rooms` (default agent), `threads` (one task each: person, owner, last, room, sessions {agent: session}, conv,
//! newest last), `replies` (a platform message -> its thread key), `inbox` (`<thread>#<agent>` -> batches of
//! messages that wait for a turn), `pending` ("who should take this?" asks) and `wakes` (the card wake notes among the
//! inbox's messages, `<thread>#<agent>` -> notes).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::Path;

/// Threads and reply links kept in the state: the newest few thousand are enough to come back to.
pub const KEEP: usize = 5000;

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct State {
    pub version: u32,
    pub conns: Map<String, Value>,
    pub rooms: Map<String, Value>,
    pub threads: Map<String, Value>,
    pub replies: Map<String, Value>,
    pub inbox: BTreeMap<String, Vec<Vec<String>>>,
    pub pending: Map<String, Value>,
    pub wakes: BTreeMap<String, Vec<String>>,
}

/// How much of a connection's state came over from the bridge.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Imported {
    pub rooms: usize,
    pub threads: usize,
    pub replies: usize,
    pub inbox: usize,
    pub pending: usize,
}

impl State {
    /// Missing, unreadable or half-written files give an empty state; missing tables are empty.
    pub fn load(path: &Path) -> Self {
        let mut st: Self = std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        st.version = 2;
        st.drop_wakes();
        st
    }

    /// A card's wake still in the inbox never reached a turn: the card store owes it again after a restart and
    /// `cards::start` offers it anew, so the copy here goes (M5), or it would run twice.
    fn drop_wakes(&mut self) {
        for (wk, notes) in std::mem::take(&mut self.wakes) {
            let Some(q) = self.inbox.get_mut(&wk) else { continue };
            for batch in q.iter_mut() {
                batch.retain(|m| !notes.contains(m));
            }
            q.retain(|b| !b.is_empty());
            if q.is_empty() {
                self.inbox.remove(&wk);
            }
        }
    }

    /// The Claude sessions of the thread a work key (`<thread>#<agent>`) names: the agent's own and those of its backups.
    /// The `kith ask` jobs these started are what a Stop in the thread ends.
    pub fn sessions_of(&self, wk: &str) -> Vec<String> {
        let Some((key, agent)) = wk.rsplit_once('#') else { return Vec::new() };
        let Some(th) = self.threads.get(key) else { return Vec::new() };
        let own = th.get("sessions").and_then(|s| s.get(agent)).and_then(Value::as_str).map(str::to_owned);
        let backups = th.get("backup_sessions").and_then(|b| b.get(agent)).and_then(Value::as_object);
        let mut out: Vec<String> = own
            .into_iter()
            .chain(backups.into_iter().flatten().filter_map(|(_, v)| v.as_str().map(str::to_owned)))
            .filter(|s| !s.is_empty())
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Prunes to the newest [`KEEP`] threads and replies (those with queued messages stay), then writes the file
    /// by writing a temporary one and renaming it.
    pub fn save(&mut self, path: &Path) -> std::io::Result<()> {
        for table in [&mut self.threads, &mut self.replies] {
            let excess = table.len().saturating_sub(KEEP);
            let doomed: Vec<String> = table
                .keys()
                .take(excess)
                .filter(|k| !self.inbox.keys().any(|w| w.starts_with(&format!("{k}#"))))
                .cloned()
                .collect();
            for k in doomed {
                table.shift_remove(&k);
            }
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, path)
    }

    /// Copies one connection's entries from the bridge's state (`bridge`, parsed). Nothing else of it, and an
    /// entry already here is left alone.
    pub fn import_connection(&mut self, bridge: &Value, conn: &str) -> Imported {
        let prefix = format!("{conn}:");
        let table = |name: &str| bridge.get(name).and_then(Value::as_object).cloned().unwrap_or_default();
        let mut n = Imported::default();
        if let Some(c) = table("conns").get(conn) {
            self.conns.entry(conn).or_insert_with(|| c.clone());
        }
        let copy = |from: Map<String, Value>, into: &mut Map<String, Value>, keep: &dyn Fn(&str, &Value) -> bool| {
            let mut count = 0;
            for (k, v) in from {
                if keep(&k, &v) && !into.contains_key(&k) {
                    into.insert(k, v);
                    count += 1;
                }
            }
            count
        };
        let starts = |k: &str, _: &Value| k.starts_with(&prefix);
        n.rooms = copy(table("rooms"), &mut self.rooms, &starts);
        n.threads = copy(table("threads"), &mut self.threads, &starts);
        n.replies = copy(table("replies"), &mut self.replies, &starts);
        n.pending = copy(table("pending"), &mut self.pending, &|_, v| {
            v.get("room").and_then(Value::as_str).is_some_and(|r| r.starts_with(&prefix))
        });
        for (wk, batches) in table("inbox") {
            if !wk.starts_with(&prefix) || self.inbox.contains_key(&wk) {
                continue;
            }
            if let Ok(b) = serde_json::from_value::<Vec<Vec<String>>>(batches) {
                self.inbox.insert(wk, b);
                n.inbox += 1;
            }
        }
        self.conns.entry(conn).or_insert_with(|| Value::Object(Map::new()));
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bridge() -> Value {
        json!({
            "version": 2,
            "conns": {"telegram/hq": {"offset": 9}, "telegram/home": {"offset": 1}},
            "rooms": {"telegram/hq:1": {"default": "house"}, "telegram/home:2": {"default": "x"}},
            "threads": {
                "telegram/hq:1:5": {"person": "kai", "last": "house", "sessions": {"house": "s1"}, "conv": {"key": "telegram/hq:1:5"}},
                "telegram/home:2:6": {"person": "kai", "last": "x"}
            },
            "replies": {"telegram/hq:1:7": "telegram/hq:1:5", "telegram/home:2:8": "telegram/home:2:6"},
            "inbox": {"telegram/hq:1:5#house": [["queued"]], "telegram/home:2:6#x": [["no"]]},
            "pending": {"p1": {"room": "telegram/hq:1", "task": "t"}, "p2": {"room": "telegram/home:2"}}
        })
    }

    #[test]
    fn a_work_key_names_the_sessions_of_its_agent_and_backups() {
        let mut st = State::default();
        st.threads.insert(
            "t:1".into(),
            json!({"sessions": {"house": "s1", "other": "s9"}, "backup_sessions": {"house": {"b1": "s2"}}}),
        );
        assert_eq!(st.sessions_of("t:1#house"), vec!["s1", "s2"]);
        assert_eq!(st.sessions_of("t:1#other"), vec!["s9"]);
        assert!(st.sessions_of("t:2#house").is_empty() && st.sessions_of("t:1").is_empty());
    }

    #[test]
    fn imports_one_connection_only() {
        let mut st = State::default();
        let n = st.import_connection(&bridge(), "telegram/hq");
        assert_eq!(n, Imported { rooms: 1, threads: 1, replies: 1, inbox: 1, pending: 1 });
        assert_eq!(st.conns["telegram/hq"]["offset"], 9);
        assert!(!st.conns.contains_key("telegram/home"));
        assert_eq!(st.threads["telegram/hq:1:5"]["sessions"]["house"], "s1");
        assert_eq!(st.threads.len(), 1);
        assert_eq!(st.inbox["telegram/hq:1:5#house"], vec![vec!["queued".to_owned()]]);
        assert!(st.pending.contains_key("p1") && !st.pending.contains_key("p2"));
    }

    #[test]
    fn a_bridge_without_the_connection_still_marks_it_imported() {
        let mut st = State::default();
        assert_eq!(st.import_connection(&json!({}), "slack/x"), Imported::default());
        assert!(st.conns.contains_key("slack/x"));
    }

    #[test]
    fn save_prunes_the_oldest_but_keeps_queued_threads() {
        let dir = std::env::temp_dir().join(format!("kith-state-{}", uuid::Uuid::new_v4()));
        let path = dir.join("state.json");
        let mut st = State::default();
        for i in 0..KEEP + 3 {
            st.threads.insert(format!("c:{i}"), json!({"person": "kai"}));
            st.replies.insert(format!("c:r{i}"), json!(format!("c:{i}")));
        }
        st.inbox.insert("c:0#house".into(), vec![vec!["waiting".into()]]);
        st.save(&path).unwrap();
        let back = State::load(&path);
        assert_eq!(back.threads.len(), KEEP + 1);
        assert!(back.threads.contains_key("c:0") && !back.threads.contains_key("c:1"));
        assert_eq!(back.replies.len(), KEEP);
        assert!(!path.with_extension("tmp").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_card_wake_left_in_the_inbox_is_dropped_at_load_as_the_card_store_offers_it_again() {
        let dir = std::env::temp_dir().join(format!("kith-state-{}", uuid::Uuid::new_v4()));
        let path = dir.join("state.json");
        let mut st = State::default();
        st.inbox.insert("c:1#house".into(), vec![vec!["hi".into()], vec!["[Kith] wake".into()]]);
        st.inbox.insert("c:2#house".into(), vec![vec!["[Kith] other".into()]]);
        st.wakes.insert("c:1#house".into(), vec!["[Kith] wake".into()]);
        st.wakes.insert("c:2#house".into(), vec!["[Kith] other".into()]);
        st.save(&path).unwrap();
        let back = State::load(&path);
        assert_eq!(back.inbox["c:1#house"], vec![vec!["hi".to_owned()]], "the person's message stays");
        assert!(!back.inbox.contains_key("c:2#house"), "a batch of wakes only goes");
        assert!(back.wakes.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_or_broken_file_is_an_empty_state() {
        let dir = std::env::temp_dir().join(format!("kith-state-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(State::load(&dir.join("none.json")).threads.is_empty(), "missing file");
        std::fs::write(dir.join("bad.json"), "{not json").unwrap();
        assert!(State::load(&dir.join("bad.json")).threads.is_empty(), "broken file");
        std::fs::write(dir.join("old.json"), r#"{"version":2,"threads":{"a:1":{}}}"#).unwrap();
        let st = State::load(&dir.join("old.json"));
        assert!(st.threads.contains_key("a:1") && st.rooms.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
