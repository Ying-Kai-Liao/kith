//! The host's reconcile of a revoke made in the core alone (`remote::core_revokes`), against a real core daemon with the
//! host switched. The rows are `Reconcile.tla`'s (`verification/revoke-access/`): a core-only revoke reaches the host
//! within one tick, a fail-closed cut survives a tick, an app undo racing a tick ends restored, and the reconcile never
//! gives back what it did not take.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::access_core_tests::Core;
use super::devices::Caller;
use super::dispatch;
use super::rig::Rig;
use crate::remote::core_revokes::{Due, reconcile};
use crate::remote::members::{self, Listed};
use serde_json::{Value, json};
use std::collections::BTreeSet;

async fn call(r: &Rig, method: &str, b: Value) -> anyhow::Result<Value> {
    dispatch(&r.hub, &Caller::Local, method, b).await
}

/// Sam, invited to `house` as a member (the switch is off while this runs), then a core with the owner and Sam, switched.
async fn switched_with_sam() -> (Rig, Core) {
    let r = Rig::new().await;
    let made = call(&r, "createInvite", json!({"name": "Sam", "agents": {"house": "member"}})).await.unwrap();
    let listed = [Listed {
        membership_id: "mem_sam".into(),
        user_id: "user_sam".into(),
        invite_id: made["inviteId"].as_str().unwrap().to_owned(),
        ..Default::default()
    }];
    members::apply(&r.hub, Some(&r.bin), &listed, Some("user_owner")).await.unwrap();
    let core = Core::start(&r);
    core.on();
    core.switched();
    (r, core)
}

fn held(r: &Rig, who: &str) -> bool {
    r.hub.store.held_people().unwrap().iter().any(|p| p == who)
}

fn set_aside(r: &Rig, who: &str) -> bool {
    r.hub.store.suspended_people().unwrap().iter().any(|p| p == who)
}

#[tokio::test]
async fn a_core_only_suspend_sets_the_person_aside_here_within_one_tick() {
    let (r, core) = switched_with_sam().await;
    assert!(held(&r, "sam") && !set_aside(&r, "sam"));
    // a quiet tick (nothing changed, not the first): sam is untouched
    let mut due = Due::default();
    due.tick(&r.hub, false).await;
    assert!(held(&r, "sam"), "the core shows them active");
    // the terminal's spelling: unmarked, tells the host nothing
    core.ask("people.suspend", json!({"id": "sam"})).expect("suspend");
    due.tick(&r.hub, true).await;
    assert!(set_aside(&r, "sam") && !held(&r, "sam"), "the next tick cut them off here");
    // a second look changes nothing and stops no one again
    let mut done = BTreeSet::from(["sam".to_owned()]);
    reconcile(&r.hub, &mut done).await;
    assert!(set_aside(&r, "sam"));
    assert!(done.contains("sam"), "the core still shows them revoked");
}

#[tokio::test]
async fn a_core_only_removal_is_cut_too() {
    let (r, core) = switched_with_sam().await;
    core.ask("people.removePerson", json!({"id": "sam"})).expect("removal");
    let listed = core.ask("people.listPeople", json!({})).unwrap();
    assert!(!listed["rows"].as_array().unwrap().iter().any(|p| p["id"] == "sam"), "the core no longer lists them");
    reconcile(&r.hub, &mut BTreeSet::new()).await;
    assert!(set_aside(&r, "sam") && !held(&r, "sam"));
}

#[tokio::test]
async fn a_fail_closed_cut_survives_a_tick_because_the_tick_never_restores() {
    let (r, _core) = switched_with_sam().await;
    // an app revoke whose core write failed: cut here, the core still has them active
    crate::api::people::cut_off(&r.hub, "sam").await;
    assert!(set_aside(&r, "sam"));
    let mut due = Due::default();
    due.tick(&r.hub, true).await;
    due.tick(&r.hub, true).await;
    assert!(set_aside(&r, "sam"), "a tick gives nothing back");
}

