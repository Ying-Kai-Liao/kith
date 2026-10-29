//! Room conversations (agent-conversations plan, PR 2): what one person's message to a group set going, kept so a
//! host restart can resume it. A lane (a group's main chat, or one of its threads) has at most one open conversation;
//! its `epoch` only grows, so a member's late reply is told from a current one even across restarts. The lane's epoch
//! is the highest `epoch` of its rows, and Stop moves it by bumping the stopped row's.

use super::{Lane, Store, now_ms};
use crate::LockExt;
use anyhow::Result;
use rusqlite::params;
use serde::{Deserialize, Serialize};

/// Where a conversation is in its life. `Ended` ran out (budget, everyone passed, 24 h idle) or a newer person
/// message took its lane; `Stopped` is a person's Stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConvState {
    Open,
    Ended,
    Stopped,
}

impl ConvState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Ended => "ended",
            Self::Stopped => "stopped",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "ended" => Self::Ended,
            "stopped" => Self::Stopped,
            _ => Self::Open,
        }
    }
}

/// The room turn's place, written with each step.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct RoomQueue {
    /// Members still to speak this round, next first. Mentioned members come first; agents a reply woke are appended.
    #[serde(default)]
    pub waiting: Vec<String>,
    /// The member whose turn was sent (and charged) but has not finished. After a restart it goes first, uncharged.
    #[serde(default)]
    pub current: Option<String>,
    /// Rounds started, and how many members spoke (did not pass) in the current one.
    #[serde(default)]
    pub round: u32,
    #[serde(default)]
    pub spoke: u32,
    /// An outside bot's turn (`current`) is posted under this key.
    #[serde(default)]
    pub turn_key: Option<String>,
    /// That bot's wake went out already: a restart waits for the post again but does not wake it twice.
    #[serde(default)]
    pub woke: bool,
}

/// What charging a turn to a conversation came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Charge {
    /// One turn taken; this many are left.
    Charged(i64),
    /// Nothing left (the conversation is open).
    Spent,
    /// Not open any more: a newer message or Stop took the lane.
    Closed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conversation {
    pub id: String,
    pub lane: Lane,
    pub epoch: u64,
    pub budget: i64,
    pub left: i64,
    pub state: ConvState,
    pub opened_by: String,
    pub opened_at: i64,
    pub last_at: i64,
    pub queue: RoomQueue,
}

const COLS: &str = "id, chat, thread, epoch, budget, left, state, opened_by, opened_at, last_at, queue";

fn row_conversation(r: &rusqlite::Row) -> rusqlite::Result<Conversation> {
    let thread: String = r.get(2)?;
    let queue: String = r.get(10)?;
    Ok(Conversation {
        id: r.get(0)?,
        lane: Lane { chat: r.get(1)?, thread: (!thread.is_empty()).then_some(thread) },
        epoch: u64::try_from(r.get::<_, i64>(3)?).unwrap_or(0),
        budget: r.get(4)?,
        left: r.get(5)?,
        state: ConvState::parse(&r.get::<_, String>(6)?),
        opened_by: r.get(7)?,
        opened_at: r.get(8)?,
        last_at: r.get(9)?,
        queue: serde_json::from_str(&queue).unwrap_or_default(),
    })
}

fn thread_of(lane: &Lane) -> &str {
    lane.thread.as_deref().unwrap_or("")
}

impl Store {
    /// A person wrote in the lane: its open conversation ends and a new one opens with the next epoch and `budget`.
    pub fn open_conversation(&self, lane: &Lane, budget: i64, opened_by: &str) -> Result<Conversation> {
        let mut c = self.db.locked();
        let tx = c.transaction()?;
        let thread = thread_of(lane);
        tx.execute(
            "UPDATE conversations SET state = 'ended' WHERE chat = ?1 AND thread = ?2 AND state = 'open'",
            params![lane.chat, thread],
        )?;
        let epoch: i64 = tx.query_row(
            "SELECT COALESCE(MAX(epoch), 0) + 1 FROM conversations WHERE chat = ?1 AND thread = ?2",
            params![lane.chat, thread],
            |r| r.get(0),
        )?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = now_ms();
        tx.execute(
            "INSERT INTO conversations(id, chat, thread, epoch, budget, left, state, opened_by, opened_at, last_at, queue)
             VALUES(?1, ?2, ?3, ?4, ?5, ?5, 'open', ?6, ?7, ?7, '{}')",
            params![id, lane.chat, thread, epoch, budget, opened_by, now],
        )?;
        tx.commit()?;
        Ok(Conversation {
            id,
            lane: lane.clone(),
            epoch: u64::try_from(epoch).unwrap_or(0),
            budget,
            left: budget,
            state: ConvState::Open,
            opened_by: opened_by.to_owned(),
            opened_at: now,
            last_at: now,
            queue: RoomQueue::default(),
        })
    }

