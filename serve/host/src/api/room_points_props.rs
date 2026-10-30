//! Property test for "from now on": an outside bot in a room is served nothing that was said before it joined. Random
//! histories of add, remove, re-add, posts, edits and reads run through the real handlers (`botRoomRead`, the bot's
//! event fan-out `event_for_bot` for every stored entry, as a catch-up would send them). The oracle is the newest `seq`
//! that existed just before each add call, kept outside the host. After any history, no door returns an entry at or
//! below the latest add point, none from a gap while the bot was out, and an edit of an old entry (a new `rev`) does
//! not bring it back. Method: `.claude/skills/invitee-access/SKILL.md`. Run only this: `cargo test room_points_props`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::devices::Caller;
use super::dispatch;
use super::rig::Rig;
use super::roles::event_for_bot;
use super::roles_props::bot_guest;
use crate::store::{Device, DeviceSource, EntryKind, Lane, Scope};
use proptest::prelude::*;
use proptest::test_runner::{Config, TestCaseError, TestRunner};
use serde_json::{Value, json};

#[derive(Clone, Debug)]
enum Op {
    Add,
    Remove,
    /// A line in the main lane (`user`), or a final agent reply, or a quiet notice, or a line in a thread.
    Say {
        kind: u8,
        thread: bool,
    },
    /// Edits an earlier line, which gives it a newer `rev`.
    Edit {
        nth: usize,
    },
    Read {
        since_from_end: u8,
        limit: u8,
    },
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => Just(Op::Add),
        2 => Just(Op::Remove),
        6 => (0..3u8, any::<bool>()).prop_map(|(kind, thread)| Op::Say { kind, thread }),
        2 => (0..32usize).prop_map(|nth| Op::Edit { nth }),
        4 => (0..40u8, 1..60u8).prop_map(|(since_from_end, limit)| Op::Read { since_from_end, limit }),
    ]
}

struct World {
    r: Rig,
    room: String,
    bot: Caller,
    /// Text of each line said, by its `seq`, and its entry id (for edits).
    said: Vec<(i64, String, String)>,
    member: bool,
    /// The newest `seq` that existed just before the latest add call.
    point: i64,
    n: u32,
}

impl World {
    async fn new() -> Self {
        let r = Rig::new().await;
        let house = r.owner_bot("house");
        let mut g = bot_guest("bg1", "bot-bg1", None);
        g.device_key = Some("bot-key".into());
        r.hub.store.put_bot_guest(&g).unwrap();
        r.hub
            .store
            .put_device(&Device {
                key: "bot-key".into(),
                name: "Rex".into(),
                platform: "bot".into(),
                source: DeviceSource::Bot,
                grant_id: Some("bot:bd1".into()),
                scopes: vec![Scope::Control],
                lease_until: None,
                created_at: 1,
                last_seen_at: None,
                person_id: None,
            })
            .unwrap();
        r.hub.store.set_device_proven("bot-key").unwrap();
        let cfg = json!({"id": "", "kind": "group", "name": "Crew", "members": [house]});
        let room = r.hub.create_bot(serde_json::from_value(cfg).unwrap()).unwrap()["id"].as_str().unwrap().to_owned();
        Self {
            r,
            room,
            bot: Caller::Device { key: "bot-key".into(), scopes: vec![Scope::Control] },
            said: vec![],
            member: false,
            point: 0,
            n: 0,
        }
    }

    fn members(&self, with_bot: bool) -> Value {
        let house = self.r.hub.store.bot(&self.room).unwrap().unwrap().config.members;
        let mut m: Vec<String> = house.into_iter().filter(|m| m != "bg1").collect();
        if with_bot {
            m.push("bg1".into());
        }
        json!({"id": self.room, "members": m})
    }

    /// The newest `seq` in the whole store (the point is global, as entries are).
    fn global_seq(&self) -> i64 {
        self.r.hub.store.entries_since(0, 100_000).unwrap().iter().map(|e| e.seq).max().unwrap_or(0)
    }

