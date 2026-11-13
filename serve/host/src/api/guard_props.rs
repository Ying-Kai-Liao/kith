//! M2-16 parity: the request guard (`roles::guard` and what it asks: `may_use`, `may_use_checked`, `resolve`,
//! `granted_agents`) gives the same answer whether "is this person granted this agent" comes from kith-auth (a stand-in
//! script, as in `roles_props`) or from the core's `people.may` over the owner socket (`KITH_CORE_AUTHORITY=1`).
//! People, `*` rows, suspensions and a bot guest are generated and loaded into both. When nobody is suspended the two
//! answers are equal; the core is never more permissive; a core that does not answer, or is asked too much, refuses.
//! Method: `.claude/skills/invitee-access/SKILL.md`. Run only these: `cargo test guard_props`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::authority::{self, CoreData};
use super::authority_props::{Tmp, chat, folders};
use super::roles;
use crate::channels::hq::TEST_ENV;
use crate::hub::Hub;
use crate::remote::account::{ONE_AT_A_TIME, TEST_BIN};
use crate::remote::identity::Identity;
use crate::store::Store;
use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;

const AGENTS: [&str; 2] = ["house", "money"];
const PEOPLE: [&str; 2] = ["sam", "ann"];
const BOT: &str = "bot-b1";

/// One generated world. `held[i][j]`: 0 nothing, 1 guest row, 2 member row of person i for agent j. `star[j]`: the `*`
/// row grants agent j (as a guest) to everyone. `bot[j]`: the bot guest of sam holds agent j. `suspended[i]`.
#[derive(Clone, Debug)]
struct World {
    held: [[u8; 2]; 2],
    star: [bool; 2],
    bot: [bool; 2],
    suspended: [bool; 2],
}

fn world() -> impl Strategy<Value = World> {
    (any::<[[u8; 2]; 2]>(), any::<[bool; 2]>(), any::<[bool; 2]>(), any::<[bool; 2]>())
        .prop_map(|(held, star, bot, suspended)| World { held: held.map(|r| r.map(|x| x % 3)), star, bot, suspended })
}

impl World {
    /// What kith-auth prints for (person, agent) when nobody is suspended: its own row, else the `*` row.
    fn standin(&self, who: usize, agent: usize) -> Option<&'static str> {
        match (self.held[who][agent], self.star[agent]) {
            (2, _) => Some("member"),
            (1, _) | (_, true) => Some("guest"),
            _ => None,
        }
    }

    /// The bot holds an agent only while its owner (sam, index 0) does too.
    fn bot_standin(&self, agent: usize) -> Option<&'static str> {
        (self.bot[agent] && self.standin(0, agent).is_some()).then_some("guest")
    }
}

struct Rig {
    rt: tokio::runtime::Runtime,
    hub: Arc<Hub>,
    dir: PathBuf,
    _tmp: Tmp,
    core: Arc<CoreData>,
    daemon: Option<crate::core_front::TestDaemon>,
    _one_at_a_time: tokio::sync::MutexGuard<'static, ()>,
}

