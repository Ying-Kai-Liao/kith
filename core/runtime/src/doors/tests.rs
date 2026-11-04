//! The doors' state and sweep (M2-14): the start against the sweep on one row, the revoke hook, the dead supervisor,
//! and the same answers as `kernel/lib/kith_door.py`'s sweep rule for the same sequences.
#![allow(
    unknown_lints,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::many_single_char_names,
    clippy::fn_params_excessive_bools,
    clippy::used_underscore_binding,
    clippy::assert_is_empty
)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};

use kith_kernel::secrets::{Ctx, Sandbox, ServeDir};
use serde_json::{Value, json};

use super::call::call_at;
use super::{Live, Start, State, end, list, mark_live, start, sweep};
use crate::authority::call::call_with;
use crate::db::Core;
use crate::error::Error;
use crate::gate::Liveness;
use crate::layout::Layout;
use crate::sync_lease;

const T0: i64 = 1_000_000;
/// The clock of the authority writes (ms): the moment a suspension records and a hook sweeps at.
const NOW: f64 = 1_800_000_000_000.0;

/// Every pid reads this start time.
struct Started(&'static str);

impl Liveness for Started {
    fn start(&self, _: u32) -> Option<String> {
        Some(self.0.to_owned())
    }
    fn gone(&self, _: u32) -> bool {
        false
    }
}

/// No start time can be read and no process is reported gone: a probe that failed.
struct Nobody;

impl Liveness for Nobody {
    fn start(&self, _: u32) -> Option<String> {
        None
    }
    fn gone(&self, _: u32) -> bool {
        false
    }
}

/// No start time can be read and the system says every process is gone.
struct Gone;

impl Liveness for Gone {
    fn start(&self, _: u32) -> Option<String> {
        None
    }
    fn gone(&self, _: u32) -> bool {
        true
    }
}

struct World {
    _t: tempfile::TempDir,
    _agents: tempfile::TempDir,
    core: Core,
    c: Ctx,
}

/// A machine with the agents `house` and `garden`, and core.db with the owner `kai`, `sam` and `amy`, a bot `bot-a` of
/// sam, and `house` shared with sam and his bot. The cloud is off on purpose, so a non-owner's door is in sync.
fn world() -> World {
    let (t, m) = crate::tests::machine();
    let core = crate::tests::open_all(&Layout::new(&m));
    let agents = tempfile::tempdir().unwrap();
    for a in ["house", "garden"] {
        let d = agents.path().join("agents").join(a).join("role");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("agent.md"), format!("---\nname: {a}\n---\n")).unwrap();
    }
    let c = Ctx {
        auth: agents.path().join("secrets/auth.json"),
        agents: agents.path().join("agents"),
        kernel: agents.path().join("kernel"),
        person: None,
        now_ms: NOW,
        sandbox: Sandbox::Fixed(false),
        serve: ServeDir::Fixed(agents.path().join("serve")),
    };
    let mut w = World {
        _t: t,
        _agents: agents,
        core,
        c,
    };
    say(
        &mut w,
        "people.addPerson",
        &json!({"id": "kai", "owner": true}),
    );
    for p in ["sam", "amy"] {
        say(&mut w, "people.addPerson", &json!({"id": p}));
    }
    say(
        &mut w,
        "people.addBot",
        &json!({"id": "bot-a", "of": "sam"}),
    );
    for (p, preset) in [("sam", "member"), ("bot-a", "guest")] {
        say(
            &mut w,
            "people.grant",
            &json!({"agent": "house", "person": p, "preset": preset}),
        );
    }
    sync_lease::unlink(&mut w.core, &sync_lease::System).unwrap();
    w
}

fn say(w: &mut World, method: &str, params: &Value) -> Value {
    call_with(&mut w.core, &w.c, method, params).unwrap_or_else(|e| panic!("{method}: {e}"))
}

fn begin(w: &mut World, person: &str) -> super::Door {
    begin_with(w, person, None).unwrap().door
}

