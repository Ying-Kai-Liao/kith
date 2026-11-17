//! The host's reconcile of a revoke made in the core alone (`remote::core_revokes`), against a real core daemon with the
//! host switched. The rows are `Reconcile.tla`'s (`verification/revoke-access/`): a core-only revoke reaches the host
//! within one tick, a fail-closed cut survives a tick, an app undo racing a tick ends restored, and the reconcile never
//! gives back what it did not take.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::access_core_tests::Core;
use super::devices::Caller;
use super::dispatch;
use super::rig::Rig;
use crate::remote::core_revokes::{Due, owed, reconcile};
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
    let mut done = BTreeSet::new();
    reconcile(&r.hub, &mut done).await;
    reconcile(&r.hub, &mut done).await;
    assert!(set_aside(&r, "sam"));
    assert!(done.iter().any(|c| c.0 == "sam"), "the core still shows them revoked");
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
async fn a_person_renamed_in_the_core_is_not_cut_off_here() {
    let (r, core) = switched_with_sam().await;
    // the host's rows stay under `sam`
    core.ask("people.renamePerson", json!({"id": "sam", "to": "samuel"})).expect("rename");
    reconcile(&r.hub, &mut BTreeSet::new()).await;
    assert!(held(&r, "sam") && !set_aside(&r, "sam"), "a rename is no removal");
}