#[tokio::test]
async fn an_app_undo_racing_a_tick_ends_restored() {
    let (r, core) = switched_with_sam().await;
    core.ask("people.suspend", json!({"id": "sam"})).expect("suspend");
    // The tick is due and waits for the lock the app's write holds: it reads only once the undo has landed.
    let lock = crate::api::people::AUTH_WRITES.lock().await;
    let tick = {
        let hub = r.hub.clone();
        tokio::spawn(async move { Due::default().tick(&hub, true).await })
    };
    tokio::task::yield_now().await;
    core.ask("people.undo", json!({"id": "sam"})).expect("undo");
    crate::api::people::restored(&r.hub, "sam");
    drop(lock);
    tick.await.unwrap();
    assert!(held(&r, "sam") && !set_aside(&r, "sam"), "the tick read after the undo and left them alone");
    // and after the host set them aside, an app undo still brings them back and a later tick leaves them
    core.ask("people.suspend", json!({"id": "sam"})).expect("suspend");
    reconcile(&r.hub, &mut BTreeSet::new()).await;
    assert!(set_aside(&r, "sam"));
    crate::api::people::local_revoke(&r.hub, &r.bin, "sam", false).await.unwrap();
    reconcile(&r.hub, &mut BTreeSet::from(["sam".to_owned()])).await;
    assert!(held(&r, "sam") && !set_aside(&r, "sam"), "undone from the app, restored, and kept so");
}

fn hq_paths(r: &Rig) -> crate::channels::hq::Paths {
    crate::channels::hq::Paths {
        hq_bin: r.bin.clone(),
        auth_bin: r.bin.clone(),
        machine: r.dir.clone(),
        agent_roots: Vec::new(),
        data: r.dir.clone(),
    }
}

/// "Link Telegram" and chat's `/link CODE` on a switched host: the core makes and spends the code (the real kith-auth, which
/// exits 3 on the tombstone, is never the one asked), refusals are the core's words with no `kith-auth:` in front, and a
/// core that is down gives one plain line.
#[tokio::test]
async fn a_switched_host_makes_and_spends_a_link_code_in_the_core() {
    let (r, mut core) = switched_with_sam().await;
    let said = call(&r, "linkCode", json!({})).await.unwrap();
    let a_code = said["code"].as_str().unwrap().to_owned();
    assert_eq!(a_code.len(), 6);
    let rows = core.ask("people.invites", json!({})).unwrap();
    assert!(rows["rows"].as_array().unwrap().iter().any(|i| i["code"] == a_code && i["person"] == "kai"), "{rows}");
    // the owner's code binds the owner's account; the same code again, and a uid that is another person's, are refused
    let paths = hq_paths(&r);
    assert_eq!(crate::channels::hq::link(&paths, "telegram", "4242", &a_code).await, Ok("kai".to_owned()));
    let again = crate::channels::hq::link(&paths, "telegram", "4242", &a_code).await.unwrap_err();
    assert!(again.starts_with("that code is unknown or expired"), "{again}");
    let sam = core.ask("people.invite", json!({"person": "sam", "minutes": 15, "viaSwitch": true})).unwrap();
    let sam_code = sam["code"].as_str().unwrap().to_owned();
    let taken = crate::channels::hq::link(&paths, "telegram", "4242", &sam_code).await.unwrap_err();
    assert_eq!(taken, "telegram 4242 is already kai");
    // a cut-off person's code reads as an unknown one, and stays
    core.ask("people.suspend", json!({"id": "sam"})).unwrap();
    let revoked = crate::channels::hq::link(&paths, "telegram", "777", &sam_code).await.unwrap_err();
    assert!(revoked.starts_with("that code is unknown or expired"), "{revoked}");
    // the core down: one plain line, nothing of a path, a socket or a method
    core.stop();
    let down = crate::channels::hq::link(&paths, "telegram", "888", &sam_code).await.unwrap_err();
    assert_eq!(down, "Linking isn't available on this computer right now.");
    let down = call(&r, "linkCode", json!({})).await.unwrap_err().to_string();
    assert_eq!(down, "Linking isn't available on this computer right now.");
}

#[tokio::test]
async fn a_core_that_cannot_be_read_changes_nothing() {
    let (r, mut core) = switched_with_sam().await;
    core.ask("people.suspend", json!({"id": "sam"})).expect("suspend");
    core.stop();
    reconcile(&r.hub, &mut BTreeSet::new()).await;
    assert!(held(&r, "sam") && !set_aside(&r, "sam"), "no read, no act");
}