fn begin_with(w: &mut World, person: &str, id: Option<&str>) -> Result<super::Started, Error> {
    start(
        &mut w.core,
        &w.c,
        &Started("T1"),
        &Start {
            id,
            agent: "house",
            door: "render",
            person,
            folder: "/files/x",
            pid: 4242,
        },
        T0,
    )
}

fn sweep_all(w: &mut World, persons: &[&str], recheck: bool, live: &dyn Liveness) -> super::Swept {
    let persons: Vec<String> = persons.iter().map(|p| (*p).to_owned()).collect();
    sweep(&mut w.core, &w.c, live, &persons, recheck, T0 + 10).unwrap()
}

fn state_of(w: &mut World, id: &str) -> State {
    list(&mut w.core, true)
        .unwrap()
        .into_iter()
        .find(|d| d.id == id)
        .unwrap_or_else(|| panic!("no door {id}"))
        .state
}

fn open_of(w: &mut World, person: &str) -> usize {
    list(&mut w.core, false)
        .unwrap()
        .iter()
        .filter(|d| d.person == person && matches!(d.state, State::Starting | State::Live))
        .count()
}

fn refusal(e: &Error) -> String {
    match e {
        Error::Refused(m) => m.clone(),
        other => panic!("not a refusal: {other}"),
    }
}

// --- start -----------------------------------------------------------------------------------------------------------

#[test]
fn a_start_writes_one_starting_row_and_a_retry_with_its_id_is_the_same_door() {
    let mut w = world();
    let d = begin(&mut w, "sam");
    assert_eq!(
        (d.state, d.version, d.pid, d.proc_start.as_str()),
        (State::Starting, 0, 4242, "T1")
    );
    assert_eq!(d.id.len(), 16);
    let again = begin_with(&mut w, "sam", Some(&d.id)).unwrap();
    assert!(again.repeat);
    assert_eq!(again.door, d);
    assert_eq!(list(&mut w.core, true).unwrap().len(), 1);
    // the same id for another person is not the same door
    let e = begin_with(&mut w, "kai", Some(&d.id)).unwrap_err();
    assert!(refusal(&e).contains("starting already"), "{e}");
}

#[test]
fn a_person_who_is_cut_off_starts_nothing() {
    let mut w = world();
    say(&mut w, "people.suspend", &json!({"id": "sam"}));
    let e = begin_with(&mut w, "sam", None).unwrap_err();
    assert!(refusal(&e).contains("access was removed"), "{e}");
    assert!(list(&mut w.core, true).unwrap().is_empty(), "no row");
}

#[test]
fn a_bot_of_a_revoked_person_starts_nothing() {
    let mut w = world();
    assert!(
        begin_with(&mut w, "bot-a", None).is_ok(),
        "before the revoke"
    );
    say(&mut w, "people.suspend", &json!({"id": "sam"}));
    let e = begin_with(&mut w, "bot-a", None).unwrap_err();
    assert!(refusal(&e).contains("access was removed"), "{e}");
    assert_eq!(open_of(&mut w, "bot-a"), 0);
}

#[test]
fn a_person_with_no_access_to_the_agent_and_an_unknown_one_start_nothing() {
    let mut w = world();
    assert!(begin_with(&mut w, "amy", None).is_err(), "amy has no row");
    assert!(begin_with(&mut w, "nobody", None).is_err());
    let e = start(
        &mut w.core,
        &w.c,
        &Started("T1"),
        &Start {
            id: None,
            agent: "garden",
            door: "render",
            person: "sam",
            folder: "",
            pid: 4242,
        },
        T0,
    )
    .unwrap_err();
    assert!(
        matches!(e, Error::Refused(_)),
        "sam has no row on garden: {e}"
    );
    assert!(list(&mut w.core, true).unwrap().is_empty());
}

