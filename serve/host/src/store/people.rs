//! Invited people: the invites this computer made, the people it made for them, the cloud memberships it
//! accepted, and the audit log of what non-owners asked. The truth about who may use which agent is
//! kith-auth's; the `agents` kept here mirror what this host set there, for the fast paths (event streams, lists).

use super::{Store, logged, now_ms};
use crate::LockExt;
use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use std::collections::BTreeMap;

/// An invite this computer made. The token itself is never stored, only its hash.
#[allow(clippy::struct_field_names, reason = "wire names")]
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Invite {
    pub invite_id: String,
    pub person_id: String,
    #[serde(skip)]
    pub token_hash: String,
    /// agent -> `member` | `guest`, as offered.
    pub agents: BTreeMap<String, String>,
    pub created_at: i64,
    pub expires_at: i64,
    pub used_at: Option<i64>,
    pub revoked_at: Option<i64>,
}

impl Invite {
    /// Whether an account may still accept it.
    pub fn open(&self, now: i64) -> bool {
        self.used_at.is_none() && self.revoked_at.is_none() && self.expires_at > now
    }
}

/// A person made for an invite.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PersonRow {
    pub person_id: String,
    pub name: String,
    /// agent -> `member` | `guest`, as this host set it in kith-auth.
    pub agents: BTreeMap<String, String>,
    /// hq project ids shared with the person.
    pub projects: Vec<String>,
    /// The agents whose kith-auth membership this host created for the person; only these are taken away again.
    #[serde(skip)]
    pub made: Vec<String>,
    pub created_at: i64,
    /// The owner allows this person's bots without asking each time.
    pub auto_bots: bool,
}

/// "Account `user_id` is a member of this computer", as the cloud recorded it and this host accepted it.
#[allow(clippy::struct_field_names, reason = "wire names")]
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Membership {
    pub membership_id: String,
    pub user_id: String,
    pub person_id: String,
    pub invite_id: String,
    pub created_at: i64,
}

/// One request a non-owner made.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AuditRow {
    pub id: i64,
    pub at: i64,
    pub person_id: String,
    pub device_key: String,
    pub method: String,
    /// The chat or agent the call named, when it named one.
    pub target: Option<String>,
    pub allowed: bool,
}

/// Audit rows older than this are dropped.
pub const AUDIT_KEEP_MS: i64 = 90 * 24 * 60 * 60 * 1000;
/// Rows one `audit` call returns at most.
pub const AUDIT_PAGE: i64 = 500;

fn row_invite(r: &rusqlite::Row) -> rusqlite::Result<Invite> {
    let agents: String = r.get(3)?;
    Ok(Invite {
        invite_id: r.get(0)?,
        person_id: r.get(1)?,
        token_hash: r.get(2)?,
        agents: serde_json::from_str(&agents).unwrap_or_default(),
        created_at: r.get(4)?,
        expires_at: r.get(5)?,
        used_at: r.get(6)?,
        revoked_at: r.get(7)?,
    })
}

const INVITE_COLS: &str = "invite_id, person_id, token_hash, agents, created_at, expires_at, used_at, revoked_at";

fn row_person(r: &rusqlite::Row) -> rusqlite::Result<PersonRow> {
    let (agents, projects, made): (String, String, String) = (r.get(2)?, r.get(3)?, r.get(4)?);
    Ok(PersonRow {
        person_id: r.get(0)?,
        name: r.get(1)?,
        agents: serde_json::from_str(&agents).unwrap_or_default(),
        projects: serde_json::from_str(&projects).unwrap_or_default(),
        made: serde_json::from_str(&made).unwrap_or_default(),
        created_at: r.get(5)?,
        auto_bots: r.get::<_, i64>(6)? != 0,
    })
}

fn row_membership(r: &rusqlite::Row) -> rusqlite::Result<Membership> {
    Ok(Membership {
        membership_id: r.get(0)?,
        user_id: r.get(1)?,
        person_id: r.get(2)?,
        invite_id: r.get(3)?,
        created_at: r.get(4)?,
    })
}

