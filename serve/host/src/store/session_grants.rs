//! "This session" grants (designs/2026-11-02-grant-card): the owner allowed one action, as `kith allow --derive` names it,
//! for the rest of one conversation of an owner run. A row is `live` until it is `removed` (the owner) or `ended` (a new
//! Claude session in that conversation, 24 h, the agent's permission settings changed). Every move off `live` is a
//! compare-and-set, so a removal and the clock never both win.

use super::Store;
use crate::LockExt;
use anyhow::Result;
use rusqlite::{OptionalExtension, Row, params};

/// One row of `session_grants`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionGrant {
    pub id: String,
    pub agent: String,
    /// The run token's conversation (`RunIdentity::session`).
    pub run_session: String,
    /// The Claude session id inside it ('' when the run didn't say).
    pub claude_session: String,
    /// `rule` or `prose`, and the rule text, as `kith allow --derive` gave them.
    pub kind: String,
    pub rule: String,
    /// The agent's settings digest when it was granted.
    pub settings: String,
    pub card_id: String,
    pub entry_id: Option<String>,
    pub state: String,
    /// `removed`, `task`, `expired` or `settings` once it is not live.
    pub end_reason: Option<String>,
    pub created_at: i64,
    pub expires_at: i64,
    pub ended_at: Option<i64>,
    /// The tool entries it let through, as a JSON array of entry ids.
    pub tools: Vec<String>,
}

/// What a new grant is made of.
pub struct NewSessionGrant<'a> {
    pub agent: &'a str,
    pub run_session: &'a str,
    pub claude_session: &'a str,
    pub kind: &'a str,
    pub rule: &'a str,
    pub settings: &'a str,
    pub card_id: &'a str,
    pub entry_id: Option<&'a str>,
    pub expires_at: i64,
}

const COLS: &str = "id, agent, run_session, claude_session, kind, rule, settings, card_id, entry_id, state, end_reason, \
                    created_at, expires_at, ended_at, tools";

fn row(r: &Row) -> rusqlite::Result<SessionGrant> {
    let tools: String = r.get(14)?;
    Ok(SessionGrant {
        id: r.get(0)?,
        agent: r.get(1)?,
        run_session: r.get(2)?,
        claude_session: r.get(3)?,
        kind: r.get(4)?,
        rule: r.get(5)?,
        settings: r.get(6)?,
        card_id: r.get(7)?,
        entry_id: r.get(8)?,
        state: r.get(9)?,
        end_reason: r.get(10)?,
        created_at: r.get(11)?,
        expires_at: r.get(12)?,
        ended_at: r.get(13)?,
        tools: serde_json::from_str(&tools).unwrap_or_default(),
    })
}

impl Store {
    /// Saves a live grant. One card makes at most one grant (a second tap finds the first).
    pub fn session_grant_add(&self, n: &NewSessionGrant, now: i64) -> Result<SessionGrant> {
        let c = self.db.locked();
        let id = uuid::Uuid::new_v4().simple().to_string();
        c.execute(
            "INSERT INTO session_grants(id, agent, run_session, claude_session, kind, rule, settings, card_id, entry_id,
                state, created_at, expires_at, tools)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'live', ?10, ?11, '[]') ON CONFLICT(card_id) DO NOTHING",
            params![
                id,
                n.agent,
                n.run_session,
                n.claude_session,
                n.kind,
                n.rule,
                n.settings,
                n.card_id,
                n.entry_id,
                now,
                n.expires_at
            ],
        )?;
        Ok(c.query_row(&format!("SELECT {COLS} FROM session_grants WHERE card_id = ?1"), [n.card_id], row)?)
    }

    pub fn session_grant(&self, id: &str) -> Option<SessionGrant> {
        let c = self.db.locked();
        c.query_row(&format!("SELECT {COLS} FROM session_grants WHERE id = ?1"), [id], row).optional().ok().flatten()
    }

    /// The live grants of one agent's conversation (any Claude session in it).
    pub fn session_grants_live(&self, agent: &str, run_session: &str) -> Vec<SessionGrant> {
        let c = self.db.locked();
        let Ok(mut st) = c.prepare(&format!(
            "SELECT {COLS} FROM session_grants WHERE agent = ?1 AND run_session = ?2 AND state = 'live' ORDER BY created_at"
        )) else {
            return Vec::new();
        };
        st.query_map(params![agent, run_session], row).map(|rows| rows.flatten().collect()).unwrap_or_default()
    }

    /// Moves a live grant to `state` (`removed` or `ended`) for `reason`. None when it was not live (already gone).
    pub fn session_grant_end(&self, id: &str, state: &str, reason: &str, now: i64) -> Result<Option<SessionGrant>> {
        let c = self.db.locked();
        let n = c.execute(
            "UPDATE session_grants SET state = ?2, end_reason = ?3, ended_at = ?4 WHERE id = ?1 AND state = 'live'",
            params![id, state, reason, now],
        )?;
        if n == 0 {
            return Ok(None);
        }
        Ok(c.query_row(&format!("SELECT {COLS} FROM session_grants WHERE id = ?1"), [id], row).optional()?)
    }

    /// Live grants past their time end (`expired`). Returns them.
    pub fn session_grants_sweep(&self, now: i64) -> Result<Vec<SessionGrant>> {
        let ids: Vec<String> = {
            let c = self.db.locked();
            let mut st = c.prepare("SELECT id FROM session_grants WHERE state = 'live' AND expires_at <= ?1")?;
            st.query_map([now], |r| r.get(0))?.flatten().collect()
        };
        let mut out = Vec::new();
        for id in ids {
            if let Some(g) = self.session_grant_end(&id, "ended", "expired", now)? {
                out.push(g);
            }
        }
        Ok(out)
    }

    /// Notes a tool entry the grant let through (for its "this session" tag).
    pub fn session_grant_tag(&self, id: &str, entry_id: &str) -> Result<()> {
        let c = self.db.locked();
        let tools: Option<String> =
            c.query_row("SELECT tools FROM session_grants WHERE id = ?1", [id], |r| r.get(0)).optional()?;
        let mut list: Vec<String> = tools.and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        if !list.iter().any(|e| e == entry_id) {
            list.push(entry_id.to_owned());
            c.execute(
                "UPDATE session_grants SET tools = ?2 WHERE id = ?1",
                params![id, serde_json::to_string(&list)?],
            )?;
        }
        Ok(())
    }
}