#[test]
fn only_the_owner_starts_while_the_mac_is_out_of_sync() {
    let mut w = world();
    // a Mac never told about the cloud: fail closed for anyone but the owner (the model's DoorGate)
    sync_lease::link(&mut w.core, &sync_lease::System).unwrap();
    let e = begin_with(&mut w, "sam", None).unwrap_err();
    assert!(refusal(&e).contains("out of sync"), "{e}");
    assert!(begin_with(&mut w, "kai", None).is_ok());
}

#[test]
fn a_supervisor_that_is_not_running_starts_nothing() {
    let mut w = world();
    let e = start(
        &mut w.core,
        &w.c,
        &Gone,
        &Start {
            id: None,
            agent: "house",
            door: "render",
            person: "sam",
            folder: "",
            pid: 4242,
        },
        T0,
    )
    .unwrap_err();
    assert!(refusal(&e).contains("not running"), "{e}");
}

// --- start against sweep ---------------------------------------------------------------------------------------------

#[test]
fn a_sweep_that_got_there_first_makes_the_late_mark_live_lose() {
    let mut w = world();
    let d = begin(&mut w, "sam");
    let out = sweep_all(&mut w, &["sam"], true, &Started("T1"));
    assert_eq!(out.swept.len(), 1);
    assert_eq!(
        (out.swept[0].state, out.swept[0].version),
        (State::Swept, 1)
    );
    match mark_live(&mut w.core, &d.id, d.version, T0 + 20).unwrap() {
        Live::Lost { state, .. } => assert_eq!(state, "swept"),
        Live::Live(_) => panic!("a swept door went live"),
    }
    assert_eq!(state_of(&mut w, &d.id), State::Swept);
}

#[test]
fn mark_live_goes_by_the_version_and_is_idempotent() {
    let mut w = world();
    let d = begin(&mut w, "sam");
    // a stale version does not win the row
    assert!(matches!(
        mark_live(&mut w.core, &d.id, 7, T0).unwrap(),
        Live::Lost {
            state: "starting",
            ..
        }
    ));
    let Live::Live(live) = mark_live(&mut w.core, &d.id, d.version, T0).unwrap() else {
        panic!("not live")
    };
    assert_eq!((live.state, live.version), (State::Live, 1));
    // a repeat finds it live: the same answer, the box is not told to stop
    assert!(matches!(
        mark_live(&mut w.core, &d.id, d.version, T0).unwrap(),
        Live::Live(_)
    ));
    assert!(matches!(
        mark_live(&mut w.core, "0000000000000000", 0, T0).unwrap(),
        Live::Lost { state: "gone", .. }
    ));
}

#[test]
fn a_swept_or_ended_door_is_never_started_again_under_its_id() {
    let mut w = world();
    let a = begin(&mut w, "sam");
    sweep_all(&mut w, &["sam"], true, &Started("T1"));
    let e = begin_with(&mut w, "sam", Some(&a.id)).unwrap_err();
    assert!(refusal(&e).contains("swept already"), "{e}");
    let b = begin(&mut w, "sam");
    end(&mut w.core, &b.id, T0).unwrap();
    let e = begin_with(&mut w, "sam", Some(&b.id)).unwrap_err();
    assert!(refusal(&e).contains("ended already"), "{e}");
}

