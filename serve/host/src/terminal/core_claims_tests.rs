//! A scripted stand-in for the core's `hostclaim.*` rows (their semantics are tested on a real core in
//! `core/runtime/src/hostclaim_tests.rs`), so the host adapter's sequences can be tested without a holder: which rows it
//! calls in which order, and what it does with a refusal.

use super::super::registry::pid_runs;
use crate::core_front::Answer;
use kith_kernel::paths::Paths;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

pub(in crate::terminal) type Fake = Arc<dyn Fn(&str, &Value) -> Answer + Send + Sync>;

fn fakes() -> &'static Mutex<HashMap<PathBuf, Fake>> {
    static F: OnceLock<Mutex<HashMap<PathBuf, Fake>>> = OnceLock::new();
    F.get_or_init(Mutex::default)
}

/// Registers a scripted core of another module's tests (`core_writer_tests`) for a state folder.
pub(in crate::terminal) fn register(state: &std::path::Path, fake: Fake) {
    fakes().lock().unwrap().insert(state.to_path_buf(), fake);
}

pub(super) fn fake_for(f: &Paths) -> Option<Fake> {
    fakes().lock().unwrap().get(&f.state).cloned()
}

#[derive(Clone, Debug)]
pub(in crate::terminal) struct FakeClaim {
    pub id: i64,
    pub conversation: String,
    pub state: String,
    pub kind: String,
    pub person: String,
    pub pid: Option<u64>,
    pub started: Option<String>,
    pub turn: Option<i64>,
    /// The holder instance the claim went to (set by the claim or the redeem, as the real core does).
    pub holder_instance: Option<String>,
}

#[derive(Default)]
#[allow(clippy::struct_excessive_bools, reason = "one switch per scripted failure")]
pub(in crate::terminal) struct World {
    pub claims: Vec<FakeClaim>,
    /// (turn id, state)
    pub turns: Vec<(i64, String)>,
    /// Every row the host called, in order.
    pub log: Vec<String>,
    /// The next epoch read is stale: enqueue and turn are refused as revoked.
    pub revoked: bool,
    /// `settle` never ends anything (a holder that will not go).
    pub stuck: bool,
    /// Every row is unreachable (the core is down).
    pub down: bool,
    /// `hostclaim.turn` finds no account left.
    pub exhausted: bool,
    /// `hostclaim.redeem` fails before the core sees it.
    pub redeem_fails: bool,
    /// `hostclaim.redeem` is applied but its reply is lost.
    pub redeem_lost: bool,
}

#[derive(Clone)]
pub(in crate::terminal) struct FakeCore {
    #[allow(dead_code, reason = "kept for reading a failing test")]
    pub folders: Paths,
    pub world: Arc<Mutex<World>>,
}

impl FakeCore {
    /// A core for the state folder `state`, registered for the adapter.
    pub fn start(state: &std::path::Path) -> Self {
        let dir = state.parent().unwrap().to_path_buf();
        let folders = Paths {
            home: dir.clone(),
            state: state.to_path_buf(),
            agents: dir.join("agents"),
            home_real: dir.clone(),
            state_real: state.to_path_buf(),
            agents_real: dir.join("agents"),
            machine: dir.join("machine"),
            secrets: dir.join("secrets"),
            logs: dir.join("logs"),
            cache: dir.join("cache"),
            store: dir.join("store"),
        };
        let core = Self { folders: folders.clone(), world: Arc::default() };
        let w = core.world.clone();
        let fake: Fake = Arc::new(move |method, p| answer(&mut w.lock().unwrap(), method, p));
        fakes().lock().unwrap().insert(state.to_path_buf(), fake);
        super::enable(state, folders);
        core
    }

    pub fn log(&self) -> Vec<String> {
        self.world.lock().unwrap().log.clone()
    }

    pub fn claim_state(&self, id: i64) -> String {
        self.world.lock().unwrap().claims.iter().find(|c| c.id == id).map(|c| c.state.clone()).unwrap_or_default()
    }

    pub fn turn_states(&self) -> Vec<String> {
        self.world.lock().unwrap().turns.iter().map(|t| t.1.clone()).collect()
    }

    /// A host restart: the adapter's memory of its claims is gone, the core's rows are not.
    pub fn restart(state: &std::path::Path) {
        super::mine().lock().unwrap().retain(|(s, _), _| s != state);
    }

    /// A claim the core already holds (a terminal's), running on `pid`.
    pub fn hold(&self, conversation: &str, kind: &str, pid: u32) -> i64 {
        self.hold_as(conversation, kind, pid, None)
    }

    /// [`Self::hold`] for a claim that went to the holder instance `instance`.
    pub fn hold_as(&self, conversation: &str, kind: &str, pid: u32, instance: Option<&str>) -> i64 {
        let started = super::proc_start(pid);
        let mut w = self.world.lock().unwrap();
        let id = i64::try_from(w.claims.len()).unwrap() + 1;
        w.claims.push(FakeClaim {
            id,
            conversation: conversation.into(),
            state: "running".into(),
            kind: kind.into(),
            person: "owner".into(),
            pid: Some(u64::from(pid)),
            started,
            turn: None,
            holder_instance: instance.map(str::to_owned),
        });
        id
    }
}

const LIVE: [&str; 3] = ["claimed", "running", "ending"];

fn shown(c: &FakeClaim) -> Value {
    json!({"claimId": c.id, "conversation": c.conversation, "state": c.state, "kind": c.kind,
           "person": c.person, "pid": c.pid, "started": c.started, "holderInstance": c.holder_instance})
}

fn refused(kind: &str) -> Answer {
    Answer::Done(json!({"refused": kind, "why": kind}))
}