    /// The lane's epoch: 0 before any conversation, then the newest one's.
    pub fn lane_epoch(&self, lane: &Lane) -> u64 {
        let c = self.db.locked();
        c.query_row(
            "SELECT COALESCE(MAX(epoch), 0) FROM conversations WHERE chat = ?1 AND thread = ?2",
            params![lane.chat, thread_of(lane)],
            |r| r.get::<_, i64>(0),
        )
        .map_or(0, |e| u64::try_from(e).unwrap_or(0))
    }

    /// Every conversation of `chat`, oldest first (tests read what a room turn left).
    #[cfg(test)]
    pub fn conversations_of(&self, chat: &str) -> Result<Vec<Conversation>> {
        let c = self.db.locked();
        let mut st = c.prepare(&format!("SELECT {COLS} FROM conversations WHERE chat = ?1 ORDER BY opened_at, epoch"))?;
        Ok(st.query_map([chat], row_conversation)?.collect::<rusqlite::Result<_>>()?)
    }

    /// Every conversation still open: what a restarted host resumes.
    pub fn open_conversations(&self) -> Result<Vec<Conversation>> {
        let c = self.db.locked();
        let mut st = c.prepare(&format!("SELECT {COLS} FROM conversations WHERE state = 'open' ORDER BY opened_at"))?;
        Ok(st.query_map([], row_conversation)?.collect::<rusqlite::Result<_>>()?)
    }

    /// The conversation `id`, open or not.
    pub fn conversation(&self, id: &str) -> Option<Conversation> {
        let c = self.db.locked();
        c.query_row(&format!("SELECT {COLS} FROM conversations WHERE id = ?1"), [id], row_conversation).ok()
    }

    /// The lane's newest conversation (the highest epoch), open or not.
    pub fn newest_conversation(&self, lane: &Lane) -> Option<Conversation> {
        let c = self.db.locked();
        c.query_row(
            &format!("SELECT {COLS} FROM conversations WHERE chat = ?1 AND thread = ?2 ORDER BY epoch DESC LIMIT 1"),
            params![lane.chat, thread_of(lane)],
            row_conversation,
        )
        .ok()
    }

    /// Takes one turn from an open conversation and writes the room turn's `queue` in the same step, so a restart
    /// never sees a turn charged but not queued (or the other way round). Every door that spends a conversation's
    /// turns goes through here or [`Store::charge_lane`].
    pub fn charge_turn(&self, id: &str, queue: &RoomQueue) -> Result<Charge> {
        let c = self.db.locked();
        let json = serde_json::to_string(queue)?;
        let n = c.execute(
            "UPDATE conversations SET left = left - 1, queue = ?2, last_at = ?3 WHERE id = ?1 AND state = 'open' AND left > 0",
            params![id, json, now_ms()],
        )?;
        if n > 0 {
            return Ok(Charge::Charged(c.query_row("SELECT left FROM conversations WHERE id = ?1", [id], |r| r.get(0))?));
        }
        let open: i64 = c.query_row("SELECT COUNT(*) FROM conversations WHERE id = ?1 AND state = 'open'", [id], |r| r.get(0))?;
        Ok(if open > 0 { Charge::Spent } else { Charge::Closed })
    }

    /// Takes one turn from the lane's open conversation for a turn that is not the room turn's own (an outside bot's
    /// post or ask). `None` when the lane has no open conversation or nothing is left; the room turn writes the
    /// remaining count back as it goes, so this and [`Store::charge_turn`] never lose a charge.
    pub fn charge_lane(&self, lane: &Lane) -> Result<Option<Conversation>> {
        let c = self.db.locked();
        let n = c.execute(
            "UPDATE conversations SET left = left - 1 WHERE chat = ?1 AND thread = ?2 AND state = 'open' AND left > 0",
            params![lane.chat, thread_of(lane)],
        )?;
        if n == 0 {
            return Ok(None);
        }
        Ok(c.query_row(
            &format!("SELECT {COLS} FROM conversations WHERE chat = ?1 AND thread = ?2 AND state = 'open'"),
            params![lane.chat, thread_of(lane)],
            row_conversation,
        )
        .ok())
    }

