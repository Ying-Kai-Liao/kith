//! The `entries` table: the transcript of every chat and thread.

use super::model::{Entry, EntryKind, Lane, ReadScope};
use super::{Store, logged, next_rev, now_ms};
use crate::LockExt;
use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde_json::Value;
use std::fmt::Write as _;

pub(super) fn row_entry(r: &rusqlite::Row) -> rusqlite::Result<Entry> {
    let data: String = r.get(6)?;
    Ok(Entry {
        seq: r.get(0)?,
        id: r.get(1)?,
        bot_id: r.get(2)?,
        rev: r.get(3)?,
        kind: r.get(4)?,
        turn: r.get(5)?,
        data: serde_json::from_str(&data).unwrap_or(Value::Null),
        created_at: r.get(7)?,
        updated_at: r.get(8)?,
        thread_id: r.get(9)?,
    })
}

pub(super) const ENTRY_COLS: &str = "seq, id, bot_id, rev, kind, turn, data, created_at, updated_at, thread_id";
/// A bot still winding down after its deletion can't leave lines behind (`?2` is the chat).
const LIVE_CHAT: &str = "WHERE NOT EXISTS (SELECT 1 FROM bots WHERE id = ?2 AND deleted = 1)";

impl Store {
    /// Production writes go through `Hub::add_entry` (which holds and retries); this is the tests' direct way in.
    #[cfg(test)]
    pub fn insert_entry(&self, lane: &Lane, kind: EntryKind, turn: i64, data: &Value) -> Result<Entry> {
        self.insert_entry_at(lane, kind, turn, data, now_ms())
    }

    /// [`Self::insert_entry`] stamped `now` (ms): lines imported from a terminal session keep when they were said.
    pub fn insert_entry_at(&self, lane: &Lane, kind: EntryKind, turn: i64, data: &Value, now: i64) -> Result<Entry> {
        self.insert_entry_as(&uuid::Uuid::new_v4().to_string(), lane, kind, turn, data, now).map(|(e, _)| e)
    }

    /// [`Self::insert_entry_at`] under a caller-chosen `id`, so a failed write can be retried without a duplicate:
    /// an `id` already stored comes back as that entry.
    pub fn insert_entry_as(
        &self,
        id: &str,
        lane: &Lane,
        kind: EntryKind,
        turn: i64,
        data: &Value,
        now: i64,
    ) -> Result<(Entry, bool)> {
        let c = self.db.locked();
        if let Some(e) =
            c.query_row(&format!("SELECT {ENTRY_COLS} FROM entries WHERE id = ?1"), [id], row_entry).optional()?
        {
            return Ok((e, false));
        }
        let rev = next_rev(&c)?;
        let added = c.execute(
            &format!(
                "INSERT INTO entries(id, bot_id, thread_id, rev, kind, turn, data, created_at, updated_at)
                 SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8 {LIVE_CHAT}"
            ),
            params![id, lane.chat, lane.thread, rev, kind.as_str(), turn, data.to_string(), now],
        )?;
        ensure!(added == 1, "the chat was deleted");
        Ok((
            Entry {
                id: id.to_owned(),
                seq: c.last_insert_rowid(),
                bot_id: lane.chat.clone(),
                thread_id: lane.thread.clone(),
                rev,
                kind: kind.as_str().to_owned(),
                turn,
                data: data.clone(),
                created_at: now,
                updated_at: now,
            },
            true,
        ))
    }

    /// Deterministic IDs make routine transcript publication retryable after a crash.
    pub fn insert_entry_once(&self, id: &str, lane: &Lane, kind: EntryKind, data: &Value) -> Result<(Entry, bool)> {
        let mut c = self.db.locked();
        let tx = c.transaction()?;
        if let Some(entry) =
            tx.query_row(&format!("SELECT {ENTRY_COLS} FROM entries WHERE id = ?"), [id], row_entry).optional()?
        {
            return Ok((entry, false));
        }
        let rev = next_rev(&tx)?;
        let now = now_ms();
        let added = tx.execute(&format!("INSERT INTO entries(id, bot_id, thread_id, rev, kind, turn, data, created_at, updated_at) SELECT ?1, ?2, ?3, ?4, ?5, 0, ?6, ?7, ?7 {LIVE_CHAT}"),
            params![id, lane.chat, lane.thread, rev, kind.as_str(), data.to_string(), now])?;
        ensure!(added == 1, "the chat was deleted");
        let entry = tx.query_row(&format!("SELECT {ENTRY_COLS} FROM entries WHERE id = ?"), [id], row_entry)?;
        tx.commit()?;
        Ok((entry, true))
    }

