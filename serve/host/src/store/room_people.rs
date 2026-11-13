//! People in a room (designs/2026-11-12-rooms-with-people, Shape B): each stay of a person in a room is an interval of
//! the room's generation (`bots.gen`), opened by a join and closed by a leave. Each join or leave bumps the gen in the
//! same transaction, and every entry carries the gen it was written at, so who may see an entry is a comparison and a
//! revoke never has to touch these rows (undo then gives back exactly what was there).

use super::Store;
use crate::LockExt;
use anyhow::{Result, bail};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;

/// One stay of a person in a room: from `joined_gen` until `left_gen` (`None` while they are in it).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoomStay {
    pub person: String,
    pub joined_gen: i64,
    pub left_gen: Option<i64>,
}

impl Store {
    /// The room's generation now; 0 for a room no one ever joined (or no room).
    pub fn room_gen(&self, room: &str) -> i64 {
        let c = self.db.locked();
        c.query_row("SELECT gen FROM bots WHERE id = ?1", [room], |r| r.get(0)).optional().ok().flatten().unwrap_or(0)
    }

    /// The people in the room now (open stays), by name.
    /// An error is never "nobody": callers refuse or start the process again.
    pub fn room_people(&self, room: &str) -> Result<Vec<String>> {
        Ok(self.room_stays(room)?.into_iter().filter(|s| s.left_gen.is_none()).map(|s| s.person).collect())
    }

    /// Every stay in the room, open or closed, oldest first.
    pub fn room_stays(&self, room: &str) -> Result<Vec<RoomStay>> {
        let c = self.db.locked();
        let mut st = c.prepare(
            "SELECT person, joined_gen, left_gen FROM room_people WHERE room = ?1 ORDER BY joined_gen, person",
        )?;
        let rows =
            st.query_map([room], |r| Ok(RoomStay { person: r.get(0)?, joined_gen: r.get(1)?, left_gen: r.get(2)? }))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// `person`'s open stay in `room`, if they are in it.
    #[allow(dead_code)] // asked by `room_member_may`, which has no caller yet
    pub fn room_stay(&self, room: &str, person: &str) -> Option<RoomStay> {
        let c = self.db.locked();
        c.query_row(
            "SELECT person, joined_gen, left_gen FROM room_people WHERE room = ?1 AND person = ?2 AND left_gen IS NULL",
            params![room, person],
            |r| Ok(RoomStay { person: r.get(0)?, joined_gen: r.get(1)?, left_gen: r.get(2)? }),
        )
        .optional()
        .ok()
        .flatten()
    }

    /// The gen entry `id` was written at.
    #[cfg(test)]
    pub fn entry_gen(&self, id: &str) -> Option<i64> {
        let c = self.db.locked();
        c.query_row("SELECT gen FROM entries WHERE id = ?1", [id], |r| r.get(0)).optional().ok().flatten()
    }

    /// Opens `person`'s stay in `room`: one transaction bumps the room's gen and starts the interval at the new gen,
    /// which it returns. Refused when the room is gone or they are already in it. The caller holds the room's lock.
    pub fn join_room(&self, room: &str, person: &str) -> Result<i64> {
        self.move_person(room, person, true)
    }

    /// Closes `person`'s stay in `room` at a newly bumped gen (returned), in one transaction. Refused when they aren't in
    /// it.
    pub fn leave_room(&self, room: &str, person: &str) -> Result<i64> {
        self.move_person(room, person, false)
    }

    fn move_person(&self, room: &str, person: &str, join: bool) -> Result<i64> {
        let mut c = self.db.locked();
        let tx = c.transaction()?;
        let open: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM room_people WHERE room = ?1 AND person = ?2 AND left_gen IS NULL)",
            params![room, person],
            |r| r.get(0),
        )?;
        match (join, open) {
            (true, true) => bail!("{person} is already in the room"),
            (false, false) => bail!("{person} is not in the room"),
            _ => {}
        }
        let room_gen: Option<i64> = tx
            .query_row("UPDATE bots SET gen = gen + 1 WHERE id = ?1 AND deleted = 0 RETURNING gen", [room], |r| {
                r.get(0)
            })
            .optional()?;
        let Some(room_gen) = room_gen else { bail!("no such room") };
        if join {
            tx.execute(
                "INSERT INTO room_people(room, person, joined_gen, left_gen) VALUES(?1, ?2, ?3, NULL)",
                params![room, person, room_gen],
            )?;
        } else {
            tx.execute(
                "UPDATE room_people SET left_gen = ?3 WHERE room = ?1 AND person = ?2 AND left_gen IS NULL",
                params![room, person, room_gen],
            )?;
        }
        tx.commit()?;
        Ok(room_gen)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{BotConfig, EntryKind, Lane};

    fn room(s: &Store) -> String {
        let cfg: BotConfig = serde_json::from_value(serde_json::json!({
            "id": "room", "kind": "group", "name": "Crew", "members": ["a"], "backend": "claude", "cwd": "/tmp",
        }))
        .unwrap();
        s.save_bot(&cfg).unwrap();
        cfg.id
    }

    #[test]
    fn a_join_and_a_leave_each_bump_the_gen_and_move_the_interval() {
        let s = Store::open(std::path::Path::new(":memory:")).unwrap();
        let r = room(&s);
        assert_eq!(s.room_gen(&r), 0);
        assert_eq!(s.join_room(&r, "sam").unwrap(), 1);
        assert_eq!(s.room_people(&r).unwrap(), ["sam"]);
        assert!(s.join_room(&r, "sam").is_err(), "already in");
        assert_eq!(s.room_gen(&r), 1, "a refused join bumps nothing");
        assert_eq!(s.leave_room(&r, "sam").unwrap(), 2);
        assert!(s.leave_room(&r, "sam").is_err(), "already out");
        assert_eq!(s.join_room(&r, "sam").unwrap(), 3);
        assert_eq!(
            s.room_stays(&r).unwrap(),
            [
                RoomStay { person: "sam".into(), joined_gen: 1, left_gen: Some(2) },
                RoomStay { person: "sam".into(), joined_gen: 3, left_gen: None },
            ]
        );
        assert!(s.join_room("nope", "sam").is_err(), "no room, no row");
        assert_eq!(s.room_stays("nope").unwrap(), []);
    }

    #[test]
    fn an_entry_takes_the_gen_it_names_or_the_rooms_now() {
        let s = Store::open(std::path::Path::new(":memory:")).unwrap();
        let r = room(&s);
        let lane = Lane::main(&r);
        let before = s.insert_entry(&lane, EntryKind::User, 1, &serde_json::json!({"text": "hi"})).unwrap();
        s.join_room(&r, "sam").unwrap();
        let after = s.insert_entry(&lane, EntryKind::User, 2, &serde_json::json!({"text": "hello"})).unwrap();
        let (old, _) = s
            .insert_entry_as("late", &lane, EntryKind::Agent, 1, &serde_json::json!({"text": "reply"}), 1, Some(0))
            .unwrap();
        assert_eq!(s.entry_gen(&before.id), Some(0));
        assert_eq!(s.entry_gen(&after.id), Some(1));
        assert_eq!(s.entry_gen(&old.id), Some(0), "a turn's own start gen, not the room's now");
        s.update_entry(&old.id, &serde_json::json!({"text": "edited"})).unwrap();
        assert_eq!(s.entry_gen(&old.id), Some(0), "an edit never re-stamps");
    }
}
