//! Room secrets (designs/2026-10-31-room-secrets): who handed whom a one-time secret in a room, and where it stands. The
//! value is never here (no column for it, a test asserts that): it is a Keychain item of kith-auth's, `kith/once/<id>`.
//!
//! A row goes `pending` -> `taking` (a bot's fetch claimed it) -> `taken`, or ends `expired` or `dropped`. Every move is
//! a compare-and-set on the state, so two fetches, a fetch and an end, or a fetch and the sweep never both win.

use super::{Store, now_ms};
use crate::LockExt;
use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use serde::Serialize;

/// Who a secret is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SecretTo {
    Agent,
    Bot,
}

impl SecretTo {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Bot => "bot",
        }
    }
}

/// One row of `room_secrets`.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RoomSecret {
    pub id: String,
    pub room: String,
    /// `owner` (sender is empty) or `agent` (sender is the member's chat id).
    pub sender_kind: String,
    pub sender: String,
    pub recipient_kind: String,
    /// The member's chat id (an agent) or the bot id.
    pub recipient: String,
    /// The hq agent behind a Kith agent recipient.
    pub agent: Option<String>,
    pub label: Option<String>,
    /// The quiet line's `seq`: a bot may fetch only what was sent after its join point.
    pub notice_seq: Option<i64>,
    pub created_at: i64,
    pub expires_at: i64,
    pub state: String,
    pub done_at: Option<i64>,
}

const COLUMNS: &str = "id, room, sender_kind, sender, recipient_kind, recipient, agent, label, notice_seq, created_at, \
                       expires_at, state, done_at";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<RoomSecret> {
    Ok(RoomSecret {
        id: r.get(0)?,
        room: r.get(1)?,
        sender_kind: r.get(2)?,
        sender: r.get(3)?,
        recipient_kind: r.get(4)?,
        recipient: r.get(5)?,
        agent: r.get(6)?,
        label: r.get(7)?,
        notice_seq: r.get(8)?,
        created_at: r.get(9)?,
        expires_at: r.get(10)?,
        state: r.get(11)?,
        done_at: r.get(12)?,
    })
}