#[test]
fn threads_starting_doors_while_a_revoke_commits_leave_none_open() {
    // One Core per process, as in the daemon (a second open of core.db is refused by the lifetime lock), so the threads
    // share it behind a Mutex: what is checked is that the row decides, not who got there first.
    for round in 0..20 {
        let World {
            _t,
            _agents,
            core,
            c,
        } = world();
        let c = Arc::new(c);
        let core = Arc::new(Mutex::new(core));
        let go = Arc::new(Barrier::new(7));
        let started = Arc::new(AtomicUsize::new(0));
        let mut workers = Vec::new();
        for n in 0..6 {
            let (core, c, go, started) = (core.clone(), c.clone(), go.clone(), started.clone());
            workers.push(std::thread::spawn(move || {
                go.wait();
                let who = if n % 2 == 0 { "sam" } else { "bot-a" };
                for _ in 0..4 {
                    let r = start(
                        &mut core.lock().unwrap(),
                        &c,
                        &Started("T1"),
                        &Start {
                            id: None,
                            agent: "house",
                            door: "render",
                            person: who,
                            folder: "",
                            pid: 4242,
                        },
                        T0,
                    );
                    match r {
                        Ok(s) => {
                            started.fetch_add(1, Ordering::SeqCst);
                            // the supervisor's next step: it may find the sweep got there first
                            let _ =
                                mark_live(&mut core.lock().unwrap(), &s.door.id, 0, T0).unwrap();
                        }
                        Err(Error::Refused(_)) => {}
                        Err(e) => panic!("{e}"),
                    }
                    std::thread::yield_now();
                }
            }));
        }
        let revoker = {
            let (core, c, go) = (core.clone(), c.clone(), go.clone());
            std::thread::spawn(move || {
                go.wait();
                std::thread::yield_now();
                call_with(
                    &mut core.lock().unwrap(),
                    &c,
                    "people.suspend",
                    &json!({"id": "sam"}),
                )
                .unwrap();
            })
        };
        for t in workers {
            t.join().unwrap();
        }
        revoker.join().unwrap();
        let mut core = core.lock().unwrap();
        let all = list(&mut core, true).unwrap();
        assert_eq!(all.len(), started.load(Ordering::SeqCst), "round {round}");
        // every door that got a row either started before the revoke and was swept by it, or lost the race: never open
        assert!(
            all.iter().all(|d| d.state == State::Swept),
            "round {round}: {all:?}"
        );
    }
}

// --- the revoke hook -------------------------------------------------------------------------------------------------