    pub fn update_entry(&self, id: &str, data: &Value) -> Result<Option<Entry>> {
        let c = self.db.locked();
        let rev = next_rev(&c)?;
        c.execute(
            "UPDATE entries SET rev = ?2, data = ?3, updated_at = ?4 WHERE id = ?1",
            params![id, rev, data.to_string(), now_ms()],
        )?;
        Ok(c.query_row(&format!("SELECT {ENTRY_COLS} FROM entries WHERE id = ?1"), [id], row_entry).optional()?)
    }

    /// Every entry of the chat (main chat and threads) made at or after `from` (ms), oldest first: what a topic folds.
    pub fn entries_from(&self, bot_id: &str, from: i64) -> Result<Vec<Entry>> {
        let c = self.db.locked();
        let mut st = c.prepare(&format!(
            "SELECT {ENTRY_COLS} FROM entries WHERE bot_id = ?1 AND created_at >= ?2 ORDER BY created_at, seq"
        ))?;
        Ok(st.query_map(params![bot_id, from], row_entry)?.collect::<rusqlite::Result<_>>()?)
    }

    /// The entries of the chat that start a topic.
    pub fn topic_roots(&self, bot_id: &str) -> Result<Vec<Entry>> {
        let c = self.db.locked();
        let mut st = c.prepare(&format!(
            "SELECT {ENTRY_COLS} FROM entries WHERE bot_id = ?1 AND json_type(data, '$.topic') = 'object' ORDER BY created_at, seq"
        ))?;
        Ok(st.query_map([bot_id], row_entry)?.collect::<rusqlite::Result<_>>()?)
    }

    pub fn entry(&self, id: &str) -> Option<Entry> {
        let c = self.db.locked();
        let row = c.query_row(&format!("SELECT {ENTRY_COLS} FROM entries WHERE id = ?1"), [id], row_entry);
        logged("entry", row.optional()).flatten()
    }

    pub fn find_by_nonce(&self, bot_id: &str, nonce: &str) -> Option<Entry> {
        let c = self.db.locked();
        let row = c.query_row(
            &format!(
                "SELECT {ENTRY_COLS} FROM entries WHERE bot_id = ?1 AND kind = 'user' AND json_extract(data, '$.clientNonce') = ?2"
            ),
            params![bot_id, nonce],
            row_entry,
        );
        logged("entry by nonce", row.optional()).flatten()
    }

    /// The ask this bot guest already sent with `nonce`, in any of its conversations (a bot's nonce is its own across
    /// agents, which `find_by_nonce` by chat row can't see).
    pub fn find_bot_guest_nonce(&self, bot_guest_id: &str, nonce: &str) -> Option<Entry> {
        let c = self.db.locked();
        let row = c.query_row(
            &format!(
                "SELECT {ENTRY_COLS} FROM entries WHERE kind = 'user' AND json_extract(data, '$.sender.botId') = ?1 AND json_extract(data, '$.clientNonce') = ?2 ORDER BY seq LIMIT 1"
            ),
            params![bot_guest_id, nonce],
            row_entry,
        );
        logged("entry by bot guest nonce", row.optional()).flatten()
    }

    pub fn max_turn(&self, bot_id: &str) -> i64 {
        let c = self.db.locked();
        let n = c.query_row("SELECT COALESCE(MAX(turn), 0) FROM entries WHERE bot_id = ?1", [bot_id], |r| r.get(0));
        logged("max turn", n).unwrap_or(0)
    }

