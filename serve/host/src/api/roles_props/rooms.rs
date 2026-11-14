//! Property 39 (rooms with people, slice B): whatever order people join, leave, are removed and added again, are
//! revoked and un-revoked, lose and get back their agent, post, and agents' lines arrive stamped with the gen their
//! turn started in, a person is shown exactly the lines the model says (R1, R3), and a post checked before a removal or
//! a re-add never lands after it (R4). The real `dispatch`, `commit_room_post`, `event_for` and `events` run against a
//! real kith-auth in a temp folder (the `Rig`); the model below is the whole of the rule:
//!
//! a person sees a line iff they have an open stay AND may use the room's agent now (not revoked, still granted) AND
//! the line carries a gen of at least their stay's `joined_gen`. A line never stamped is shown to no person.
//!
//! The broken variants of the design model (`split-post-w`: check and append apart; `notag`: a "member now" filter that
//! shows what the person could see today rather than from their stay on) fail this property: see the PR's verification.
//! Method: `.claude/skills/invitee-access/SKILL.md`. Run only these: `cargo test --bin kith -- roles_props`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::api::devices::Caller;
use crate::api::dispatch;
use crate::api::rig::Rig;
use crate::chat::room_people::{FLAG, Seat, live_seat};
use crate::remote::members::{self, Listed};
use crate::store::{Entry, EntryKind, Lane};
use proptest::prelude::*;
use proptest::test_runner::{Config, TestCaseError, TestRunner};
use serde_json::{Value, json};

const PEOPLE: [&str; 2] = ["sam", "ann"];

fn runner() -> TestRunner {
    let mut config = Config { failure_persistence: None, ..Config::default() };
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = 48;
    }
    TestRunner::new(config)
}

#[derive(Clone, Debug)]
enum Op {
    Join(usize),
    Leave(usize),
    Remove(usize),
    Revoke(usize),
    Undo(usize),
    GrantOff(usize),
    GrantOn(usize),
    Post(usize),
    /// A line from a turn that started at one of the gens seen so far (an agent's or a person's), landing now.
    Turn(usize, bool),
    /// A line nobody stamped (Kai's own, or a writer that names no gen), as the room has them today.
    Unstamped(bool),
    /// A post whose live check has been made but not yet appended, and its append.
    Queue(usize),
    Land(usize),
    /// The race itself in one op: a post is checked, the person is taken out (0), leaves (1), or is taken out and put
    /// back (2), or nothing happens (3), and then the post is appended.
    Race(usize, u8),
}

fn op() -> impl Strategy<Value = Op> {
    let p = || 0..PEOPLE.len();
    prop_oneof![
        4 => p().prop_map(Op::Join),
        2 => p().prop_map(Op::Leave),
        2 => p().prop_map(Op::Remove),
        1 => p().prop_map(Op::Revoke),
        1 => p().prop_map(Op::Undo),
        1 => p().prop_map(Op::GrantOff),
        1 => p().prop_map(Op::GrantOn),
        4 => p().prop_map(Op::Post),
        4 => (0..16usize, any::<bool>()).prop_map(|(k, user)| Op::Turn(k, user)),
        2 => any::<bool>().prop_map(Op::Unstamped),
        3 => p().prop_map(Op::Queue),
        3 => p().prop_map(Op::Land),
        4 => (p(), 0..4u8).prop_map(|(i, how)| Op::Race(i, how)),
    ]
}

#[derive(Clone, Default)]
struct Person {
    /// The `joined_gen` of the open stay.
    open: Option<i64>,
    revoked: bool,
    /// Whether they hold the room's agent (when not revoked).
    granted: bool,
}

impl Person {
    fn may(&self) -> bool {
        !self.revoked && self.granted
    }

    /// The gen from which they are shown lines, when they are shown the room at all.
    fn seat(&self) -> Option<i64> {
        self.open.filter(|_| self.may())
    }
}

struct Line {
    entry: Entry,
    origin: Option<i64>,
}

struct Queued {
    stay: i64,
    seat: Seat,
}

