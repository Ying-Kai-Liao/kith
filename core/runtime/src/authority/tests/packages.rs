//! A package folder that does not answer, seen from the authority rules: the narrower answer, in its own words, and
//! nothing written for it. (The rest of the authority suite runs through the package cache too: see `Rig::call`.)
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::FUTURE;
use super::writes::{Rig, base};
use crate::packages::PackageCache;

/// Makes the rig's refresher one whose read never ends (until the flag is cleared).
fn stuck(rig: &mut Rig) -> Arc<AtomicBool> {
    let hold = Arc::new(AtomicBool::new(true));
    let h = hold.clone();
    rig.cache = PackageCache::start_with(
        Some(Arc::new(move || {
            while h.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(10));
            }
        })),
        Duration::from_millis(200),
    );
    hold
}

fn world() -> Value {
    let mut w = base();
    w["members"] =
        json!({"house": {"sam": {"preset": "member", "helper_once": {"garden": FUTURE}}}});
    w
}

#[test]
fn an_owner_write_naming_an_agent_that_cannot_be_read_is_refused_and_writes_nothing() {
    let mut rig = Rig::new(&base());
    let hold = stuck(&mut rig);
    let began = Instant::now();
    let said = rig
        .call(
            "people.grant",
            &json!({"person": "sam", "agent": "house", "preset": "member"}),
        )
        .unwrap_err();
    assert!(said.contains("can't read house's package"), "{said}");
    assert!(
        began.elapsed() < Duration::from_secs(2),
        "{:?}",
        began.elapsed()
    );
    assert!(rig.tables()["members"].get("house").is_none());
    // Once the disk answers the same call goes through.
    hold.store(false, Ordering::SeqCst);
    rig.cache = PackageCache::start(None);
    rig.call(
        "people.grant",
        &json!({"person": "sam", "agent": "house", "preset": "member"}),
    )
    .unwrap();
    assert!(rig.tables()["members"]["house"].get("sam").is_some());
}

#[test]
fn a_guests_agent_that_cannot_be_read_is_owner_only_and_the_owner_is_unchanged() {
    let mut rig = Rig::new(&world());
    let hold = stuck(&mut rig);
    let began = Instant::now();
    let guest = rig
        .call(
            "people.resolve",
            &json!({"agent": "house", "person": "sam"}),
        )
        .unwrap();
    assert!(
        began.elapsed() < Duration::from_secs(2),
        "{:?}",
        began.elapsed()
    );
    assert_eq!(guest["allowed"], false, "{guest}");
    assert!(
        guest.to_string().contains("can't read house's package"),
        "{guest}"
    );
    let owner = rig
        .call(
            "people.resolve",
            &json!({"agent": "house", "person": "kai"}),
        )
        .unwrap();
    assert_eq!(owner["allowed"], true, "{owner}");
    hold.store(false, Ordering::SeqCst);
}

// --- the folders of a person and the kernel's service specs: never read under the lock, and a disk that does not answer
// --- gives the narrower answer for each use ------------------------------------------------------------------------

use std::path::PathBuf;

use crate::authority::revoke::person_folders;

pub(super) fn blocked(rig: &mut Rig) -> Arc<AtomicBool> {
    stuck(rig)
}

fn put(rig: &Rig, rel: &str) -> PathBuf {
    let p = rig.machine_dir.path().join(rel);
    std::fs::create_dir_all(&p).unwrap();
    p
}

const NO_FOLDERS: &str = "can't read the agents folders: the disk did not answer, try again";

