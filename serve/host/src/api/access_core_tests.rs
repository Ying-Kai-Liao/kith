//! M2-17: the owner's `setPersonAccess` with `KITH_CORE_AUTHORITY` on and off, against a real kith-auth and a real core
//! daemon. On: the ticks are `people.grant` / `people.revoke` in the core, kith-auth's file is not written, a grant made
//! in the core shows in the host's listing, and a core that can't be reached is an error. Off: kith-auth as before and
//! the core untouched.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::authority::{CoreData, TEST_CORE};
use super::authority_props::{Tmp, folders};
use super::devices::Caller;
use super::dispatch;
use super::rig::Rig;
use crate::remote::members::{self, Listed};
use serde_json::{Value, json};
use std::sync::Arc;

async fn call(r: &Rig, method: &str, b: Value) -> anyhow::Result<Value> {
    dispatch(&r.hub, &Caller::Local, method, b).await
}

/// Sam, invited with `house` and `money` through kith-auth (the switch is off while this runs).
async fn joined(r: &Rig) {
    let made = call(r, "createInvite", json!({"name": "Sam", "agents": {"house": "member", "money": "member"}}))
        .await
        .unwrap();
    let listed = [Listed {
        membership_id: "mem_sam".into(),
        user_id: "user_sam".into(),
        invite_id: made["inviteId"].as_str().unwrap().to_owned(),
        ..Default::default()
    }];
    members::apply(&r.hub, Some(&r.bin), &listed, Some("user_owner")).await.unwrap();
}

/// A core daemon beside the rig's kith-auth, with the owner and `sam`. `on()` makes it the host's authority.
struct Core {
    _tmp: Tmp,
    daemon: Option<crate::core_front::TestDaemon>,
    data: Arc<CoreData>,
}

impl Core {
    fn start(r: &Rig) -> Self {
        let tmp = Tmp::new();
        let home = r.dir.join("home");
        let env = json!({"KITH_AGENTS": home.join("agents").to_string_lossy()});
        let data = Arc::new(CoreData::new(
            folders(&tmp.path().join("machine")),
            home.clone(),
            home.join("kernel"),
            env.as_object().unwrap().clone(),
        ));
        let daemon = crate::core_front::serve_for_test(&data.folders);
        let core = Self { _tmp: tmp, daemon: Some(daemon), data };
        core.ask("people.addPerson", json!({"id": "kai", "owner": true})).expect("owner");
        core.ask("people.addPerson", json!({"id": "sam"})).expect("sam");
        core
    }

    fn ask(&self, method: &str, mut params: Value) -> Option<Value> {
        params["home"] = self.data.home.to_str().unwrap().into();
        params["kernel"] = self.data.kernel.to_str().unwrap().into();
        params["env"] = Value::Object(self.data.env.clone());
        crate::core_front::ask(&self.data.folders, method, &params)
    }

    fn on(&self) {
        *TEST_CORE.lock().unwrap() = Some(self.data.clone());
    }

    /// What the core says sam holds by their own row: (agent, preset).
    fn rows(&self) -> Vec<(String, String)> {
        let list = self.ask("people.list", json!({"person": "sam"})).expect("list");
        list["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| (r["agent"].as_str().unwrap().to_owned(), r["preset"].as_str().unwrap().to_owned()))
            .collect()
    }

    fn stop(&mut self) {
        if let Some(d) = self.daemon.take() {
            d.stop();
        }
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        *TEST_CORE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        self.stop();
    }
}

#[tokio::test]
async fn with_the_switch_on_the_ticks_go_to_the_core_and_kith_auth_is_not_written() {
    let r = Rig::new().await;
    joined(&r).await;
    let core = Core::start(&r);
    core.on();

    let out = call(&r, "setPersonAccess", json!({"personId": "sam", "agents": {"money": "member"}})).await.unwrap();
    assert_eq!(core.rows(), [("money".to_owned(), "member".to_owned())]);
    assert_eq!(out["person"]["agents"], json!({"money": "member"}), "the answer has its old shape");
    assert!(r.allowed("sam", "house"), "kith-auth still has what the invite gave: it was not written");

    // Sent twice, the same.
    let again = call(&r, "setPersonAccess", json!({"personId": "sam", "agents": {"money": "member"}})).await.unwrap();
    assert_eq!(again["person"]["agents"], out["person"]["agents"]);
    assert_eq!(core.rows().len(), 1);

    // A grant made in the core shows in the listing and survives the 10-second refresh.
    core.ask("people.grant", json!({"person": "sam", "agent": "house", "preset": "guest"})).unwrap();
    let listed = call(&r, "people", json!({})).await.unwrap();
    let sam = listed["people"].as_array().unwrap().iter().find(|p| p["personId"] == "sam").unwrap();
    assert_eq!(sam["agents"], json!({"house": "guest", "money": "member"}), "{sam}");
    members::refresh_mirror(&r.hub).await;
    assert_eq!(r.hub.store.person("sam").unwrap().agents.len(), 2);

    // Unticking takes the rows away in the core; kith-auth is still not written.
    let none = call(&r, "setPersonAccess", json!({"personId": "sam", "agents": {}})).await.unwrap();
    assert_eq!(none["person"]["agents"], json!({}));
    assert_eq!(core.rows(), Vec::<(String, String)>::new());
    assert!(r.allowed("sam", "house"));
}

#[tokio::test]
async fn with_the_switch_on_a_core_that_cannot_be_reached_is_an_error_and_nothing_is_written() {
    let r = Rig::new().await;
    joined(&r).await;
    let mut core = Core::start(&r);
    core.on();
    core.stop();

    let err = call(&r, "setPersonAccess", json!({"personId": "sam", "agents": {"money": "member"}})).await.unwrap_err();
    assert!(format!("{err:#}").contains("isn't answering"), "{err:#}");
    assert!(r.allowed("sam", "house") && r.allowed("sam", "money"), "no silent fallback wrote kith-auth");
}

#[tokio::test]
async fn with_the_switch_off_kith_auth_is_written_and_the_core_is_not_touched() {
    let r = Rig::new().await;
    joined(&r).await;
    let core = Core::start(&r);

    let out = call(&r, "setPersonAccess", json!({"personId": "sam", "agents": {"money": "member"}})).await.unwrap();
    assert_eq!(out["person"]["agents"], json!({"money": "member"}));
    assert!(!r.allowed("sam", "house"), "kith-auth took house away");
    assert!(r.allowed("sam", "money"));
    assert!(core.rows().is_empty(), "the core was never asked");
}
