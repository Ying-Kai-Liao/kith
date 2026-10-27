//! Revoking a person and undoing it (designs/revoke-person), against a real kith-auth in a temp folder, a temp store and
//! a fake cloud: the cloud first, rows kept, undo exact, refused when the cloud can't be reached, a revoke from another
//! Mac applied by the pull.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::devices::Caller;
use super::dispatch;
use super::rig::Rig;
use crate::remote::cloud;
use crate::remote::members::{self, Listed};
use serde_json::{Value, json};

async fn call(r: &Rig, method: &str, b: Value) -> anyhow::Result<Value> {
    dispatch(&r.hub, &Caller::Local, method, b).await
}

/// `sam`, invited to house and money, bound to `user_sam`, with one phone; `garden` is everyone's.
async fn joined(r: &Rig) -> Caller {
    let made =
        call(r, "createInvite", json!({"name": "Sam", "agents": {"house": "member", "money": "guest"}})).await.unwrap();
    let listed = [Listed {
        membership_id: "mem_sam".into(),
        user_id: "user_sam".into(),
        invite_id: made["inviteId"].as_str().unwrap().to_owned(),
        ..Default::default()
    }];
    members::apply(&r.hub, Some(&r.bin), &listed, Some("user_owner")).await.unwrap();
    r.device("sam-phone", Some("sam"))
}

/// Every row kith-auth holds for who may use what.
fn rows(r: &Rig) -> Value {
    let d: Value = serde_json::from_str(&std::fs::read_to_string(r.dir.join("auth.json")).unwrap()).unwrap();
    json!([d["members"], d["grants"]])
}

fn posted(r: &Rig, path: &str) -> Option<usize> {
    r.posts().iter().position(|(p, _)| p == path)
}

#[tokio::test]
async fn a_revoke_goes_through_the_cloud_first_keeps_every_row_and_the_undo_gives_them_back() {
    let r = Rig::new().await;
    let phone = joined(&r).await;
    r.auth(&["member", "money", "*"]);
    let before = rows(&r);
    assert!(r.allowed("sam", "house") && r.allowed("sam", "money"));

    let out = call(&r, "revokePerson", json!({"personId": "sam"})).await.unwrap();
    assert_eq!(out, json!({"revoked": true, "everywhere": true}));
    // The cloud is told before anything changes here (R6, R9), and the membership is cut after it.
    let revoke = posted(&r, "/v1/host/people/revoke").expect("the cloud was told");
    assert_eq!(r.posts()[revoke].1, json!({"userId": "user_sam"}));
    assert!(posted(&r, "/v1/host/memberships/mem_sam/revoke").is_some_and(|m| m > revoke));
    // Refused everywhere, the everyone row included (R1, R2); the rows and the handle stay.
    assert!(!r.allowed("sam", "house") && !r.allowed("sam", "money"));
    assert_eq!(rows(&r), before, "no row is touched");
    assert_eq!(r.who("user_sam")["person"], "sam");
    assert_eq!(r.who("user_sam")["revoked"], true);
    assert!(r.hub.store.devices_of("sam").unwrap().is_empty());
    assert!(dispatch(&r.hub, &phone, "hello", json!({})).await.is_err(), "their phone is out");
    let listed = call(&r, "people", json!({})).await.unwrap();
    assert_eq!(listed["people"][0]["revoked"], true);

    let out = call(&r, "unrevokePerson", json!({"personId": "sam"})).await.unwrap();
    assert_eq!(out["revoked"], false);
    assert_eq!(r.posts().last().unwrap(), &("/v1/host/people/unrevoke".to_owned(), json!({"userId": "user_sam"})));
    // Exactly what they had (R4); their app needs a new invite, as the membership was cut.
    assert_eq!(rows(&r), before);
    assert!(r.allowed("sam", "house") && r.allowed("sam", "money"));
    assert_eq!(r.who("user_sam")["revoked"], false);
    assert_eq!(call(&r, "people", json!({})).await.unwrap()["people"][0]["revoked"], false);
}

#[tokio::test]
async fn with_the_cloud_unreachable_a_revoke_is_refused_and_changes_nothing() {
    let r = Rig::new().await;
    joined(&r).await;
    r.hub.store.kv_set("test_cloud_url", "http://127.0.0.1:1").unwrap();
    let e = call(&r, "revokePerson", json!({"personId": "sam"})).await.unwrap_err();
    assert!(format!("{e:#}").contains("Kith cloud"), "{e:#}");
    assert!(r.allowed("sam", "house"), "nothing changed here (R9)");
    assert_eq!(r.who("user_sam")["revoked"], false);
    assert_eq!(r.hub.store.devices_of("sam").unwrap().len(), 1);
    assert_eq!(r.hub.store.memberships().unwrap().len(), 1);
}