impl Rig {
    fn new() -> Self {
        let guard = ONE_AT_A_TIME.blocking_lock();
        let tmp = Tmp::new();
        let dir = tmp.path().join("auth");
        std::fs::create_dir_all(dir.join("grants")).unwrap();
        let bin = dir.join("kith-auth");
        // `kith-auth resolve AGENT --person PERSON`: the canned answer of that pair, failing when there is none.
        std::fs::write(&bin, "#!/bin/sh\n[ \"$1\" = resolve ] && [ \"$3\" = --person ] || exit 2\nf=\"$KITH_FAKE/$4__$2\"\n[ -f \"$f\" ] || exit 1\ncat \"$f\"\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        *TEST_ENV.lock().unwrap() = vec![("KITH_FAKE", dir.join("grants"))];
        *TEST_BIN.lock().unwrap() = Some(bin);

        let home = tmp.path().join("home");
        for a in AGENTS {
            let role = home.join("agents").join(a).join("role");
            std::fs::create_dir_all(&role).unwrap();
            std::fs::write(role.join("agent.md"), format!("---\nname: {a}\n---\n")).unwrap();
        }
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let hub = rt.block_on(async {
            let store = Store::open(&tmp.path().join("t.db")).unwrap();
            // The owner's chats of each agent: what "New chat" lists agents from.
            for a in AGENTS {
                store.save_bot(&chat(&format!("owner-{a}"), None, a)).unwrap();
            }
            for p in PEOPLE {
                store.put_person(p, p).unwrap();
                for a in AGENTS {
                    store.save_bot(&chat(&format!("{p}-{a}"), Some(p), a)).unwrap();
                }
            }
            Hub::new(store, "host".into(), Identity::load_or_create(tmp.path()).unwrap(), "token".into(), 0)
        });
        let env = json!({"KITH_AGENTS": home.join("agents").to_string_lossy()});
        let core = Arc::new(CoreData::new(
            folders(&tmp.path().join("machine")),
            home.clone(),
            home.join("kernel"),
            env.as_object().unwrap().clone(),
        ));
        let daemon = crate::core_front::serve_for_test(&core.folders);
        let rig = Self { rt, hub, dir, _tmp: tmp, core, daemon: Some(daemon), _one_at_a_time: guard };
        rig.ask("people.addPerson", json!({"id": "kai", "owner": true})).expect("kai");
        for p in PEOPLE {
            rig.ask("people.addPerson", json!({"id": p})).expect("person");
        }
        rig.ask("people.addBot", json!({"id": BOT, "of": "sam"})).expect("bot");
        rig
    }

    fn ask(&self, method: &str, mut params: Value) -> Option<Value> {
        params["home"] = self.core.home.to_str().unwrap().into();
        params["kernel"] = self.core.kernel.to_str().unwrap().into();
        params["env"] = Value::Object(self.core.env.clone());
        crate::core_front::ask(&self.core.folders, method, &params)
    }

    /// `w` into both: the stand-in's answer files, and the core's rows. A refusal while undoing is "nothing there".
    fn load(&self, w: &World) {
        let grants = self.dir.join("grants");
        std::fs::remove_dir_all(&grants).unwrap();
        std::fs::create_dir_all(&grants).unwrap();
        let put = |who: &str, j: usize, preset: Option<&str>| {
            if let Some(p) = preset {
                std::fs::write(
                    grants.join(format!("{who}__{}", AGENTS[j])),
                    json!({"allowed": true, "preset": p}).to_string(),
                )
                .unwrap();
            }
        };
        for (j, a) in AGENTS.iter().enumerate() {
            self.ask("people.revoke", json!({"person": "*", "agent": a}));
            if w.star[j] {
                self.ask("people.grant", json!({"person": "*", "agent": a, "preset": "guest"})).expect("star");
            }
            put(BOT, j, w.bot_standin(j));
        }
        for (i, p) in PEOPLE.iter().enumerate() {
            self.ask("people.undo", json!({"id": p}));
            for (j, a) in AGENTS.iter().enumerate() {
                self.ask("people.revoke", json!({"person": p, "agent": a}));
                match w.held[i][j] {
                    2 => {
                        drop(
                            self.ask("people.grant", json!({"person": p, "agent": a, "preset": "member"})).expect("g"),
                        );
                    }
                    1 => {
                        drop(self.ask("people.grant", json!({"person": p, "agent": a, "preset": "guest"})).expect("g"));
                    }
                    _ => {}
                }
                put(p, j, w.standin(i, j));
            }
        }
        for (j, a) in AGENTS.iter().enumerate() {
            self.ask("people.revoke", json!({"person": BOT, "agent": a}));
            if w.bot_standin(j).is_some() {
                self.ask("people.grant", json!({"person": BOT, "agent": a, "preset": "guest"})).expect("bot grant");
            }
        }
        for (i, p) in PEOPLE.iter().enumerate() {
            if w.suspended[i] {
                self.ask("people.suspend", json!({"id": p})).expect("suspend");
            }
        }
    }

    /// The same question under each answer: (kith-auth stand-in, core).
    fn both<T>(&self, f: impl Fn() -> T) -> (T, T) {
        authority::set_test_core(None);
        let host = f();
        authority::set_test_core(Some(self.core.clone()));
        let core = f();
        authority::set_test_core(None);
        (host, core)
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        authority::set_test_core(None);
        *TEST_BIN.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        TEST_ENV.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
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

/// Generated worlds: every person, agent and the bot gets the same `may_use`, preset and guard verdict under both
/// answers when nobody is suspended; the core never says yes where kith-auth says no, and always no for a suspended
/// person and the bot of one.
#[test]
fn the_guard_asks_the_core_and_gets_the_same_answers_as_kith_auth() {
    let rig = Rig::new();
    let granted = std::cell::Cell::new(0usize);
    let refused = std::cell::Cell::new(0usize);
    runner(16)
        .run(&world(), |w| {
            rig.load(&w);
            let nobody_suspended = !w.suspended.iter().any(|s| *s);
            let who: Vec<(String, Option<usize>)> = PEOPLE
                .iter()
                .enumerate()
                .map(|(i, p)| ((*p).to_owned(), Some(i)))
                .chain([(BOT.to_owned(), None)])
                .collect();
            for (name, idx) in &who {
                for (j, a) in AGENTS.iter().enumerate() {
                    let (h, c) = rig.both(|| {
                        rig.rt.block_on(async {
                            (
                                roles::may_use(name, a).await,
                                roles::may_use_checked(name, a).await,
                                roles::resolve(name, a).await.map(|v| v["preset"].clone()),
                            )
                        })
                    });
                    let truth = match idx {
                        Some(i) => w.standin(*i, j),
                        None => w.bot_standin(j),
                    };
                    prop_assert_eq!(h.0, truth.is_some(), "stand-in {}/{} {:?}", name, a, w);
                    // Never more permissive.
                    prop_assert!(!c.0 || h.0, "the core granted {name}/{a} that kith-auth does not: {w:?}");
                    let off = match idx {
                        Some(i) => w.suspended[*i],
                        None => w.suspended[0],
                    };
                    if off {
                        prop_assert!(!c.0, "a suspended person (or their bot) got {name}/{a}: {w:?}");
                    }
                    if nobody_suspended || !off {
                        prop_assert_eq!(&h.0, &c.0, "{}/{} {:?}", name, a, w);
                        prop_assert_eq!(h.1 == Some(true), c.1 == Some(true), "{}/{} {:?}", name, a, w);
                        // The preset only matters to a member: "member" under both, or neither.
                        prop_assert_eq!(
                            h.2.as_ref().is_some_and(|p| p == "member"),
                            c.2.as_ref().is_some_and(|p| p == "member"),
                            "{}/{} {:?}",
                            name,
                            a,
                            w
                        );
                    }
                    if c.0 {
                        granted.set(granted.get() + 1);
                    } else {
                        refused.set(refused.get() + 1);
                    }
                }
            }
            // The guard itself: a call on each person's own chat.
            for (i, p) in PEOPLE.iter().enumerate() {
                for (j, a) in AGENTS.iter().enumerate() {
                    let body = json!({"botId": format!("{p}-{a}"), "text": "hi"});
                    let (h, c) = rig.both(|| rig.rt.block_on(roles::guard(&rig.hub, p, true, "send", &body)).is_ok());
                    prop_assert!(!c || h, "guard let {p} into {a} on the core and not on kith-auth: {w:?}");
                    if w.suspended[i] {
                        prop_assert!(!c, "guard let a suspended {p} into {a}: {w:?}");
                    } else {
                        prop_assert_eq!(h, c, "guard {}/{} {:?}", p, a, w);
                    }
                    prop_assert_eq!(h, w.standin(i, j).is_some());
                }
            }
            Ok(())
        })
        .unwrap();
    assert!(granted.get() > 0 && refused.get() > 0, "the worlds never produced both answers");
}

/// The "New chat" list: the core lists exactly the agents it allows, with the member role from its preset.
#[test]
fn the_agents_a_person_holds_come_from_the_core_with_their_role() {
    let rig = Rig::new();
    let w = World { held: [[2, 1], [0, 0]], star: [false, false], bot: [false, false], suspended: [false, false] };
    rig.load(&w);
    let (h, c) = rig.both(|| rig.rt.block_on(roles::granted_agents(&rig.hub, "sam")));
    assert_eq!(c.len(), 2, "{c:?}");
    assert_eq!(c.iter().find(|r| r["agent"] == "house").unwrap()["role"], "member");
    assert_eq!(c.iter().find(|r| r["agent"] == "money").unwrap()["role"], "guest");
    assert_eq!(h, c);
    rig.ask("people.suspend", json!({"id": "sam"})).expect("suspend");
    let (_, gone) = rig.both(|| rig.rt.block_on(roles::granted_agents(&rig.hub, "sam")));
    assert!(gone.is_empty(), "{gone:?}");
}

/// The owner never comes through the core's asker: an empty person is not asked, and an answer of "owner" is a no.
#[test]
fn the_owner_is_never_a_member_of_the_guard() {
    let rig = Rig::new();
    let w = World { held: [[0; 2]; 2], star: [true, true], bot: [false, false], suspended: [false, false] };
    rig.load(&w);
    let (_, c) = rig.both(|| {
        rig.rt.block_on(async { (roles::may_use("kai", "house").await, roles::resolve("kai", "house").await) })
    });
    assert!(!c.0 && c.1.is_none(), "the owner got through the guard");
    assert!(rig.rt.block_on(authority::core_verdict("", "house")).is_none());
}

/// A core that does not answer refuses every member and every bot; a revoke is seen on the next question.
#[test]
fn a_core_that_does_not_answer_refuses_and_a_revoke_is_seen_at_once() {
    let rig = Rig::new();
    let w = World { held: [[2, 0], [0, 0]], star: [false, false], bot: [false, false], suspended: [false, false] };
    rig.load(&w);
    let body = json!({"botId": "sam-house", "text": "hi"});
    let (h, c) = rig.both(|| rig.rt.block_on(roles::guard(&rig.hub, "sam", true, "send", &body)).is_ok());
    assert!(h && c);
    rig.ask("people.revoke", json!({"person": "sam", "agent": "house"})).expect("revoke");
    let (_, c) = rig.both(|| rig.rt.block_on(roles::guard(&rig.hub, "sam", true, "send", &body)).is_ok());
    assert!(!c, "a revoke did not stop the next call");

    // No daemon at all (and an empty one): the same refusal; "can't ask" for the checked form, so a card is not dropped.
    let silent = Tmp::new();
    let none = Arc::new(CoreData::new(
        folders(&silent.path().join("machine")),
        rig.core.home.clone(),
        rig.core.kernel.clone(),
        rig.core.env.clone(),
    ));
    authority::set_test_core(Some(none));
    assert!(rig.rt.block_on(roles::guard(&rig.hub, "sam", true, "send", &body)).is_err());
    assert_eq!(rig.rt.block_on(roles::may_use_checked("sam", "house")), None);
    authority::set_test_core(None);

    // A daemon that knows nobody (the core's database empty before the import): members are refused.
    let blank = Tmp::new();
    let empty = Arc::new(CoreData::new(
        folders(&blank.path().join("machine")),
        rig.core.home.clone(),
        rig.core.kernel.clone(),
        rig.core.env.clone(),
    ));
    let daemon = crate::core_front::serve_for_test(&empty.folders);
    authority::set_test_core(Some(empty));
    assert!(rig.rt.block_on(roles::guard(&rig.hub, "sam", true, "send", &body)).is_err());
    assert!(!rig.rt.block_on(roles::may_use("sam", "house")));
    authority::set_test_core(None);
    daemon.stop();
}

/// Bounded: with every blocking ask taken (a core that hangs), a flood of requests gets "no" after the wait and starts
/// no thread; the permits come back when the hung ask returns.
#[test]
fn a_flood_against_a_hung_core_is_refused_within_the_wait_and_starts_no_threads() {
    let rig = Rig::new();
    let w = World { held: [[2, 0], [0, 0]], star: [false, false], bot: [false, false], suspended: [false, false] };
    rig.load(&w);
    authority::set_test_core(Some(rig.core.clone()));
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let held: Vec<_> =
        (0..authority::MAX_ASKS).map(|_| rt.block_on(authority::asks().acquire_owned()).unwrap()).collect();
    let started = tokio::time::Instant::now();
    let answers = rt.block_on(async {
        let all: Vec<_> =
            (0..50).map(|_| tokio::spawn(async { authority::core_verdict("sam", "house").await })).collect();
        let mut out = vec![];
        for t in all {
            out.push(t.await.unwrap());
        }
        out
    });
    assert!(answers.iter().all(Option::is_none), "a hung core granted something");
    assert!(
        started.elapsed() <= authority::ASK_WAIT + std::time::Duration::from_millis(1500),
        "{:?}",
        started.elapsed()
    );
    drop(held);
    assert!(
        rt.block_on(authority::core_verdict("sam", "house")).is_some_and(|v| v.allowed),
        "the permits did not come back"
    );
    authority::set_test_core(None);
}
