//! Topic cards: the owner folds a run of a chat into one card.
//!
//! A topic is a view over the chat's existing entries, not a session. Making one starts no turn, adds no line and
//! re-parents nothing (`entries.thread_id` stays as written). It lives as `data.topic` on its first entry, written
//! through `Hub::set_entry` so the normal entry event carries it to every client:
//!
//! ```text
//! data.topic = { id, title, untilId, untilAt, turns: [turn numbers], createdAt }
//! ```
//!
//! `untilAt` is the `createdAt` of `untilId`, the newest entry of the chat when the topic was made. `turns` are the
//! `turn` numbers (`Entry.turn`, which clients receive) of agent turns still running then that have an entry in the
//! range, so the rest of an in-flight reply folds too.
//!
//! Fold rule (clients copy it exactly): an entry of the chat's flat view (every entry of that bot, task and thread
//! replies included) belongs to the topic iff
//! `root.createdAt <= e.createdAt <= untilAt`, OR (`e.turn` is in `topic.turns` and `e.createdAt > root.createdAt`).
//! The root (the entry holding `data.topic`) is the card's first message. A topic never sits inside another one.
//!
//! A topic marker is not a task root (`data.task`) and does not touch `tasks::continued`: the next message still
//! lands where it would have.

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use std::sync::Mutex;

use crate::hub::Hub;
use crate::store::{Entry, Lane, now_ms};

const MAX_TITLE: usize = 80;

/// The methods this module answers (the owner's alone: no list of the contract's roles holds them).
pub const METHODS: [&str; 3] = ["makeTopic", "renameTopic", "unfoldTopic"];

/// Serialises check-then-write, so two quick calls make one topic.
static WRITE: Mutex<()> = Mutex::new(());

/// `makeTopic`, `renameTopic` and `unfoldTopic`; `None` for any other method.
pub fn call(hub: &Hub, method: &str, b: &Value) -> Option<Result<Value>> {
    if !METHODS.contains(&method) {
        return None;
    }
    Some(run(hub, method, b).map(|entry| json!({"entry": entry})))
}

fn run(hub: &Hub, method: &str, b: &Value) -> Result<Entry> {
    let bot = b["botId"].as_str().ok_or_else(|| anyhow!("botId required"))?;
    let id = b["entryId"].as_str().ok_or_else(|| anyhow!("entryId required"))?;
    let _g = WRITE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let entry = hub.store.entry(id).filter(|e| e.bot_id == bot).ok_or_else(|| anyhow!("no such message"))?;
    let mut data = if entry.data.is_object() { entry.data.clone() } else { json!({}) };
    match method {
        "makeTopic" => {
            let title = title_of(b)?;
            if entry.data["topic"].is_object() {
                bail!("That message already starts a topic. Rename it instead.");
            }
            let cfg = hub.store.bot(bot)?.ok_or_else(|| anyhow!("no such agent"))?.config;
            data["topic"] = start(hub, &hub.working_lanes(&cfg), &entry, &title)?;
        }
        "renameTopic" => {
            let title = title_of(b)?;
            if !entry.data["topic"].is_object() {
                bail!("That message doesn't start a topic.");
            }
            data["topic"]["title"] = title.into();
        }
        _ => {
            if !entry.data["topic"].is_object() {
                bail!("That message doesn't start a topic.");
            }
            data.as_object_mut().map(|o| o.remove("topic"));
        }
    }
    hub.set_entry(id, &data).ok_or_else(|| anyhow!("couldn't save the topic"))
}

fn title_of(b: &Value) -> Result<String> {
    let title = b["title"]
        .as_str()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| anyhow!("A topic needs a title."))?;
    if title.chars().count() > MAX_TITLE {
        bail!("A topic title can be at most {MAX_TITLE} characters.");
    }
    Ok(title.to_owned())
}

/// The `data.topic` for a topic rooted at `root`, after the checks that keep topics apart.
fn start(hub: &Hub, working: &[Lane], root: &Entry, title: &str) -> Result<Value> {
    let roots = hub.store.topic_roots(&root.bot_id)?;
    if roots.iter().any(|r| r.id != root.id && folds(&r.data["topic"], r.created_at, root)) {
        bail!("That message is already inside a topic.");
    }
    let range = hub.store.entries_from(&root.bot_id, root.created_at)?;
    let until = range.last().unwrap_or(root);
    let turns = running_turns(&range, working);
    let topic = json!({
        "id": uuid::Uuid::new_v4().to_string(),
        "title": title,
        "untilId": until.id,
        "untilAt": until.created_at,
        "turns": turns,
        "createdAt": now_ms(),
    });
    if roots.iter().any(|r| r.id != root.id && folds(&topic, root.created_at, r)) {
        bail!("A topic can't hold another topic.");
    }
    Ok(topic)
}

