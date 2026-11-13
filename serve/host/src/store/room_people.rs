//! The people in a group room (rooms with people, design 2026-11-12): one row per stay, `joined_gen` to `left_gen`.
//!
//! A gen is the `seq` of the `personJoined` / `personLeft` line in the room's main lane, written in the same
//! transaction as the row, so the room has one counter (its entries' `seq`) and a join and its line are never apart.
//! The room's own gen is not stored: it is the newest gen of its stays, `0` when it never had one.
//!
//! Whether a person may be in the room is never kept here. A stay says they were let in; the live answer of the
//! authority (`chat::room_people::room_member_may`) says whether they still may, so a revoke writes no row and an undo
//! puts back exactly what was there.

use super::entries::insert_entry_in;
use super::{Entry, EntryKind, Lane, Store, now_ms};
use crate::LockExt;
use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde_json::Value;

/// One stay of a person in a room: open while `left_gen` is `None`. A second stay of the same person (after a leave)
/// is a new row with a later `joined_gen`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stay {
    pub person: String,
    pub joined_gen: i64,
    pub left_gen: Option<i64>,
}

impl Store {
    /// Every stay in `room`, oldest first.
    pub fn room_stays(&self, room: &str) -> Vec<Stay> {
        let c = self.db.locked();
        let Ok(mut st) =
            c.prepare("SELECT person, joined_gen, left_gen FROM room_people WHERE room = ?1 ORDER BY joined_gen")
        else {
            return vec![];
        };
        st.query_map([room], |r| Ok(Stay { person: r.get(0)?, joined_gen: r.get(1)?, left_gen: r.get(2)? }))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }

    /// The people with an open stay in `room`, sorted.
    pub fn room_open_people(&self, room: &str) -> Vec<String> {
        let c = self.db.locked();
        let Ok(mut st) =
            c.prepare("SELECT person FROM room_people WHERE room = ?1 AND left_gen IS NULL ORDER BY person")
        else {
            return vec![];
        };
        st.query_map([room], |r| r.get(0)).map(|rows| rows.flatten().collect()).unwrap_or_default()
    }

    /// The room's gen: the newest join or leave in it (`0`: no person has ever been in it). Derived, so there is no
    /// second counter to keep in step with the entries.
    pub fn room_gen(&self, room: &str) -> i64 {
        let c = self.db.locked();
        c.query_row(
            "SELECT COALESCE(MAX(MAX(joined_gen), MAX(COALESCE(left_gen, 0))), 0) FROM room_people WHERE room = ?1",
            [room],
            |r| r.get(0),
        )
        .unwrap_or(0)
    }

    /// `person` joins `room`: one transaction writes `notice` (the `personJoined` line, whose `seq` is the new gen) and
    /// the open stay. `None` and nothing written when they already have an open stay (a repeat is not a second join).
    #[cfg_attr(not(test), allow(dead_code, reason = "the host joins through join_room_person_for"))]
    pub fn join_room_person(&self, room: &str, person: &str, notice: &Value) -> Result<Option<Entry>> {
        self.join_room_person_for(room, person, notice, None)
    }

    /// [`Self::join_room_person`] for an invite's room part (`(invite, room)`): the part's done mark is written in the
    /// same transaction as the join. A part already done (or not there) joins no one, so a person Kai removed is not
    /// let back in by the invite; a person already in has the part marked and nothing else written.
    pub fn join_room_person_for(
        &self,
        room: &str,
        person: &str,
        notice: &Value,
        part: Option<&str>,
    ) -> Result<Option<Entry>> {
        let mut c = self.db.locked();
        let tx = c.transaction()?;
        if let Some(invite) = part {
            let open: bool = tx
                .query_row(
                    "SELECT done_at IS NULL FROM invite_rooms WHERE invite_id = ?1 AND room = ?2",
                    params![invite, room],
                    |r| r.get(0),
                )
                .optional()?
                .unwrap_or(false);
            if !open {
                return Ok(None);
            }
        }
        let mark = |tx: &rusqlite::Transaction| -> Result<()> {
            if let Some(invite) = part {
                tx.execute(
                    "UPDATE invite_rooms SET done_at = ?3 WHERE invite_id = ?1 AND room = ?2 AND done_at IS NULL",
                    params![invite, room, now_ms()],
                )?;
            }
            Ok(())
        };
        let open: Option<i64> = tx
            .query_row(
                "SELECT joined_gen FROM room_people WHERE room = ?1 AND person = ?2 AND left_gen IS NULL",
                params![room, person],
                |r| r.get(0),
            )
            .optional()?;
        if open.is_some() {
            mark(&tx)?;
            tx.commit()?;
            return Ok(None);
        }
        let id = uuid::Uuid::new_v4().to_string();
        let (entry, _) = insert_entry_in(&tx, &id, &Lane::main(room), EntryKind::Notice, 0, notice, now_ms(), None)?;
        tx.execute(
            "INSERT INTO room_people(room, person, joined_gen, left_gen) VALUES(?1, ?2, ?3, NULL)",
            params![room, person, entry.seq],
        )?;
        mark(&tx)?;
        tx.commit()?;
        Ok(Some(entry))
    }