#[tokio::test]
async fn an_undo_needs_the_cloud_too() {
    let r = Rig::new().await;
    joined(&r).await;
    call(&r, "revokePerson", json!({"personId": "sam"})).await.unwrap();
    let live = cloud::url(&r.hub.store).unwrap();
    r.hub.store.kv_set("test_cloud_url", "http://127.0.0.1:1").unwrap();
    assert!(call(&r, "unrevokePerson", json!({"personId": "sam"})).await.is_err());
    assert!(!r.allowed("sam", "house"), "still revoked (R9)");
    r.hub.store.kv_set("test_cloud_url", &live).unwrap();
    call(&r, "unrevokePerson", json!({"personId": "sam"})).await.unwrap();
    assert!(r.allowed("sam", "house"));
}

#[tokio::test]
async fn a_person_known_only_here_is_revoked_here_without_the_cloud() {
    let r = Rig::new().await;
    r.auth(&["person", "tom"]);
    r.auth(&["member", "house", "tom"]);
    r.auth(&["handle", "tom", "telegram", "42"]);
    r.hub.store.kv_set("test_cloud_url", "http://127.0.0.1:1").unwrap();
    let out = call(&r, "revokePerson", json!({"personId": "tom"})).await.unwrap();
    assert_eq!(out, json!({"revoked": true, "everywhere": false}));
    assert!(!r.allowed("tom", "house"));
    call(&r, "unrevokePerson", json!({"personId": "tom"})).await.unwrap();
    assert!(r.allowed("tom", "house"));
}

#[tokio::test]
async fn the_owner_a_bot_and_a_stranger_cannot_be_revoked() {
    let r = Rig::new().await;
    joined(&r).await;
    r.auth(&["bot", "sambot", "--of", "sam"]);
    for who in ["kai", "sambot", "nobody"] {
        assert!(call(&r, "revokePerson", json!({"personId": who})).await.is_err(), "{who}");
    }
    assert!(posted(&r, "/v1/host/people/revoke").is_none());
}

#[tokio::test]
async fn a_revoke_on_another_mac_arrives_with_the_pull_and_so_does_its_undo() {
    let r = Rig::new().await;
    joined(&r).await;
    let before = rows(&r);
    let base = cloud::url(&r.hub.store).unwrap();
    // The cloud cut the membership and lists the account as revoked.
    r.set_cloud_state(
        json!({"owner": {"userId": "user_owner"}, "memberships": [], "grants": [], "requests": [], "revoked": ["user_sam"]}),
    );
    cloud::pull(&r.hub, &base).await.unwrap();
    assert!(!r.allowed("sam", "house"));
    assert_eq!(r.who("user_sam")["revoked"], true);
    assert_eq!(r.who("user_sam")["person"], "sam", "the handle stays for the bridge and the undo");
    assert!(r.hub.store.devices_of("sam").unwrap().is_empty());
    assert_eq!(rows(&r), before);
    // Pulling again changes nothing.
    cloud::pull(&r.hub, &base).await.unwrap();
    assert_eq!(r.who("user_sam")["revoked"], true);

    r.set_cloud_state(
        json!({"owner": {"userId": "user_owner"}, "memberships": [], "grants": [], "requests": [], "revoked": []}),
    );
    cloud::pull(&r.hub, &base).await.unwrap();
    assert!(r.allowed("sam", "house"));
    assert_eq!(rows(&r), before);

    // A cloud that predates revoking (no `revoked`) changes nothing.
    r.auth(&["person", "sam", "--revoke", "--local"]);
    r.set_cloud_state(json!({"owner": {"userId": "user_owner"}, "memberships": [], "grants": [], "requests": []}));
    cloud::pull(&r.hub, &base).await.unwrap();
    assert_eq!(r.who("user_sam")["revoked"], true);
}

#[tokio::test]
async fn a_change_the_cloud_announces_during_a_pull_keeps_the_host_out_of_sync() {
    use crate::remote::sync_lease;
    let r = Rig::new().await;
    let mark = sync_lease::mark(&r.hub);
    sync_lease::changed(&r.hub);
    sync_lease::pulled(&r.hub, mark);
    assert!(!sync_lease::wait(&r.hub, std::time::Duration::from_millis(150)).await, "a stale pull doesn't sync");
    let mark = sync_lease::mark(&r.hub);
    sync_lease::pulled(&r.hub, mark);
    assert!(sync_lease::wait(&r.hub, std::time::Duration::ZERO).await);
    let (linked, until) = sync_lease::read(&sync_lease::path()).unwrap();
    assert!(linked && until > crate::store::now_ms(), "the bridge's lease runs");
    sync_lease::changed(&r.hub);
    assert_eq!(sync_lease::read(&sync_lease::path()), Some((true, 0)), "and ends the moment the cloud changes");
}