impl Store {
    pub fn put_invite(&self, i: &Invite) -> Result<()> {
        self.db.locked().execute(
            &format!("INSERT INTO invites({INVITE_COLS}) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"),
            params![
                i.invite_id,
                i.person_id,
                i.token_hash,
                serde_json::to_string(&i.agents)?,
                i.created_at,
                i.expires_at,
                i.used_at,
                i.revoked_at
            ],
        )?;
        Ok(())
    }

    pub fn invite(&self, id: &str) -> Option<Invite> {
        let c = self.db.locked();
        let row = c.query_row(&format!("SELECT {INVITE_COLS} FROM invites WHERE invite_id = ?1"), [id], row_invite);
        logged("invite", row.optional()).flatten()
    }

    /// Newest first.
    pub fn invites(&self) -> Result<Vec<Invite>> {
        let c = self.db.locked();
        let mut st = c.prepare(&format!("SELECT {INVITE_COLS} FROM invites ORDER BY created_at DESC"))?;
        Ok(st.query_map([], row_invite)?.collect::<rusqlite::Result<_>>()?)
    }

    /// Takes an invite that is still open for good; false when it was used, revoked or expired meanwhile.
    pub fn use_invite(&self, id: &str, now: i64) -> Result<bool> {
        let n = self.db.locked().execute(
            "UPDATE invites SET used_at = ?2 WHERE invite_id = ?1 AND used_at IS NULL AND revoked_at IS NULL AND expires_at > ?2",
            params![id, now],
        )?;
        Ok(n > 0)
    }

    /// Revokes an invite that was not used yet; false when there was nothing to revoke (a repeat is fine).
    pub fn revoke_invite(&self, id: &str) -> Result<bool> {
        let n = self.db.locked().execute(
            "UPDATE invites SET revoked_at = ?2 WHERE invite_id = ?1 AND used_at IS NULL AND revoked_at IS NULL",
            params![id, now_ms()],
        )?;
        Ok(n > 0)
    }

    /// Removes an invite that never left this computer (the cloud refused it).
    pub fn delete_invite(&self, id: &str) -> Result<()> {
        let c = self.db.locked();
        c.execute("DELETE FROM invites WHERE invite_id = ?1", [id])?;
        c.execute("DELETE FROM invite_rooms WHERE invite_id = ?1", [id])?;
        Ok(())
    }

    /// The rooms an invite names, each not done yet. Kept beside the invite, not in it, so the cloud's invite and a
    /// revoked person's suspended copy stay as they were.
    pub fn put_invite_rooms(&self, id: &str, rooms: &[String]) -> Result<()> {
        let c = self.db.locked();
        for room in rooms {
            c.execute("INSERT OR IGNORE INTO invite_rooms(invite_id, room) VALUES(?1, ?2)", params![id, room])?;
        }
        Ok(())
    }