    /// `person` leaves `room` (or is taken out): the `personLeft` line and the end of their open stay in one
    /// transaction. `None` and nothing written when they have no open stay.
    pub fn leave_room_person(&self, room: &str, person: &str, notice: &Value) -> Result<Option<Entry>> {
        let mut c = self.db.locked();
        let tx = c.transaction()?;
        let open: Option<i64> = tx
            .query_row(
                "SELECT joined_gen FROM room_people WHERE room = ?1 AND person = ?2 AND left_gen IS NULL",
                params![room, person],
                |r| r.get(0),
            )
            .optional()?;
        let Some(joined) = open else { return Ok(None) };
        let id = uuid::Uuid::new_v4().to_string();
        let (entry, _) = insert_entry_in(&tx, &id, &Lane::main(room), EntryKind::Notice, 0, notice, now_ms(), None)?;
        let ended = tx.execute(
            "UPDATE room_people SET left_gen = ?3 WHERE room = ?1 AND joined_gen = ?2 AND left_gen IS NULL",
            params![room, joined, entry.seq],
        )?;
        ensure!(ended == 1, "the stay ended meanwhile");
        tx.commit()?;
        Ok(Some(entry))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn store() -> Store {
        Store::open(std::path::Path::new(":memory:")).unwrap()
    }

    fn line(what: &str) -> Value {
        json!({"notice": what, "style": "info", "text": what})
    }

    #[test]
    fn a_join_and_a_leave_each_make_one_gen_and_a_second_join_is_a_new_stay() {
        let s = store();
        assert_eq!(s.room_gen("r"), 0, "no person ever: no gen");
        let join = s.join_room_person("r", "ben", &line("personJoined")).unwrap().unwrap();
        assert_eq!(s.room_gen("r"), join.seq);
        assert_eq!(s.room_open_people("r"), vec!["ben".to_owned()]);
        assert!(s.join_room_person("r", "ben", &line("personJoined")).unwrap().is_none(), "a repeat writes nothing");
        assert_eq!(s.room_gen("r"), join.seq);
        let leave = s.leave_room_person("r", "ben", &line("personLeft")).unwrap().unwrap();
        assert!(leave.seq > join.seq);
        assert_eq!(s.room_gen("r"), leave.seq);
        assert_eq!(s.room_open_people("r"), Vec::<String>::new());
        assert!(s.leave_room_person("r", "ben", &line("personLeft")).unwrap().is_none(), "nobody left to leave");
        let again = s.join_room_person("r", "ben", &line("personJoined")).unwrap().unwrap();
        assert_eq!(
            s.room_stays("r"),
            vec![
                Stay { person: "ben".into(), joined_gen: join.seq, left_gen: Some(leave.seq) },
                Stay { person: "ben".into(), joined_gen: again.seq, left_gen: None },
            ]
        );
        assert_eq!(s.room_gen("r"), again.seq);
    }

    #[test]
    fn the_gen_is_the_seq_of_the_line_in_the_rooms_main_lane() {
        let s = store();
        let join = s.join_room_person("r", "ben", &line("personJoined")).unwrap().unwrap();
        let seen = s.entries_from("r", 0).unwrap();
        assert!(seen.iter().any(|e| e.seq == join.seq), "{seen:?}");
        assert_eq!(join.data["notice"], "personJoined");
    }

    #[test]
    fn dropping_a_room_takes_its_stays_and_only_its_own() {
        let s = store();
        s.join_room_person("r", "ben", &line("personJoined")).unwrap();
        s.join_room_person("other", "ben", &line("personJoined")).unwrap();
        s.drop_room("r").unwrap();
        assert_eq!(s.room_stays("r"), vec![]);
        assert_eq!(s.room_gen("r"), 0);
        assert_eq!(s.room_open_people("other"), vec!["ben".to_owned()]);
    }

    #[test]
    fn an_entry_keeps_the_gen_it_was_written_with_and_old_ones_have_none() {
        let s = store();
        let lane = Lane::main("r");
        let old = s.insert_entry(&lane, EntryKind::Notice, 0, &line("old")).unwrap();
        let stamped = s.insert_entry_from_gen(&lane, EntryKind::Notice, 0, &line("new"), 5, 42).unwrap();
        assert_eq!(s.origin_gen(&old.id), None, "from before any person");
        assert_eq!(s.origin_gen(&stamped.id), Some(42));
        s.update_entry(&stamped.id, &line("edited")).unwrap();
        assert_eq!(s.origin_gen(&stamped.id), Some(42), "an edit never re-stamps it");
    }

    #[test]
    fn the_migration_adds_the_column_and_table_to_a_database_from_before() {
        let dir = std::env::temp_dir().join(format!("kith-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("t.db");
        drop(Store::open(&db).unwrap());
        {
            let c = rusqlite::Connection::open(&db).unwrap();
            c.execute_batch(
                "DROP TABLE room_people; ALTER TABLE entries DROP COLUMN origin_gen;
                 UPDATE kv SET v = '18' WHERE k = 'schema';",
            )
            .unwrap();
        }
        let s = Store::open(&db).unwrap();
        assert_eq!(s.kv_get("schema").as_deref(), Some("20"));
        assert!(s.join_room_person("r", "ben", &line("personJoined")).unwrap().is_some());
        let _ = std::fs::remove_dir_all(dir);
    }
}
