//! People in a group room (rooms with people, design 2026-11-12): who is let in, and when.
//!
//! A person's stay is a row in the host store (`store::room_people`) whose gen is the `seq` of its `personJoined` /
//! `personLeft` line in the room's main lane, written with the line under [`group::membership_lock`], the lock a bot's
//! join and a room edit already use, so the room has one lock and one gen space.
//!
//! Whether a person may be there is never stored. [`room_member_may`] asks the authority live (`roles::may_use`, which
//! answers for the switched core and for kith-auth alike): an open stay AND a yes for every agent in the room. A revoke
//! writes no room row, so an undo gives back exactly what was there.
//!
//! The room's people are Kai's to set (owner-only methods in `api/room_people.rs`), and the whole feature is behind the
//! host setting [`FLAG`], off until the room-person turns exist.

use super::group::membership_lock;
use crate::api::roles::may_use;
use crate::hub::Hub;
use crate::store::{Stay, Store};
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::Arc;

/// The host setting (kv, read each time) that lets a person be added to a room: `on`, `true` or `1`. Off by default.
/// Only tests write it; Kai turns it on with the last commit of the room-person turns.
pub const FLAG: &str = "rooms.people";

pub fn enabled(store: &Store) -> bool {
    matches!(store.kv_get(FLAG).as_deref().map(str::trim), Some("on" | "true" | "1"))
}

/// What a join or a leave did: the room's gen after it, and whether it changed anything (a repeat does not).
#[derive(Debug, PartialEq, Eq)]
pub struct Change {
    pub gen: i64,
    pub changed: bool,
}

/// The agents of the room's members, sorted and without repeats. A room takes people only when every member is a Kith
/// agent bot and there is at least one: a bot guest reads by seq floor, not by gen, and a room with no agent leaves
/// nothing for the live check to fail (a revoked person would read it). Errors say why, for Kai.
fn room_agents(store: &Store, room: &str) -> Result<Vec<String>> {
    let Some(group) = store.bot(room)?.filter(|r| !r.deleted && r.config.is_group()) else {
        bail!("That isn't a group room.");
    };
    let mut agents = BTreeSet::new();
    for id in &group.config.members {
        if store.bot_guest(id).is_some() {
            bail!("A room with people takes only Kith agents; {id} is an outside bot.");
        }
        match store.bot(id)?.filter(|b| !b.deleted) {
            None => {}
            Some(bot) => match bot.config.hq_agent {
                Some(agent) => {
                    agents.insert(agent);
                }
                None => bail!("A room with people takes only Kith agents; {} isn't one.", bot.config.name),
            },
        }
    }
    if agents.is_empty() {
        bail!("A room with no Kith agent takes no people.");
    }
    Ok(agents.into_iter().collect())
}

/// The room's open people and its gen, read together under the membership lock (so no join or leave lands between).
pub fn people_and_gen(store: &Store, room: &str) -> (Vec<String>, i64) {
    let _members = membership_lock();
    (store.room_open_people(room), store.room_gen(room))
}

/// Whether `person` may be in `room` right now: an open stay AND the live may(person, agent) for every agent in it,
/// and a room that takes people at all. Never stored; fails closed on any doubt. The member side asks this before
/// every read, post and event.
#[cfg_attr(not(test), allow(dead_code))] // the member side (rooms with people, PR B) is its caller
pub async fn room_member_may(store: &Store, person: &str, room: &str) -> bool {
    if !store.room_open_people(room).iter().any(|p| p == person) {
        return false;
    }
    let Ok(agents) = room_agents(store, room) else { return false };
    for agent in &agents {
        if !may_use(person, agent).await {
            return false;
        }
    }
    true
}

fn notice(store: &Store, person: &str, joined: bool) -> Value {
    let name = store.person(person).map_or_else(|| person.to_owned(), |p| p.name);
    let (kind, text) = if joined {
        ("personJoined", format!("{name} joined · sees this room from now on"))
    } else {
        ("personLeft", format!("{name} left"))
    };
    json!({"notice": kind, "person": {"id": person, "name": name}, "style": "info", "text": text})
}

/// Whether `person` can be a room person at all: Kai is never one (the room's basis is its non-owner people), nor a
/// room person's own id.
fn check_person(person: &str) -> Result<&str> {
    let person = person.trim();
    if person.is_empty() || person == "owner" || person.starts_with("room-") {
        bail!("That can't be added to a room's people.");
    }
    Ok(person)
}

/// Adds `person` to `room`: one step, one new gen. Refused while the setting is off, for the owner and `room-` ids,
/// for a room that takes no people ([`room_agents`]), and for a person not granted every agent in it, asked live. A
/// person already in the room changes nothing and bumps no gen.
pub async fn add_person(hub: &Arc<Hub>, room: &str, person: &str) -> Result<Change> {
    if !enabled(&hub.store) {
        bail!("People in rooms aren't turned on.");
    }
    let person = check_person(person)?;
    // The live checks are async and the lock is not held across them: the room's agents are read again under it, and a
    // room that changed meanwhile is checked again.
    for _ in 0..3 {
        let agents = room_agents(&hub.store, room)?;
        for agent in &agents {
            if !may_use(person, agent).await {
                bail!("{person} isn't granted {agent}; a room's people need every agent in it.");
            }
        }
        if let Some(change) = commit(hub, room, person, true, Some(&agents))? {
            return Ok(change);
        }
    }
    bail!("The room changed while it was checked; try again.");
}