struct World<'a> {
    r: &'a Rig,
    room: String,
    phones: Vec<Caller>,
    model: Vec<Person>,
    room_gen: i64,
    gens: Vec<i64>,
    lines: Vec<Line>,
    queued: Vec<Option<Queued>>,
    n: usize,
}

fn fail(msg: impl Into<String>) -> TestCaseError {
    TestCaseError::fail(msg.into())
}

/// A rig with the setting on and both people invited to `house`.
async fn rig() -> Rig {
    let r = Rig::new().await;
    r.hub.store.kv_set(FLAG, "on").unwrap();
    let mut listed = vec![];
    for p in PEOPLE {
        let made = dispatch(&r.hub, &Caller::Local, "createInvite", json!({"name": p, "agents": {"house": "member"}}))
            .await
            .unwrap();
        listed.push(Listed {
            membership_id: format!("mem_{p}"),
            user_id: format!("user_{p}"),
            invite_id: made["inviteId"].as_str().unwrap().to_owned(),
            ..Default::default()
        });
    }
    members::apply(&r.hub, Some(&r.bin), &listed, Some("user_owner")).await.unwrap();
    r
}

impl<'a> World<'a> {
    /// A new case on the rig: both people granted and not revoked, and a room of its own that nobody is in.
    async fn build(r: &'a Rig) -> Self {
        for p in PEOPLE {
            let _ = dispatch(&r.hub, &Caller::Local, "unrevokePerson", json!({"personId": p})).await;
            let both = json!({"personId": p, "agents": {"house": "member"}});
            dispatch(&r.hub, &Caller::Local, "setPersonAccess", both).await.unwrap();
        }
        let members = vec![r.owner_bot("house")];
        let cfg =
            serde_json::from_value(json!({"id": "", "kind": "group", "name": "Crew", "members": members})).unwrap();
        let room = r.hub.create_bot(cfg).unwrap()["id"].as_str().unwrap().to_owned();
        let phones = PEOPLE.iter().map(|p| r.device(&format!("{p}-phone"), Some(p))).collect();
        let n = PEOPLE.len();
        Self {
            r,
            room,
            phones,
            model: vec![Person { granted: true, ..Person::default() }; n],
            room_gen: 0,
            gens: vec![0],
            lines: vec![],
            queued: (0..n).map(|_| None).collect(),
            n: 0,
        }
    }

    async fn owner(&self, method: &str, b: Value) -> anyhow::Result<Value> {
        dispatch(&self.r.hub, &Caller::Local, method, b).await
    }

    async fn person(&self, i: usize, method: &str, b: Value) -> anyhow::Result<Value> {
        dispatch(&self.r.hub, &self.phones[i], method, b).await
    }

    fn text(&mut self) -> String {
        self.n += 1;
        format!("t{}", self.n)
    }

    fn moved(&mut self, at: i64) {
        self.room_gen = at;
        self.gens.push(at);
    }

    async fn sync_gen(&mut self) {
        let v = self.owner("roomPeople", json!({"room": self.room})).await.unwrap();
        let at = v["gen"].as_i64().unwrap();
        if at != self.room_gen {
            self.moved(at);
        }
    }

    fn keep(&mut self, entry: Entry, origin: Option<i64>) {
        self.lines.push(Line { entry, origin });
    }