/// Whether `e` belongs to `topic`, rooted at an entry made at `root_at` (the fold rule in the module docs).
pub fn folds(topic: &Value, root_at: i64, e: &Entry) -> bool {
    let until = topic["untilAt"].as_i64().unwrap_or(root_at);
    (root_at <= e.created_at && e.created_at <= until)
        || (e.turn != 0
            && e.created_at > root_at
            && topic["turns"].as_array().is_some_and(|t| t.contains(&json!(e.turn))))
}

/// The turns still running (in `working` lanes) that have a line in `range`: the newest agent-side line of each
/// working lane names its turn.
pub fn running_turns(range: &[Entry], working: &[Lane]) -> Vec<i64> {
    let mut turns: Vec<i64> = working
        .iter()
        .filter_map(|lane| {
            range
                .iter()
                .rev()
                .find(|e| e.kind != "user" && e.turn != 0 && e.bot_id == lane.chat && e.thread_id == lane.thread)
                .map(|e| e.turn)
        })
        .collect();
    turns.sort_unstable();
    turns.dedup();
    turns
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::identity::Identity;
    use crate::store::{BotConfig, EntryKind, Store};
    use std::sync::Arc;

    fn hub() -> Arc<Hub> {
        let dir = std::env::temp_dir().join(format!("kith-topics-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(std::path::Path::new(":memory:")).unwrap();
        for id in ["a", "b"] {
            let cfg: BotConfig = serde_json::from_value(
                json!({"id": id, "name": id, "backend": "claude", "cwd": dir.to_string_lossy()}),
            )
            .unwrap();
            store.save_bot(&cfg).unwrap();
        }
        Hub::new(store, "t".into(), Identity::load_or_create(&dir).unwrap(), "t".into(), 0)
    }

    /// Entry `n` of bot `a`'s main chat, made at `n * 10` ms.
    fn say(hub: &Hub, n: i64, turn: i64, kind: EntryKind) -> Entry {
        hub.store.insert_entry_at(&Lane::main("a"), kind, turn, &json!({"text": format!("m{n}")}), n * 10).unwrap()
    }

    fn make(hub: &Hub, id: &str) -> Result<Value> {
        call(hub, "makeTopic", &json!({"botId": "a", "entryId": id, "title": " Pricing "})).unwrap()
    }

    fn topic(hub: &Hub, id: &str) -> Value {
        hub.store.entry(id).unwrap().data["topic"].clone()
    }

    #[test]
    fn making_a_topic_stores_it_on_its_first_message_and_the_event_carries_it() {
        let hub = hub();
        let mut events = hub.events.subscribe();
        let root = say(&hub, 1, 0, EntryKind::User);
        let last = say(&hub, 2, 1, EntryKind::Agent);
        let out = make(&hub, &root.id).unwrap();
        let t = topic(&hub, &root.id);
        assert_eq!(t["title"], "Pricing");
        assert_eq!((t["untilId"].as_str(), t["untilAt"].as_i64()), (Some(last.id.as_str()), Some(20)));
        assert_eq!(t["turns"], json!([]));
        assert_eq!(out["entry"]["data"]["topic"], t);
        let event = events.try_recv().unwrap();
        assert_eq!(event["type"], "entry");
        assert_eq!(event["entry"]["data"]["topic"], t);
        // nothing else changed: no new line, the thread stays as written
        assert_eq!(hub.store.entries_from("a", 0).unwrap().len(), 2);
        assert_eq!(hub.store.entry(&root.id).unwrap().thread_id, None);
    }

    #[test]
    fn rename_and_unfold_edit_only_the_topic_and_keep_the_rest_of_data() {
        let hub = hub();
        let data = json!({"text": "x", "task": true, "thread": {"count": 2}});
        let root = hub.store.insert_entry_at(&Lane::main("a"), EntryKind::User, 0, &data, 10).unwrap();
        make(&hub, &root.id).unwrap();
        call(&hub, "renameTopic", &json!({"botId": "a", "entryId": root.id, "title": "New"})).unwrap().unwrap();
        let data = hub.store.entry(&root.id).unwrap().data;
        assert_eq!(data["topic"]["title"], "New");
        assert_eq!((data["task"].as_bool(), data["thread"]["count"].as_i64()), (Some(true), Some(2)));
        let out = call(&hub, "unfoldTopic", &json!({"botId": "a", "entryId": root.id})).unwrap().unwrap();
        assert!(out["entry"]["data"].get("topic").is_none());
        assert_eq!(out["entry"]["data"]["thread"]["count"], 2);
        for m in ["renameTopic", "unfoldTopic"] {
            let err = call(&hub, m, &json!({"botId": "a", "entryId": root.id, "title": "t"})).unwrap().unwrap_err();
            assert_eq!(err.to_string(), "That message doesn't start a topic.");
        }
    }

    #[test]
    fn refusals() {
        let hub = hub();
        let e1 = say(&hub, 1, 0, EntryKind::User);
        let e2 = say(&hub, 2, 0, EntryKind::Agent);
        say(&hub, 3, 0, EntryKind::User);
        let err = |r: Result<Value>| r.unwrap_err().to_string();
        let titled = |id: &str, title: &str| {
            call(&hub, "makeTopic", &json!({"botId": "a", "entryId": id, "title": title})).unwrap()
        };
        assert_eq!(err(titled(&e1.id, "  ")), "A topic needs a title.");
        assert!(err(titled(&e1.id, &"x".repeat(81))).contains("at most 80"));
        let wrong_bot = call(&hub, "makeTopic", &json!({"botId": "b", "entryId": e2.id, "title": "t"})).unwrap();
        assert_eq!(err(wrong_bot), "no such message");
        assert!(titled(&e1.id, &"x".repeat(80)).is_ok());
        // the topic covers e1..e3: its own message again and a message inside it are refused
        assert!(err(make(&hub, &e1.id)).contains("already starts a topic"));
        assert_eq!(err(make(&hub, &e2.id)), "That message is already inside a topic.");
    }

    #[test]
    fn a_range_holding_another_topics_root_is_refused() {
        let hub = hub();
        let e1 = say(&hub, 1, 0, EntryKind::User);
        let e2 = say(&hub, 2, 0, EntryKind::Agent);
        // a topic on e2 covers e2 only; a topic from e1 would run over it
        make(&hub, &e2.id).unwrap();
        say(&hub, 3, 0, EntryKind::User);
        let err = make(&hub, &e1.id).unwrap_err().to_string();
        assert_eq!(err, "A topic can't hold another topic.");
    }

    #[test]
    fn a_message_after_a_topic_is_not_a_task_root_and_is_not_folded() {
        let hub = hub();
        let root = say(&hub, 1, 0, EntryKind::User);
        make(&hub, &root.id).unwrap();
        let t = topic(&hub, &root.id);
        let bot = hub.store.bot("a").unwrap().unwrap().config;
        // tasks::continued only looks at `data.task` roots: the topic marker makes none
        assert_eq!(crate::chat::tasks::continued(&hub, &bot, false, 1_000_000), None);
        assert!(!crate::chat::tasks::is_task(&hub.store.entry(&root.id).unwrap()));
        let later =
            hub.store.insert_entry_at(&Lane::main("a"), EntryKind::User, 0, &json!({"text": "next"}), 5_000).unwrap();
        assert!(!folds(&t, root.created_at, &later));
        assert_eq!(later.thread_id, None);
    }

    #[test]
    fn a_running_turn_is_kept_so_the_rest_of_its_reply_folds() {
        let main = Lane::main("a");
        let mk = |n: i64, turn: i64, kind: &str, thread: Option<&str>| Entry {
            id: n.to_string(),
            seq: n,
            bot_id: "a".into(),
            thread_id: thread.map(Into::into),
            rev: n,
            kind: kind.into(),
            turn,
            data: json!({}),
            created_at: n * 10,
            updated_at: n * 10,
        };
        let range =
            [mk(1, 0, "user", None), mk(2, 4, "agent", None), mk(3, 5, "agent", Some("t")), mk(4, 4, "tool", None)];
        assert_eq!(running_turns(&range, std::slice::from_ref(&main)), [4]);
        assert_eq!(running_turns(&range, &[Lane::in_thread("a", "t"), main]), [4, 5]);
        assert_eq!(running_turns(&range, &[]), Vec::<i64>::new());
        let t = json!({"untilAt": 40, "turns": [4]});
        assert!(folds(&t, 10, &mk(9, 4, "agent", None)), "later line of a kept turn");
        assert!(!folds(&t, 10, &mk(9, 7, "agent", None)));
        assert!(!folds(&t, 10, &mk(0, 4, "agent", None)), "before the root");
    }
}
