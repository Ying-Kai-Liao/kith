use super::*;
use crate::api::devices::Caller;
use crate::api::events_stream;
use crate::remote::identity::Identity;
use crate::store::{BotConfig, Device, DeviceSource, Scope, Store, now_ms};
use futures::StreamExt;
use std::sync::Arc;

struct Fixture {
    hub: Arc<Hub>,
    dir: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("kith-feed-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(std::path::Path::new(":memory:")).unwrap();
        let hub = Hub::new(store, "feed-test".into(), Identity::load_or_create(&dir).unwrap(), "test".into(), 0);
        let f = Self { hub, dir };
        f.chat("mine", None);
        f.chat("sams", Some("sam"));
        f
    }

    fn chat(&self, id: &str, person: Option<&str>) {
        let config: BotConfig = serde_json::from_value(json!({
            "id": id, "name": id, "backend": "fixture", "cwd": self.dir, "hqAgent": "ops", "person": person,
        }))
        .unwrap();
        self.hub.store.save_bot(&config).unwrap();
    }

    async fn types(&self, caller: &Caller, since: i64) -> Vec<Value> {
        let stream = events_stream(&self.hub, since, None, caller).unwrap();
        tokio::pin!(stream);
        let mut events = vec![];
        while let Ok(Some(e)) = tokio::time::timeout(std::time::Duration::from_millis(50), stream.next()).await {
            events.push(e);
        }
        events
    }
}

fn write(chat: &str, fold: &str, title: &str, id: &str) -> PostWrite {
    PostWrite {
        fold: Fold::Tool { fold: Some(fold.into()) },
        agent: "ops".into(),
        kind: "file".into(),
        title: title.into(),
        line: String::new(),
        attachment: None,
        source: PostSource { chat: chat.into(), thread: None, entry: None },
        by: By::Agent,
        fact_at: 1,
        fact_id: id.into(),
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).unwrap();
    }
}

#[test]
fn a_changed_post_goes_out_once_with_its_rev_and_a_repeat_goes_out_not_at_all() {
    let fx = Fixture::new();
    let mut live = fx.hub.events.subscribe();
    let fact = write("mine", "daily", "One", "a");
    let post = put_post(&fx.hub, &fact).unwrap().unwrap();
    let event = live.try_recv().unwrap();
    assert_eq!(
        (event["type"].as_str(), event["rev"].as_i64(), event["post"]["id"].as_str()),
        (Some("post"), Some(post.rev), Some(post.id.as_str()))
    );
    assert!(put_post(&fx.hub, &fact).unwrap().is_none());
    assert!(live.try_recv().is_err(), "a repeat is quiet");
    assert!(put_post(&fx.hub, &write("sams", "daily", "One", "a")).is_err(), "an invited person's chat");
    assert!(live.try_recv().is_err(), "a refusal is quiet");
    let hid = hide_post(&fx.hub, &post.id, true).unwrap();
    let event = live.try_recv().unwrap();
    assert_eq!((event["post"]["hidden"].as_bool(), event["rev"].as_i64()), (Some(true), Some(hid.rev)));
    hide_post(&fx.hub, &post.id, true).unwrap();
    assert!(live.try_recv().is_err(), "hiding a hidden post is quiet");
    assert!(hide_post(&fx.hub, "p-missing", true).is_err());
}

#[tokio::test]
async fn the_owners_catch_up_has_posts_and_a_members_does_not() {
    let f = Fixture::new();
    let p = put_post(&f.hub, &write("mine", "daily", "One", "a")).unwrap().unwrap();
    for (key, person) in [("owner-phone", None), ("sam-phone", Some("sam"))] {
        f.hub
            .store
            .put_device(&Device {
                key: key.into(),
                name: key.into(),
                platform: "ios".into(),
                source: DeviceSource::Account,
                grant_id: Some(format!("grt_{key}")),
                scopes: vec![Scope::Control],
                lease_until: Some(now_ms() + 60_000),
                created_at: now_ms(),
                last_seen_at: None,
                person_id: person.map(str::to_owned),
            })
            .unwrap();
    }
    f.hub.store.set_device_proven("sam-phone").unwrap();
    let device = |key: &str| Caller::Device { key: key.into(), scopes: vec![Scope::Control] };
    let posts = |events: &[Value]| events.iter().filter(|e| e["type"] == "post").count();
    let local = f.types(&Caller::Local, 0).await;
    assert_eq!(posts(&local), 1);
    assert_eq!(local.iter().find(|e| e["type"] == "post").unwrap()["post"]["id"], p.id.as_str());
    assert_eq!(posts(&f.types(&device("owner-phone"), 0).await), 1, "the owner's phone too");
    assert_eq!(posts(&f.types(&Caller::Local, p.rev).await), 0, "nothing after the last rev");
    let member = f.types(&device("sam-phone"), 0).await;
    assert_eq!(member[0]["type"], "hello");
    assert_eq!(posts(&member), 0, "a member's catch-up has no post");
}

#[tokio::test]
async fn a_catch_up_has_at_most_a_hundred_posts() {
    let f = Fixture::new();
    for n in 0..105 {
        put_post(&f.hub, &write("mine", &format!("f{n}"), "T", "a")).unwrap();
    }
    let events = f.types(&Caller::Local, 0).await;
    let revs: Vec<i64> = events.iter().filter(|e| e["type"] == "post").map(|e| e["rev"].as_i64().unwrap()).collect();
    assert_eq!(revs.len(), 100);
    assert!(revs.windows(2).all(|w| w[0] < w[1]) && *revs.last().unwrap() == f.hub.store.current_rev());
}