    async fn step(&mut self, o: &Op) -> Result<(), String> {
        let hub = &self.r.hub;
        match o {
            Op::Add if !self.member => {
                self.point = self.global_seq();
                hub.update_bot(&self.members(true)).map_err(|e| format!("add: {e:#}"))?;
                self.member = true;
            }
            Op::Remove if self.member => {
                hub.update_bot(&self.members(false)).map_err(|e| format!("remove: {e:#}"))?;
                self.member = false;
            }
            Op::Say { kind, thread } => {
                self.n += 1;
                let text = format!("line-{}", self.n);
                let lane = match (thread, self.said.first()) {
                    (true, Some((_, _, root))) => Lane::in_thread(&self.room, root),
                    _ => Lane::main(&self.room),
                };
                let (k, data) = match kind {
                    0 => (EntryKind::User, json!({"text": text})),
                    1 => (EntryKind::Agent, json!({"text": text, "final": true, "author": "house"})),
                    _ => (EntryKind::Notice, json!({"text": text, "style": "info"})),
                };
                let e = hub.add_entry(&lane, k, 1, &data).ok_or("couldn't add a line")?;
                self.said.push((e.seq, text, e.id));
            }
            Op::Edit { nth } if !self.said.is_empty() => {
                let (_, text, id) = self.said[nth % self.said.len()].clone();
                hub.set_entry(&id, &json!({"text": text, "final": true, "edited": true}));
            }
            Op::Read { since_from_end, limit } => {
                let head = hub.store.current_rev();
                let since = (head - i64::from(*since_from_end)).max(0);
                let read =
                    dispatch(hub, &self.bot, "botRoomRead", json!({"room": self.room, "since": since, "limit": limit}))
                        .await;
                match (self.member, read) {
                    (false, Ok(v)) => return Err(format!("a bot that is out read the room: {v}")),
                    (false, Err(_)) => {}
                    (true, Err(e)) => return Err(format!("a member's read failed: {e:#}")),
                    (true, Ok(v)) => self.holds(&v["entries"], "botRoomRead")?,
                }
            }
            _ => {}
        }
        self.check_doors().await
    }

    /// Every returned entry was said after the latest add.
    fn holds(&self, entries: &Value, door: &str) -> Result<(), String> {
        for e in entries.as_array().into_iter().flatten() {
            let text = e["text"].as_str().unwrap_or_default();
            if let Some((seq, ..)) = self.said.iter().find(|(_, t, _)| t == text)
                && *seq <= self.point
            {
                return Err(format!("{door} served {text} (seq {seq}) at or below the add point {}", self.point));
            }
        }
        Ok(())
    }

    async fn check_doors(&self) -> Result<(), String> {
        let hub = &self.r.hub;
        // Everything the owner's stream carries, offered to the bot's fan-out as a catch-up would.
        let mut seen = vec![];
        for e in hub.store.entries_since(0, 100_000).unwrap() {
            let ev = json!({"type": "entry", "rev": e.rev, "entry": e});
            if let Some(out) = event_for_bot(&hub.store, "bg1", ev) {
                if !self.member {
                    return Err(format!("an event reached a bot that is out: {out}"));
                }
                seen.push(out["entry"].clone());
            }
        }
        self.holds(&json!(seen), "event_for_bot")?;
        let conversation = json!({"type": "conversation", "chat": self.room, "conversation": "c"});
        if event_for_bot(&hub.store, "bg1", conversation).is_some() {
            return Err("a bot was told of a conversation".into());
        }
        // Completeness when it is in: a full read from the start returns every line said since the add.
        if self.member {
            let read = dispatch(hub, &self.bot, "botRoomRead", json!({"room": self.room, "limit": 200}))
                .await
                .map_err(|e| format!("{e:#}"))?;
            let got: Vec<&str> =
                read["entries"].as_array().unwrap().iter().filter_map(|e| e["text"].as_str()).collect();
            self.holds(&read["entries"], "botRoomRead from the start")?;
            let want: Vec<&str> = self
                .said
                .iter()
                .filter(|(seq, ..)| *seq > self.point)
                .filter(|(_, _, id)| matches!(hub.store.entry(id).map(|e| e.kind).as_deref(), Some("user" | "agent")))
                .map(|(_, t, _)| t.as_str())
                .collect();
            for t in want {
                if !got.contains(&t) {
                    return Err(format!("a member can't read {t}, said after it joined: {got:?}"));
                }
            }
        }
        Ok(())
    }
}

#[test]
fn a_bot_is_served_nothing_from_before_it_joined() {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let mut config = Config { failure_persistence: None, max_shrink_iters: 24, ..Config::default() };
    config.cases = std::env::var("PROPTEST_CASES").ok().and_then(|c| c.parse().ok()).unwrap_or(12);
    TestRunner::new(config)
        .run(&proptest::collection::vec(op(), 1..24), |ops| {
            // Dropped before the next case builds its own: the rig holds the process-wide environment.
            let mut world = rt.block_on(World::new());
            let outcome = rt.block_on(async {
                for o in &ops {
                    world.step(o).await?;
                }
                // Out and in once more, then everything again: a re-add moves the point past the gap.
                for o in [Op::Remove, Op::Say { kind: 0, thread: false }, Op::Add, Op::Say { kind: 0, thread: false }] {
                    world.step(&o).await?;
                }
                Ok::<(), String>(())
            });
            drop(world);
            outcome.map_err(|e| TestCaseError::fail(format!("{e}; ops {ops:?}")))
        })
        .unwrap();
}
