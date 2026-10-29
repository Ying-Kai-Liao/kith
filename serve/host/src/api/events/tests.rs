use super::*;
use crate::remote::identity::Identity;
use crate::store::{BotConfig, Store};
use futures::StreamExt;
use std::path::PathBuf;

struct Fixture {
    hub: Arc<Hub>,
    dir: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("kith-sync-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(std::path::Path::new(":memory:")).unwrap();
        let hub = Hub::new(store, "sync-test".into(), Identity::load_or_create(&dir).unwrap(), "test".into(), 0);
        Self { hub, dir }
    }

    fn add_bot(&self, id: &str) -> i64 {
        let config: BotConfig = serde_json::from_value(json!({
            "id": id, "name": id, "backend": "fixture", "cwd": self.dir,
        }))
        .unwrap();
        self.hub.store.save_bot(&config).unwrap()
    }

    async fn catch_up(&self, since: i64) -> Vec<Value> {
        let stream = events_stream(&self.hub, since, None, &Caller::Local).unwrap();
        tokio::pin!(stream);
        let mut events = Vec::new();
        while let Ok(Some(event)) = tokio::time::timeout(std::time::Duration::from_millis(50), stream.next()).await {
            events.push(event);
        }
        events
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).unwrap();
    }
}

#[tokio::test]
async fn full_sync_removes_a_bot_deleted_while_the_client_was_offline() {
    let f = Fixture::new();
    f.add_bot("deleted");
    f.add_bot("kept");
    let deleted_rev = f.hub.store.delete_bot("deleted").unwrap();

    // An app/host update rewinds the cursor to zero while keeping the cached roster.
    let events = f.catch_up(0).await;
    assert_eq!(events[0]["type"], "hello");
    let mut cached = std::collections::HashSet::from(["deleted".to_owned()]);
    for event in &events[1..] {
        let bot = &event["bot"];
        let id = bot["id"].as_str().unwrap();
        if bot["deleted"] == true {
            cached.remove(id);
            assert_eq!(bot["rev"], deleted_rev);
        } else {
            cached.insert(id.to_owned());
        }
    }
    assert_eq!(cached, std::collections::HashSet::from(["kept".to_owned()]));
    assert!(events[1..].windows(2).all(|pair| pair[0]["rev"].as_i64() < pair[1]["rev"].as_i64()));
}

#[tokio::test]
async fn incremental_sync_replays_deletions_only_after_the_saved_cursor() {
    let f = Fixture::new();
    let before = f.add_bot("deleted");
    let deleted_rev = f.hub.store.delete_bot("deleted").unwrap();
    let events = f.catch_up(before).await;
    assert_eq!(events.len(), 2);
    assert_eq!(events[1]["bot"]["deleted"], true);
    assert_eq!(events[1]["rev"], deleted_rev);
    assert_eq!(f.catch_up(deleted_rev).await.len(), 1);
}

/// Members' access requests reach the owner's other devices (the phone may answer them), the owner's own device
/// requests only this computer, and a member nothing (invitee-access property 6).
#[tokio::test]
async fn access_requests_reach_the_owners_devices_only_and_a_phone_only_members() {
    use crate::store::{Device, DeviceSource, Scope, now_ms};
    let f = Fixture::new();
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
    let streams = [Caller::Local, device("owner-phone"), device("sam-phone")]
        .map(|caller| events_stream(&f.hub, i64::MAX, None, &caller).unwrap());
    let requests = json!([{"requestId": "req_owner", "personId": null}, {"requestId": "req_sam", "personId": "sam"}]);
    f.hub.events.send(json!({"type": "accessRequests", "requests": requests})).unwrap();
    let mut seen = vec![];
    for stream in streams {
        tokio::pin!(stream);
        let mut ids = None;
        while let Ok(Some(event)) = tokio::time::timeout(std::time::Duration::from_millis(50), stream.next()).await {
            if event["type"] == "accessRequests" {
                let list = event["requests"].as_array().unwrap().iter();
                ids = Some(list.map(|r| r["requestId"].as_str().unwrap().to_owned()).collect::<Vec<_>>());
            }
        }
        seen.push(ids);
    }
    assert_eq!(seen[0].as_deref(), Some(&["req_owner".to_owned(), "req_sam".to_owned()][..]), "this computer: all");
    assert_eq!(seen[1].as_deref(), Some(&["req_sam".to_owned()][..]), "the owner's phone: members' only");
    assert_eq!(seen[2], None, "a member: none");
}
