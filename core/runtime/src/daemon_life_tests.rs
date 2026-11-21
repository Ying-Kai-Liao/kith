//! A person's folders through the real daemon: nothing under the core lock reads the agents or state folders (the hook of
//! `packages::forbid_disk_under_lock` panics the thread that does), a disk that does not answer gives each rule its
//! narrower answer, and a rename's folders move after the lock is let go, in the order the renames committed.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{Config, Daemon, Reached, put, quiet, read, serving, until};
use crate::tests::{machine, paths};

fn people(auth: &Value) -> Value {
    json!({"people": auth})
}

/// A daemon whose core holds `kai` (the owner), `sam` and `amy`, with the check that no folder is read under the lock on.
fn life_daemon(m: &Path, probe: Option<crate::packages::Probe>, auth: &Value) -> Daemon {
    crate::packages::forbid_disk_under_lock();
    let d = serving(
        &paths(m),
        Config {
            package_probe: probe,
            ..quiet()
        },
    );
    d.shared()
        .with_core(|c| {
            crate::authority::load_for_tests(c.conn(), &people(auth));
            Ok(())
        })
        .unwrap();
    d
}

fn world() -> Value {
    json!({"kai": {"owner": true}, "sam": {"name": "Sam"}, "amy": {}})
}

fn write(m: &Path, method: &str, params: &Value) -> Reached {
    crate::reach(&paths(m), method, params, false)
}

fn lines(r: Reached) -> Vec<String> {
    match r {
        Reached::Answered(v) => v["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l.as_str().unwrap().to_owned())
            .collect(),
        other => panic!("{other:?}"),
    }
}

fn text(r: &Reached) -> String {
    format!("{r:?}")
}

/// A probe that holds the package threads while the flag is set (`only_moves`: just the folder mover).
fn probe(hold: &Arc<AtomicBool>, only_moves: bool) -> crate::packages::Probe {
    let h = hold.clone();
    Arc::new(move || {
        while h.load(Ordering::SeqCst) && (!only_moves || crate::packages::current_job() == "move")
        {
            std::thread::sleep(Duration::from_millis(10));
        }
    })
}

fn rows_named(d: &Daemon, id: &str) -> i64 {
    let id = id.to_owned();
    d.shared()
        .with_core(move |c| {
            Ok(c.conn()
                .query_row("SELECT COUNT(*) FROM people WHERE id = ?1", [&id], |r| {
                    r.get::<_, i64>(0)
                })?)
        })
        .unwrap()
}

#[test]
fn every_row_that_looks_at_a_persons_folders_runs_through_the_daemon_without_a_read_under_the_lock()
{
    let (_t, m) = machine();
    let d = life_daemon(&m, None, &world());
    put(&m.join("house/memory/people/ghost/n.md"), "x");
    put(&m.join("garden/files/people/amy/f.md"), "x");
    put(&m.join("house/memory/people/sam/n.md"), "x");
    put(&m.join("people/sam.md"), "y");
    // the id-free check of addPerson
    let r = write(&m, "people.addPerson", &json!({"id": "ghost"}));
    assert!(
        text(&r).contains("memory and files of a removed 'ghost' are still in house"),
        "{r:?}"
    );
    assert_eq!(
        lines(write(&m, "people.addPerson", &json!({"id": "zed"}))).len(),
        1
    );
    // people.retired
    match read(&paths(&m), "people.retired", &json!({"id": "ghost"})) {
        Reached::Answered(v) => {
            assert_eq!(v["retired"], true, "{v}");
            assert!(v["lines"][0].as_str().unwrap().contains("still in house"));
        }
        other => panic!("{other:?}"),
    }
    // remove --unused: the folders decide the refusal
    let r = write(
        &m,
        "people.removePerson",
        &json!({"id": "amy", "unused": true}),
    );
    assert!(text(&r).contains("amy is not unused (garden)"), "{r:?}");
    // person_remove words its line with them once it is committed: the line waits for the facts, the removal does not
    let said = lines(write(&m, "people.removePerson", &json!({"id": "amy"})));
    assert!(
        said[0].contains("their memory and files in garden are kept"),
        "{said:?}"
    );
    // the rename's `to` is checked, then the folders move once the lock is let go
    let said = lines(write(
        &m,
        "people.renamePerson",
        &json!({"id": "sam", "to": "sally"}),
    ));
    assert!(
        said[0].contains("moved their memory and files in house, people"),
        "{said:?}"
    );
    assert!(m.join("house/memory/people/sally/n.md").exists());
    assert!(m.join("people/sally.md").exists());
    assert!(!m.join("house/memory/people/sam").exists());
    // the positive control: the same read made under the lock panics, so the rows above were not silent passes
    let shared = d.shared().clone();
    let (agents, state) = (m.clone(), m.clone());
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        shared.with_core(|_| {
            crate::authority::revoke::person_folders(&agents, &state, "sam");
            Ok(())
        })
    }));
    assert!(caught.is_err(), "a folder read under the lock must panic");
}