impl Store {
    /// A new `pending` secret. The quiet line's `seq` is set once the line is written (`set_secret_notice`); until
    /// then the row can't be fetched.
    pub fn put_room_secret(&self, s: &RoomSecret) -> Result<()> {
        let c = self.db.locked();
        c.execute(
            "INSERT INTO room_secrets(id, room, sender_kind, sender, recipient_kind, recipient, agent, label, notice_seq,
                                      created_at, expires_at, state, done_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, ?9, ?10, 'pending', NULL)",
            params![
                s.id,
                s.room,
                s.sender_kind,
                s.sender,
                s.recipient_kind,
                s.recipient,
                s.agent,
                s.label,
                s.created_at,
                s.expires_at
            ],
        )?;
        Ok(())
    }

    pub fn set_secret_notice(&self, id: &str, seq: i64) -> Result<()> {
        let c = self.db.locked();
        c.execute("UPDATE room_secrets SET notice_seq = ?2 WHERE id = ?1", params![id, seq])?;
        Ok(())
    }

    pub fn room_secret(&self, id: &str) -> Option<RoomSecret> {
        let c = self.db.locked();
        c.query_row(&format!("SELECT {COLUMNS} FROM room_secrets WHERE id = ?1"), [id], row).optional().ok().flatten()
    }

    /// The newest secrets of `room`, newest first (never a value).
    pub fn room_secrets_of(&self, room: &str, limit: i64) -> Vec<RoomSecret> {
        let c = self.db.locked();
        let Ok(mut st) = c.prepare(&format!(
            "SELECT {COLUMNS} FROM room_secrets WHERE room = ?1 ORDER BY created_at DESC, id LIMIT ?2"
        )) else {
            return vec![];
        };
        st.query_map(params![room, limit], row).map(|rows| rows.flatten().collect()).unwrap_or_default()
    }

    /// What `recipient` may still use in `room` right now: `pending` and not past its time.
    pub fn pending_room_secrets(&self, room: &str, recipient: &str) -> Vec<RoomSecret> {
        let c = self.db.locked();
        let Ok(mut st) = c.prepare(&format!(
            "SELECT {COLUMNS} FROM room_secrets
             WHERE room = ?1 AND recipient = ?2 AND state = 'pending' AND expires_at > ?3 ORDER BY created_at, id"
        )) else {
            return vec![];
        };
        st.query_map(params![room, recipient, now_ms()], row).map(|rows| rows.flatten().collect()).unwrap_or_default()
    }

    /// Every `pending` secret for a Kith agent, for the sweep.
    pub fn pending_agent_secrets(&self) -> Vec<RoomSecret> {
        let c = self.db.locked();
        let Ok(mut st) = c.prepare(&format!(
            "SELECT {COLUMNS} FROM room_secrets WHERE recipient_kind = 'agent' AND state = 'pending'"
        )) else {
            return vec![];
        };
        st.query_map([], row).map(|rows| rows.flatten().collect()).unwrap_or_default()
    }

    /// `pending` -> `taking`, only while it has not lapsed. True for the one caller that won.
    pub fn claim_room_secret(&self, id: &str, now: i64) -> bool {
        let c = self.db.locked();
        c.execute(
            "UPDATE room_secrets SET state = 'taking' WHERE id = ?1 AND state = 'pending' AND expires_at > ?2",
            params![id, now],
        )
        .is_ok_and(|n| n > 0)
    }

    /// Ends a claim: `taking` -> `taken` (the answer may go out) or back to `pending` (the take failed and the item
    /// is still there). A compare-and-set: false when something ended the row meanwhile, and then nothing is answered.
    pub fn finish_room_secret(&self, id: &str, taken: bool) -> bool {
        let c = self.db.locked();
        let (to, at) = if taken { ("taken", Some(now_ms())) } else { ("pending", None) };
        c.execute(
            "UPDATE room_secrets SET state = ?2, done_at = ?3 WHERE id = ?1 AND state = 'taking'",
            params![id, to, at],
        )
        .is_ok_and(|n| n > 0)
    }

    /// A Kith agent's secret was used (its item is gone from kith-auth): `pending` -> `taken`.
    pub fn mark_room_secret_taken(&self, id: &str) -> bool {
        let c = self.db.locked();
        c.execute(
            "UPDATE room_secrets SET state = 'taken', done_at = ?2 WHERE id = ?1 AND state = 'pending'",
            params![id, now_ms()],
        )
        .is_ok_and(|n| n > 0)
    }

    /// Ends every `pending` or `taking` secret of `room` (when given) and of member `member` (when given, as its
    /// recipient or its sender): `dropped`. Returns their ids so the caller removes the items. With neither it ends
    /// nothing.
    pub fn drop_room_secrets(&self, room: Option<&str>, member: Option<&str>) -> Vec<String> {
        if room.is_none() && member.is_none() {
            return vec![];
        }
        let c = self.db.locked();
        let now = now_ms();
        let ids: Vec<String> = c
            .prepare(
                "UPDATE room_secrets SET state = 'dropped', done_at = ?3
                 WHERE state IN ('pending', 'taking') AND (?1 IS NULL OR room = ?1)
                   AND (?2 IS NULL OR recipient = ?2 OR sender = ?2)
                 RETURNING id",
            )
            .and_then(|mut st| {
                st.query_map(params![room, member, now], |r| r.get(0)).map(|rows| rows.flatten().collect())
            })
            .unwrap_or_default();
        ids
    }

    /// One secret ends `dropped` (the owner withdrew it). True when it was still `pending` or `taking`.
    pub fn drop_room_secret(&self, id: &str) -> bool {
        let c = self.db.locked();
        c.execute(
            "UPDATE room_secrets SET state = 'dropped', done_at = ?2 WHERE id = ?1 AND state IN ('pending', 'taking')",
            params![id, now_ms()],
        )
        .is_ok_and(|n| n > 0)
    }

    /// Past their time: `pending` and `taking` rows end `expired` (a `taking` row left by a restart has no fetch in
    /// flight, and a fetch that is in flight loses its finish and answers nothing). Returns their ids.
    pub fn expire_room_secrets(&self, now: i64) -> Vec<String> {
        let c = self.db.locked();
        c.prepare(
            "UPDATE room_secrets SET state = 'expired', done_at = ?1
             WHERE state IN ('pending', 'taking') AND expires_at <= ?1 RETURNING id",
        )
        .and_then(|mut st| st.query_map([now], |r| r.get(0)).map(|rows| rows.flatten().collect()))
        .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret(id: &str, room: &str, to: &str, expires: i64) -> RoomSecret {
        RoomSecret {
            id: id.into(),
            room: room.into(),
            sender_kind: "owner".into(),
            sender: String::new(),
            recipient_kind: "bot".into(),
            recipient: to.into(),
            agent: None,
            label: Some("token".into()),
            notice_seq: None,
            created_at: 10,
            expires_at: expires,
            state: "pending".into(),
            done_at: None,
        }
    }

    #[test]
    fn the_table_has_no_column_that_could_hold_a_value() {
        let s = Store::open(std::path::Path::new(":memory:")).unwrap();
        let c = s.db.locked();
        let cols: Vec<String> = c
            .prepare("SELECT name FROM pragma_table_info('room_secrets') ORDER BY cid")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(
            cols,
            [
                "id",
                "room",
                "sender_kind",
                "sender",
                "recipient_kind",
                "recipient",
                "agent",
                "label",
                "notice_seq",
                "created_at",
                "expires_at",
                "state",
                "done_at"
            ]
        );
    }

    #[test]
    fn a_claim_is_taken_by_one_caller_and_finishes_once() {
        let s = Store::open(std::path::Path::new(":memory:")).unwrap();
        s.put_room_secret(&secret("rs_a", "r", "b", i64::MAX)).unwrap();
        assert!(s.claim_room_secret("rs_a", 1));
        assert!(!s.claim_room_secret("rs_a", 1), "a second fetch loses");
        assert!(s.finish_room_secret("rs_a", true));
        assert!(!s.finish_room_secret("rs_a", true), "only one finish wins");
        assert_eq!(s.room_secret("rs_a").unwrap().state, "taken");
        assert!(!s.claim_room_secret("rs_a", 1), "taken is final");
    }

    #[test]
    fn a_lapsed_row_is_not_claimed_and_an_end_during_a_claim_wins() {
        let s = Store::open(std::path::Path::new(":memory:")).unwrap();
        s.put_room_secret(&secret("rs_a", "r", "b", 100)).unwrap();
        assert!(!s.claim_room_secret("rs_a", 100), "at its time it is over");
        s.put_room_secret(&secret("rs_b", "r", "b", i64::MAX)).unwrap();
        assert!(s.claim_room_secret("rs_b", 1));
        // The room edit that removed the bot ends the row while the fetch holds the value.
        let mut ended = s.drop_room_secrets(None, Some("b"));
        ended.sort();
        assert_eq!(ended, vec!["rs_a".to_owned(), "rs_b".to_owned()]);
        assert!(!s.finish_room_secret("rs_b", true), "the fetch's finish loses: nothing is answered");
        assert_eq!(s.room_secret("rs_b").unwrap().state, "dropped");
    }

    #[test]
    fn a_failed_take_puts_the_row_back_and_expiry_ends_a_stale_taking_row() {
        let s = Store::open(std::path::Path::new(":memory:")).unwrap();
        s.put_room_secret(&secret("rs_a", "r", "b", 50)).unwrap();
        assert!(s.claim_room_secret("rs_a", 1));
        assert!(s.finish_room_secret("rs_a", false));
        assert_eq!(s.room_secret("rs_a").unwrap().state, "pending");
        // A restart left it `taking`; the clock ends it.
        assert!(s.claim_room_secret("rs_a", 1));
        assert!(s.expire_room_secrets(10).is_empty());
        assert_eq!(s.expire_room_secrets(50), vec!["rs_a".to_owned()]);
        assert_eq!(s.room_secret("rs_a").unwrap().state, "expired");
    }

    #[test]
    fn dropping_follows_the_room_and_the_member_as_recipient_or_sender() {
        let s = Store::open(std::path::Path::new(":memory:")).unwrap();
        s.put_room_secret(&secret("rs_a", "r1", "b1", i64::MAX)).unwrap();
        s.put_room_secret(&secret("rs_b", "r2", "b1", i64::MAX)).unwrap();
        let mut from_agent = secret("rs_c", "r1", "b2", i64::MAX);
        from_agent.sender_kind = "agent".into();
        from_agent.sender = "agentchat".into();
        s.put_room_secret(&from_agent).unwrap();
        assert!(s.drop_room_secrets(None, None).is_empty(), "no filter ends nothing");
        let mut by_member = s.drop_room_secrets(Some("r1"), Some("agentchat"));
        by_member.sort();
        assert_eq!(by_member, vec!["rs_c".to_owned()], "the sender leaving ends it");
        assert_eq!(s.drop_room_secrets(Some("r1"), None), vec!["rs_a".to_owned()], "a deleted room");
        assert_eq!(s.drop_room_secrets(None, Some("b1")), vec!["rs_b".to_owned()], "a bot ended anywhere");
        assert!(s.pending_room_secrets("r1", "b1").is_empty());
    }
}
