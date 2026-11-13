//! A person's join or leave in a group room, written and told in one step (rooms with people).

use super::Hub;
use crate::LockExt;
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
}