#[test]
fn with_the_package_disk_stuck_a_new_id_is_refused_and_the_narrowings_commit_and_it_works_after() {
    let (_t, m) = machine();
    let hold = Arc::new(AtomicBool::new(false));
    let d = life_daemon(&m, Some(probe(&hold, false)), &world());
    // a call that looks at no folder, to start from a working refresher
    hold.store(true, Ordering::SeqCst);
    let began = Instant::now();
    let r = write(&m, "people.addPerson", &json!({"id": "zed"}));
    assert!(
        text(&r).contains("can't read the agents folders: the disk did not answer"),
        "{r:?}"
    );
    assert!(
        began.elapsed() < crate::packages::REFRESH_WAIT + Duration::from_millis(2500),
        "{:?}",
        began.elapsed()
    );
    assert_eq!(rows_named(&d, "zed"), 0);
    // the narrowings and the readers don't wait on a folder
    let began = Instant::now();
    assert_eq!(
        lines(write(&m, "people.suspend", &json!({"id": "sam"}))).len(),
        1
    );
    // The removal commits (and its revoke takes effect) well before the answer: only the kept line waits for the folders.
    let asked = {
        let m = m.clone();
        std::thread::spawn(move || write(&m, "people.removePerson", &json!({"id": "amy"})))
    };
    until("the removal to commit", || rows_named(&d, "amy") == 0);
    assert!(
        began.elapsed() < crate::packages::REFRESH_WAIT / 2,
        "the commit waited for the disk: {:?}",
        began.elapsed()
    );
    assert!(
        !asked.is_finished(),
        "the answer was not waiting for the folders"
    );
    let said = lines(asked.join().unwrap());
    assert!(
        said[0].contains("can't tell which of their memory and files are kept"),
        "{said:?}"
    );
    match read(
        &paths(&m),
        "people.may",
        &json!({"person": "kai", "agent": "house"}),
    ) {
        Reached::Answered(_) => {}
        other => panic!("{other:?}"),
    }
    // the disk answers again
    hold.store(false, Ordering::SeqCst);
    until("a new id to be accepted", || {
        matches!(
            write(&m, "people.addPerson", &json!({"id": "zed"})),
            Reached::Answered(_)
        )
    });
    assert_eq!(rows_named(&d, "zed"), 1);
}

#[test]
fn a_rename_with_the_mover_blocked_answers_with_the_note_and_the_folders_follow_when_it_is_let_go()
{
    let (_t, m) = machine();
    let hold = Arc::new(AtomicBool::new(true));
    let d = life_daemon(&m, Some(probe(&hold, true)), &world());
    put(&m.join("house/memory/people/sam/n.md"), "x");
    let began = Instant::now();
    let said = lines(write(
        &m,
        "people.renamePerson",
        &json!({"id": "sam", "to": "sally"}),
    ));
    assert!(
        began.elapsed() < crate::packages::MOVE_WAIT + Duration::from_secs(3),
        "{:?}",
        began.elapsed()
    );
    assert!(
        said.contains(&"note: the folders of sam move to sally when the disk answers".to_owned()),
        "{said:?}"
    );
    assert_eq!((rows_named(&d, "sally"), rows_named(&d, "sam")), (1, 0));
    assert!(m.join("house/memory/people/sam/n.md").exists());
    hold.store(false, Ordering::SeqCst);
    until("the folders to follow", || {
        m.join("house/memory/people/sally/n.md").exists()
    });
    assert!(!m.join("house/memory/people/sam").exists());
}

#[test]
fn a_chain_of_two_renames_with_the_mover_blocked_ends_under_the_last_id_and_nothing_under_the_middle()
 {
    let (_t, m) = machine();
    let hold = Arc::new(AtomicBool::new(true));
    let d = life_daemon(&m, Some(probe(&hold, true)), &world());
    put(&m.join("house/memory/people/sam/n.md"), "x");
    put(&m.join("garden/files/people/sam/f.md"), "x");
    // A to B: the answer comes with the note. B to C is decided while A's folders still sit under A.
    let first = lines(write(
        &m,
        "people.renamePerson",
        &json!({"id": "sam", "to": "sally"}),
    ));
    assert!(
        first.iter().any(|l| l.contains("when the disk answers")),
        "{first:?}"
    );
    let second = lines(write(
        &m,
        "people.renamePerson",
        &json!({"id": "sally", "to": "sue"}),
    ));
    assert!(
        second.iter().any(|l| l.contains("when the disk answers")),
        "{second:?}"
    );
    assert_eq!((rows_named(&d, "sue"), rows_named(&d, "sally")), (1, 0));
    hold.store(false, Ordering::SeqCst);
    until("everything to be under sue", || {
        m.join("house/memory/people/sue/n.md").exists()
            && m.join("garden/files/people/sue/f.md").exists()
    });
    for (agent, kind) in [("house", "memory"), ("garden", "files")] {
        for old in ["sam", "sally"] {
            assert!(
                !m.join(agent).join(kind).join("people").join(old).exists(),
                "{agent}/{kind}/{old}"
            );
        }
    }
}

#[test]
fn a_start_repairs_what_an_earlier_rename_left_under_an_old_id() {
    let (_t, m) = machine();
    let auth = json!({
        "kai": {"owner": true},
        "carol": {"was": ["alice"]},
        "erin": {"was": ["eve"]},
    });
    drop(life_daemon(&m, None, &auth));
    put(&m.join("house/memory/people/alice/n.md"), "x");
    put(&m.join("people/alice.md"), "y");
    // a destination that exists is left alone
    put(&m.join("house/memory/people/eve/old.md"), "x");
    put(&m.join("house/memory/people/erin/new.md"), "x");
    // a removed person has no row: whatever they left is untouched
    put(&m.join("house/memory/people/gone/n.md"), "x");
    let _d = serving(&paths(&m), quiet());
    until("the folders to be under carol", || {
        m.join("house/memory/people/carol/n.md").exists() && m.join("people/carol.md").exists()
    });
    assert!(!m.join("house/memory/people/alice").exists());
    assert!(m.join("house/memory/people/eve/old.md").exists());
    assert!(m.join("house/memory/people/erin/new.md").exists());
    assert!(m.join("house/memory/people/gone/n.md").exists());
}
