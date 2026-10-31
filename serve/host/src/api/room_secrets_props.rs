//! Property test for room secrets (designs/2026-10-31-room-secrets, invitee-access skill): random histories of two bots
//! joining and leaving a room, the owner handing each a secret, fetches by the right bot, the wrong bot and after the
//! end, the clock running out, and the minute's sweep, all through the real handlers (`sendSecret`, `botSecretFetch`,
//! `botRoomRead`, `event_for_bot`) on a real kith-auth with a stand-in Keychain. The oracle is a small model kept outside
//! the host: a secret is live from its send until the first of its fetch, its time, its recipient leaving the room or the
//! owner withdrawing it. After any history a fetch answers only the live secret of the bot that asks and answers it
//! once (every other call is an error, with no value in it), `roomSecret` events and `botRoomRead.secrets` name
//! exactly the live secrets of their own bot, and no bot's answer ever holds another's value.
//! Run only this: `cargo test --bin kith room_secrets_props`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::dispatch;
use super::roles::event_for_bot;
use crate::cards::room_secret;
use crate::cards::room_secret_tests::{World, marker};
use crate::store::now_ms;
use proptest::prelude::*;
use proptest::test_runner::{Config, TestCaseError, TestRunner};
use serde_json::json;

const BOTS: [&str; 2] = ["bg1", "bg2"];

#[derive(Clone, Debug)]
enum Op {
    Add(usize),
    Remove(usize),
    Send(usize),
    /// Bot `bot` fetches secret number `nth` (of any bot's), or one that doesn't exist.
    Fetch {
        bot: usize,
        nth: usize,
    },
    Expire(usize),
    Withdraw(usize),
    Sweep,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => (0..2usize).prop_map(Op::Add),
        2 => (0..2usize).prop_map(Op::Remove),
        5 => (0..2usize).prop_map(Op::Send),
        6 => (0..2usize, 0..6usize).prop_map(|(bot, nth)| Op::Fetch { bot, nth }),
        1 => (0..6usize).prop_map(Op::Expire),
        1 => (0..6usize).prop_map(Op::Withdraw),
        1 => Just(Op::Sweep),
    ]
}

struct Made {
    id: String,
    to: usize,
    value: String,
    live: bool,
}

struct Model {
    w: World,
    member: [bool; 2],
    made: Vec<Made>,
}

impl Model {
    async fn new() -> Self {
        let w = World::new().await;
        w.guest("bg2", None, "bot-key-2");
        // `World::new` made bg1 a member; start with neither.
        w.set_members(&[&w.house.clone()]);
        Self { w, member: [false; 2], made: vec![] }
    }

