//! Where an outside bot's view of a room starts (agent-conversations plan, "from now on"): the entry the bot was added
//! after, per room and bot. A bot reads, hears of and is woken by nothing at or before it. Taking the bot out and
//! putting it in again moves the point to the second join, so the gap in between stays unreadable too.

use super::{Store, now_ms};
use crate::LockExt;
use anyhow::Result;
use rusqlite::{OptionalExtension, params};

/// The newest entry that existed when the bot joined, by `seq` (insertion order, which an edit never changes) and by
/// `rev` (what a reader's cursor counts).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RoomPoint {
    pub seq: i64,
    pub rev: i64,
}

impl Store {
    /// The point of bot `bot_id` in `room`; `None` when it has none (it is no member).
    pub fn room_bot_point(&self, room: &str, bot_id: &str) -> Option<RoomPoint> {
        let c = self.db.locked();
        c.query_row(
            "SELECT point_seq, point_rev FROM room_bots WHERE room = ?1 AND bot_id = ?2",
            params![room, bot_id],
            |r| Ok(RoomPoint { seq: r.get(0)?, rev: r.get(1)? }),
        )
        .optional()
        .ok()
        .flatten()
    }

    /// The `seq` after which `bot_id` may see `room`'s entries. A member with no point (it should not happen) sees
    /// nothing rather than everything.
    pub fn room_bot_floor(&self, room: &str, bot_id: &str) -> i64 {
        self.room_bot_point(room, bot_id).map_or(i64::MAX, |p| p.seq)
    }

    /// Starts (or restarts) the bot's view of `room` at `point`.
    pub fn put_room_bot(&self, room: &str, bot_id: &str, point: RoomPoint) -> Result<()> {
        let c = self.db.locked();
        c.execute(
            "INSERT INTO room_bots(room, bot_id, point_seq, point_rev, added_at) VALUES(?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(room, bot_id) DO UPDATE SET point_seq = ?3, point_rev = ?4, added_at = ?5",
            params![room, bot_id, point.seq, point.rev, now_ms()],
        )?;
        Ok(())
    }

    /// The bot is no member of `room` any more. True when it had a point.
    pub fn drop_room_bot(&self, room: &str, bot_id: &str) -> Result<bool> {
        let c = self.db.locked();
        Ok(c.execute("DELETE FROM room_bots WHERE room = ?1 AND bot_id = ?2", params![room, bot_id])? > 0)
    }

    /// The rooms bot `bot_id` has a point in, oldest first.
    pub fn rooms_of_bot(&self, bot_id: &str) -> Vec<String> {
        let c = self.db.locked();
        let Ok(mut st) = c.prepare("SELECT room FROM room_bots WHERE bot_id = ?1 ORDER BY added_at, room") else {
            return vec![];
        };
        st.query_map([bot_id], |r| r.get(0)).map(|rows| rows.flatten().collect()).unwrap_or_default()
    }

    /// The room is gone: its points go with it.
    pub fn drop_room(&self, room: &str) -> Result<()> {
        let c = self.db.locked();
        c.execute("DELETE FROM room_bots WHERE room = ?1", [room])?;
        Ok(())
    }

    /// The newest entry by `seq` and by `rev` right now: where a point set before the join line is written would start.
    pub fn newest_point(&self) -> RoomPoint {
        let c = self.db.locked();
        let seq = c.query_row("SELECT COALESCE(MAX(seq), 0) FROM entries", [], |r| r.get(0)).unwrap_or(0);
        RoomPoint { seq, rev: self.current_rev() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_readd_moves_the_point_and_a_drop_clears_it() {
        let s = Store::open(std::path::Path::new(":memory:")).unwrap();
        assert_eq!(s.room_bot_floor("r", "b"), i64::MAX, "no point reads nothing");
        s.put_room_bot("r", "b", RoomPoint { seq: 4, rev: 9 }).unwrap();
        assert_eq!(s.room_bot_point("r", "b"), Some(RoomPoint { seq: 4, rev: 9 }));
        assert_eq!(s.rooms_of_bot("b"), vec!["r".to_owned()]);
        assert!(s.drop_room_bot("r", "b").unwrap());
        assert!(!s.drop_room_bot("r", "b").unwrap());
        s.put_room_bot("r", "b", RoomPoint { seq: 12, rev: 20 }).unwrap();
        assert_eq!(s.room_bot_floor("r", "b"), 12);
    }

    #[test]
    fn schema_fourteen_gives_bots_already_in_a_room_the_moment_of_the_migration() {
        use crate::store::{BotConfig, BotGuest, BotState, EntryKind, Lane};
        use rusqlite::Connection;
        let dir = std::env::temp_dir().join(format!("kith-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("t.db");
        let (room, last) = {
            let s = Store::open(&db).unwrap();
            let guest = BotGuest {
                bot_id: "bg1".into(),
                person_id: "bot-bg1".into(),
                owner_person: None,
                name: "Grok".into(),
                provider: "grok".into(),
                agents: vec![],
                state: BotState::Active,
                device_key: None,
                cloud_device: None,
                rate_per_hour: 60,
                concurrent: 2,
                turn_timeout_s: 300,
                wake_profile: None,
                created_at: 1,
                joined_at: Some(1),
                decided_at: None,
                decided_by: None,
                last_seen_at: None,
                revoked_at: None,
                revoked_why: None,
            };
            s.put_bot_guest(&guest).unwrap();
            let cfg: BotConfig = serde_json::from_value(serde_json::json!({
                "id": "room", "kind": "group", "name": "Crew", "members": ["a", "bg1"], "backend": "claude", "cwd": "/tmp",
            }))
            .unwrap();
            s.save_bot(&cfg).unwrap();
            s.insert_entry(&Lane::main("room"), EntryKind::User, 1, &serde_json::json!({"text": "before"})).unwrap();
            let last =
                s.insert_entry(&Lane::main("room"), EntryKind::User, 1, &serde_json::json!({"text": "later"})).unwrap();
            (cfg.id, last)
        };
        // A host from before: no points, schema 13.
        Connection::open(&db)
            .unwrap()
            .execute_batch("DROP TABLE room_bots; UPDATE kv SET v = '13' WHERE k = 'schema';")
            .unwrap();
        let s = Store::open(&db).unwrap();
        assert_eq!(s.kv_get("schema").as_deref(), Some("14"));
        assert_eq!(s.room_bot_point(&room, "bg1"), Some(RoomPoint { seq: last.seq, rev: s.current_rev() }));
        assert!(s.room_bot_point(&room, "a").is_none(), "an agent has no point");
    }
}