    /// Entries changed after `since`. A fresh client (`since == 0`) only gets the
    /// newest `cap` entries per bot and pages older ones in with `history`. The roots of threads whose replies
    /// made that window come too, or a long task's card (root far back, replies fresh) would reach no client.
    pub fn entries_since(&self, since: i64, cap: i64) -> Result<Vec<Entry>> {
        let c = self.db.locked();
        let sql = if since == 0 {
            format!(
                "WITH ranked AS (SELECT *, ROW_NUMBER() OVER (PARTITION BY bot_id ORDER BY seq DESC) AS rn FROM entries)
                 SELECT {ENTRY_COLS} FROM ranked
                 WHERE (rn <= ?2 AND rev > ?1)
                    OR id IN (SELECT thread_id FROM ranked WHERE rn <= ?2 AND thread_id IS NOT NULL)
                 ORDER BY seq"
            )
        } else {
            format!("SELECT {ENTRY_COLS} FROM entries WHERE rev > ?1 AND ?2 > 0 ORDER BY seq")
        };
        let mut st = c.prepare(&sql)?;
        Ok(st.query_map(params![since, cap], row_entry)?.collect::<rusqlite::Result<_>>()?)
    }

    /// The entries of one chat (a room, threads included) after `since`, oldest first, at most `cap`. A fresh reader
    /// (`since == 0`) gets the newest `cap`; one with a cursor gets the next `cap` after it, so it skips nothing.
    /// Only entries inserted after `after_seq` count (an outside bot's join point), however far an edit moved their `rev`.
    pub fn chat_entries_since(&self, chat: &str, since: i64, cap: i64, after_seq: i64) -> Result<Vec<Entry>> {
        let c = self.db.locked();
        let newest = since == 0;
        let mut st = c.prepare(&format!(
            "SELECT {ENTRY_COLS} FROM entries WHERE bot_id = ?1 AND rev > ?2 AND seq > ?4
             ORDER BY seq {} LIMIT ?3",
            if newest { "DESC" } else { "ASC" }
        ))?;
        let mut rows: Vec<Entry> =
            st.query_map(params![chat, since, cap, after_seq], row_entry)?.collect::<rusqlite::Result<_>>()?;
        if newest {
            rows.reverse();
        }
        Ok(rows)
    }

    /// Older main-chat entries (threads load whole with [`Self::thread`]).
    pub fn history(&self, bot_id: &str, before_seq: i64, limit: i64) -> Result<Vec<Entry>> {
        let c = self.db.locked();
        let mut st = c.prepare(&format!(
            "SELECT {ENTRY_COLS} FROM entries WHERE bot_id = ?1 AND thread_id IS NULL AND seq < ?2
             ORDER BY seq DESC LIMIT ?3"
        ))?;
        let mut rows: Vec<Entry> =
            st.query_map(params![bot_id, before_seq, limit], row_entry)?.collect::<rusqlite::Result<_>>()?;
        rows.reverse();
        Ok(rows)
    }

    /// Cards still waiting for the user to sign in to agent `backend`, oldest first.
    // ponytail: scans notices without an index; sign-ins are rare, add one if it shows up in profiles.
    pub fn pending_sign_ins(&self, backend: &str) -> Result<Vec<Entry>> {
        let c = self.db.locked();
        let mut st = c.prepare(&format!(
            "SELECT {ENTRY_COLS} FROM entries WHERE kind = 'notice'
               AND json_extract(data, '$.connectionRequest.kind') = 'agent'
               AND json_extract(data, '$.connectionRequest.status') = 'pending'
               AND json_extract(data, '$.connectionRequest.backend') = ?1
             ORDER BY rev"
        ))?;
        Ok(st.query_map([backend], row_entry)?.collect::<rusqlite::Result<_>>()?)
    }

    /// The open limit card of `kind` (agent | person) in a lane, if any: a second held message joins it.
    pub fn pending_limit_card(&self, bot_id: &str, thread: Option<&str>, kind: &str) -> Option<Entry> {
        let c = self.db.locked();
        let mut st = c
            .prepare(&format!(
                "SELECT {ENTRY_COLS} FROM entries WHERE bot_id = ?1 AND thread_id IS ?2 AND kind = 'permission'
                   AND json_extract(data, '$.status') = 'pending' AND json_extract(data, '$.limit.kind') = ?3
                 ORDER BY seq DESC LIMIT 1"
            ))
            .ok()?;
        st.query_row(rusqlite::params![bot_id, thread, kind], row_entry).ok()
    }

