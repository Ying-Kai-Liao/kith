//! M2-15 parity: the event filter gives the same answer whether "is this person granted this agent" comes from the
//! host's people mirror or from the core's authority (`people.list` over the owner socket). People, grants and
//! suspensions are generated and loaded into both; events are generated; `event_for_with` is run with each answer.
//! A core that does not answer drops what an invitee would get (fail closed) and leaves the owner's events alone.
//! Method: `.claude/skills/invitee-access/SKILL.md`. Run only these: `cargo test authority_props`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::authority::{Authority, CoreData, HostData};
use super::roles;
use crate::store::Store;
use kith_kernel::paths::Paths;
use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

const AGENTS: [&str; 2] = ["house", "money"];
const PEOPLE: [&str; 2] = ["sam", "ann"];

/// A short folder of its own (the owner socket's path has a length limit), removed on drop.
pub(crate) struct Tmp(std::path::PathBuf);

impl Tmp {
    pub(crate) fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("kap-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

pub(crate) fn folders(machine: &Path) -> Paths {
    let p = machine.to_path_buf();
    Paths {
        home: p.clone(),
        state: p.clone(),
        agents: p.clone(),
        home_real: p.clone(),
        state_real: p.clone(),
        agents_real: p.clone(),
        machine: p.clone(),
        secrets: p.clone(),
        logs: p.clone(),
        cache: p.clone(),
        store: p,
    }
}

pub(super) fn chat(id: &str, person: Option<&str>, agent: &str) -> crate::store::BotConfig {
    serde_json::from_value(json!({
        "id": id, "name": id, "backend": "claude", "cwd": "/Users/secret", "command": "/Users/secret/run",
        "hqAgent": agent, "person": person, "permission": "ask", "tasks": false, "connectors": [], "skills": [],
    }))
    .unwrap()
}

/// One generated world: per person, the agents granted by their own row, the agents the `*` (everyone) row grants, and
/// whether each person is suspended.
#[derive(Clone, Debug)]
struct World {
    granted: [[bool; 2]; 2],
    star: [bool; 2],
    suspended: [bool; 2],
}

struct Rig {
    _dir: Tmp,
    store: Store,
    core: CoreData,
    daemon: Option<crate::core_front::TestDaemon>,
    /// Chat ids: for every (person, agent) a chat of that person, and the owner's private chat.
    chats: Vec<String>,
}

impl Rig {
    fn new() -> Self {
        let dir = Tmp::new();
        let machine = dir.path().join("machine");
        let home = dir.path().join("home");
        for a in AGENTS {
            let role = home.join("agents").join(a).join("role");
            std::fs::create_dir_all(&role).unwrap();
            std::fs::write(role.join("agent.md"), format!("---\nname: {a}\n---\n")).unwrap();
        }
        let store = Store::open(&dir.path().join("t.db")).unwrap();
        let mut chats = vec!["private".to_owned()];
        store.save_bot(&chat("private", None, "house")).unwrap();
        for p in PEOPLE {
            store.put_person(p, p).unwrap();
            for a in AGENTS {
                let id = format!("{p}-{a}");
                store.save_bot(&chat(&id, Some(p), a)).unwrap();
                chats.push(id);
            }
        }
        let env = json!({"KITH_AGENTS": home.join("agents").to_string_lossy()});
        let core =
            CoreData::new(folders(&machine), home.clone(), home.join("kernel"), env.as_object().unwrap().clone());
        let daemon = crate::core_front::serve_for_test(&core.folders);
        let rig = Self { _dir: dir, store, core, daemon: Some(daemon), chats };
        rig.ask("people.addPerson", json!({"id": "kai", "owner": true})).expect("kai");
        for p in PEOPLE {
            rig.ask("people.addPerson", json!({"id": p})).expect("person");
        }
        rig
    }

    fn ask(&self, method: &str, mut params: Value) -> Option<Value> {
        params["home"] = self.core.home.to_str().unwrap().into();
        params["kernel"] = self.core.kernel.to_str().unwrap().into();
        params["env"] = Value::Object(self.core.env.clone());
        crate::core_front::ask(&self.core.folders, method, &params)
    }

    /// Puts `w` into the host's mirror and into the core. The mirror holds a person's own agents and the `*` row's
    /// (`members::mirror_agents`), and a suspended person's copy stands (it skips a revoked person); the core
    /// `people.suspend`s them.
    fn load(&self, w: &World) {
        for (j, a) in AGENTS.iter().enumerate() {
            self.ask("people.revoke", json!({"person": "*", "agent": a}));
            if w.star[j] {
                self.ask("people.grant", json!({"person": "*", "agent": a, "preset": "guest"})).expect("star");
            }
        }
        for (i, p) in PEOPLE.iter().enumerate() {
            let agents: BTreeMap<String, String> = AGENTS
                .iter()
                .enumerate()
                .filter(|(j, _)| w.granted[i][*j] || w.star[*j])
                .map(|(_, a)| ((*a).to_owned(), "member".to_owned()))
                .collect();
            self.store.set_person_agents(p, &agents).unwrap();
            // A refusal here is "nothing to undo / remove": the state asked for is what counts.
            self.ask("people.undo", json!({"id": p}));
            for (j, a) in AGENTS.iter().enumerate() {
                self.ask("people.revoke", json!({"person": p, "agent": a}));
                if w.granted[i][j] {
                    self.ask("people.grant", json!({"person": p, "agent": a, "preset": "member"})).expect("grant");
                }
            }
            if w.suspended[i] {
                self.ask("people.suspend", json!({"id": p})).expect("suspend");
            }
        }
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        if let Some(d) = self.daemon.take() {
            d.stop();
        }
    }
}

fn runner(default: u32) -> TestRunner {
    let mut config = Config { failure_persistence: None, max_shrink_iters: 16, ..Config::default() };
    config.cases = std::env::var("PROPTEST_CASES").ok().and_then(|c| c.parse().ok()).unwrap_or(default);
    TestRunner::new(config)
}

fn world() -> impl Strategy<Value = World> {
    (any::<[[bool; 2]; 2]>(), any::<[bool; 2]>(), any::<[bool; 2]>()).prop_map(|(granted, star, suspended)| World {
        granted,
        star,
        suspended,
    })
}

#[derive(Clone, Debug)]
struct Ev {
    kind: usize,
    chat: usize,
    entry_kind: usize,
}

const KINDS: [&str; 5] = ["entry", "bot", "call", "conversation", "usage"];
const ENTRY_KINDS: [&str; 3] = ["agent", "permission", "notice"];

fn event(rig: &Rig, e: &Ev) -> Value {
    let chat = &rig.chats[e.chat];
    let entry = json!({
        "id": "e1", "rev": 3, "turn": 1, "createdAt": 5, "kind": ENTRY_KINDS[e.entry_kind],
        "data": {"text": "hello", "status": "pending"}, "botId": chat, "threadId": null,
    });
    json!({
        "type": KINDS[e.kind], "rev": 4, "entry": entry, "bot": {"id": chat, "cwd": "/Users/secret"},
        "call": {"botId": chat}, "chat": chat,
    })
}

fn events(rig: &Rig) -> impl Strategy<Value = Ev> + use<> {
    (0..KINDS.len(), 0..rig.chats.len(), 0..ENTRY_KINDS.len()).prop_map(|(kind, chat, entry_kind)| Ev {
        kind,
        chat,
        entry_kind,
    })
}

/// For generated worlds and events: the owner's events are the same either way; an invitee who is not suspended is
/// sent exactly the same; a suspended invitee is sent nothing by the core (the host's mirror still stands for them
/// until its next sync, so it may send more: the core never sends more than the host does).
#[test]
fn the_core_and_the_host_filter_events_alike() {
    let rig = Rig::new();
    let strategy = (world(), proptest::collection::vec(events(&rig), 12));
    let seen = std::cell::Cell::new(0usize);
    let by_star = std::cell::Cell::new(0usize);
    runner(24)
        .run(&strategy, |(w, evs)| {
            rig.load(&w);
            let host = HostData(&rig.store);
            for e in &evs {
                let v = event(&rig, e);
                for (who, idx) in [(None, None), (Some("sam"), Some(0)), (Some("ann"), Some(1))] {
                    let a = roles::event_for_with(&host, &rig.store, who, v.clone());
                    let b = roles::event_for_with(&rig.core, &rig.store, who, v.clone());
                    let suspended = idx.is_some_and(|i| w.suspended[i]);
                    if suspended {
                        prop_assert!(b.is_none(), "a suspended person was sent {b:?}, world={w:?}");
                    } else {
                        prop_assert_eq!(&a, &b, "who={:?} world={:?} event={:?}", who, w, e);
                    }
                    if b.is_some() && who.is_some() {
                        seen.set(seen.get() + 1);
                        if let Some(i) = idx
                            && !AGENTS.iter().enumerate().any(|(j, _)| {
                                let own = w.granted[i][j];
                                own && rig.chats[e.chat].ends_with(AGENTS[j])
                            })
                        {
                            by_star.set(by_star.get() + 1);
                        }
                    }
                }
            }
            // Each answer itself, not only the events: the grant of every person and agent.
            for (i, p) in PEOPLE.iter().enumerate() {
                for (j, a) in AGENTS.iter().enumerate() {
                    let held = w.granted[i][j] || w.star[j];
                    prop_assert_eq!(host.granted(p, a), held);
                    prop_assert_eq!(rig.core.granted(p, a), held && !w.suspended[i], "core, world={:?}", w);
                }
            }
            Ok(())
        })
        .unwrap();
    assert!(seen.get() > 0, "no event ever reached an invitee, so the parity above proves little");
    assert!(by_star.get() > 0, "no event ever reached an invitee through the `*` row");
}

/// A person who holds an agent only through the `*` row is sent their chat of it, and loses it with the row.
#[test]
fn the_everyone_row_grants_and_its_removal_stops_events() {
    let rig = Rig::new();
    let w = World { granted: [[false; 2]; 2], star: [true, false], suspended: [false, false] };
    rig.load(&w);
    let v = event(&rig, &Ev { kind: 0, chat: 1, entry_kind: 0 });
    assert_eq!(rig.chats[1], "sam-house");
    assert!(roles::event_for_with(&rig.core, &rig.store, Some("sam"), v.clone()).is_some());
    rig.ask("people.revoke", json!({"person": "*", "agent": "house"})).expect("revoke the row");
    assert!(roles::event_for_with(&rig.core, &rig.store, Some("sam"), v).is_none());
}

/// The catch-up memo asks once per pair and the live path never remembers.
#[test]
fn a_catch_up_batch_asks_once_and_a_live_event_asks_afresh() {
    let rig = Rig::new();
    let w = World { granted: [[true, false], [false, false]], star: [false, false], suspended: [false, false] };
    rig.load(&w);
    let batch = rig.core.with_memo();
    assert!(batch.granted("sam", "house"));
    rig.ask("people.revoke", json!({"person": "sam", "agent": "house"})).expect("revoke");
    assert!(batch.granted("sam", "house"), "the batch remembers within itself");
    assert!(!rig.core.granted("sam", "house"), "a live question does not");
}

/// A revoke is seen on the next event: nothing is remembered between them.
#[test]
fn a_revoke_stops_the_next_event() {
    let rig = Rig::new();
    let w = World { granted: [[true, false], [false, false]], star: [false, false], suspended: [false, false] };
    rig.load(&w);
    let v = event(&rig, &Ev { kind: 0, chat: 1, entry_kind: 0 });
    assert_eq!(rig.chats[1], "sam-house");
    assert!(roles::event_for_with(&rig.core, &rig.store, Some("sam"), v.clone()).is_some());
    rig.ask("people.revoke", json!({"person": "sam", "agent": "house"})).expect("revoke");
    assert!(roles::event_for_with(&rig.core, &rig.store, Some("sam"), v).is_none());
}

/// No core answering: an invitee gets nothing of their own chat; the owner is unchanged. An empty core database (people
/// written before the import) is the same answer.
#[test]
fn a_core_that_does_not_answer_drops_the_invitees_events_and_not_the_owners() {
    let rig = Rig::new();
    let w = World { granted: [[true, true], [true, true]], star: [false, false], suspended: [false, false] };
    rig.load(&w);
    let v = event(&rig, &Ev { kind: 0, chat: 1, entry_kind: 0 });
    let host = HostData(&rig.store);
    assert!(roles::event_for_with(&host, &rig.store, Some("sam"), v.clone()).is_some());

    let nobody = Tmp::new();
    let silent = CoreData::new(
        folders(&nobody.path().join("machine")),
        rig.core.home.clone(),
        rig.core.kernel.clone(),
        rig.core.env.clone(),
    );
    assert!(roles::event_for_with(&silent, &rig.store, Some("sam"), v.clone()).is_none());
    assert_eq!(
        roles::event_for_with(&silent, &rig.store, None, v.clone()),
        roles::event_for_with(&host, &rig.store, None, v.clone()),
    );

    // The daemon serves but knows nobody (the core empty): the same "not granted".
    let empty = Tmp::new();
    let machine = empty.path().join("machine");
    let blank = CoreData::new(folders(&machine), silent.home.clone(), silent.kernel.clone(), silent.env.clone());
    let daemon = crate::core_front::serve_for_test(&blank.folders);
    assert!(roles::event_for_with(&blank, &rig.store, Some("sam"), v).is_none());
    daemon.stop();
}