    /// The room parts of invites that have any not done, as `(invite, room)`, oldest invite first. Old invites have none.
    pub fn invite_rooms_todo(&self) -> Vec<(String, String)> {
        let c = self.db.locked();
        let Ok(mut st) = c.prepare(
            "SELECT r.invite_id, r.room FROM invite_rooms r JOIN invites i ON i.invite_id = r.invite_id
             WHERE r.done_at IS NULL ORDER BY i.created_at, r.room",
        ) else {
            return vec![];
        };
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).map(|rows| rows.flatten().collect()).unwrap_or_default()
    }

    /// Whether `room` of invite `id` is done (joined, or the room was gone). A part that was never made is not done.
    pub fn invite_room_done(&self, id: &str, room: &str) -> bool {
        let c = self.db.locked();
        let row = c.query_row(
            "SELECT done_at IS NOT NULL FROM invite_rooms WHERE invite_id = ?1 AND room = ?2",
            params![id, room],
            |r| r.get::<_, bool>(0),
        );
        logged("invite room", row.optional()).flatten().unwrap_or(false)
    }

    /// Marks one room part done. Never undone: that mark is what keeps a removed person out.
    pub fn mark_invite_room_done(&self, id: &str, room: &str) -> Result<()> {
        self.db.locked().execute(
            "UPDATE invite_rooms SET done_at = ?3 WHERE invite_id = ?1 AND room = ?2 AND done_at IS NULL",
            params![id, room, now_ms()],
        )?;
        Ok(())
    }

    /// Every room part of an invite, done or not.
    pub fn invite_rooms(&self, id: &str) -> Vec<String> {
        let c = self.db.locked();
        let Ok(mut st) = c.prepare("SELECT room FROM invite_rooms WHERE invite_id = ?1 ORDER BY room") else {
            return vec![];
        };
        st.query_map([id], |r| r.get(0)).map(|rows| rows.flatten().collect()).unwrap_or_default()
    }

    pub fn person(&self, id: &str) -> Option<PersonRow> {
        let c = self.db.locked();
        let row = c.query_row(
            "SELECT person_id, name, agents, projects, made, created_at, auto_bots FROM people WHERE person_id = ?1",
            [id],
            row_person,
        );
        logged("person", row.optional()).flatten()
    }

    pub fn people(&self) -> Result<Vec<PersonRow>> {
        let c = self.db.locked();
        let mut st = c.prepare(
            "SELECT person_id, name, agents, projects, made, created_at, auto_bots FROM people ORDER BY created_at",
        )?;
        Ok(st.query_map([], row_person)?.collect::<rusqlite::Result<_>>()?)
    }

    /// A person made for an invite; a repeat keeps what is there.
    pub fn put_person(&self, id: &str, name: &str) -> Result<()> {
        self.db.locked().execute(
            "INSERT INTO people(person_id, name, created_at) VALUES(?1, ?2, ?3) ON CONFLICT(person_id) DO NOTHING",
            params![id, name, now_ms()],
        )?;
        Ok(())
    }

    pub fn delete_person(&self, id: &str) -> Result<()> {
        self.db.locked().execute("DELETE FROM people WHERE person_id = ?1", [id])?;
        Ok(())
    }

    pub fn set_person_agents(&self, id: &str, agents: &BTreeMap<String, String>) -> Result<()> {
        self.db.locked().execute(
            "UPDATE people SET agents = ?2 WHERE person_id = ?1",
            params![id, serde_json::to_string(agents)?],
        )?;
        Ok(())
    }

    pub fn set_person_made(&self, id: &str, made: &[String]) -> Result<()> {
        self.db
            .locked()
            .execute("UPDATE people SET made = ?2 WHERE person_id = ?1", params![id, serde_json::to_string(made)?])?;
        Ok(())
    }

    pub fn set_person_projects(&self, id: &str, projects: &[String]) -> Result<()> {
        self.db.locked().execute(
            "UPDATE people SET projects = ?2 WHERE person_id = ?1",
            params![id, serde_json::to_string(projects)?],
        )?;
        Ok(())
    }

    pub fn add_membership(&self, m: &Membership) -> Result<()> {
        self.db.locked().execute(
            "INSERT INTO memberships(membership_id, user_id, person_id, invite_id, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5) ON CONFLICT(membership_id) DO NOTHING",
            params![m.membership_id, m.user_id, m.person_id, m.invite_id, m.created_at],
        )?;
        Ok(())
    }

    pub fn memberships(&self) -> Result<Vec<Membership>> {
        let c = self.db.locked();
        let mut st = c.prepare(
            "SELECT membership_id, user_id, person_id, invite_id, created_at FROM memberships ORDER BY created_at",
        )?;
        Ok(st.query_map([], row_membership)?.collect::<rusqlite::Result<_>>()?)
    }

    /// The membership of the account `user_id` on this computer.
    pub fn membership_of_user(&self, user_id: &str) -> Option<Membership> {
        let c = self.db.locked();
        let row = c.query_row(
            "SELECT membership_id, user_id, person_id, invite_id, created_at FROM memberships WHERE user_id = ?1",
            [user_id],
            row_membership,
        );
        logged("membership", row.optional()).flatten()
    }

    pub fn remove_membership(&self, id: &str) -> Result<()> {
        self.db.locked().execute("DELETE FROM memberships WHERE membership_id = ?1", [id])?;
        Ok(())
    }

    pub fn audit_add(&self, person: &str, device_key: &str, method: &str, target: Option<&str>, allowed: bool) {
        let c = self.db.locked();
        let done = c.execute(
            "INSERT INTO audit(at, person_id, device_key, method, target, allowed) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![now_ms(), person, device_key, method, target, allowed],
        );
        // A full disk must not turn a refused call into an allowed one, so this only logs.
        if let Err(error) = done {
            tracing::warn!(%error, "couldn't write the audit log");
        }
    }

    /// Newest first, at most [`AUDIT_PAGE`].
    pub fn audit_list(&self, since: i64, person: Option<&str>) -> Result<Vec<AuditRow>> {
        let c = self.db.locked();
        let mut st = c.prepare(
            "SELECT id, at, person_id, device_key, method, target, allowed FROM audit
             WHERE at >= ?1 AND (?2 IS NULL OR person_id = ?2) ORDER BY id DESC LIMIT ?3",
        )?;
        let rows = st.query_map(params![since, person, AUDIT_PAGE], |r| {
            Ok(AuditRow {
                id: r.get(0)?,
                at: r.get(1)?,
                person_id: r.get(2)?,
                device_key: r.get(3)?,
                method: r.get(4)?,
                target: r.get(5)?,
                allowed: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Drops audit rows older than 90 days; returns how many went.
    pub fn audit_prune(&self, now: i64) -> Result<usize> {
        Ok(self.db.locked().execute("DELETE FROM audit WHERE at < ?1", [now - AUDIT_KEEP_MS])?)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::store::tests::temp_store;

    fn invite(id: &str, expires_at: i64) -> Invite {
        Invite {
            invite_id: id.into(),
            person_id: "sam".into(),
            token_hash: "h".into(),
            agents: BTreeMap::from([("house".to_owned(), "member".to_owned())]),
            created_at: 1,
            expires_at,
            used_at: None,
            revoked_at: None,
        }
    }

    #[test]
    fn an_invite_is_used_once_and_only_while_open() {
        let s = temp_store();
        s.put_invite(&invite("a", now_ms() + 10_000)).unwrap();
        s.put_invite(&invite("old", 5)).unwrap();
        assert!(s.invite("a").unwrap().open(now_ms()));
        assert!(!s.use_invite("old", now_ms()).unwrap(), "expired");
        assert!(s.use_invite("a", now_ms()).unwrap());
        assert!(!s.use_invite("a", now_ms()).unwrap(), "single use");
        assert!(!s.revoke_invite("a").unwrap(), "a used invite can't be revoked");
        s.put_invite(&invite("b", now_ms() + 10_000)).unwrap();
        assert!(s.revoke_invite("b").unwrap());
        assert!(!s.revoke_invite("b").unwrap(), "revoking twice is fine");
        assert!(!s.use_invite("b", now_ms()).unwrap(), "revoked");
    }

    #[test]
    fn audit_keeps_ninety_days() {
        let s = temp_store();
        s.audit_add("sam", "dk", "send", Some("bot1"), true);
        s.audit_add("kim", "dk2", "settings", None, false);
        assert_eq!(s.audit_list(0, None).unwrap().len(), 2);
        let sam = s.audit_list(0, Some("sam")).unwrap();
        assert_eq!((sam.len(), sam[0].target.as_deref(), sam[0].allowed), (1, Some("bot1"), true));
        assert_eq!(s.audit_prune(now_ms()).unwrap(), 0);
        assert_eq!(s.audit_prune(now_ms() + AUDIT_KEEP_MS + 10_000).unwrap(), 2);
    }

    #[test]
    fn people_and_memberships_round_trip() {
        let s = temp_store();
        s.put_person("sam", "Sam").unwrap();
        s.put_person("sam", "Other name").unwrap();
        s.set_person_agents("sam", &BTreeMap::from([("house".to_owned(), "guest".to_owned())])).unwrap();
        s.set_person_projects("sam", &["site".to_owned()]).unwrap();
        let p = s.person("sam").unwrap();
        assert_eq!(
            (p.name.as_str(), p.agents["house"].as_str(), p.projects.as_slice()),
            ("Sam", "guest", &["site".to_owned()][..])
        );
        let m = Membership {
            membership_id: "mem_1".into(),
            user_id: "user_1".into(),
            person_id: "sam".into(),
            invite_id: "inv_1".into(),
            created_at: 1,
        };
        s.add_membership(&m).unwrap();
        s.add_membership(&m).unwrap();
        assert_eq!(s.memberships().unwrap(), std::slice::from_ref(&m));
        assert_eq!(s.membership_of_user("user_1"), Some(m));
        s.remove_membership("mem_1").unwrap();
        assert!(s.membership_of_user("user_1").is_none());
    }
}
