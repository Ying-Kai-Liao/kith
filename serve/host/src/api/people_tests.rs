//! Removing a person, against a real kith-auth in a temp folder: what the host holds goes even when kith-auth refuses.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::devices::Caller;
use super::dispatch;
use super::rig::Rig;
use crate::remote::members::{self, Listed};
use serde_json::{Value, json};

async fn call(r: &Rig, method: &str, b: Value) -> anyhow::Result<Value> {
    dispatch(&r.hub, &Caller::Local, method, b).await
}

async fn joined(r: &Rig) {
    let made = call(r, "createInvite", json!({"name": "Sam", "agents": {"house": "member", "money": "member"}}))
        .await
        .unwrap();
    let listed = [Listed {
        membership_id: "mem_sam".into(),
        user_id: "user_sam".into(),
        invite_id: made["inviteId"].as_str().unwrap().to_owned(),
    }];
    members::apply(&r.hub, Some(&r.bin), &listed, Some("user_owner")).await.unwrap();
    // A second, still open invite for the same person.
    call(r, "createInvite", json!({"personId": "sam", "agents": {"house": "member"}})).await.unwrap();
}

fn lose_agent_folder(r: &Rig, agent: &str) {
    std::fs::remove_dir_all(r.dir.join("home").join("agents").join(agent)).unwrap();
}

#[tokio::test]
async fn revoking_a_person_whose_agent_folder_is_gone_still_removes_their_devices_and_invites() {
    let r = Rig::new().await;
    joined(&r).await;
    let _phone = r.device("sam-phone", Some("sam"));
    lose_agent_folder(&r, "house");

    let out = call(&r, "revokePerson", json!({"personId": "sam"})).await.unwrap();
    assert_eq!(out["revoked"], true);
    assert!(r.hub.store.devices_of("sam").unwrap().is_empty());
    assert_eq!(r.hub.store.memberships().unwrap(), []);
    assert!(r.hub.store.invites().unwrap().iter().all(|i| !i.open(crate::store::now_ms())));
    assert!(!r.allowed("sam", "money"), "revoked in kith-auth, rows or not");
    // Twice is fine.
    assert_eq!(call(&r, "revokePerson", json!({"personId": "sam"})).await.unwrap()["revoked"], true);
}

#[tokio::test]
async fn changing_access_survives_a_lost_agent_folder() {
    let r = Rig::new().await;
    joined(&r).await;
    lose_agent_folder(&r, "house");
    let out = call(&r, "setPersonAccess", json!({"personId": "sam", "agents": {"money": "member"}})).await.unwrap();
    assert_eq!(out["person"]["agents"], json!({"money": "member"}));
    assert!(r.allowed("sam", "money"));
}