    async fn step(&mut self, o: &Op) -> Result<(), TestCaseError> {
        let room = self.room.clone();
        match *o {
            Op::Join(i) => {
                let who = PEOPLE[i];
                let got = self.owner("addRoomPerson", json!({"room": room, "person": who})).await;
                if !self.model[i].may() {
                    return match got {
                        Ok(_) => Err(fail(format!("{who} was let in without the agent"))),
                        Err(_) => Ok(()),
                    };
                }
                let v = got.map_err(|e| fail(format!("join {who}: {e:#}")))?;
                if self.model[i].open.is_none() {
                    let at = v["gen"].as_i64().unwrap();
                    self.model[i].open = Some(at);
                    self.moved(at);
                }
            }
            Op::Leave(i) => {
                let got = self.person(i, "leaveRoom", json!({"room": room})).await;
                let expect = self.model[i].open.is_some() && !self.model[i].revoked;
                if got.is_ok() != expect {
                    return Err(fail(format!("leave {}: expected ok={expect}, got {got:?}", PEOPLE[i])));
                }
                if expect {
                    self.model[i].open = None;
                    self.sync_gen().await;
                }
            }
            Op::Remove(i) => {
                self.owner("removeRoomPerson", json!({"room": room, "person": PEOPLE[i]})).await.unwrap();
                if self.model[i].open.take().is_some() {
                    self.sync_gen().await;
                }
            }
            Op::Revoke(i) => {
                let _ = self.owner("revokePerson", json!({"personId": PEOPLE[i]})).await;
                self.model[i].revoked = true;
            }
            Op::Undo(i) => {
                let _ = self.owner("unrevokePerson", json!({"personId": PEOPLE[i]})).await;
                self.model[i].revoked = false;
            }
            Op::GrantOff(i) | Op::GrantOn(i) => {
                // A revoked person's grants are kept as they were (the undo gives them back), so they are not edited.
                if !self.model[i].revoked {
                    let on = matches!(o, Op::GrantOn(_));
                    let agents = if on { json!({"house": "member"}) } else { json!({"money": "member"}) };
                    self.owner("setPersonAccess", json!({"personId": PEOPLE[i], "agents": agents})).await.unwrap();
                    self.model[i].granted = on;
                }
            }
            Op::Post(i) => {
                let text = self.text();
                let got = self.person(i, "send", json!({"botId": room, "text": text})).await;
                let expect = self.model[i].seat().is_some();
                if got.is_ok() != expect {
                    return Err(fail(format!("post by {}: expected ok={expect}, got {got:?}", PEOPLE[i])));
                }
                if let Ok(v) = got {
                    let id = v["entry"]["id"].as_str().unwrap().to_owned();
                    let entry = self.r.hub.store.entry(&id).unwrap();
                    if self.r.hub.store.origin_gen(&id) != Some(self.room_gen) {
                        return Err(fail(format!(
                            "a post took gen {:?}, the room is at {}",
                            self.r.hub.store.origin_gen(&id),
                            self.room_gen
                        )));
                    }
                    self.keep(entry, Some(self.room_gen));
                }
            }
            Op::Turn(k, user) => {
                let at = self.gens[k % self.gens.len()];
                let text = self.text();
                let kind = if user { EntryKind::User } else { EntryKind::Agent };
                let data = json!({"text": text, "author": "bot", "final": true});
                let e = self.r.hub.store.insert_entry_from_gen(&Lane::main(&room), kind, 1, &data, 1, at).unwrap();
                self.keep(e, Some(at));
            }
            Op::Unstamped(user) => {
                let text = self.text();
                let kind = if user { EntryKind::User } else { EntryKind::Agent };
                let e =
                    self.r.hub.add_entry(&Lane::main(&room), kind, 1, &json!({"text": text, "final": true})).unwrap();
                self.keep(e, None);
            }
            Op::Queue(i) => {
                let seat = live_seat(&self.r.hub.store, PEOPLE[i], &room).await;
                let expect = self.model[i].seat();
                if seat.as_ref().map(|s| s.joined) != expect {
                    return Err(fail(format!("live check for {}: expected {expect:?}, got {seat:?}", PEOPLE[i])));
                }
                self.queued[i] = seat.map(|seat| Queued { stay: seat.joined, seat });
            }
            Op::Race(..) => unreachable!("flattened before it runs"),
            Op::Land(i) => {
                let Some(q) = self.queued[i].take() else { return Ok(()) };
                let text = self.text();
                let data = json!({"text": text, "status": "sent", "sender": {"person": PEOPLE[i], "name": PEOPLE[i]}});
                let got = self.r.hub.commit_room_post(&room, PEOPLE[i], &q.seat, "", &data);
                // Lands only into the stay it was checked against: not after a removal, nor after a re-add (R4).
                let expect = self.model[i].open == Some(q.stay);
                if got.is_ok() != expect {
                    return Err(fail(format!("landing for {}: expected ok={expect}, got {got:?}", PEOPLE[i])));
                }
                if let Ok(e) = got {
                    self.keep(e, Some(self.room_gen));
                }
            }
        }
        Ok(())
    }

