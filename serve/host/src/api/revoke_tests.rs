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
    // The cloud is told before anything changes here (R6, R9); it cuts the membership itself, so this Mac does not.
    let revoke = posted(&r, "/v1/host/people/revoke").expect("the cloud was told");
    assert_eq!(r.posts()[revoke].1, json!({"userId": "user_sam"}));
    assert!(posted(&r, "/v1/host/memberships/mem_sam/revoke").is_none(), "undo needs the membership");
    // Refused everywhere, the everyone row included (R1, R2); the rows and the handle stay.
    assert!(!r.allowed("sam", "house") && !r.allowed("sam", "money"));
    assert_eq!(rows(&r), before, "no row is touched");
    assert_eq!(r.who("user_sam")["person"], "sam");
    assert_eq!(r.who("user_sam")["revoked"], true);
    assert!(r.hub.store.devices_of("sam").unwrap().is_empty());
    assert!(r.hub.store.memberships().unwrap().is_empty(), "a membership grants nothing while revoked");
    assert!(r.hub.store.invites().unwrap().iter().all(|i| i.person_id != "sam" || !i.open(crate::store::now_ms())));
    assert_eq!(
        r.hub.store.suspended_of("sam").unwrap(),
        crate::store::Moved { devices: 1, memberships: 1, invites: 0 }
    );
    assert!(dispatch(&r.hub, &phone, "hello", json!({})).await.is_err(), "their phone is out");
    let listed = call(&r, "people", json!({})).await.unwrap();
    assert_eq!(listed["people"][0]["revoked"], true);

    let out = call(&r, "unrevokePerson", json!({"personId": "sam"})).await.unwrap();
    assert_eq!(out["revoked"], false);
    assert_eq!(r.posts().last().unwrap(), &("/v1/host/people/unrevoke".to_owned(), json!({"userId": "user_sam"})));
    // Exactly what they had (R4): rows, membership and phone.
    assert_eq!(rows(&r), before);
    assert_eq!(r.hub.store.memberships().unwrap().len(), 1);
    assert_eq!(r.hub.store.devices_of("sam").unwrap().len(), 1);
    assert!(dispatch(&r.hub, &phone, "hello", json!({})).await.is_ok(), "their phone is back");
    // Revoke, undo, revoke, undo again: each is idempotent and nothing is lost or doubled.
    for _ in 0..2 {
        call(&r, "revokePerson", json!({"personId": "sam"})).await.unwrap();
        call(&r, "revokePerson", json!({"personId": "sam"})).await.unwrap();
        call(&r, "unrevokePerson", json!({"personId": "sam"})).await.unwrap();
        call(&r, "unrevokePerson", json!({"personId": "sam"})).await.unwrap();
    }
    assert_eq!(r.hub.store.memberships().unwrap().len(), 1);
    assert_eq!(r.hub.store.devices_of("sam").unwrap().len(), 1);
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
    let invite_id = r.hub.store.memberships().unwrap()[0].invite_id.clone();
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

    // The cloud's own undo lists the account's membership again, and grants its device anew under another id (the relay
    // lifts its block only for another grant); this Mac gives back what the revoke kept.
    let membership = r.hub.store.suspended_of("sam").unwrap();
    assert_eq!(membership.memberships, 1, "the membership waited for the undo");
    r.set_cloud_state(json!({"owner": {"userId": "user_owner"}, "requests": [], "revoked": [],
        "grants": [{"grantId": "grt_sam-phone-again", "deviceKey": "sam-phone", "userId": "user_sam"}],
        "memberships": [{"membershipId": "mem_sam", "userId": "user_sam", "inviteId": invite_id}]}));
    cloud::pull(&r.hub, &base).await.unwrap();
    assert!(r.allowed("sam", "house"));
    assert_eq!(r.hub.store.memberships().unwrap().len(), 1, "their membership is back");
    assert_eq!(r.hub.store.devices_of("sam").unwrap().len(), 1, "and their phone");
    assert_eq!(rows(&r), before);

    // A cloud that predates revoking (no `revoked`) changes nothing.
    r.auth(&["person", "sam", "--revoke", "--local"]);
    r.set_cloud_state(json!({"owner": {"userId": "user_owner"}, "memberships": [], "grants": [], "requests": []}));
    cloud::pull(&r.hub, &base).await.unwrap();
    assert_eq!(r.who("user_sam")["revoked"], true);
}