    /// The lane's open conversation, if any.
    pub fn open_conversation_of(&self, lane: &Lane) -> Option<Conversation> {
        let c = self.db.locked();
        c.query_row(
            &format!("SELECT {COLS} FROM conversations WHERE chat = ?1 AND thread = ?2 AND state = 'open'"),
            params![lane.chat, thread_of(lane)],
            row_conversation,
        )
        .ok()
    }

    /// Writes where the room turn stands (without touching what is left). False when the conversation is no longer
    /// open (a newer message or Stop took the lane), so a superseded room turn can't write over its successor.
    pub fn save_queue(&self, id: &str, queue: &RoomQueue) -> Result<bool> {
        let c = self.db.locked();
        let queue = serde_json::to_string(queue)?;
        let n = c.execute(
            "UPDATE conversations SET queue = ?2, last_at = ?3 WHERE id = ?1 AND state = 'open'",
            params![id, queue, now_ms()],
        )?;
        Ok(n > 0)
    }

    /// Sets a step of an open conversation outright, `left` included (tests build the state a restart finds).
    #[cfg(test)]
    pub fn save_conversation(&self, id: &str, left: i64, queue: &RoomQueue) -> Result<bool> {
        let c = self.db.locked();
        let queue = serde_json::to_string(queue)?;
        let n = c.execute(
            "UPDATE conversations SET left = ?2, queue = ?3, last_at = ?4 WHERE id = ?1 AND state = 'open'",
            params![id, left, queue, now_ms()],
        )?;
        Ok(n > 0)
    }

    #[cfg(test)]
    pub fn set_last_at_for_test(&self, id: &str, last_at: i64) {
        let c = self.db.locked();
        c.execute("UPDATE conversations SET last_at = ?2 WHERE id = ?1", params![id, last_at]).unwrap();
    }

    /// Closes an open conversation (no-op if it already is closed).
    pub fn end_conversation(&self, id: &str, state: ConvState) -> Result<()> {
        let c = self.db.locked();
        c.execute("UPDATE conversations SET state = ?2 WHERE id = ?1 AND state = 'open'", params![id, state.as_str()])?;
        Ok(())
    }

    /// Stop: every open conversation of `chat` (main chat and threads) is stopped and its lane's epoch moves.
    pub fn stop_conversations(&self, chat: &str) -> Result<usize> {
        let c = self.db.locked();
        Ok(c.execute(
            "UPDATE conversations SET state = 'stopped', epoch = epoch + 1, last_at = ?2
             WHERE chat = ?1 AND state = 'open'",
            params![chat, now_ms()],
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lanes_epoch_only_grows_and_one_conversation_is_open_per_lane() {
        let store = Store::open(std::path::Path::new(":memory:")).unwrap();
        let main = Lane::main("g");
        let thread = Lane::in_thread("g", "root");
        assert_eq!(store.lane_epoch(&main), 0);
        let first = store.open_conversation(&main, 10, "owner").unwrap();
        let other = store.open_conversation(&thread, 4, "owner").unwrap();
        assert_eq!((first.epoch, other.epoch), (1, 1), "lanes count on their own");
        let second = store.open_conversation(&main, 5, "p1").unwrap();
        assert_eq!(second.epoch, 2);
        let rows = store.conversations_of("g").unwrap();
        assert_eq!(rows.iter().filter(|c| c.state == ConvState::Open).count(), 2);
        assert_eq!(rows[0].state, ConvState::Ended, "a newer message ends the older conversation");
        // A superseded room turn can't write over its successor.
        assert!(!store.save_conversation(&first.id, 1, &RoomQueue::default()).unwrap());
        // Stop ends both lanes' conversations and moves their epochs past anything a member holds.
        assert_eq!(store.stop_conversations("g").unwrap(), 2);
        assert_eq!((store.lane_epoch(&main), store.lane_epoch(&thread)), (3, 2));
        assert_eq!(store.open_conversation(&main, 10, "owner").unwrap().epoch, 4);
        assert!(store.open_conversations().unwrap().iter().all(|c| c.state == ConvState::Open));
    }
}
