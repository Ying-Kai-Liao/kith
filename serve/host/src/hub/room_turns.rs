//! The room turns running as a room person (rooms with people, PR C): who each is for, so a revoke can end the turns
//! whose basis holds the person (rule 12) and nothing else.
//!
//! A turn registers BEFORE it asks the authority whether every basis person may still use the agent, and leaves when
//! it ends. A revoke writes no room row, so this ordering is what closes the race: a revoke after the registration finds
//! the turn here and stops it; a revoke before it is seen by the live check that follows.

use super::Hub;
use crate::LockExt;
use crate::agent::bot::{Cmd, person_of};
use crate::chat::group::membership_lock;
use crate::store::{Entry, EntryKind, Lane};
use serde_json::Value;

impl Hub {
    /// The turn of the actor `key` runs for `people` (its basis).
    pub fn room_turn_begins(&self, key: &str, people: &[String]) {
        self.room_turns.locked().insert(key.to_owned(), people.to_vec());
    }

    /// The turn of the actor `key` is over, or never started.
    pub fn room_turn_ends(&self, key: &str) {
        self.room_turns.locked().remove(key);
    }

    /// The owner's own message in `lane`. In a group it is stamped with the room's gen, read under the membership lock in
    /// the same step as the append (as a person's post is, `commit_room_post`), so a turn later reads it by where it
    /// stands: written with people in the room or not (rule 3).
    pub fn add_owner_post(&self, lane: &Lane, turn: i64, data: &Value, group: bool) -> Option<Entry> {
        if !group {
            return self.add_entry(lane, EntryKind::User, turn, data);
        }
        let _members = membership_lock();
        let room_gen = self.store.room_gen(&lane.chat);
        self.add_entry_from(lane, EntryKind::User, turn, data, Some(room_gen))
    }

    /// The people the actor `key` runs a room turn for now.
    #[cfg(test)]
    pub fn room_turn_of(&self, key: &str) -> Option<Vec<String>> {
        self.room_turns.locked().get(key).cloned()
    }

    /// Every id a room person has run as in a room `person` has had a stay in, whatever the epoch: what the sweep of claims,
    /// helpers and doors after a revoke or a removal must name too (a run a restarted host no longer knows has no entry
    /// above, only its claim).
    pub fn room_person_ids_of(&self, person: &str) -> Vec<String> {
        let mut ids = Vec::new();
        for room in self.store.rooms_with_person(person) {
            for stay in self.store.room_stays(&room) {
                let id = crate::chat::room_turn::room_person_id(&room, stay.joined_gen);
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
        ids
    }

    /// Ends every room turn whose basis holds `person` (and anything queued behind it on those actors): their actors
    /// stop, which kills the agent process and what it started. Returns the room person ids they ran as, for the sweep of
    /// their claims, helpers and doors that follows a revoke or a removal.
    pub fn end_room_turns_of(&self, person: &str) -> Vec<String> {
        let keys: Vec<String> =
            self.room_turns.locked().iter().filter(|(_, people)| people.iter().any(|p| p == person)).map(|(k, _)| k.clone()).collect();
        let mut ids = Vec::new();
        for key in keys {
            if let Some(id) = person_of(&key)
                && !ids.iter().any(|i| i == id)
            {
                ids.push(id.to_owned());
            }
            if let Err(error) = self.send_to_actor(&key, Cmd::Stop) {
                tracing::warn!(error = format!("{error:#}"), person, "couldn't stop a room turn");
            }
        }
        ids
    }
}
