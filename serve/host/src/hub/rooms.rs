//! People in a room (designs/2026-11-12-rooms-with-people, rules 1, 5 and 9): a join or a leave is one store
//! transaction under the room's lock, after every entry the host still holds has landed, so a line queued before the
//! change can never be written at the gen after it.

use super::Hub;
use crate::LockExt;
use crate::store::Store;
use anyhow::{Result, anyhow, bail};
use std::collections::BTreeSet;

/// The Kith agents a room's members run, the ones a person in it must be granted. Bot guests are Kai's own and need
/// no grant. `None` when a member is a bot with no Kith agent: no grant can cover it, so such a room takes no people.
pub fn room_agents(store: &Store, members: &[String]) -> Option<BTreeSet<String>> {
    let mut agents = BTreeSet::new();
    for id in members {
        if store.bot_guest(id).is_some() {
            continue;
        }
        let row = store.bot(id).ok().flatten().filter(|r| !r.deleted)?;
        agents.insert(row.config.hq_agent?);
    }
    Some(agents)
}

/// Whether `person` may be in room `room` right now: an open stay AND the live authority's yes for every agent in it
/// (Shape B). Nothing about access is stored with the stay, so a revoke or suspend closes this at once and an undo opens
/// it again. The yes is [`crate::api::roles::may_use`]'s: kith-auth, or the core when it is the authority, asked now and
/// failing closed (the host's people mirror keeps a revoked person's rows, so it can't answer this).
#[allow(dead_code)] // the member-side calls that ask it come in the next PRs of the slice
pub async fn room_member_may(store: &Store, person: &str, room: &str) -> bool {
    if store.room_stay(room, person).is_none() {
        return false;
    }
    let Some(row) = store.bot(room).ok().flatten().filter(|r| !r.deleted && r.config.is_group()) else { return false };
    let Some(agents) = room_agents(store, &row.config.members).filter(|a| !a.is_empty()) else { return false };
    for agent in &agents {
        if !crate::api::roles::may_use(person, agent).await {
            return false;
        }
    }
    true
}

impl Hub {
    /// Opens `person`'s stay in room `room` and returns the new gen. `granted` is the agents the caller found the person
    /// may use (live); the room's agents are read again here, under the lock, so an agent added meanwhile is checked too.
    pub fn add_room_person(&self, room: &str, person: &str, granted: &BTreeSet<String>) -> Result<i64> {
        self.move_room_person(room, person, Some(granted))
    }

    /// Closes `person`'s stay in room `room` and returns the new gen.
    pub fn remove_room_person(&self, room: &str, person: &str) -> Result<i64> {
        self.move_room_person(room, person, None)
    }

    /// `granted` is `Some` for a join. Membership lock, then emit lock, as every room edit.
    fn move_room_person(&self, room: &str, person: &str, granted: Option<&BTreeSet<String>>) -> Result<i64> {
        let (room_gen, landed) = {
            let _members = crate::chat::group::membership_lock();
            let _g = self.emit_lock.locked();
            let row = self.store.bot(room)?.filter(|r| !r.deleted && r.config.is_group());
            let row = row.ok_or_else(|| anyhow!("no such room"))?;
            if let Some(granted) = granted {
                let agents = room_agents(&self.store, &row.config.members)
                    .ok_or_else(|| anyhow!("this room has a bot that isn't a Kith agent, so it can't take people"))?;
                if agents.is_empty() {
                    bail!("this room has no Kith agents for {person} to talk to");
                }
                if let Some(missing) = agents.iter().find(|a| !granted.contains(*a)) {
                    bail!("{person} doesn't have {missing}: grant it first");
                }
            }
            let landed = self.flush_unsaved();
            if !self.unsaved.locked().queue.is_empty() {
                bail!("the store is not writing");
            }
            let room_gen = if granted.is_some() {
                self.store.join_room(room, person)?
            } else {
                self.store.leave_room(room, person)?
            };
            (room_gen, landed)
        };
        for e in &landed {
            self.update_thread_summary(e);
        }
        Ok(room_gen)
    }

    /// Refuses a room edit that adds an agent some person in the room isn't granted (rule 5). `granted` holds the
    /// (person, agent) pairs the caller checked live. Called under the room's lock, so the people can't change after.
    pub(super) fn check_room_agents(
        &self,
        room: &str,
        before: &[String],
        after: &[String],
        granted: &BTreeSet<(String, String)>,
    ) -> Result<()> {
        let people = self.store.room_people(room)?;
        if people.is_empty() {
            return Ok(());
        }
        let after = room_agents(&self.store, after)
            .ok_or_else(|| anyhow!("a room with people takes only Kith agents and your own bots"))?;
        let before = room_agents(&self.store, before).unwrap_or_default();
        for agent in after.difference(&before) {
            if let Some(p) = people.iter().find(|p| !granted.contains(&((*p).clone(), agent.clone()))) {
                bail!("{p} doesn't have {agent}: grant it first");
            }
        }
        Ok(())
    }
}