/// Takes `person` out of `room`: one step, one new gen. Always allowed (also while the setting is off); someone who is
/// not in the room changes nothing and bumps no gen.
pub fn remove_person(hub: &Hub, room: &str, person: &str) -> Result<Change> {
    if hub.store.bot(room)?.is_none_or(|r| r.deleted || !r.config.is_group()) {
        bail!("That isn't a group room.");
    }
    Ok(commit(hub, room, person.trim(), false, None)?.unwrap_or(Change { gen: hub.store.room_gen(room), changed: false }))
}

/// The write itself, under the membership lock. `None` only when `agents` (what a join was checked against) is no
/// longer the room's agents.
fn commit(hub: &Hub, room: &str, person: &str, join: bool, agents: Option<&[String]>) -> Result<Option<Change>> {
    let _members = membership_lock();
    if let Some(checked) = agents
        && room_agents(&hub.store, room)? != checked
    {
        return Ok(None);
    }
    let line = notice(&hub.store, person, join);
    let written = hub.commit_room_person(room, person, join, &line)?;
    Ok(Some(match written {
        Some(e) => Change { gen: e.seq, changed: true },
        None => Change { gen: hub.store.room_gen(room), changed: false },
    }))
}

/// Every stay in `room` and its gen, for `roomPeople`.
pub fn stays(store: &Store, room: &str) -> (i64, Vec<Stay>) {
    let _members = membership_lock();
    (store.room_gen(room), store.room_stays(room))
}

/// What the async handler checked before an edit of a room's members, which [`guard_adds`] holds the save to.
#[derive(Debug)]
pub struct Verified {
    /// The people in the room when it was checked.
    people: Vec<String>,
    /// The members the edit adds, each of them checked against every person in the room.
    added: Vec<String>,
}

fn added_members(before: &[String], after: &[String]) -> Vec<String> {
    after.iter().filter(|m| !before.contains(m)).cloned().collect()
}

/// A Kith agent bot's agent: not an outside bot, not a bot that isn't an hq agent.
fn agent_of(store: &Store, member: &str) -> Result<String> {
    if store.bot_guest(member).is_some() {
        bail!("A room with people takes only Kith agents; {member} is an outside bot.");
    }
    match store.bot(member)?.filter(|b| !b.deleted).and_then(|b| b.config.hq_agent) {
        Some(agent) => Ok(agent),
        None => bail!("A room with people takes only Kith agents; {member} isn't one."),
    }
}

/// The edit `patch` makes to a room's members, checked live before it is saved: while the room has people, each member
/// it adds must be a Kith agent that every one of them is granted. `None` when the edit adds nothing or the room has
/// nobody (and [`guard_adds`] sees to a person joining before the save).
pub async fn verify_adds(hub: &Hub, patch: &Value) -> Result<Option<Verified>> {
    let (Some(room), Some(members)) = (patch["id"].as_str(), patch["members"].as_array()) else { return Ok(None) };
    let Some(row) = hub.store.bot(room)?.filter(|r| !r.deleted && r.config.is_group()) else { return Ok(None) };
    let after: Vec<String> = members.iter().filter_map(|m| m.as_str()).map(|m| m.trim().to_owned()).collect();
    let added = added_members(&row.config.members, &after);
    let people = hub.store.room_open_people(room);
    if added.is_empty() || people.is_empty() {
        return Ok(None);
    }
    for member in &added {
        let agent = agent_of(&hub.store, member)?;
        for person in &people {
            if !may_use(person, &agent).await {
                bail!("{person} isn't granted {agent}; an agent can join a room only when every person in it has it.");
            }
        }
    }
    Ok(Some(Verified { people, added }))
}

/// Under the membership lock, at the save of a group's members: the members `after` that were not there are allowed
/// only when the room has no people, or `verified` checked exactly this room's people and covers each added member. So
/// a join and an agent's add check each other under one lock, and neither passes on a stale read.
pub fn guard_adds(store: &Store, room: &str, after: &[String], verified: Option<&Verified>) -> Result<()> {
    let before = store.bot(room)?.map(|r| r.config.members).unwrap_or_default();
    let added = added_members(&before, after);
    let people = store.room_open_people(room);
    if added.is_empty() || people.is_empty() {
        return Ok(());
    }
    match verified {
        Some(v) if v.people == people && added.iter().all(|m| v.added.contains(m)) => Ok(()),
        _ => bail!("The room's people changed, or its members did; check again."),
    }
}
