//! Test rig for invited people: a temp store, a temp `KITH_AUTH` driven by the repo's own kith-auth, a temp Kith home
//! with a few agents, and a fake Kith cloud. Tests hold the rig for their whole run: the environment is process-wide.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::api::devices::Caller;
use crate::channels::hq::TEST_ENV;
use crate::hub::Hub;
use crate::remote::account::{ONE_AT_A_TIME, TEST_BIN};
use crate::remote::cloud::{self, Owner};
use crate::remote::identity::Identity;
use crate::store::{BotConfig, Device, DeviceSource, Scope, Store, now_ms};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct FakeCloud {
    pub state: Mutex<Value>,
    pub posts: Mutex<Vec<(String, Value)>>,
    pub refuse_invites: std::sync::atomic::AtomicBool,
}

pub struct Rig {
    pub dir: PathBuf,
    pub bin: PathBuf,
    pub hub: Arc<Hub>,
    pub fake: Arc<FakeCloud>,
    _guard: tokio::sync::MutexGuard<'static, ()>,
}

impl Rig {
    /// A claimed computer (owner account `user_owner`, owner person `kai`) with agents `house` and `money`.
    pub async fn new() -> Self {
        let guard = ONE_AT_A_TIME.lock().await;
        let dir = std::env::temp_dir().join(format!("kith-roles-{}", uuid::Uuid::new_v4()));
        let home = dir.join("home");
        for agent in ["house", "money"] {
            std::fs::create_dir_all(home.join("agents").join(agent)).unwrap();
            std::fs::write(home.join("agents").join(agent).join("agent.md"), format!("# {agent}\n")).unwrap();
        }
        // Only the kith-auth children see these: the process environment is shared with every other test.
        *TEST_ENV.lock().unwrap() =
            vec![("KITH_AUTH", dir.join("auth.json")), ("KITH_HOME", home.clone()), ("KITH_STATE", dir.join("state"))];
        let bin = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../kernel/bin/kith-auth");
        *TEST_BIN.lock().unwrap() = Some(bin.clone());
        let store = Store::open(&dir.join("t.db")).unwrap();
        let hub = Hub::new(store, "host".into(), Identity::load_or_create(&dir).unwrap(), "token".into(), 0);
        let fake = Arc::new(FakeCloud::default());
        let base = fake_cloud(fake.clone()).await;
        hub.store.kv_set("test_cloud_url", &base).unwrap();
        // The resume half runs on its own tasks; the tests that count owed resumes switch it on themselves.
        hub.store.kv_set("cards.resume", "off").unwrap();
        cloud::update_status(&hub, |s| s.owner = Some(Owner { user_id: "user_owner".into(), email: None }));
        // As on a live relay connection whose pull was applied: bot asks don't wait for a sync.
        hub.cloud_synced.store(true, std::sync::atomic::Ordering::Release);
        let rig = Self { dir, bin, hub, fake, _guard: guard };
        rig.auth(&["person", "kai", "owner"]);
        rig
    }

    pub fn auth(&self, args: &[&str]) -> String {
        let out = self.command(args).output().unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn command(&self, args: &[&str]) -> std::process::Command {
        let mut c = std::process::Command::new(&self.bin);
        c.args(args).envs(TEST_ENV.lock().unwrap().iter().map(|(k, v)| (*k, v)));
        c
    }

    pub fn auth_ok(&self, args: &[&str]) -> bool {
        self.command(args).output().unwrap().status.success()
    }

    /// Whether kith-auth lets `person` use `agent`.
    pub fn allowed(&self, person: &str, agent: &str) -> bool {
        let v: Value = serde_json::from_str(&self.auth(&["resolve", agent, "--person", person])).unwrap();
        v["allowed"] == true
    }

    pub fn who(&self, uid: &str) -> Value {
        serde_json::from_str(&self.auth(&["who", "kith", uid])).unwrap()
    }

    /// An authorized account device of `person` (None: the owner's); a person's has shown its invite token.
    pub fn device(&self, key: &str, person: Option<&str>) -> Caller {
        let caller = self.unproven_device(key, person);
        if person.is_some() {
            self.hub.store.set_device_proven(key).unwrap();
        }
        caller
    }

    pub fn unproven_device(&self, key: &str, person: Option<&str>) -> Caller {
        self.hub
            .store
            .put_device(&Device {
                key: key.into(),
                name: "Phone".into(),
                platform: "ios".into(),
                source: DeviceSource::Account,
                grant_id: Some(format!("grt_{key}")),
                scopes: vec![Scope::Control, Scope::Screen],
                lease_until: Some(now_ms() + 60_000),
                created_at: now_ms(),
                last_seen_at: None,
                person_id: person.map(str::to_owned),
            })
            .unwrap();
        Caller::Device { key: key.into(), scopes: vec![Scope::Control, Scope::Screen] }
    }

    /// The owner's bot of an hq agent, as the hq sync makes it.
    pub fn owner_bot(&self, agent: &str) -> String {
        let cfg: BotConfig = serde_json::from_value(json!({
            "id": "", "name": agent, "backend": "claude", "cwd": self.dir.to_string_lossy(),
            "hqAgent": agent, "permission": "default", "tasks": false, "connectors": [], "skills": [],
        }))
        .unwrap();
        self.hub.create_bot(cfg).unwrap()["id"].as_str().unwrap().to_owned()
    }

    pub fn posts(&self) -> Vec<(String, Value)> {
        self.fake.posts.lock().unwrap().clone()
    }

    pub fn set_cloud_state(&self, v: Value) {
        *self.fake.state.lock().unwrap() = v;
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        *TEST_BIN.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        TEST_ENV.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
    }
}

async fn fake_cloud(fake: Arc<FakeCloud>) -> String {
    use axum::extract::{OriginalUri, State as Ax};
    use axum::http::StatusCode;
    use axum::routing::{get, post};
    let app = axum::Router::new()
        .route(
            "/v1/host/state",
            get(|Ax(f): Ax<Arc<FakeCloud>>| async move { axum::Json(f.state.lock().unwrap().clone()) }),
        )
        .route(
            "/v1/host/{*rest}",
            post(
                |Ax(f): Ax<Arc<FakeCloud>>, OriginalUri(uri): OriginalUri, axum::Json(b): axum::Json<Value>| async move {
                    let path = uri.path().to_owned();
                    f.posts.lock().unwrap().push((path.clone(), b.clone()));
                    if path == "/v1/host/invites" && f.refuse_invites.load(std::sync::atomic::Ordering::Relaxed) {
                        return (StatusCode::FORBIDDEN, axum::Json(json!({"error": {"code": "forbidden", "message": "no"}})));
                    }
                    let answer = if path.ends_with("/decision") && b["decision"] == "approve" {
                        json!({"status": "approved", "grantId": "grt_new"})
                    } else {
                        json!({"ok": true})
                    };
                    (StatusCode::OK, axum::Json(answer))
                },
            ),
        )
        .with_state(fake);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}
