//! Property tests for bot guests: whatever happens to its owner's grant, a bot guest never gets more than its owner
//! holds right now, never reaches a method off its short list, and never touches another bot or another owner's
//! bots. The real handlers (`dispatch`) run against a real kith-auth in a temp folder, a temp store and a fake cloud
//! (the `Rig` the bot guest tests use). The sequences are generated; the oracle is a small model of who holds what.
//! Method: `.claude/skills/invitee-access/SKILL.md`. Run only these: `cargo test botguests_props`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::devices::{Caller, Forbidden, NotAllowed, Principal, principal};
use super::rig::Rig;
use super::roles::{self, BOT_METHODS, BOT_PENDING_METHODS};
use super::roles_props::{bot_guest, router_methods};
use super::{dispatch, error_code};
use crate::remote::botsync::{self, Listed};
use crate::remote::crypto::{self, b64};
use crate::remote::members::{self, Listed as Member};
use crate::store::{BotConversation, BotState, EntryKind, Lane, Scope, Store};
use proptest::prelude::*;
use proptest::test_runner::{Config, TestCaseError, TestRunner};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};

const AGENTS: [&str; 2] = ["house", "money"];
/// Kai (the Mac owner, no person row), and the two invited people.
const WHO: [&str; 3] = ["kai", "sam", "ann"];
const SECRET: &str = "SECRET-PATH";

/// Cases per property; every case builds a kith-auth and a store of its own and runs subprocesses, so the default is
/// small. `PROPTEST_CASES` overrides it.
fn runner(default: u32) -> TestRunner {
    let mut config = Config { failure_persistence: None, max_shrink_iters: 24, ..Config::default() };
    config.cases = std::env::var("PROPTEST_CASES").ok().and_then(|c| c.parse().ok()).unwrap_or(default);
    TestRunner::new(config)
}

fn fail(msg: impl Into<String>) -> TestCaseError {
    TestCaseError::fail(msg.into())
}

macro_rules! check {
    ($cond:expr, $($msg:tt)+) => {
        if !$cond {
            return Err(format!($($msg)+));
        }
    };
}

/// A refusal to a caller who may not do this (not a validation error and not a limit).
fn refused(e: &anyhow::Error) -> bool {
    e.is::<Forbidden>() || e.is::<NotAllowed>() || matches!(error_code(e), Some("pending" | "notGranted"))
}

fn token_of(link: &str) -> String {
    link.split("#t=").nth(1).unwrap().split('&').next().unwrap().to_owned()
}

// ---- the model ----

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum St {
    Pending,
    Active,
    Ended,
}

struct MBot {
    id: String,
    caller: Caller,
    /// Who owns it: 0 Kai, 1 and 2 the invited people.
    owner: usize,
    /// The agents it was given.
    agents: [bool; 2],
    state: St,
    invite: String,
    /// Whether an ask of it has gone through, so the first one is known to meet no limit.
    served: bool,
    /// Conversations it has made.
    convs: Vec<String>,
}

struct State {
    r: Rig,
    /// Who calls: Kai's own apps, then the two people's phones.
    callers: [Caller; 3],
    /// What each person holds, by agent: 0 nothing, 1 member, 2 guest. Row 0 (Kai) is unused: Kai holds everything.
    roles: [[u8; 2]; 3],
    removed: [bool; 3],
    auto: [bool; 3],
    bots: Vec<MBot>,
    nonce: u32,
    pool: Vec<String>,
}

/// One thing that happens. Indexes into bots and methods are taken modulo what exists.
#[derive(Clone, Debug)]
enum Op {
    Create { actor: usize, agents: u8 },
    Allow { bot: usize, always: bool },
    RevokeBot { actor: usize, bot: usize },
    RevokeInvite { actor: usize, bot: usize },
    Reinvite { actor: usize, bot: usize },
    MyBots { actor: usize },
    SetLimits { actor: usize, bot: usize, rate: usize, conc: usize },
    SetAccess { person: usize, roles: [u8; 2] },
    Remove { person: usize },
    Call { bot: usize, method: usize },
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => (0..3usize, 0..4u8).prop_map(|(actor, agents)| Op::Create { actor, agents }),
        2 => (0..8usize, any::<bool>()).prop_map(|(bot, always)| Op::Allow { bot, always }),
        2 => (0..3usize, 0..8usize).prop_map(|(actor, bot)| Op::RevokeBot { actor, bot }),
        1 => (0..3usize, 0..8usize).prop_map(|(actor, bot)| Op::RevokeInvite { actor, bot }),
        2 => (0..3usize, 0..8usize).prop_map(|(actor, bot)| Op::Reinvite { actor, bot }),
        1 => (0..3usize).prop_map(|actor| Op::MyBots { actor }),
        2 => (0..3usize, 0..8usize, 0..7usize, 0..6usize)
            .prop_map(|(actor, bot, rate, conc)| Op::SetLimits { actor, bot, rate, conc }),
        3 => (1..3usize, proptest::array::uniform2(0..3u8)).prop_map(|(person, roles)| Op::SetAccess { person, roles }),
        1 => (1..3usize).prop_map(|person| Op::Remove { person }),
        4 => (0..8usize, 0..500usize).prop_map(|(bot, method)| Op::Call { bot, method }),
    ]
}