    /// Chat-visible messages (the user's, final replies) in any of `bot_id`'s lanes that
    /// contain every term (case-insensitive for ASCII), newest first, from `from` (a `seq`) back.
    // ponytail: substring scan, no index (CJK needs no tokenizer this way): 0.6 s over 180k messages of one bot.
    // An FTS5 trigram table once a bot's search passes a second.
    pub fn search_messages(&self, bot_id: &str, terms: &[String], from: Option<i64>, limit: i64) -> Result<Vec<Entry>> {
        let mut sql = format!(
            "SELECT {ENTRY_COLS} FROM entries WHERE bot_id = ?1 AND seq <= ?3 AND
               (kind = 'user' OR (kind = 'agent' AND json_extract(data, '$.final') = 1))"
        );
        for i in 0..terms.len() {
            let _ = write!(sql, " AND instr(lower(json_extract(data, '$.text')), ?{}) > 0", i + 4);
        }
        sql.push_str(" ORDER BY seq DESC LIMIT ?2");
        let mut args: Vec<rusqlite::types::Value> =
            vec![bot_id.to_owned().into(), limit.into(), from.unwrap_or(i64::MAX).into()];
        args.extend(terms.iter().map(|t| t.to_lowercase().into()));
        let c = self.db.locked();
        let mut st = c.prepare(&sql)?;
        Ok(st.query_map(rusqlite::params_from_iter(args), row_entry)?.collect::<rusqlite::Result<_>>()?)
    }

    /// A thread's newest `limit` entries, oldest first.
    pub fn thread(&self, bot_id: &str, root: &str, limit: i64) -> Result<Vec<Entry>> {
        let c = self.db.locked();
        let mut st = c.prepare(&format!(
            "SELECT {ENTRY_COLS} FROM entries WHERE bot_id = ?1 AND thread_id = ?2 ORDER BY seq DESC LIMIT ?3"
        ))?;
        let mut rows: Vec<Entry> =
            st.query_map(params![bot_id, root, limit], row_entry)?.collect::<rusqlite::Result<_>>()?;
        rows.reverse();
        Ok(rows)
    }

    /// The newest `limit` bot-to-bot notices in `bot_id`'s main chat that involve `peer_id`, oldest first.
    pub fn bot_conversation(&self, bot_id: &str, peer_id: &str, limit: i64) -> Result<Vec<Entry>> {
        let c = self.db.locked();
        let mut st = c.prepare(&format!(
            "SELECT {ENTRY_COLS} FROM entries WHERE bot_id = ?1 AND thread_id IS NULL AND kind = 'notice'
               AND json_type(data, '$.botMessage') = 'object'
               AND ?2 IN (json_extract(data, '$.botMessage.sourceBotId'), json_extract(data, '$.botMessage.targetBotId'))
             ORDER BY seq DESC LIMIT ?3"
        ))?;
        let mut rows: Vec<Entry> =
            st.query_map(params![bot_id, peer_id, limit], row_entry)?.collect::<rusqlite::Result<_>>()?;
        rows.reverse();
        Ok(rows)
    }