    fn key(b: usize) -> &'static str {
        ["bot-key", "bot-key-2"][b]
    }

    fn members(&self) -> Vec<String> {
        let mut m = vec![self.w.house.clone()];
        m.extend(BOTS.iter().zip(self.member).filter(|(_, on)| *on).map(|(b, _)| (*b).to_owned()));
        m
    }

    fn apply_members(&self) {
        let m = self.members();
        self.w.set_members(&m.iter().map(String::as_str).collect::<Vec<_>>());
    }

    async fn step(&mut self, o: &Op) -> Result<(), String> {
        match o {
            Op::Add(b) if !self.member[*b] => {
                self.member[*b] = true;
                self.apply_members();
            }
            Op::Remove(b) if self.member[*b] => {
                self.member[*b] = false;
                self.apply_members();
                for s in self.made.iter_mut().filter(|s| s.to == *b) {
                    s.live = false;
                }
            }
            Op::Send(b) => {
                let value = marker();
                let sent = self.w.send(BOTS[*b], &value).await;
                match (self.member[*b], sent) {
                    (true, Ok(v)) => {
                        self.made.push(Made { id: v["id"].as_str().unwrap().to_owned(), to: *b, value, live: true });
                    }
                    (false, Ok(v)) => return Err(format!("a secret was sent to a bot that is out: {v}")),
                    (true, Err(e)) => return Err(format!("a send to a member failed: {e:#}")),
                    (false, Err(e)) => {
                        if format!("{e:#}").contains(&value) {
                            return Err("a refusal carried the value".into());
                        }
                    }
                }
            }
            Op::Fetch { bot, nth } => {
                let secret = self.made.get(*nth);
                let id = secret.map_or("rs_ffffffffffffffff".to_owned(), |s| s.id.clone());
                let answer =
                    dispatch(&self.w.r.hub, &self.w.bot(Self::key(*bot)), "botSecretFetch", json!({"id": id})).await;
                let want = secret.filter(|s| s.to == *bot && s.live && self.member[*bot]);
                match (want, answer) {
                    (Some(s), Ok(v)) => {
                        if v["value"] != s.value.as_str() {
                            return Err(format!("the wrong value was answered for {id}"));
                        }
                        self.made[*nth].live = false;
                    }
                    (Some(_), Err(e)) => return Err(format!("the recipient was refused its live secret: {e:#}")),
                    (None, Ok(v)) => {
                        let who = if self.member[*bot] { "a member" } else { "a bot that is out" };
                        return Err(format!("{who} was answered a secret that was not its to take: {v}"));
                    }
                    (None, Err(e)) => {
                        // Not a word of any value in a refusal.
                        for s in &self.made {
                            if format!("{e:#}").contains(&s.value) {
                                return Err("a refusal carried a value".into());
                            }
                        }
                    }
                }
            }
            Op::Expire(n) => {
                if let Some(s) = self.made.get_mut(*n) {
                    self.w.set_expires(&s.id, now_ms() - 1);
                    s.live = false;
                }
            }
            Op::Withdraw(n) => {
                if let Some(s) = self.made.get_mut(*n) {
                    dispatch(&self.w.r.hub, &crate::api::devices::Caller::Local, "withdrawSecret", json!({"id": s.id}))
                        .await
                        .map_err(|e| format!("{e:#}"))?;
                    s.live = false;
                }
            }
            Op::Sweep => room_secret::sweep(&self.w.r.hub).await,
            _ => {}
        }
        self.check().await
    }

    /// What each bot is told, against the model.
    async fn check(&self) -> Result<(), String> {
        let store = &self.w.r.hub.store;
        for (b, bot) in BOTS.iter().enumerate() {
            // The `roomSecret` event a send made, offered again: only its live recipient gets it.
            for s in &self.made {
                let ev =
                    json!({"type": "roomSecret", "room": self.w.room, "id": s.id, "to": BOTS[s.to], "from": "Kai"});
                let got = event_for_bot(store, bot, ev);
                let want = s.to == b && s.live && self.member[b];
                if got.is_some() != want {
                    return Err(format!("event_for_bot({bot}) for {} gave {got:?}, wanted {want}", s.id));
                }
                if got.is_some_and(|g| g.to_string().contains(&s.value)) {
                    return Err("an event carried the value".into());
                }
            }
            if self.member[b] {
                let read = dispatch(
                    &self.w.r.hub,
                    &self.w.bot(Self::key(b)),
                    "botRoomRead",
                    json!({"room": self.w.room, "limit": 200}),
                )
                .await
                .map_err(|e| format!("{e:#}"))?;
                let mut listed: Vec<&str> =
                    read["secrets"].as_array().unwrap().iter().filter_map(|s| s["id"].as_str()).collect();
                listed.sort_unstable();
                let mut want: Vec<&str> =
                    self.made.iter().filter(|s| s.to == b && s.live).map(|s| s.id.as_str()).collect();
                want.sort_unstable();
                if listed != want {
                    return Err(format!("botRoomRead for {bot} listed {listed:?}, wanted {want:?}"));
                }
                for s in &self.made {
                    if read.to_string().contains(&s.value) {
                        return Err("a room read carried a value".into());
                    }
                }
            }
        }
        Ok(())
    }
}

#[test]
fn a_secret_reaches_only_its_live_recipient_once() {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let mut config = Config { failure_persistence: None, max_shrink_iters: 16, ..Config::default() };
    config.cases = std::env::var("PROPTEST_CASES").ok().and_then(|c| c.parse().ok()).unwrap_or(8);
    TestRunner::new(config)
        .run(&proptest::collection::vec(op(), 1..14), |ops| {
            // Dropped before the next case builds its own: the rig holds the process-wide environment.
            let mut model = rt.block_on(Model::new());
            let outcome = rt.block_on(async {
                for o in &ops {
                    model.step(o).await?;
                }
                // Everything out and in again: nothing sent before comes back to a re-added bot.
                for o in [Op::Remove(0), Op::Remove(1), Op::Add(0), Op::Add(1)] {
                    model.step(&o).await?;
                }
                for n in 0..model.made.len() {
                    for bot in 0..2 {
                        model.step(&Op::Fetch { bot, nth: n }).await?;
                    }
                }
                Ok::<(), String>(())
            });
            drop(model);
            outcome.map_err(|e| TestCaseError::fail(format!("{e}; ops {ops:?}")))
        })
        .unwrap();
}