#[tokio::test]
async fn a_person_renamed_then_revoked_in_the_core_is_set_aside_under_the_old_id() {
    let (r, core) = switched_with_sam().await;
    core.ask("people.renamePerson", json!({"id": "sam", "to": "samuel"})).expect("rename");
    core.ask("people.suspend", json!({"id": "samuel"})).expect("suspend");
    let mut done = BTreeSet::new();
    reconcile(&r.hub, &mut done).await;
    assert!(set_aside(&r, "sam") && !held(&r, "sam"), "the rows under the old id are set aside");
    let before = r.hub.store.suspended_people().unwrap();
    reconcile(&r.hub, &mut done).await;
    assert_eq!(r.hub.store.suspended_people().unwrap(), before, "a second look changes nothing");
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
    reconcile(&r.hub, &mut BTreeSet::new()).await;
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

/// sam renamed samuel in the core (the host keeps sam's rows and device under `sam`), a room turn whose basis names
/// `sam`, then the core down. Answers the rig, the core (stopped) and the turn's key.
async fn renamed_with_the_core_down() -> (Rig, Core, &'static str) {
    let (r, mut core) = switched_with_sam().await;
    core.ask("people.renamePerson", json!({"id": "sam", "to": "samuel"})).expect("rename");
    r.device("sam-mac", Some("sam"));
    let turn = "room-turn-of-sam";
    r.hub.room_turn_begins(turn, &["sam".into()]);
    core.stop();
    (r, core, turn)
}

/// Kai revokes samuel and the core write fails (`local_revoke`'s failure branch, the core down after the person was
/// read): cut fail-closed by the id given, and owed under every other.
async fn revoke_samuel_without_the_core(r: &Rig, turn: &str) {
    assert!(crate::api::people::local_revoke(&r.hub, &r.bin, "samuel", true).await.is_err(), "not taken by the core");
    assert_eq!(owed(&r.hub), ["samuel"], "the ids couldn't be read: the cut is owed");
    // The window the design leaves: nothing names `sam` while the core is down.
    assert!(held(r, "sam") && r.hub.room_turn_registered(turn));
}

/// Finding 1 of PR #1020 (`ReconcileOwed.tla` fix.cfg, FailCutReached): the owed cut is paid under every id once the core
/// answers.
#[tokio::test]
async fn a_renamed_persons_revoke_without_the_core_cuts_the_old_id_once_the_core_is_back() {
    let (r, mut core, turn) = renamed_with_the_core_down().await;
    revoke_samuel_without_the_core(&r, turn).await;
    // a tick while the core is still down: the row stays
    Due::default().tick(&r.hub, true).await;
    assert_eq!(owed(&r.hub), ["samuel"]);
    core.restart();
    Due::default().tick(&r.hub, false).await;
    assert!(set_aside(&r, "sam") && !held(&r, "sam"), "sam's device and membership are set aside");
    assert!(!r.hub.room_turn_registered(turn), "and the room turn whose basis names sam is stopped");
    assert!(owed(&r.hub).is_empty(), "paid: the row is dropped");
}

/// `revoke_person`'s own core-down branch (the person read from this host's record) owes the cut the same way.
#[tokio::test]
async fn the_app_revoke_with_the_core_down_owes_the_cut_too() {
    let (r, mut core, turn) = renamed_with_the_core_down().await;
    let said = call(&r, "revokePerson", json!({"personId": "sam"})).await;
    assert!(said.is_err(), "the owner is told the revoke is not complete: {said:?}");
    assert_eq!(owed(&r.hub), ["sam"]);
    assert!(set_aside(&r, "sam") && !r.hub.room_turn_registered(turn), "cut by the id given at once");
    core.restart();
    Due::default().tick(&r.hub, false).await;
    assert!(owed(&r.hub).is_empty(), "paid under samuel and sam");
}

/// `undo-keeps.cfg` (NoLostUndo): an undo before the poll pays the row gives sam back and clears it, so no later tick
/// cuts a person the owner restored.
#[tokio::test]
async fn an_undo_before_the_owed_cut_is_paid_restores_and_clears_it() {
    let (r, mut core, turn) = renamed_with_the_core_down().await;
    revoke_samuel_without_the_core(&r, turn).await;
    core.restart();
    call(&r, "unrevokePerson", json!({"personId": "samuel"})).await.unwrap();
    assert!(owed(&r.hub).is_empty(), "the undo cleared the owed cut");
    Due::default().tick(&r.hub, true).await;
    assert!(held(&r, "sam") && !set_aside(&r, "sam"), "undone, and kept so");
}

/// The cloud's undo (`apply_revoked`'s `--unrevoke`, from `pull`) clears an owed cut in its own locked section too, under
/// every id: a row kept under the old id is dropped by an undo of the new one.
#[tokio::test]
async fn the_clouds_undo_clears_an_owed_cut_too() {
    let (r, core) = switched_with_sam().await;
    core.ask("people.renamePerson", json!({"id": "sam", "to": "samuel"})).expect("rename");
    members::locked_run(&r.bin, &["handle", "samuel", "kith", "user_sam"]).await.unwrap();
    let cut = members::apply_revoked(&r.hub, Some(&r.bin), Some(&["user_sam".to_owned()])).await.unwrap();
    assert_eq!(cut.cut, ["samuel"], "the cloud's revoke applied");
    crate::remote::core_revokes::owe_cut(&r.hub, "sam");
    let undone = members::apply_revoked(&r.hub, Some(&r.bin), Some(&[])).await.unwrap();
    assert_eq!(undone.restored, ["samuel"]);
    assert!(owed(&r.hub).is_empty(), "the cloud's undo cleared the row kept under the old id");
}

/// Revoke refusal wording: the renamed person's NEW id is not in the host's record, so the core being down refuses the
/// revoke before any cut. The owner reads that nobody was revoked, why, and what to do.
#[tokio::test]
async fn a_revoke_of_the_new_id_with_the_core_down_says_it_did_not_happen_and_what_to_do() {
    let (r, _core, _turn) = renamed_with_the_core_down().await;
    let said = call(&r, "revokePerson", json!({"personId": "samuel"})).await.unwrap_err().to_string();
    assert!(said.contains("samuel was not revoked"), "{said}");
    assert!(said.contains("core isn't running") && said.contains("Start it, then revoke again"), "{said}");
    assert!(!said.contains("Changes made before this point may stand"), "{said}");
    assert!(owed(&r.hub).is_empty() && !set_aside(&r, "sam"), "nothing was cut: the words are true");
    let undo = call(&r, "unrevokePerson", json!({"personId": "samuel"})).await.unwrap_err().to_string();
    assert!(undo.contains("samuel was not restored") && undo.contains("undo the revoke again"), "{undo}");
}

/// The undo-read-fail row: an undo that can't read the ids is given back by the one id and says so, until a later undo
/// reads them.
#[tokio::test]
async fn an_undo_that_cannot_read_the_ids_leaves_a_note_until_a_later_undo_reads_them() {
    let (r, mut core, _turn) = renamed_with_the_core_down().await;
    let ids = ["samuel".to_owned(), "sam".to_owned()];
    crate::remote::core_revokes::restore_held(&r.hub, "sam").await;
    let note = crate::remote::core_revokes::undo_note(&r.hub, &ids).expect("the owner is told it isn't finished");
    assert!(note.contains("core isn't running") && note.contains("undo again"), "{note}");
    let audit = r.hub.store.audit_list(0, Some("sam")).unwrap();
    assert!(audit.iter().any(|a| a.method == "undo.unread"), "{audit:?}");
    core.restart();
    crate::remote::core_revokes::restore_held(&r.hub, "samuel").await;
    assert!(crate::remote::core_revokes::undo_note(&r.hub, &ids).is_none(), "a later undo read the ids");
}

/// A cut owed under a former id when the person was undone while the ids were unreadable (the undo's clear could not
/// find the row) is dropped, never paid; a revoke after that undo is paid as before.
#[tokio::test]
async fn an_owed_cut_older_than_an_undo_that_could_not_read_the_ids_is_dropped_not_paid() {
    let (r, mut core, turn) = renamed_with_the_core_down().await;
    revoke_samuel_without_the_core(&r, turn).await;
    // the undo (by the new id) with the core down: its read of the ids fails, so the row under `sam` is not found
    crate::remote::core_revokes::owe_cut(&r.hub, "sam");
    crate::remote::core_revokes::clear_owed(&r.hub, "samuel").await;
    assert_eq!(owed(&r.hub), ["sam"], "an undo that could not read the ids left the former id's row");
    core.restart();
    Due::default().tick(&r.hub, false).await;
    assert!(owed(&r.hub).is_empty(), "the row is gone");
    assert!(held(&r, "sam") && !set_aside(&r, "sam"), "the undone person was not cut");
}