    /// Messages in all of `bot_id`'s own lanes (its chat and threads) after `after_seq`, oldest first, narrowed to
    /// the owner's own words, which tidying reads: a user message with no `sender` (a bot
    /// guest) and no `person` (an invited person), and a final reply only when every user message of its turn is
    /// the owner's. Tidying learns from these; whoever wrote the rest is not the owner.
    pub fn owner_conversation_since(&self, bot_id: &str, after_seq: i64, limit: i64) -> Result<Vec<Entry>> {
        let c = self.db.locked();
        let mut st = c.prepare(&format!(
            "SELECT {ENTRY_COLS} FROM entries e WHERE bot_id = ?1 AND seq > ?2 AND (
               (kind = 'user' AND json_type(data, '$.sender') IS NULL AND json_type(data, '$.person') IS NULL)
               OR (kind = 'agent' AND json_extract(data, '$.final') = 1 AND NOT EXISTS (
                 SELECT 1 FROM entries u WHERE u.bot_id = e.bot_id AND u.thread_id IS e.thread_id
                   AND u.turn = e.turn AND u.kind = 'user'
                   AND (json_type(u.data, '$.sender') IS NOT NULL OR json_type(u.data, '$.person') IS NOT NULL))))
             ORDER BY seq LIMIT ?3"
        ))?;
        Ok(st.query_map(params![bot_id, after_seq, limit], row_entry)?.collect::<rusqlite::Result<_>>()?)
    }

    /// The newest entry of a bot's chat and threads.
    pub fn last_seq(&self, bot_id: &str) -> i64 {
        let c = self.db.locked();
        logged(
            "last seq",
            c.query_row("SELECT COALESCE(MAX(seq), 0) FROM entries WHERE bot_id = ?1", [bot_id], |r| r.get(0)),
        )
        .unwrap_or(0)
    }

    /// Chat-visible messages (the user's, final replies) in `lane` after `after_seq`, oldest first.
    pub fn messages_after(&self, lane: &Lane, after_seq: i64, limit: i64) -> Result<Vec<Entry>> {
        let c = self.db.locked();
        let mut st = c.prepare(&format!(
            "SELECT {ENTRY_COLS} FROM entries WHERE bot_id = ?1 AND thread_id IS ?2 AND seq > ?3 AND
               (kind = 'user' OR (kind = 'agent' AND json_extract(data, '$.final') = 1))
             ORDER BY seq DESC LIMIT ?4"
        ))?;
        let mut rows: Vec<Entry> = st
            .query_map(params![lane.chat, lane.thread, after_seq, limit], row_entry)?
            .collect::<rusqlite::Result<_>>()?;
        rows.reverse();
        Ok(rows)
    }

    /// The `rev` of the last entry bot guest `bot_guest_id` wrote in `chat` (0 if it never has): where its next read
    /// of the room starts.
    pub fn last_rev_of_bot_guest(&self, chat: &str, bot_guest_id: &str) -> i64 {
        let c = self.db.locked();
        let n = c.query_row(
            "SELECT COALESCE(MAX(rev), 0) FROM entries WHERE bot_id = ?1 AND kind = 'user'
               AND json_extract(data, '$.sender.botId') = ?2",
            params![chat, bot_guest_id],
            |r| r.get(0),
        );
        logged("last rev of bot guest", n).unwrap_or(0)
    }

    /// The `seq` of `bot`'s last reply in `lane` (0 if it hasn't spoken there).
    pub fn last_spoke(&self, lane: &Lane, bot: &str) -> i64 {
        let c = self.db.locked();
        let n = c.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM entries WHERE bot_id = ?1 AND thread_id IS ?2 AND kind = 'agent'
               AND json_extract(data, '$.final') = 1 AND json_extract(data, '$.author') = ?3",
            params![lane.chat, lane.thread, bot],
            |r| r.get(0),
        );
        logged("last spoke", n).unwrap_or(0)
    }

    /// A thread's reply count, newest reply time, the bots that replied (first reply first)
    /// and how many replies are unread.
    pub fn thread_summary(&self, chat: &str, root: &str) -> Result<Value> {
        let read_rev = self.bot(chat)?.map_or(0, |r| r.read_rev);
        let unread = self.unread(chat, read_rev, ReadScope::Thread(root));
        let c = self.db.locked();
        let mut st = c.prepare(
            "SELECT kind, json_extract(data, '$.author'), created_at FROM entries WHERE bot_id = ?1 AND thread_id = ?2
               AND (kind = 'user' OR (kind = 'agent' AND json_extract(data, '$.final') = 1)) ORDER BY seq",
        )?;
        let rows = st.query_map(params![chat, root], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, i64>(2)?))
        })?;
        let (mut count, mut last_at, mut authors) = (0, 0, Vec::<String>::new());
        for row in rows {
            let (kind, author, at) = row?;
            count += 1;
            last_at = at;
            let author = if kind == EntryKind::User.as_str() { "user".to_owned() } else { author.unwrap_or_default() };
            if !author.is_empty() && !authors.contains(&author) {
                authors.push(author);
            }
        }
        Ok(serde_json::json!({"count": count, "lastAt": last_at, "authors": authors, "unread": unread}))
    }

    /// Permission cards left `pending` (and sends left `queued`) by a crash or restart
    /// can never be honored (unless the card store still holds the card open, parked); returns how many were closed out.
    pub fn expire_pending(&self) -> Result<usize> {
        let ids: Vec<String> = {
            let c = self.db.locked();
            let mut st = c.prepare(
                "SELECT id FROM entries WHERE (kind = 'permission' AND json_extract(data, '$.status') = 'pending'
                      AND NOT EXISTS (SELECT 1 FROM cards WHERE cards.entry_id = entries.id
                                      AND cards.state IN ('pending','live','parked')))
                  OR (kind = 'user' AND json_extract(data, '$.status') = 'queued')
                  OR (kind = 'notice' AND json_extract(data, '$.delegationId') IS NOT NULL
                      AND json_extract(data, '$.status') IN ('queued', 'sent'))",
            )?;
            st.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
        };
        for id in &ids {
            if let Some(mut e) = self.entry(id) {
                let status = if e.kind == EntryKind::Notice.as_str() {
                    let heading = e.data["heading"].as_str().unwrap_or("Bot request");
                    e.data["text"] = format!(
                        "{heading}\nInterrupted by host restart. Partial work may have happened; check before retrying."
                    )
                    .into();
                    if let Some(message) = e.data.get_mut("botMessage").and_then(Value::as_object_mut) {
                        message.insert(
                            "detail".into(),
                            "Interrupted by host restart. Partial work may have happened; check before retrying."
                                .into(),
                        );
                    }
                    e.data["style"] = "error".into();
                    "failed"
                } else if e.kind == EntryKind::User.as_str() {
                    "failed"
                } else {
                    "expired"
                };
                e.data["status"] = status.into();
                self.update_entry(id, &e.data)?;
            }
        }
        Ok(ids.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::temp_store;
    use serde_json::json;

    #[test]
    fn the_owner_conversation_leaves_out_guests_and_people() {
        let s = temp_store();
        let main = Lane::main("b1");
        s.insert_entry(&main, EntryKind::User, 1, &json!({"text": "owner"})).unwrap();
        s.insert_entry(&main, EntryKind::Agent, 1, &json!({"text": "to owner", "final": true})).unwrap();
        s.insert_entry(&main, EntryKind::User, 2, &json!({"text": "guest", "sender": {"botId": "g"}})).unwrap();
        s.insert_entry(&main, EntryKind::Agent, 2, &json!({"text": "to guest", "final": true})).unwrap();
        s.insert_entry(&main, EntryKind::User, 3, &json!({"text": "sam", "person": "sam"})).unwrap();
        s.insert_entry(&main, EntryKind::Agent, 3, &json!({"text": "to sam", "final": true})).unwrap();
        s.insert_entry(&main, EntryKind::User, 4, &json!({"text": "owner again"})).unwrap();
        s.insert_entry(&main, EntryKind::Agent, 4, &json!({"text": "narration", "final": false})).unwrap();
        let texts: Vec<String> = s
            .owner_conversation_since("b1", 0, 50)
            .unwrap()
            .into_iter()
            .map(|e| e.data["text"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(texts, ["owner", "to owner", "owner again"]);
    }

    #[test]
    fn a_fresh_sync_brings_the_root_of_a_thread_whose_replies_fill_the_window() {
        let s = temp_store();
        let root =
            s.insert_entry(&Lane::main("b1"), EntryKind::User, 1, &json!({"text": "task", "task": true})).unwrap();
        s.insert_entry(&Lane::main("b1"), EntryKind::User, 2, &json!({"text": "other chat"})).unwrap();
        let thread = Lane::in_thread("b1", &root.id);
        for i in 0..5 {
            s.insert_entry(&thread, EntryKind::Agent, 1, &json!({"text": format!("reply {i}"), "final": true}))
                .unwrap();
        }
        let ids = |cap| -> Vec<String> {
            s.entries_since(0, cap).unwrap().into_iter().map(|e| e.data["text"].as_str().unwrap().to_owned()).collect()
        };
        // The newest 3 are all replies; the old root rides along, and the main chat row outside the window does not.
        assert_eq!(ids(3), ["task", "reply 2", "reply 3", "reply 4"]);
    }

    #[test]
    fn history_search_matches_every_term_in_chat_messages() {
        let s = temp_store();
        let main = Lane::main("b1");
        s.insert_entry(&main, EntryKind::User, 1, &json!({"text": "我們用 Swift 6 寫 Kith"})).unwrap();
        s.insert_entry(&main, EntryKind::Agent, 1, &json!({"text": "swift narration", "final": false})).unwrap();
        s.insert_entry(&main, EntryKind::Agent, 1, &json!({"text": "Swift 6 strict mode is on", "final": true}))
            .unwrap();
        s.insert_entry(&Lane::main("b2"), EntryKind::User, 1, &json!({"text": "Swift 6 elsewhere"})).unwrap();
        let texts = |terms: &[&str]| -> Vec<String> {
            let terms: Vec<String> = terms.iter().map(|t| (*t).to_owned()).collect();
            s.search_messages("b1", &terms, None, 10)
                .unwrap()
                .into_iter()
                .map(|e| e.data["text"].as_str().unwrap().to_owned())
                .collect()
        };
        assert_eq!(texts(&["SWIFT", "6"]), ["Swift 6 strict mode is on", "我們用 Swift 6 寫 Kith"]);
        assert_eq!(texts(&["kith", "寫"]), ["我們用 Swift 6 寫 Kith"]);
        assert_eq!(texts(&["narration"]).len(), 0);
    }

    #[test]
    fn history_search_reads_back_from_a_message() {
        let s = temp_store();
        let main = Lane::main("b1");
        let seqs: Vec<i64> = (1..=4)
            .map(|i| s.insert_entry(&main, EntryKind::User, 1, &json!({"text": format!("note {i}")})).unwrap().seq)
            .collect();
        let texts = |terms: &[String], from| -> Vec<String> {
            let rows = s.search_messages("b1", terms, from, 2).unwrap();
            rows.into_iter().map(|e| e.data["text"].as_str().unwrap().to_owned()).collect()
        };
        assert_eq!(texts(&[], Some(seqs[2])), ["note 3", "note 2"], "starts at the message itself");
        assert_eq!(texts(&["1".to_owned()], Some(seqs[2])), ["note 1"]);
        assert_eq!(texts(&[], None), ["note 4", "note 3"]);
    }

    #[test]
    fn restart_expires_pending_cards_and_queued_sends() {
        let s = temp_store();
        s.insert_entry(&Lane::main("b1"), EntryKind::Permission, 1, &serde_json::json!({"status": "pending"})).unwrap();
        let q =
            s.insert_entry(&Lane::main("b1"), EntryKind::User, 2, &serde_json::json!({"status": "queued"})).unwrap();
        s.insert_entry(&Lane::main("b1"), EntryKind::User, 1, &serde_json::json!({"status": "sent"})).unwrap();
        assert_eq!(s.expire_pending().unwrap(), 2);
        assert_eq!(s.entry(&q.id).unwrap().data["status"], "failed");
        assert_eq!(s.expire_pending().unwrap(), 0, "idempotent");
    }

    #[test]
    fn bot_conversation_lists_one_peers_notices_oldest_first() {
        let s = temp_store();
        let notice = |bot: &str, source: &str, target: &str, text: &str| {
            let data = json!({"text": text, "sourceBotId": source, "targetBotId": target,
                "botMessage": {"sourceBotId": source, "targetBotId": target, "text": text}});
            s.insert_entry(&Lane::main(bot), EntryKind::Notice, 1, &data).unwrap();
        };
        notice("a", "a", "b", "first");
        notice("a", "a", "c", "other peer");
        notice("a", "b", "a", "second");
        notice("b", "a", "b", "other chat");
        s.insert_entry(&Lane::main("a"), EntryKind::Notice, 1, &json!({"text": "plain"})).unwrap();
        let old = s
            .insert_entry(
                &Lane::main("a"),
                EntryKind::Notice,
                1,
                &json!({"text": "Messaged b: old", "heading": "Messaged b: old", "delegationId": "old",
                "sourceBotId": "a", "targetBotId": "b", "status": "completed"}),
            )
            .unwrap();
        let old_data = old.data.clone();
        let texts: Vec<_> = s
            .bot_conversation("a", "b", 10)
            .unwrap()
            .into_iter()
            .map(|e| e.data["text"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(texts, ["first", "second"]);
        assert_eq!(s.entry(&old.id).unwrap().data, old_data, "existing notice is untouched");
        assert_eq!(s.bot_conversation("a", "b", 1).unwrap().len(), 1, "limit keeps the newest");
    }
}