fn answer(w: &mut World, method: &str, p: &Value) -> Answer {
    w.log.push(method.to_owned());
    let conv = p["conversation"].as_str().unwrap_or_default().to_owned();
    let live = |w: &World| w.claims.iter().position(|c| c.conversation == conv && LIVE.contains(&c.state.as_str()));
    let id = p["claimId"].as_i64().unwrap_or_default();
    if w.down {
        return Answer::Down { why: "the core is down".into(), lost: false };
    }
    if method == "hostclaim.redeem" && w.redeem_fails {
        return Answer::Down { why: "the redeem never arrived".into(), lost: false };
    }
    if method == "hostclaim.turn" && w.exhausted {
        return Answer::Done(json!({"status": "exhausted", "text": "no account left"}));
    }
    let lost = method == "hostclaim.redeem" && w.redeem_lost;
    let answered = Answer::Done(match method {
        "hostclaim.ensure" => json!({"conversation": conv}),
        "hostclaim.epoch" => json!({"epoch": i64::from(w.revoked)}),
        "hostclaim.claim" => {
            if live(w).is_some() {
                return refused("held");
            }
            let n = i64::try_from(w.claims.len()).unwrap() + 1;
            w.claims.push(FakeClaim {
                id: n,
                conversation: conv,
                state: "claimed".into(),
                kind: p["kind"].as_str().unwrap().into(),
                person: p["person"].as_str().unwrap().into(),
                pid: p["pid"].as_u64(),
                started: p["started"].as_str().map(str::to_owned),
                turn: None,
                holder_instance: p["holderInstance"].as_str().map(str::to_owned),
            });
            json!({"claimId": n, "generation": 1})
        }
        "hostclaim.enqueue" => {
            if w.revoked {
                return refused("revoked");
            }
            let n = i64::try_from(w.turns.len()).unwrap() + 1;
            w.turns.push((n, "queued".into()));
            json!({"turn": n})
        }
        "hostclaim.turn" => {
            if w.revoked {
                return refused("revoked");
            }
            if live(w).is_some() {
                return refused("held");
            }
            let n = i64::try_from(w.claims.len()).unwrap() + 1;
            let turn = p["turn"].as_i64().unwrap();
            w.claims.push(FakeClaim {
                id: n,
                conversation: conv,
                state: "claimed".into(),
                kind: "turn".into(),
                person: p["person"].as_str().unwrap().into(),
                pid: None,
                started: None,
                turn: Some(turn),
                holder_instance: p["holderInstance"].as_str().map(str::to_owned),
            });
            w.turns.iter_mut().filter(|t| t.0 == turn).for_each(|t| t.1 = "claimed".into());
            json!({"status": "claimed", "claimId": n, "generation": 1, "account": "main", "fresh": false})
        }
        "hostclaim.redeem" => match w.claims.iter_mut().find(|c| c.id == id && c.state == "claimed") {
            Some(c) => {
                c.state = "running".into();
                c.pid = p["pid"].as_u64();
                c.started = p["started"].as_str().map(str::to_owned);
                c.holder_instance = p["holderInstance"].as_str().map(str::to_owned);
                json!({"running": true, "again": false})
            }
            None => json!({"refused": "unknown"}),
        },
        "hostclaim.report" => {
            let pid = serde_json::from_str::<Value>(p["body"].as_str().unwrap()).unwrap()["pid"].as_u64();
            let kind = p["kind"].as_str().unwrap().to_owned();
            let hit = w.claims.iter_mut().find(|c| c.id == id && LIVE.contains(&c.state.as_str()) && c.pid == pid);
            match hit {
                Some(c) => {
                    c.state = "ended".into();
                    let turn = c.turn;
                    // As the real core: `done` finishes the turn, `exit` puts it back in the queue.
                    let to = if kind == "done" { "done" } else { "queued" };
                    w.turns.iter_mut().filter(|t| Some(t.0) == turn).for_each(|t| t.1 = to.into());
                    json!({"stored": true, "applied": kind})
                }
                None => json!({"stored": true, "applied": null}),
            }
        }
        "hostclaim.stop" => match w.claims.iter_mut().find(|c| c.id == id && LIVE.contains(&c.state.as_str())) {
            Some(c) => {
                c.state = "ending".into();
                json!({"kill": {"claimId": id, "pid": c.pid, "started": c.started}})
            }
            None => json!({"kill": null}),
        },
        "hostclaim.settle" => {
            let stuck = w.stuck;
            let mut ended = Vec::new();
            for c in &mut w.claims {
                let gone = c.pid.and_then(|p| u32::try_from(p).ok()).is_some_and(|p| !pid_runs(p));
                if !stuck && gone && LIVE.contains(&c.state.as_str()) && c.state != "claimed" {
                    c.state = "ended".into();
                    ended.push(c.id);
                }
            }
            json!({"ended": ended})
        }
        "hostclaim.show" => {
            let c = if p["claimId"].is_i64() {
                w.claims.iter().find(|c| c.id == id)
            } else {
                live(w).map(|i| &w.claims[i])
            };
            json!({"claim": c.map(shown)})
        }
        "hostclaim.cancel" => {
            let t = p["turn"].as_i64().unwrap();
            let mut done = false;
            for x in w.turns.iter_mut().filter(|x| x.0 == t && ["queued", "parked", "claimed"].contains(&x.1.as_str()))
            {
                x.1 = "cancelled".into();
                done = true;
            }
            // As the real core: a claim that was never redeemed goes with its turn.
            w.claims
                .iter_mut()
                .filter(|c| c.turn == Some(t) && c.state == "claimed")
                .for_each(|c| c.state = "ended".into());
            json!({"cancelled": done})
        }
        other => return Answer::Refused(format!("the fake core has no {other}")),
    });
    if lost {
        return Answer::Down { why: "the reply was lost".into(), lost: true };
    }
    answered
}