    /// What the model says person `i` is shown, oldest first, or `None` when they are not in the room at all.
    fn expected(&self, i: usize) -> Option<Vec<String>> {
        let seat = self.model[i].seat()?;
        Some(
            self.lines
                .iter()
                .filter(|l| l.origin.is_some_and(|g| seat <= g))
                .map(|l| l.entry.data["text"].as_str().unwrap().to_owned())
                .collect(),
        )
    }

    async fn check(&self) -> Result<(), TestCaseError> {
        let store = &self.r.hub.store;
        for (i, who) in PEOPLE.iter().enumerate() {
            let expected = self.expected(i);
            // History.
            let got = self.person(i, "history", json!({"botId": self.room})).await;
            match (&expected, got) {
                (Some(want), Ok(v)) => {
                    let mut have: Vec<(i64, String)> = v["entries"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|e| (e["seq"].as_i64().unwrap(), e["data"]["text"].as_str().unwrap().to_owned()))
                        .collect();
                    have.sort();
                    let have: Vec<String> = have.into_iter().map(|(_, t)| t).collect();
                    if &have != want {
                        return Err(fail(format!("{who} was shown {have:?}, the model says {want:?}")));
                    }
                }
                (None, Err(_)) => {}
                (want, got) => return Err(fail(format!("{who}: history expected {want:?}, got {got:?}"))),
            }
            // Every line as a live event, and as a thread reply's lane is the same table, the same rule.
            for l in &self.lines {
                let ev = json!({"type": "entry", "rev": l.entry.rev, "entry": l.entry});
                let shown = crate::api::roles::event_for(store, Some(who), ev).is_some();
                let want = self.model[i].seat().is_some_and(|s| l.origin.is_some_and(|g| s <= g));
                if shown != want {
                    return Err(fail(format!(
                        "event of {:?} to {who}: shown {shown}, model {want}",
                        l.entry.data["text"]
                    )));
                }
            }
        }
        // Kai sees every line, and the ones a current person is not shown say so.
        let v = self.owner("history", json!({"botId": self.room})).await.map_err(|e| fail(format!("owner: {e:#}")))?;
        let seen: Vec<&str> =
            v["entries"].as_array().unwrap().iter().filter_map(|e| e["data"]["text"].as_str()).collect();
        for l in &self.lines {
            let text = l.entry.data["text"].as_str().unwrap();
            if !seen.contains(&text) {
                return Err(fail(format!("the owner lost {text}")));
            }
        }
        Ok(())
    }
}

/// The ops as they run: a `Race` is its steps.
fn flatten(ops: &[Op]) -> Vec<Op> {
    ops.iter()
        .flat_map(|o| match *o {
            Op::Race(i, how) => {
                let between = match how {
                    0 => vec![Op::Remove(i)],
                    1 => vec![Op::Leave(i)],
                    2 => vec![Op::Remove(i), Op::Join(i)],
                    _ => vec![],
                };
                [vec![Op::Queue(i)], between, vec![Op::Land(i)]].concat()
            }
            ref other => vec![other.clone()],
        })
        .collect()
}

/// (39) The model above, over generated histories.
#[test]
fn a_person_is_shown_a_room_from_their_stay_on_and_a_post_never_lands_after_their_removal() {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let r = rt.block_on(rig());
    runner()
        .run(&proptest::collection::vec(op(), 1..12), |ops| {
            let mut w = rt.block_on(World::build(&r));
            let outcome = rt.block_on(async {
                for o in &flatten(&ops) {
                    w.step(o).await?;
                    w.check().await?;
                }
                Ok::<(), TestCaseError>(())
            });
            drop(w);
            outcome.map_err(|e| fail(format!("{e}; ops {ops:?}")))
        })
        .unwrap();
}
