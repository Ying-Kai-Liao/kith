use super::*;
use crate::feed::{By, Fold, PostSource, PostWrite, put_post};
use crate::remote::identity::Identity;
use crate::store::{BotConfig, EntryKind, Lane, Store};
use std::sync::Arc;

struct Fixture {
    hub: Arc<Hub>,
    dir: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("kith-feedapi-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(std::path::Path::new(":memory:")).unwrap();
        let hub = Hub::new(store, "feed-api".into(), Identity::load_or_create(&dir).unwrap(), "test".into(), 0);
        Self { hub, dir }
    }

    fn chat(&self, id: &str, person: Option<&str>) {
        let config: BotConfig = serde_json::from_value(json!({
            "id": id, "name": id, "backend": "fixture", "cwd": self.dir, "hqAgent": "ops", "person": person,
        }))
        .unwrap();
        self.hub.store.save_bot(&config).unwrap();
    }

    fn post(&self, fold: &str) -> crate::store::Post {
        let w = PostWrite {
            fold: Fold::Tool { fold: Some(fold.into()) },
            agent: "ops".into(),
            kind: "file".into(),
            title: fold.into(),
            line: String::new(),
            attachment: None,
            source: PostSource { chat: "mine".into(), thread: None, entry: None },
            by: By::Agent,
            fact_at: 1,
            fact_id: fold.into(),
        };
        put_post(&self.hub, &w).unwrap().unwrap()
    }

    fn answer(&self, method: &str, body: Value) -> Result<Value> {
        match call(&self.hub, method, body)? {
            ControlFlow::Break(v) => Ok(v),
            ControlFlow::Continue(_) => bail!("not answered"),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).unwrap();
    }
}

#[test]
fn the_feed_pages_newest_first_and_says_where_the_next_page_starts() {
    let f = Fixture::new();
    f.chat("mine", None);
    for n in 0..3 {
        let p = f.post(&format!("f{n}"));
        f.hub.store.exec_for_test(&format!("UPDATE posts SET updated_at = {} WHERE id = '{}'", 1000 + n, p.id));
    }
    let titles = |v: &Value| -> Vec<String> {
        v["posts"].as_array().unwrap().iter().map(|p| p["title"].as_str().unwrap().to_owned()).collect()
    };
    let first = f.answer("feed", json!({"limit": 2})).unwrap();
    assert_eq!((titles(&first), first["next"].as_i64()), (vec!["f2".into(), "f1".into()], Some(1001)));
    let second = f.answer("feed", json!({"before": 1001, "limit": 2})).unwrap();
    assert_eq!(titles(&second), ["f0"]);
    assert!(second.get("next").is_none(), "the last page has no next");
    assert_eq!(titles(&f.answer("feed", json!({})).unwrap()).len(), 3, "30 when left out");
    let wire = &first["posts"][0];
    assert_eq!(
        (wire["liked"].as_bool(), wire["hidden"].as_bool(), wire["source"]["chat"].as_str()),
        (Some(false), Some(false), Some("mine"))
    );
}

#[test]
fn hiding_a_post_takes_it_off_the_feed_and_back() {
    let f = Fixture::new();
    f.chat("mine", None);
    let p = f.post("a");
    let hidden = f.answer("hidePost", json!({"id": p.id, "hidden": true})).unwrap();
    assert_eq!(hidden["post"]["hidden"], true);
    assert_eq!(f.answer("feed", json!({})).unwrap()["posts"].as_array().unwrap().len(), 0);
    f.answer("hidePost", json!({"id": p.id, "hidden": false})).unwrap();
    assert_eq!(f.answer("feed", json!({})).unwrap()["posts"].as_array().unwrap().len(), 1);
    assert!(f.answer("hidePost", json!({"id": p.id})).is_err(), "hidden is required");
    assert!(f.answer("hidePost", json!({"id": "p-missing", "hidden": true})).is_err());
}

#[test]
fn like_and_route_are_in_the_contract_and_not_answered_yet() {
    let f = Fixture::new();
    assert!(f.answer("likePost", json!({"id": "p", "liked": true})).is_err());
    assert!(f.answer("feedRoute", json!({"text": "hi"})).is_err());
    assert!(matches!(call(&f.hub, "send", json!({})).unwrap(), ControlFlow::Continue(_)), "other methods pass on");
}

#[test]
fn a_story_is_an_owner_chat_whose_agent_spoke_in_the_last_three_hours() {
    let f = Fixture::new();
    for (id, person) in [("mine", None), ("old", None), ("sams", Some("sam")), ("gone", None), ("quiet", None)] {
        f.chat(id, person);
    }
    let say = |bot: &str, kind: EntryKind| {
        f.hub.store.insert_entry(&Lane::main(bot), kind, 1, &json!({"text": "x", "final": true})).unwrap()
    };
    let last = say("mine", EntryKind::Agent);
    say("quiet", EntryKind::User);
    say("sams", EntryKind::Agent);
    say("gone", EntryKind::Agent);
    f.hub.store.delete_bot("gone").unwrap();
    let old = say("old", EntryKind::Agent);
    let long_ago = now_ms() - STORY_MS - 1000;
    f.hub.store.exec_for_test(&format!("UPDATE entries SET created_at = {long_ago} WHERE id = '{}'", old.id));
    let stories = f.answer("stories", json!({})).unwrap();
    let stories = stories["stories"].as_array().unwrap();
    assert_eq!(stories.len(), 1, "{stories:?}");
    let s = &stories[0];
    assert_eq!(
        (s["chat"].as_str(), s["agent"].as_str(), s["lastEntryId"].as_str()),
        (Some("mine"), Some("ops"), Some(last.id.as_str()))
    );
    assert_eq!(s["unread"], 1);
}
