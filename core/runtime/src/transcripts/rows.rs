//! The `engine_sessions` table (migration 110) and the `conversation.transcript` row that reads it.

use rusqlite::{Connection, OptionalExtension as _, params};
use serde_json::{Value, json};

use super::{SessionPtr, hash_session};
use crate::db::Core;
use crate::error::{Error, Result};

/// One row of `engine_sessions`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EngineSession {
    pub conversation: String,
    pub account: String,
    pub engine: String,
    pub session_id: String,
    pub pointer: String,
    pub cwd: String,
    pub hash: Option<String>,
    pub size: Option<i64>,
    pub last_read_at: Option<i64>,
}

const COLS: &str =
    "conversation, account, engine, session_id, pointer, cwd, hash, size, last_read_at";

fn from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<EngineSession> {
    Ok(EngineSession {
        conversation: r.get(0)?,
        account: r.get(1)?,
        engine: r.get(2)?,
        session_id: r.get(3)?,
        pointer: r.get(4)?,
        cwd: r.get(5)?,
        hash: r.get(6)?,
        size: r.get(7)?,
        last_read_at: r.get(8)?,
    })
}

/// The conversation's engine sessions, by account.
pub(crate) fn list(conn: &Connection, conversation: &str) -> Result<Vec<EngineSession>> {
    let mut q = conn.prepare(&format!(
        "SELECT {COLS} FROM engine_sessions WHERE conversation = ?1 ORDER BY account"
    ))?;
    let rows = q
        .query_map([conversation], from_row)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

pub(crate) fn get(
    conn: &Connection,
    conversation: &str,
    account: &str,
) -> Result<Option<EngineSession>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLS} FROM engine_sessions WHERE conversation = ?1 AND account = ?2"),
            [conversation, account],
            from_row,
        )
        .optional()?)
}

/// Writes the row, replacing the one with the same key.
pub(crate) fn put(conn: &Connection, s: &EngineSession, now: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO engine_sessions (conversation, account, engine, session_id, pointer, cwd, hash, size, last_read_at, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT (conversation, account) DO UPDATE SET engine = ?3, session_id = ?4, pointer = ?5, cwd = ?6,
             hash = ?7, size = ?8, last_read_at = ?9",
        params![
            s.conversation,
            s.account,
            s.engine,
            s.session_id,
            s.pointer,
            s.cwd,
            s.hash,
            s.size,
            s.last_read_at,
            now
        ],
    )?;
    Ok(())
}

impl Core {
    /// `conversation.transcript`: the conversation's engine sessions as stored, with what the engine's copy is now
    /// (`present`, `changed` since the stored hash). Reads the files, writes nothing.
    pub(crate) fn transcript(&self, params: &Value) -> Result<Value> {
        let id = params.get("id").and_then(Value::as_str).ok_or_else(|| {
            Error::BadParams("kith-core: conversation.transcript needs an id".into())
        })?;
        let known: bool = self
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM conversations WHERE id = ?1",
                [id],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n > 0)?;
        if !known {
            return Err(Error::Refused(format!("kith-core: no conversation {id}")));
        }
        let only = params.get("account").and_then(Value::as_str);
        let sessions: Vec<Value> = list(self.conn(), id)?
            .into_iter()
            .filter(|s| only.is_none_or(|a| a == s.account))
            .map(|s| {
                let now = SessionPtr::from_text(&s.pointer)
                    .filter(|_| s.engine == "claude")
                    .and_then(|p| hash_session(&p).ok().flatten());
                json!({
                    "account": s.account,
                    "engine": s.engine,
                    "session": s.session_id,
                    "pointer": s.pointer,
                    "cwd": s.cwd,
                    "hash": s.hash,
                    "size": s.size,
                    "last_read_at": s.last_read_at,
                    "held": s.engine == "claude",
                    "present": now.is_some(),
                    "changed": now.as_ref().map(|d| Some(&d.hash) != s.hash.as_ref()),
                })
            })
            .collect();
        Ok(json!({"conversation": id, "sessions": sessions}))
    }
}
