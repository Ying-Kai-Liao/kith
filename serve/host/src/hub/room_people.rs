//! A person's join or leave in a group room, written and told in one step (rooms with people).

use super::Hub;
use crate::LockExt;
use crate::chat::group::membership_lock;
use crate::chat::room_people::{Seat, room_agents};
use crate::store::Entry;
use anyhow::Result;
use serde_json::{Value, json};

impl Hub {
    /// `person` joins (`join`) or leaves `room`: the `personJoined` / `personLeft` line and the stay's row land in one
    /// store transaction, then the line is sent like any entry. `None` when nothing changed (already in, or not in).
    ///
    /// The caller holds `chat::group::membership_lock`, so this is the lock order every room edit uses (membership,
    /// then emit).
    pub(crate) fn commit_room_person(
        &self,
        room: &str,
        person: &str,
        join: bool,
        notice: &Value,
    ) -> Result<Option<Entry>> {
        self.commit_room_person_for(room, person, join, notice, None)
    }

    /// [`Self::commit_room_person`] for an invite's room part: `part` is the invite whose done mark for `room` lands in
    /// the join's own transaction (`Store::join_room_person_for`). Only a join carries one.
    pub(crate) fn commit_room_person_for(
        &self,
        room: &str,
        person: &str,
        join: bool,
        notice: &Value,
        part: Option<&str>,
    ) -> Result<Option<Entry>> {
        let _g = self.emit_lock.locked();
        let written = if join {
            self.store.join_room_person_for(room, person, notice, part)?
        } else {
            self.store.leave_room_person(room, person, notice)?
        };
        if let Some(e) = &written {
            let _ = self.events.send(json!({"type": "entry", "rev": e.rev, "entry": e}));
        }
        Ok(written)
    }

    /// A person's post in `room`, written with the check that lets it in as ONE step (rule 4): under the membership
    /// lock (the one every join, leave and room edit holds) and the emit lock, `person` still has the open stay `seat`
    /// the live check found and the room still has the agents it asked about, and only then is the line appended,
    /// stamped with the room's gen as it is in that same step. So a post can't land after its author was taken out,
    /// nor after a re-add (a new stay has a new `joined_gen`: R4).
    ///
    /// A repeat of `nonce` by the same person returns their first line and writes nothing.
    pub(crate) fn commit_room_post(
        &self,
        room: &str,
        person: &str,
        seat: &Seat,
        nonce: &str,
        data: &Value,
    ) -> Result<Entry> {
        let _members = membership_lock();
        let _g = self.emit_lock.locked();
        if !nonce.is_empty()
            && let Some(e) = self.store.find_person_nonce(room, person, nonce)
        {
            return Ok(e);
        }
        if self.store.room_open_seat(room, person) != Some(seat.joined) {
            return Err(PostRefused::NotInRoom.into());
        }
        if room_agents(&self.store, room).ok().as_ref() != Some(&seat.agents) {
            return Err(PostRefused::RoomChanged.into());
        }
        // Held entries go first (they keep their order), and a store that still won't write takes no post at all: a
        // line written ahead of them would be out of order.
        self.flush_unsaved();
        if !self.unsaved.locked().queue.is_empty() {
            return Err(PostRefused::NotWriting.into());
        }
        // The stay and the revoke are checked again in the store's own transaction with the append, so a revoke
        // recorded between the live check and here refuses the post too.
        let Some(e) = self.store.insert_room_post(room, person, seat.joined, data)? else {
            return Err(PostRefused::NotInRoom.into());
        };
        let _ = self.events.send(json!({"type": "entry", "rev": e.rev, "entry": e}));
        Ok(e)
    }
}

/// Why a person's post was not written, for the handler to say.
#[derive(Debug, PartialEq, Eq)]
pub enum PostRefused {
    /// They have no open stay (any more), or it is not the one the post was checked against: a removal, or a removal
    /// and a re-add, came between the check and the append.
    NotInRoom,
    /// The room's agents are not the ones the live check asked about.
    RoomChanged,
    /// The store is not taking writes, so nothing can be written in order.
    NotWriting,
}

impl std::fmt::Display for PostRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotInRoom => "You aren't in that room.",
            Self::RoomChanged => "The room changed while your message was checked; send it again.",
            Self::NotWriting => "This computer can't save messages right now; try again in a moment.",
        })
    }
}

impl std::error::Error for PostRefused {}