#[test]
fn a_suspension_sweeps_the_doors_of_the_person_and_their_bots_and_not_the_owners() {
    let mut w = world();
    let sam = begin(&mut w, "sam");
    let bot = begin(&mut w, "bot-a");
    let kai = begin(&mut w, "kai");
    assert!(matches!(
        mark_live(&mut w.core, &sam.id, 0, T0).unwrap(),
        Live::Live(_)
    ));
    let said = say(&mut w, "people.suspend", &json!({"id": "sam"}));
    let swept: Vec<&str> = said["doors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["id"].as_str().unwrap())
        .collect();
    assert_eq!(swept.len(), 2);
    assert!(swept.contains(&sam.id.as_str()) && swept.contains(&bot.id.as_str()));
    assert_eq!(state_of(&mut w, &sam.id), State::Swept);
    assert_eq!(state_of(&mut w, &bot.id), State::Swept);
    assert_eq!(
        state_of(&mut w, &kai.id),
        State::Starting,
        "the owner's stays"
    );
    // twice: nothing left to sweep
    let again = say(&mut w, "people.suspend", &json!({"id": "sam"}));
    assert_eq!(again["doors"], json!([]));
}

#[test]
fn undo_leaves_swept_doors_swept_and_the_person_starts_a_new_one() {
    let mut w = world();
    let a = begin(&mut w, "sam");
    say(&mut w, "people.suspend", &json!({"id": "sam"}));
    let undone = say(&mut w, "people.undo", &json!({"id": "sam"}));
    assert!(
        undone.get("doors").is_none(),
        "undo sweeps and wakes nothing"
    );
    assert_eq!(state_of(&mut w, &a.id), State::Swept);
    assert!(matches!(
        mark_live(&mut w.core, &a.id, 0, T0).unwrap(),
        Live::Lost { .. }
    ));
    assert!(begin_with(&mut w, "sam", Some(&a.id)).is_err());
    let b = begin(&mut w, "sam");
    assert_ne!(b.id, a.id);
    assert_eq!(state_of(&mut w, &b.id), State::Starting);
}

#[test]
fn removing_a_person_or_a_bot_sweeps_their_doors() {
    let mut w = world();
    let sam = begin(&mut w, "sam");
    let bot = begin(&mut w, "bot-a");
    let said = say(&mut w, "people.removeBot", &json!({"id": "bot-a"}));
    assert_eq!(said["doors"].as_array().unwrap().len(), 1);
    assert_eq!(state_of(&mut w, &bot.id), State::Swept);
    assert_eq!(state_of(&mut w, &sam.id), State::Starting);
    let said = say(&mut w, "people.removePerson", &json!({"id": "sam"}));
    assert_eq!(said["doors"].as_array().unwrap().len(), 1);
    assert_eq!(state_of(&mut w, &sam.id), State::Swept);
}

#[test]
fn a_rename_carries_the_doors_so_a_later_revoke_finds_them() {
    let mut w = world();
    let d = begin(&mut w, "sam");
    say(
        &mut w,
        "people.renamePerson",
        &json!({"id": "sam", "to": "samuel"}),
    );
    assert_eq!(open_of(&mut w, "samuel"), 1);
    let said = say(&mut w, "people.suspend", &json!({"id": "samuel"}));
    assert_eq!(said["doors"].as_array().unwrap().len(), 1);
    assert_eq!(state_of(&mut w, &d.id), State::Swept);
}

// --- the sweep -------------------------------------------------------------------------------------------------------

#[test]
fn sweeping_twice_reports_a_door_whose_box_is_not_confirmed_gone_until_it_is() {
    let mut w = world();
    let d = begin(&mut w, "sam");
    let first = sweep_all(&mut w, &["sam"], true, &Started("T1"));
    assert_eq!(first.swept.len(), 1);
    let second = sweep_all(&mut w, &["sam"], true, &Started("T1"));
    assert!(second.swept.is_empty());
    assert_eq!(second.pending.len(), 1, "try to stop it again");
    let done = end(&mut w.core, &d.id, T0 + 30).unwrap();
    assert_eq!((done.state, done.reaped), (State::Swept, true));
    assert!(
        end(&mut w.core, &d.id, T0 + 40).unwrap().reaped,
        "twice is fine"
    );
    let third = sweep_all(&mut w, &["sam"], true, &Started("T1"));
    assert!(third.swept.is_empty() && third.pending.is_empty());
}

#[test]
fn naming_the_owner_sweeps_nothing_but_naming_a_person_takes_their_bots() {
    let mut w = world();
    let kai = begin(&mut w, "kai");
    let bot = begin(&mut w, "bot-a");
    let out = sweep_all(&mut w, &["kai"], true, &Started("T1"));
    assert!(out.swept.is_empty());
    assert_eq!(state_of(&mut w, &kai.id), State::Starting);
    let out = sweep_all(&mut w, &["sam"], false, &Started("T1"));
    assert_eq!(out.swept.len(), 1, "bot-a answers to sam");
    assert_eq!(state_of(&mut w, &bot.id), State::Swept);
}

#[test]
fn a_door_whose_supervisor_is_gone_is_swept_by_its_start_time() {
    let mut w = world();
    let d = begin(&mut w, "sam");
    // alive: the same pid with the start time the door recorded
    assert!(
        sweep_all(&mut w, &[], false, &Started("T1"))
            .swept
            .is_empty()
    );
    // a probe that failed never ends a live door
    assert!(sweep_all(&mut w, &[], false, &Nobody).swept.is_empty());
    // the pid is another process now
    let out = sweep_all(&mut w, &[], false, &Started("T2"));
    assert_eq!(out.swept.len(), 1);
    assert_eq!(out.swept[0].why, "its supervisor is gone");
    assert_eq!(state_of(&mut w, &d.id), State::Swept);
    // and the system saying there is no such process
    let e = begin(&mut w, "sam");
    assert_eq!(sweep_all(&mut w, &[], false, &Gone).swept.len(), 1);
    assert_eq!(state_of(&mut w, &e.id), State::Swept);
}

#[test]
fn a_door_whose_person_lost_the_agent_is_swept_by_the_recheck_only() {
    let mut w = world();
    let d = begin(&mut w, "sam");
    say(
        &mut w,
        "people.revoke",
        &json!({"agent": "house", "person": "sam"}),
    );
    assert!(
        sweep_all(&mut w, &[], false, &Started("T1"))
            .swept
            .is_empty()
    );
    let out = sweep_all(&mut w, &[], true, &Started("T1"));
    assert_eq!(out.swept.len(), 1);
    assert_eq!(out.swept[0].why, "its person may no longer run it");
    assert_eq!(state_of(&mut w, &d.id), State::Swept);
}

// --- the same answers as kith_door.py --------------------------------------------------------------------------------

/// `kernel/lib/kith_door.py` `sweep`: a claim is doomed when its person is listed, its supervisor is not alive, or
/// (recheck) `kith-auth door` is not ok. The held state alone never dooms one, and this port does not model it.
fn python_dooms(listed: bool, alive: bool, ok: bool, recheck: bool) -> bool {
    listed || !alive || (recheck && !ok)
}

#[test]
fn the_sweep_dooms_the_doors_kith_door_py_dooms_for_every_combination() {
    for bits in 0..16u8 {
        let (listed, alive, ok, recheck) =
            (bits & 1 != 0, bits & 2 != 0, bits & 4 != 0, bits & 8 != 0);
        let mut w = world();
        let d = begin(&mut w, "sam");
        if !ok {
            say(
                &mut w,
                "people.revoke",
                &json!({"agent": "house", "person": "sam"}),
            );
        }
        let live: &dyn Liveness = if alive {
            &Started("T1")
        } else {
            &Started("T2")
        };
        let persons: &[&str] = if listed { &["sam"] } else { &[] };
        let out = sweep_all(&mut w, persons, recheck, live);
        assert_eq!(
            out.swept.iter().any(|x| x.id == d.id),
            python_dooms(listed, alive, ok, recheck),
            "listed {listed} alive {alive} ok {ok} recheck {recheck}"
        );
    }
}

// --- the rows --------------------------------------------------------------------------------------------------------

#[test]
fn the_rows_start_mark_live_list_sweep_and_end() {
    let mut w = world();
    let home: PathBuf = w._agents.path().to_path_buf();
    let params = |more: Value| {
        let mut p = json!({
            "home": home,
            "kernel": home.join("kernel"),
            "env": {"KITH_AGENTS": home.join("agents")},
        });
        for (k, v) in more.as_object().unwrap() {
            p[k] = v.clone();
        }
        p
    };
    let live = Started("T1");
    let s = call_at(
        &mut w.core,
        "doors.start",
        &params(json!({"agent": "house", "door": "render", "person": "sam", "pid": 4242})),
        &live,
        T0,
    )
    .unwrap();
    assert_eq!(
        (s["state"].as_str(), s["repeat"].as_bool()),
        (Some("starting"), Some(false))
    );
    let id = s["id"].as_str().unwrap().to_owned();
    let m = call_at(
        &mut w.core,
        "doors.markLive",
        &json!({"id": id, "version": s["version"]}),
        &live,
        T0,
    )
    .unwrap();
    assert_eq!(m["result"], "live");
    let l = call_at(&mut w.core, "doors.list", &json!({}), &live, T0).unwrap();
    assert_eq!(l["doors"].as_array().unwrap().len(), 1);
    say(&mut w, "people.suspend", &json!({"id": "sam"}));
    let out = call_at(
        &mut w.core,
        "doors.sweep",
        &params(json!({"persons": ["sam"]})),
        &live,
        T0 + 5,
    )
    .unwrap();
    assert_eq!(out["swept"], json!([]));
    assert_eq!(out["pending"].as_array().unwrap().len(), 1);
    let e = call_at(&mut w.core, "doors.end", &json!({"id": id}), &live, T0 + 6).unwrap();
    assert_eq!(e["result"], "swept");
    let none = call_at(&mut w.core, "doors.list", &json!({}), &live, T0).unwrap();
    assert_eq!(none["doors"], json!([]));
    let e = call_at(&mut w.core, "doors.end", &json!({"id": "nope"}), &live, T0).unwrap_err();
    assert!(matches!(e, Error::Refused(_)));
}