const RATES: [i64; 7] = [0, 1, 2, 60, 61, 600, 601];
const CONCS: [i64; 6] = [0, 1, 2, 3, 8, 9];

#[allow(clippy::needless_range_loop)]
impl State {
    async fn new(pool: Vec<String>) -> Self {
        let r = Rig::new().await;
        r.owner_bot("house");
        r.owner_bot("money");
        let mut invites = Vec::new();
        for name in ["Sam", "Ann"] {
            let body = json!({"name": name, "agents": {"house": "member", "money": "member"}});
            let made = dispatch(&r.hub, &Caller::Local, "createInvite", body).await.unwrap();
            invites.push(made["inviteId"].as_str().unwrap().to_owned());
        }
        let listed: Vec<Member> = ["sam", "ann"]
            .iter()
            .zip(invites)
            .map(|(p, invite_id)| Member { membership_id: format!("mem_{p}"), user_id: format!("user_{p}"), invite_id })
            .collect();
        members::apply(&r.hub, Some(&r.bin), &listed, Some("user_owner")).await.unwrap();
        let callers = [Caller::Local, r.device("sam-phone", Some("sam")), r.device("ann-phone", Some("ann"))];
        Self {
            r,
            callers,
            roles: [[0; 2], [1, 1], [1, 1]],
            removed: [false; 3],
            auto: [false; 3],
            bots: Vec::new(),
            nonce: 0,
            pool,
        }
    }

    async fn call(&self, who: &Caller, method: &str, b: Value) -> anyhow::Result<Value> {
        dispatch(&self.r.hub, who, method, b).await
    }

    fn next_nonce(&mut self) -> String {
        self.nonce += 1;
        format!("n{}", self.nonce)
    }

    /// Whether person `o` holds agent `a` now, by the model.
    fn holds(&self, o: usize, a: usize) -> bool {
        o == 0 || (!self.removed[o] && self.roles[o][a] != 0)
    }

    fn live(&self, o: usize) -> usize {
        self.bots.iter().filter(|b| b.owner == o && b.state != St::Ended).count()
    }

    fn manages(&self, actor: usize, b: &MBot) -> bool {
        !self.removed[actor] && (actor == 0 || b.owner == actor)
    }

    fn store_state(&self, id: &str) -> Option<BotState> {
        self.r.hub.store.bot_guest(id).map(|g| g.state)
    }

    /// A bot of `actor`, joined as the cloud lists it and proven, as an app would.
    async fn create(&mut self, actor: usize, agents: u8) -> Result<(), String> {
        let list: Vec<&str> = (0..2).filter(|i| (agents >> i) & 1 == 1).map(|i| AGENTS[i]).collect();
        let held = (0..2).all(|i| (agents >> i) & 1 == 0 || self.holds(actor, i));
        let ok = !self.removed[actor] && self.live(actor) < 10 && held;
        let name = format!("B{}", self.next_nonce());
        let before = self.r.hub.store.bot_guests().unwrap().len();
        let body = json!({"name": name, "provider": "grok", "agents": list});
        let res = self.call(&self.callers[actor], "createBotInvite", body).await;
        let made = match (ok, res) {
            (true, Ok(made)) => made,
            (true, Err(e)) => return Err(format!("{} was refused a bot of {list:?} they may have: {e:#}", WHO[actor])),
            (false, Ok(_)) => {
                return Err(format!(
                    "{} was given a bot of {list:?} (they hold {:?}, removed {}, live {})",
                    WHO[actor],
                    self.roles[actor],
                    self.removed[actor],
                    self.live(actor)
                ));
            }
            (false, Err(_)) => {
                let after = self.r.hub.store.bot_guests().unwrap().len();
                check!(after == before, "a refused createBotInvite left a bot behind");
                return Ok(());
            }
        };
        let key = b64(&crypto::random::<32>());
        let invite = made["inviteId"].as_str().unwrap().to_owned();
        let listed =
            Listed { bot_device_id: format!("bd_{}", &key[..8]), invite_id: invite.clone(), device_key: key.clone() };
        // The cloud lists every bot device it has, so the ones already admitted stay in the list.
        let mut all: Vec<Listed> = self
            .r
            .hub
            .store
            .bot_guests()
            .unwrap()
            .into_iter()
            .filter(|g| g.state.live())
            .filter_map(|g| g.cloud_device)
            .map(|bot_device_id| Listed { bot_device_id, invite_id: String::new(), device_key: String::new() })
            .collect();
        all.push(listed);
        botsync::apply(&self.r.hub, &all).await.unwrap();
        let caller = Caller::Device { key, scopes: vec![Scope::Control] };
        let token = token_of(made["link"].as_str().unwrap());
        let proven = self
            .call(&caller, "proveBotInvite", json!({"inviteId": invite, "token": token}))
            .await
            .map_err(|e| format!("a new bot could not show its invite: {e:#}"))?;
        // Its own owner's bots are in at once; a person's wait for the computer's owner unless they said always.
        let state = if actor == 0 || self.auto[actor] { St::Active } else { St::Pending };
        let want = if state == St::Active { "active" } else { "pending" };
        check!(
            proven["state"] == want,
            "a bot of {} came in as {} when it should be {want}",
            WHO[actor],
            proven["state"]
        );
        let id = made["botId"].as_str().unwrap().to_owned();
        let agent_set: [bool; 2] = std::array::from_fn(|i| (agents >> i) & 1 == 1);
        self.bots.push(MBot {
            id,
            caller,
            owner: actor,
            agents: agent_set,
            state,
            invite,
            served: false,
            convs: vec![],
        });
        Ok(())
    }