#[tokio::test]
async fn a_revoke_applies_while_the_cloud_names_another_owner() {
    use crate::remote::crypto::{self, b64};
    let r = Rig::new().await;
    joined(&r).await;
    let base = cloud::url(&r.hub.store).unwrap();
    // A host claimed before pins: what it knows of its owner is the account it bound to kai.
    r.hub.store.kv_set(crate::remote::account::APPLIED, "user_owner").unwrap();
    cloud::set_approval(&r.hub, cloud::Approval::Auto).unwrap();
    r.device("kai-phone", None);
    let lease = |key: &str| r.hub.store.device(key).unwrap().lease_until.unwrap();
    let before = lease("kai-phone");
    let (dk, nd): ([u8; 32], [u8; 32]) = (crypto::random(), crypto::random());
    let request = json!({"requestId": "req_1", "deviceKey": b64(&dk), "userId": "user_mallory", "deviceName": "Phone",
        "platform": "ios", "commit": b64(&crypto::sas_commit(crypto::Labels::Kith, &dk, &nd)), "createdAt": 0, "expiresAt": 0});
    r.set_cloud_state(json!({"owner": {"userId": "user_mallory"}, "requests": [request], "revoked": ["user_sam"],
        "grants": [{"grantId": "grt_kai-phone", "deviceKey": "kai-phone", "userId": "user_mallory"}]}));
    cloud::pull(&r.hub, &base).await.unwrap();

    let status = r.hub.cloud.status();
    assert_eq!(status.owner.unwrap().user_id, "user_owner", "pinned from what this Mac knew, not from the cloud");
    assert_eq!(status.owner_change.unwrap().owner.unwrap().user_id, "user_mallory");
    assert!(!status.owner_from_cloud);
    // The revoke in the same pull applies.
    assert_eq!(r.who("user_sam")["revoked"], true);
    assert!(!r.allowed("sam", "house"));
    // Nothing that depends on the owner does.
    assert_eq!(lease("kai-phone"), before, "the owner's device is neither renewed nor removed");
    assert_eq!(r.hub.cloud.requests_json(), Vec::<Value>::new(), "no access request is taken up");
    assert!(r.posts().iter().all(|(p, _)| !p.contains("access-requests")), "nor answered");
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

/// A run of `person` that a restarted host lost track of: a live claim naming them, held by a process of its own.
fn claimed_run(r: &Rig, sid: &str, person: &str) -> std::process::Child {
    let child = std::process::Command::new("sleep").arg("300").spawn().unwrap();
    let state = r.hub.terminal.env().state;
    crate::terminal::claim::take_as(
        &state,
        sid,
        crate::terminal::claim::Kind::Bot,
        child.id(),
        false,
        Some(person),
        Some("house"),
    )
    .unwrap()
    .unwrap();
    child
}

#[tokio::test]
async fn a_revoke_sweeps_the_persons_runs_and_what_their_sessions_are_owed() {
    let r = Rig::new().await;
    joined(&r).await;
    let state = r.hub.terminal.env().state;
    let mut run = claimed_run(&r, "sess-sam", "sam");
    let mut owners = claimed_run(&r, "sess-kai", "kai");
    // sam is owed a wake (a card answer, an ask's reply).
    let owed = crate::wake::Request {
        session: "sess-sam",
        id: "owed-1",
        agent: "house",
        person: Some("sam"),
        room: None,
        host_door: true,
        why: "a reply",
        text: "an answer",
    };
    crate::wake::enqueue(&state, &owed).unwrap();
    assert_eq!(crate::wake::status(&state, "owed-1").as_deref(), Some("queued"));

    call(&r, "revokePerson", json!({"personId": "sam"})).await.unwrap();
    assert_eq!(
        crate::wake::status(&state, "owed-1").as_deref(),
        Some("withdrawn"),
        "no wake reaches them after the revoke"
    );
    assert!(run.wait().is_ok(), "their run is ended");
    assert!(owners.try_wait().unwrap().is_none(), "the owner's claim is never touched");
    let _ = owners.kill();
    let _ = owners.wait();
}

#[tokio::test]
async fn a_removal_deletes_what_a_revoke_keeps() {
    let r = Rig::new().await;
    joined(&r).await;
    call(&r, "revokePerson", json!({"personId": "sam"})).await.unwrap();
    assert_eq!(r.hub.store.suspended_of("sam").unwrap().memberships, 1);
    // Removed while still revoked (kith-auth --rm has no host step): the next pull forgets what was kept.
    r.auth(&["person", "sam", "--rm"]);
    r.set_cloud_state(
        json!({"owner": {"userId": "user_owner"}, "memberships": [], "grants": [], "requests": [], "revoked": []}),
    );
    cloud::pull(&r.hub, &cloud::url(&r.hub.store).unwrap()).await.unwrap();
    assert_eq!(r.hub.store.suspended_of("sam").unwrap(), crate::store::Moved::default());
    assert_eq!(r.hub.store.suspended_people().unwrap(), Vec::<String>::new());
}

#[tokio::test]
async fn a_membership_gone_from_the_cloud_while_not_revoked_is_a_removal() {
    let r = Rig::new().await;
    joined(&r).await;
    call(&r, "revokePerson", json!({"personId": "sam"})).await.unwrap();
    call(&r, "unrevokePerson", json!({"personId": "sam"})).await.unwrap();
    r.set_cloud_state(
        json!({"owner": {"userId": "user_owner"}, "memberships": [], "grants": [], "requests": [], "revoked": []}),
    );
    cloud::pull(&r.hub, &cloud::url(&r.hub.store).unwrap()).await.unwrap();
    assert!(r.hub.store.memberships().unwrap().is_empty() && r.hub.store.devices_of("sam").unwrap().is_empty());
    assert_eq!(r.hub.store.suspended_of("sam").unwrap(), crate::store::Moved::default());
}

#[tokio::test]
async fn a_handle_revoke_arrives_from_a_paired_computer_and_the_latest_change_wins() {
    use crate::remote::handle_revokes::exchange_in;
    use crate::store::HandleRevoke;
    let r = Rig::new().await;
    r.auth(&["person", "tom"]);
    r.auth(&["member", "house", "tom"]);
    r.auth(&["handle", "tom", "telegram", "42"]);
    let rev = |revoked, at| HandleRevoke { handle: "telegram:42".into(), revoked, at };
    let sent = vec![rev(true, 2_000), HandleRevoke { handle: "kith:user_x".into(), revoked: true, at: 9 }];
    let mine = exchange_in(&r.hub, sent).await.unwrap();
    assert!(!r.allowed("tom", "house"), "revoked from the other computer");
    assert_eq!(mine, vec![rev(true, 2_000)], "a Kith account is not a handle; it is never taken from a peer");
    // A late, older undo loses; a newer one wins; a repeat is harmless.
    exchange_in(&r.hub, vec![rev(false, 1_000)]).await.unwrap();
    assert!(!r.allowed("tom", "house"));
    exchange_in(&r.hub, vec![rev(false, 3_000)]).await.unwrap();
    assert!(r.allowed("tom", "house"));
    exchange_in(&r.hub, vec![rev(false, 3_000)]).await.unwrap();
    assert!(r.allowed("tom", "house"));
}

#[tokio::test]
async fn a_handle_revoke_waits_for_a_computer_that_is_off_and_arrives_when_it_is_on() {
    use crate::remote::handle_revokes::{due, exchange_with};
    let r = Rig::new().await;
    r.auth(&["person", "tom"]);
    r.auth(&["member", "house", "tom"]);
    r.auth(&["handle", "tom", "telegram", "42"]);
    call(&r, "revokePerson", json!({"personId": "tom"})).await.unwrap();
    let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    // The peer is off: nothing is acknowledged, so it stays owed.
    let off = exchange_with(&r.hub, "cmp_b", |_| async { Err(anyhow::anyhow!("not reachable")) }).await;
    assert!(off.is_err() && due(&r.hub, "cmp_b"));
    // It is on again: it gets the revoke, and tells what it knows.
    let seen = sent.clone();
    let there = vec![crate::store::HandleRevoke { handle: "telegram:77".into(), revoked: true, at: 5 }];
    exchange_with(&r.hub, "cmp_b", |list| async move {
        *seen.lock().unwrap() = list;
        Ok(there)
    })
    .await
    .unwrap();
    let got = sent.lock().unwrap().clone();
    assert_eq!(got.len(), 1);
    assert!(got[0].revoked && got[0].handle == "telegram:42");
    assert!(!due(&r.hub, "cmp_b"), "acknowledged now");
    assert!(due(&r.hub, "cmp_c"), "another computer that never heard is still owed");
}

#[tokio::test]
async fn a_revoke_made_on_a_clock_behind_the_kept_stamp_is_still_newer_than_it() {
    let r = Rig::new().await;
    r.auth(&["person", "tom"]);
    r.auth(&["member", "house", "tom"]);
    r.auth(&["handle", "tom", "telegram", "42"]);
    // This clock runs an hour behind the computer that stamped the last change.
    let ahead = crate::store::now_ms() + 3_600_000;
    let kept = crate::store::HandleRevoke { handle: "telegram:42".into(), revoked: false, at: ahead };
    assert!(r.hub.store.put_handle_revoke(&kept).unwrap());
    call(&r, "revokePerson", json!({"personId": "tom"})).await.unwrap();
    let mine = r.hub.store.handle_revokes().unwrap();
    assert!(mine[0].revoked && mine[0].at > ahead, "a stamp that is not newer is ignored and no computer would hear");
}

#[tokio::test]
async fn a_stamp_from_far_in_the_future_is_refused() {
    use crate::remote::handle_revokes::exchange_in;
    let r = Rig::new().await;
    r.auth(&["person", "tom"]);
    r.auth(&["member", "house", "tom"]);
    r.auth(&["handle", "tom", "telegram", "42"]);
    let at = crate::store::now_ms() + 3_600_000;
    let future = crate::store::HandleRevoke { handle: "telegram:42".into(), revoked: true, at };
    let mine = exchange_in(&r.hub, vec![future]).await.unwrap();
    assert!(r.allowed("tom", "house"), "not applied");
    assert!(mine.is_empty(), "and not kept, or it would win over every real change after it");
}