fn people(rig: &mut Rig) -> Vec<String> {
    rig.tables()["people"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect()
}

#[test]
fn a_read_of_a_persons_folders_under_the_lock_panics() {
    // The positive control of the structural check: the same read the call path now keeps out of the lock.
    crate::packages::forbid_disk_under_lock();
    let rig = Rig::new(&base());
    let agents = rig.c.agents.clone();
    let state = rig.machine_dir.path().join("state");
    crate::daemon::mark_lock_held(true);
    let caught = std::panic::catch_unwind(|| person_folders(&agents, &state, "sam"));
    crate::daemon::mark_lock_held(false);
    assert!(caught.is_err(), "a read under the lock must panic");
    // and under no lock it is a plain read
    assert_eq!(person_folders(&agents, &state, "sam").len(), 0);
}

#[test]
fn a_new_id_is_refused_when_its_folders_cannot_be_looked_at_and_works_after() {
    let mut rig = Rig::new(&base());
    let hold = stuck(&mut rig);
    let began = Instant::now();
    let said = rig
        .call("people.addPerson", &json!({"id": "zed"}))
        .unwrap_err();
    assert_eq!(said, NO_FOLDERS);
    assert!(
        began.elapsed() < Duration::from_secs(2),
        "{:?}",
        began.elapsed()
    );
    assert!(!people(&mut rig).contains(&"zed".to_owned()));
    // the rename's new id is held to the same
    let said = rig
        .call("people.renamePerson", &json!({"id": "sam", "to": "zed"}))
        .unwrap_err();
    assert_eq!(said, NO_FOLDERS);
    assert!(people(&mut rig).contains(&"sam".to_owned()));
    hold.store(false, Ordering::SeqCst);
    rig.cache = PackageCache::start(None);
    rig.call("people.addPerson", &json!({"id": "zed"})).unwrap();
    assert!(people(&mut rig).contains(&"zed".to_owned()));
}

#[test]
fn the_retired_line_and_an_unused_removal_say_the_disk_did_not_answer_never_no_folders() {
    let mut rig = Rig::new(&base());
    let hold = stuck(&mut rig);
    let said = rig
        .call("people.retired", &json!({"id": "nobody"}))
        .unwrap();
    assert_eq!(said["retired"], true, "{said}");
    assert_eq!(said["lines"], json!([NO_FOLDERS]));
    let said = rig
        .call("people.removePerson", &json!({"id": "amy", "unused": true}))
        .unwrap_err();
    assert_eq!(said, NO_FOLDERS);
    assert!(people(&mut rig).contains(&"amy".to_owned()));
    hold.store(false, Ordering::SeqCst);
}

#[test]
fn a_removal_goes_ahead_without_the_folders_and_says_it_cannot_tell() {
    let mut rig = Rig::new(&base());
    put(&rig, "agents/house/memory/people/amy");
    let hold = stuck(&mut rig);
    let began = Instant::now();
    let lines = rig
        .lines("people.removePerson", &json!({"id": "amy"}))
        .unwrap();
    assert!(
        began.elapsed() < Duration::from_secs(2),
        "{:?}",
        began.elapsed()
    );
    assert!(
        lines[0].contains("can't tell which of their memory and files are kept"),
        "{lines:?}"
    );
    assert!(!people(&mut rig).contains(&"amy".to_owned()));
    hold.store(false, Ordering::SeqCst);
}

#[test]
fn a_narrowing_never_waits_on_the_folders_and_commits_with_the_disk_stuck() {
    let mut rig = Rig::new(&base());
    let hold = stuck(&mut rig);
    for (m, p) in [
        ("people.suspend", json!({"id": "sam"})),
        ("people.undo", json!({"id": "sam"})),
        ("people.suspend", json!({"id": "amy"})),
        ("people.removeBot", json!({"id": "bot-a"})),
    ] {
        let began = Instant::now();
        rig.call(m, &p).unwrap();
        assert!(
            began.elapsed() < Duration::from_millis(500),
            "{m}: {:?}",
            began.elapsed()
        );
    }
    assert!(rig.tables()["people"]["amy"].get("revoked").is_some());
    assert!(rig.tables()["people"].get("bot-a").is_none());
    hold.store(false, Ordering::SeqCst);
}

#[test]
fn a_rename_leaves_its_folders_to_the_mover_and_reports_what_moved() {
    let mut rig = Rig::new(&base());
    let mem = put(&rig, "agents/house/memory/people/sam");
    // The structural check is on (the rig marks the lock held for the call): the move happened after, outside it.
    let lines = rig
        .lines("people.renamePerson", &json!({"id": "sam", "to": "anna"}))
        .unwrap();
    assert!(
        lines[0].contains("moved their memory and files in house"),
        "{lines:?}"
    );
    assert!(!mem.exists());
    assert!(
        rig.machine_dir
            .path()
            .join("agents/house/memory/people/anna")
            .exists()
    );
}

#[test]
fn a_rename_hands_the_move_the_old_ids_it_wrote_itself_and_leaves_no_trace_in_the_answer() {
    let mut rig = Rig::new(&base());
    let first = put(&rig, "agents/house/memory/people/sam");
    // A to B to C, one commit after the other: each move is built from the ids its own rename wrote to the row.
    let one = rig
        .call("people.renamePerson", &json!({"id": "sam", "to": "anna"}))
        .unwrap();
    let two = rig
        .call("people.renamePerson", &json!({"id": "anna", "to": "bea"}))
        .unwrap();
    for said in [&one, &two] {
        assert!(said.get("lifeWas").is_none(), "{said}");
        assert!(said.get("lifeMove").is_none(), "{said}");
    }
    assert_eq!(
        rig.tables()["people"]["bea"]["was"],
        json!(["samuel", "sam", "anna"])
    );
    assert!(!first.exists());
    let home = rig.machine_dir.path().join("agents/house/memory/people");
    assert!(home.join("bea").exists());
    assert!(!home.join("anna").exists());
}