    async fn step(&mut self, op: &Op) -> Result<(), String> {
        let n = self.bots.len();
        match *op {
            Op::Create { actor, agents } => self.create(actor, agents).await?,
            Op::Allow { bot, always } if n > 0 => {
                let bi = bot % n;
                let id = self.bots[bi].id.clone();
                let state = self.bots[bi].state;
                let owner = self.bots[bi].owner;
                let body = json!({"botId": id, "decision": "allow", "alwaysForOwner": always});
                let res = self.call(&Caller::Local, "decideBot", body).await;
                if state == St::Ended {
                    check!(res.is_err(), "decideBot let an ended bot back in");
                } else {
                    res.map_err(|e| format!("decideBot allow failed: {e:#}"))?;
                    self.bots[bi].state = St::Active;
                    if always && owner != 0 {
                        self.auto[owner] = true;
                        for b in self.bots.iter_mut().filter(|b| b.owner == owner && b.state == St::Pending) {
                            b.state = St::Active;
                        }
                    }
                }
            }
            Op::RevokeBot { actor, bot } if n > 0 => {
                let bi = bot % n;
                let manages = self.manages(actor, &self.bots[bi]);
                let id = self.bots[bi].id.clone();
                let res = self.call(&self.callers[actor], "revokeBot", json!({"botId": id})).await;
                if manages {
                    res.map_err(|e| format!("{} could not remove their own bot: {e:#}", WHO[actor]))?;
                    self.bots[bi].state = St::Ended;
                } else {
                    let e = res.err();
                    check!(e.as_ref().is_some_and(refused), "{} removed a bot that is not theirs", WHO[actor]);
                }
            }
            Op::RevokeInvite { actor, bot } if n > 0 => {
                let bi = bot % n;
                let manages = self.manages(actor, &self.bots[bi]);
                let invite = self.bots[bi].invite.clone();
                let res = self.call(&self.callers[actor], "revokeBotInvite", json!({"inviteId": invite})).await;
                if manages {
                    res.map_err(|e| format!("{} could not revoke their own bot's invite: {e:#}", WHO[actor]))?;
                } else {
                    check!(
                        res.err().as_ref().is_some_and(refused),
                        "{} revoked an invite that is not theirs",
                        WHO[actor]
                    );
                }
            }
            Op::Reinvite { actor, bot } if n > 0 => {
                // A fresh invite for a bot the caller already has (a rebuilt VM): theirs and still live, or refused.
                let bi = bot % n;
                let b = &self.bots[bi];
                let ok = self.manages(actor, b) && b.state != St::Ended;
                let id = b.id.clone();
                let res =
                    self.call(&self.callers[actor], "createBotInvite", json!({"name": "again", "botId": id})).await;
                if ok {
                    res.map_err(|e| format!("{} could not re-invite their own bot: {e:#}", WHO[actor]))?;
                } else {
                    check!(
                        res.err().as_ref().is_some_and(refused),
                        "{} made an invite for a bot that is not theirs or is gone",
                        WHO[actor]
                    );
                }
            }
            Op::MyBots { actor } => {
                let res = self.call(&self.callers[actor], "myBots", json!({})).await;
                if self.removed[actor] {
                    check!(res.is_err(), "a removed person still lists bots");
                } else {
                    let v = res.map_err(|e| format!("myBots failed: {e:#}"))?;
                    let mut got: Vec<String> =
                        v["bots"].as_array().unwrap().iter().map(|b| b["botId"].as_str().unwrap().to_owned()).collect();
                    let mut want: Vec<String> =
                        self.bots.iter().filter(|b| b.owner == actor).map(|b| b.id.clone()).collect();
                    got.sort();
                    want.sort();
                    check!(got == want, "myBots of {} listed {got:?}, their bots are {want:?}", WHO[actor]);
                }
            }
            Op::SetLimits { actor, bot, rate, conc } if n > 0 => {
                let bi = bot % n;
                let b = &self.bots[bi];
                let manages = self.manages(actor, b);
                let (rate, conc) = (RATES[rate], CONCS[conc]);
                let (max_rate, max_conc) = if actor == 0 { (600, 8) } else { (60, 2) };
                let ok =
                    manages && b.state != St::Ended && (1..=max_rate).contains(&rate) && (1..=max_conc).contains(&conc);
                let id = b.id.clone();
                let before = self.r.hub.store.bot_guest(&id).map(|g| (g.rate_per_hour, g.concurrent)).unwrap();
                let body = json!({"botId": id, "ratePerHour": rate, "concurrent": conc});
                let res = self.call(&self.callers[actor], "setBotLimits", body).await;
                let after = self.r.hub.store.bot_guest(&id).map(|g| (g.rate_per_hour, g.concurrent)).unwrap();
                if ok {
                    res.map_err(|e| format!("a fair limit was refused: {e:#}"))?;
                    check!(after == (rate, conc), "limits are {after:?}, asked for {:?}", (rate, conc));
                } else {
                    check!(res.is_err(), "{} set limits {rate}/{conc} on a bot they may not", WHO[actor]);
                    check!(after == before, "a refused setBotLimits still changed {before:?} to {after:?}");
                    if !manages {
                        check!(
                            res.err().as_ref().is_some_and(refused),
                            "setBotLimits on another's bot said something else"
                        );
                    }
                }
            }
            Op::SetAccess { person, roles } if !self.removed[person] => {
                let agents: serde_json::Map<String, Value> = (0..2)
                    .filter(|i| roles[*i] != 0)
                    .map(|i| (AGENTS[i].to_owned(), json!(["", "member", "guest"][usize::from(roles[i])])))
                    .collect();
                let body = json!({"personId": WHO[person], "agents": agents});
                self.call(&Caller::Local, "setPersonAccess", body)
                    .await
                    .map_err(|e| format!("setPersonAccess {roles:?} failed: {e:#}"))?;
                self.roles[person] = roles;
            }
            Op::Remove { person } if !self.removed[person] => {
                self.call(&Caller::Local, "revokePerson", json!({"personId": WHO[person]}))
                    .await
                    .map_err(|e| format!("revokePerson failed: {e:#}"))?;
                self.removed[person] = true;
                for b in self.bots.iter_mut().filter(|b| b.owner == person) {
                    b.state = St::Ended;
                }
            }
            Op::Call { bot, method } if n > 0 => {
                let bi = bot % n;
                let m = self.pool[method % self.pool.len()].clone();
                let res = self.call(&self.bots[bi].caller, &m, json!({})).await;
                let state = self.bots[bi].state;
                match state {
                    St::Ended => check!(
                        res.as_ref().err().is_some_and(anyhow::Error::is::<Forbidden>),
                        "a bot that was ended could call {m}: {res:?}"
                    ),
                    St::Active => {
                        let listed = BOT_METHODS.contains(&m.as_str());
                        match &res {
                            Err(e) if listed => {
                                check!(
                                    !refused(e) || m == "proveBotInvite",
                                    "{m} is on the bot list but an active bot was refused: {e:#}"
                                );
                            }
                            Err(e) => check!(refused(e), "an active bot reached {m}, which is not on its list: {e:#}"),
                            Ok(_) => check!(listed, "an active bot called {m}, which is not on its list"),
                        }
                        if m == "botLeave" && res.is_ok() {
                            self.bots[bi].state = St::Ended;
                        }
                    }
                    St::Pending => {
                        let listed = BOT_PENDING_METHODS.contains(&m.as_str());
                        match &res {
                            Err(e) if listed => check!(
                                !refused(e) || m == "proveBotInvite",
                                "{m} is for a bot not yet let in but it was refused: {e:#}"
                            ),
                            Err(e) => check!(refused(e), "a pending bot reached {m}: {e:#}"),
                            Ok(_) => check!(listed, "a bot that waits for its owner called {m}"),
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Every method a bot may not call, by name, for every bot: refused whatever the state (a random draw hits a
    /// single method too rarely).
    async fn sweep(&self) -> Result<(), String> {
        for b in &self.bots {
            for m in &self.pool {
                let listed = BOT_METHODS.contains(&m.as_str());
                let waiting_ok = BOT_PENDING_METHODS.contains(&m.as_str());
                let must_refuse = match b.state {
                    St::Ended => true,
                    St::Active => !listed,
                    St::Pending => !waiting_ok,
                };
                if must_refuse {
                    let res = self.call(&b.caller, m, json!({})).await;
                    check!(
                        res.as_ref().err().is_some_and(refused),
                        "a bot of {} ({:?}) reached {m:?}: {res:?}",
                        WHO[b.owner],
                        b.state
                    );
                }
            }
        }
        self.sweep_owners().await
    }

    /// What people can do to bots that are not theirs, and the limits a bot's owner may set, for every bot.
    async fn sweep_owners(&self) -> Result<(), String> {
        for actor in 0..3 {
            if self.removed[actor] {
                continue;
            }
            // myBots lists the caller's own bots and no others.
            let v = self.call(&self.callers[actor], "myBots", json!({})).await.map_err(|e| format!("myBots: {e:#}"))?;
            let mut got: Vec<String> =
                v["bots"].as_array().unwrap().iter().map(|b| b["botId"].as_str().unwrap().to_owned()).collect();
            let mut want: Vec<String> = self.bots.iter().filter(|b| b.owner == actor).map(|b| b.id.clone()).collect();
            got.sort();
            want.sort();
            check!(got == want, "myBots of {} listed {got:?}, their bots are {want:?}", WHO[actor]);
            // A bot is given only what its owner holds.
            for ai in 0..2 {
                if actor > 0 && !self.holds(actor, ai) && self.live(actor) < 10 {
                    let body = json!({"name": "too much", "agents": [AGENTS[ai]]});
                    let res = self.call(&self.callers[actor], "createBotInvite", body).await;
                    check!(
                        res.as_ref().err().is_some_and(refused),
                        "{} was given a bot of {} they do not hold: {res:?}",
                        WHO[actor],
                        AGENTS[ai]
                    );
                }
            }
            for b in self.bots.iter().filter(|b| b.owner != actor && actor > 0) {
                let (id, invite) = (&b.id, &b.invite);
                let calls = [
                    ("revokeBot", json!({"botId": id})),
                    ("revokeBotInvite", json!({"inviteId": invite})),
                    ("setBotLimits", json!({"botId": id, "ratePerHour": 1})),
                    ("createBotInvite", json!({"name": "mine now", "botId": id})),
                ];
                for (m, body) in calls {
                    let res = self.call(&self.callers[actor], m, body).await;
                    check!(
                        res.as_ref().err().is_some_and(refused),
                        "{} called {m} on a bot of {}: {res:?}",
                        WHO[actor],
                        WHO[b.owner]
                    );
                }
            }
        }
        // A person lowers a bot's limits to the defaults at most; only the computer's owner raises them.
        for b in self.bots.iter().filter(|b| b.owner > 0 && b.state != St::Ended && !self.removed[b.owner]) {
            for body in
                [json!({"ratePerHour": 61}), json!({"concurrent": 3}), json!({"ratePerHour": 600, "concurrent": 8})]
            {
                let mut body = body;
                body["botId"] = json!(b.id);
                let before = self.r.hub.store.bot_guest(&b.id).map(|g| (g.rate_per_hour, g.concurrent)).unwrap();
                let res = self.call(&self.callers[b.owner], "setBotLimits", body.clone()).await;
                let after = self.r.hub.store.bot_guest(&b.id).map(|g| (g.rate_per_hour, g.concurrent)).unwrap();
                check!(
                    res.is_err() && after == before,
                    "{} raised their bot's limits with {body}: {res:?}",
                    WHO[b.owner]
                );
            }
        }
        Ok(())
    }

    /// Every bot, against the model: the store agrees on who is in, the principal follows, and each ask is served only
    /// when the bot is in, was given the agent and its owner holds it now.
    async fn check_all(&mut self) -> Result<(), String> {
        for bi in 0..self.bots.len() {
            let (id, owner, state) = (self.bots[bi].id.clone(), self.bots[bi].owner, self.bots[bi].state);
            let stored = self.store_state(&id).ok_or("a bot row vanished")?;
            let agrees = match state {
                St::Pending => stored == BotState::Pending,
                St::Active => stored == BotState::Active,
                St::Ended => matches!(stored, BotState::Revoked | BotState::Denied),
            };
            check!(agrees, "bot {id} of {} is {stored:?} in the store, the model has {state:?}", WHO[owner]);
            let who = principal(&self.r.hub.store, &self.bots[bi].caller);
            let right = match state {
                St::Pending => matches!(who, Principal::BotPending(_)),
                St::Active => matches!(who, Principal::Bot { .. }),
                St::Ended => who == Principal::Nobody,
            };
            check!(right, "bot {id} is {state:?} but its key is {who:?}");
            for ai in 0..2 {
                let nonce = self.next_nonce();
                let ask = json!({"agent": AGENTS[ai], "text": "hi", "clientNonce": nonce});
                let res = self.call(&self.bots[bi].caller, "botAsk", ask).await;
                let may = self.bots[bi].agents[ai] && self.holds(owner, ai);
                match (state, &res) {
                    (St::Ended, Err(e)) => check!(e.is::<Forbidden>(), "an ended bot's ask: {e:#}"),
                    (St::Ended, Ok(_)) => return Err(format!("bot {id} was ended and still asked {}", AGENTS[ai])),
                    (St::Pending, Err(e)) => check!(error_code(e) == Some("pending"), "a pending bot's ask: {e:#}"),
                    (St::Pending, Ok(_)) => return Err(format!("bot {id} asked {} before it was let in", AGENTS[ai])),
                    (St::Active, Ok(v)) => {
                        check!(
                            may,
                            "bot {id} of {} asked {} and was served; given {:?}, owner holds {:?}, removed {}",
                            WHO[owner],
                            AGENTS[ai],
                            self.bots[bi].agents,
                            self.roles[owner],
                            self.removed[owner]
                        );
                        self.bots[bi].served = true;
                        self.bots[bi].convs.push(v["conversation"].as_str().unwrap().to_owned());
                        SERVED.fetch_add(1, Ordering::Relaxed);
                    }
                    (St::Active, Err(e)) if may => {
                        check!(!refused(e), "bot {id} may use {} but was refused: {e:#}", AGENTS[ai]);
                        check!(
                            self.bots[bi].served,
                            "the first ask of a bot that may use {} failed: {e:#}",
                            AGENTS[ai]
                        );
                    }
                    (St::Active, Err(e)) => check!(
                        error_code(e) == Some("notGranted"),
                        "bot {id} of {} asked {} it may not use and got: {e:#}",
                        WHO[owner],
                        AGENTS[ai]
                    ),
                }
            }
            if state == St::Active {
                // What it is told it holds is never more than its owner holds.
                let status = self.call(&self.bots[bi].caller, "botStatus", json!({})).await;
                let status = status.map_err(|e| format!("botStatus failed for an active bot: {e:#}"))?;
                let listed: Vec<String> = status["agents"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|a| a["agent"].as_str().unwrap().to_owned())
                    .collect();
                for ai in 0..2 {
                    let may = self.bots[bi].agents[ai] && self.holds(owner, ai);
                    check!(
                        listed.iter().any(|a| a == AGENTS[ai]) == may,
                        "botStatus of {id} lists {listed:?}; it may use {} = {may}",
                        AGENTS[ai]
                    );
                }
                // Another bot's conversation is a 404 and its words never come back.
                let other: Option<String> = self
                    .bots
                    .iter()
                    .enumerate()
                    .find_map(|(j, o)| (j != bi).then(|| o.convs.first().cloned()).flatten());
                if let Some(conv) = other {
                    let nonce = self.next_nonce();
                    let ask = json!({"conversation": conv, "text": "mine now", "clientNonce": nonce});
                    let res = self.call(&self.bots[bi].caller, "botAsk", ask).await;
                    check!(
                        res.as_ref().err().is_some_and(|e| error_code(e) == Some("notFound")),
                        "bot {id} asked into another bot's conversation: {res:?}"
                    );
                    let res = self.call(&self.bots[bi].caller, "botThread", json!({"conversation": conv})).await;
                    check!(
                        res.as_ref().err().is_some_and(|e| error_code(e) == Some("notFound")),
                        "bot {id} read another bot's conversation: {res:?}"
                    );
                }
            }
        }
        Ok(())
    }
}

static SERVED: AtomicUsize = AtomicUsize::new(0);

fn pool() -> Vec<String> {
    let mut pool = router_methods();
    pool.extend(["nope".to_owned(), String::new(), "HELLO".to_owned(), "botAsk ".to_owned()]);
    pool
}

fn sim() -> (tokio::runtime::Runtime, Vec<String>) {
    (tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap(), pool())
}

/// (a) (b) (c) (d): a bot of Kai's or of a person's, whatever happens to its owner's grant, to it, and to the
/// people around it.
#[test]
fn a_bot_never_has_more_than_its_owner_holds_now() {
    let (rt, pool) = sim();
    assert!(pool.len() > 100, "router walk found {} methods", pool.len());
    runner(10)
        .run(&proptest::collection::vec(op(), 1..9), |ops| {
            // Dropped before the next case builds its own: the rig holds the process-wide environment.
            let mut state = rt.block_on(State::new(pool.clone()));
            let outcome = rt.block_on(async {
                // Kai's own bot first, so a case never passes by refusing everything.
                state.create(0, 3).await?;
                // And one of each person's, so the cascades below always have something to cut.
                state.create(1, 3).await?;
                state.create(2, 1).await?;
                state.check_all().await?;
                for o in &ops {
                    state.step(o).await?;
                    state.check_all().await?;
                }
                state.sweep().await?;
                // Then, whatever came before: one person loses an agent, the other is removed.
                for o in [Op::SetAccess { person: 2, roles: [0, 0] }, Op::Remove { person: 1 }] {
                    state.step(&o).await?;
                    state.check_all().await?;
                }
                state.sweep().await?;
                Ok::<(), String>(())
            });
            drop(state);
            outcome.map_err(|e| fail(format!("{e}; ops {ops:?}")))
        })
        .unwrap();
    assert!(SERVED.load(Ordering::Relaxed) > 0, "no ask was ever served: the property checked nothing");
}

/// (f) Ten live bots per owner, counted per owner: the eleventh is refused with `tooMany` whatever another owner
/// holds, another owner's are not counted, and one removed makes room.
#[test]
fn a_bot_owner_has_at_most_ten_live_bots() {
    let (rt, pool) = sim();
    let tail = proptest::collection::vec((proptest::bool::weighted(0.3), 0..16usize), 0..6);
    runner(4)
        .run(&(0..3usize, 1..4usize, tail), |(hammer, others, tail)| {
            let other = (hammer + 1) % 3;
            let mut state = rt.block_on(State::new(pool.clone()));
            let outcome = rt.block_on(async {
                let mut ids: [Vec<String>; 3] = [vec![], vec![], vec![]];
                // Eleven attempts by one owner with nothing removed in between, after another owner made a few;
                // then whatever the tail does (removing one makes room, a repeat removal is harmless).
                let mut steps: Vec<(usize, bool, usize)> = (0..others).map(|_| (other, false, 0)).collect();
                steps.extend((0..11).map(|_| (hammer, false, 0)));
                steps.extend(tail.iter().map(|(revoke, nth)| (hammer, *revoke, *nth)));
                for (actor, revoke, nth) in steps {
                    if revoke {
                        if ids[actor].is_empty() {
                            continue;
                        }
                        let id = ids[actor].remove(nth % ids[actor].len());
                        let res = state.call(&state.callers[actor], "revokeBot", json!({"botId": id})).await;
                        res.map_err(|e| format!("could not remove a bot: {e:#}"))?;
                    } else {
                        let live = state.r.hub.store.live_bot_count((actor > 0).then(|| WHO[actor]));
                        check!(
                            live == ids[actor].len(),
                            "{} has {live} live bots, the model {}",
                            WHO[actor],
                            ids[actor].len()
                        );
                        let name = format!("B{}", state.next_nonce());
                        let res = state
                            .call(&state.callers[actor], "createBotInvite", json!({"name": name, "agents": []}))
                            .await;
                        if ids[actor].len() < 10 {
                            let made =
                                res.map_err(|e| format!("bot number {} was refused: {e:#}", ids[actor].len() + 1))?;
                            ids[actor].push(made["botId"].as_str().unwrap().to_owned());
                        } else {
                            let e = res.err().ok_or(format!("{} got an eleventh live bot", WHO[actor]))?;
                            check!(error_code(&e) == Some("tooMany"), "the eleventh bot was refused with: {e:#}");
                        }
                    }
                    for o in 0..3 {
                        let n = state.r.hub.store.live_bot_count((o > 0).then(|| WHO[o]));
                        check!(
                            n <= 10 && n == ids[o].len(),
                            "{} has {n} live bots, the model {}",
                            WHO[o],
                            ids[o].len()
                        );
                    }
                }
                Ok::<(), String>(())
            });
            drop(state);
            outcome.map_err(|e| fail(format!("{e}; hammer {hammer}, others {others}, tail {tail:?}")))
        })
        .unwrap();
}

// ---- (e) what a bot is sent ----

fn temp_store() -> (std::path::PathBuf, Store) {
    let dir = std::env::temp_dir().join(format!("kith-botguests-props-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("t.db")).unwrap();
    (dir, store)
}

fn chat(id: &str, person: Option<&str>) -> crate::store::BotConfig {
    serde_json::from_value(json!({
        "id": id, "name": id, "backend": "claude", "cwd": "/Users/secret", "command": "/Users/secret/run",
        "hqAgent": "house", "person": person, "permission": "ask", "tasks": false, "connectors": [], "skills": [],
    }))
    .unwrap()
}

/// What `event_for_bot` is judged on: three bot guests (two in, one waiting), each with a chat and a conversation,
/// the owner's private chat, a person's chat and a thread of a bot's own chat that is not its conversation.
struct Feed {
    store: Store,
    dir: std::path::PathBuf,
    /// Per bot guest: id, chat, thread root, conversation id.
    bots: Vec<(String, String, String, String)>,
    chats: Vec<String>,
    threads: Vec<String>,
}

impl Feed {
    fn new() -> Self {
        let (dir, store) = temp_store();
        let mut bots = Vec::new();
        let mut chats = Vec::new();
        let mut threads = Vec::new();
        let root = |chat: &str| {
            let e = store.insert_entry(&Lane::main(chat), EntryKind::Notice, 1, &json!({"text": "root"})).unwrap();
            e.id
        };
        for i in 0..3 {
            let (bot_id, person) = (format!("bg{i}"), format!("bot-g{i}"));
            let chat_id = format!("chat{i}");
            let owner = (i == 1).then(|| "p1".to_owned());
            let mut g = bot_guest(&bot_id, &person, owner);
            g.state = if i == 2 { BotState::Pending } else { BotState::Active };
            store.put_bot_guest(&g).unwrap();
            store.save_bot(&chat(&chat_id, Some(&person))).unwrap();
            let thread = root(&chat_id);
            let conv = BotConversation {
                conversation_id: format!("cv{i}"),
                bot_id: bot_id.clone(),
                agent: "house".into(),
                chat_bot_id: chat_id.clone(),
                thread_root: thread.clone(),
                external_ref: None,
                room_id: None,
                speaker: None,
                origin_label: "from Grok".into(),
                created_at: 1,
                last_at: 1,
            };
            store.put_bot_conversation(&conv).unwrap();
            chats.push(chat_id.clone());
            threads.push(thread.clone());
            bots.push((bot_id, chat_id, thread, format!("cv{i}")));
        }
        // The owner's private chat and a person's, each with a thread of its own, and a thread in a bot's chat that
        // no conversation points to.
        for (id, person) in [("private", None), ("persons", Some("p1"))] {
            store.save_bot(&chat(id, person)).unwrap();
            chats.push(id.to_owned());
            threads.push(root(id));
        }
        threads.push(root("chat0"));
        chats.push("ghost".to_owned());
        threads.push("no-such-thread".to_owned());
        Self { store, dir, bots, chats, threads }
    }
}

impl Drop for Feed {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

const KINDS: [&str; 7] = ["entry", "bot", "call", "usage", "botUpdated", "resync", "screens"];
const ENTRY_KINDS: [&str; 6] = ["user", "agent", "permission", "notice", "tool", "thought"];

#[derive(Clone, Debug)]
struct Ev {
    to: usize,
    kind: usize,
    chat: usize,
    thread: usize,
    entry_kind: usize,
}

/// (e) A bot is sent entries of its own conversations only, shaped to words: never another bot's, a person's, the
/// owner's private chat, another thread of its own chat, status, calls, usage or a card.
#[test]
fn a_bot_is_sent_only_its_own_conversations() {
    let feed = Feed::new();
    let ev = (0..3usize, 0..KINDS.len(), 0..feed.chats.len(), 0..feed.threads.len(), 0..ENTRY_KINDS.len())
        .prop_map(|(to, kind, chat, thread, entry_kind)| Ev { to, kind, chat, thread, entry_kind })
        // Two thirds name a bot's chat and thread, or no word would ever reach a bot.
        .prop_flat_map(|e| (Just(e), 0..3u8))
        .prop_map(|(mut e, mode)| {
            // 1: the receiver's own chat and thread; 2: another bot's, which holds a conversation too.
            if mode > 0 {
                let chat = if mode == 1 { e.to } else { (e.to + 1) % 3 };
                e.chat = chat;
                e.thread = chat;
                e.kind = 0;
            }
            e
        })
        // 0 an ordinary entry, 1 a key card, 2 an account card (cards/secret.rs): those never reach a bot.
        .prop_flat_map(|e| (Just(e), 0..4u8));
    let served = std::cell::Cell::new(0usize);
    runner(400)
        .run(&ev, |(e, secret)| {
            let (bot_id, own_chat, own_thread, conv) = &feed.bots[e.to];
            let (chat_id, thread_id) = (&feed.chats[e.chat], &feed.threads[e.thread]);
            let mut data = json!({
                "text": "hello", "status": "pending", "style": "error", "final": true,
                "tool": SECRET, "args": SECRET, "title": SECRET, "path": SECRET, "cwd": SECRET, "author": SECRET,
                "options": [SECRET], "attachments": [SECRET], "input": {"command": SECRET},
            });
            if secret > 0 {
                data["card"] = json!(["", "key", "account", "account"][usize::from(secret)]);
            }
            let entry = json!({
                "id": "e1", "rev": 3, "turn": 1, "createdAt": 5, "kind": ENTRY_KINDS[e.entry_kind], "data": data,
                "botId": chat_id, "threadId": thread_id,
            });
            let kind = KINDS[e.kind];
            let v = json!({"type": kind, "rev": 4, "entry": entry, "bot": {"id": chat_id, "cwd": SECRET}, "call": {"botId": chat_id}});
            let out = roles::event_for_bot(&feed.store, bot_id, v.clone());
            match kind {
                "resync" => prop_assert_eq!(out, Some(v)),
                "entry" => {
                    let own = chat_id == own_chat && thread_id == own_thread;
                    let active = e.to != 2;
                    if secret > 0 {
                        prop_assert!(out.is_none(), "a key or account card reached {bot_id}: {out:?}");
                        return Ok(());
                    }
                    match out {
                        Some(out) => {
                            prop_assert!(own && active, "{bot_id} was sent {chat_id}/{thread_id}: {out}");
                            prop_assert!(!out.to_string().contains(SECRET), "{out}");
                            prop_assert_eq!(out["conversation"].as_str(), Some(conv.as_str()));
                            served.set(served.get() + 1);
                        }
                        // Words must reach the bot, or "send nothing" would pass.
                        None => prop_assert!(
                            !(own && active && matches!(ENTRY_KINDS[e.entry_kind], "user" | "agent")),
                            "{bot_id} was not sent its own words"
                        ),
                    }
                }
                other => prop_assert!(out.is_none(), "{other} reached a bot: {out:?}"),
            }
            Ok(())
        })
        .unwrap();
    assert!(served.get() > 0, "no entry was ever sent to a bot: the property checked nothing");
}
