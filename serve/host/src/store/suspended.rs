//! What a revoke keeps so undo gives it back (R1, R2): the revoked person's devices, memberships and open invites move to
//! `suspended_*` tables, where nothing reads them as access, and come back as they were. A removal deletes them as before.
//! Also the revokes of people known only by a chat handle (`handle_revokes`), which the owner's other computers take over
//! Mac link: one row per handle, the latest change wins.

use super::{Store, now_ms};
use crate::LockExt;
use anyhow::Result;
use rusqlite::params;

/// What [`Store::suspend_person`] or [`Store::restore_person`] moved.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Moved {
    pub devices: usize,
    pub memberships: usize,
    pub invites: usize,
}

/// One handle-only person's revoke or undo, and when it was made (ms).
pub use kith_wire::rpc::HandleRevoke;

const DEVICE_COLS: &str =
    "key, name, platform, source, grant_id, scopes, lease_until, created_at, last_seen_at, person_id, proven";
const MEMBERSHIP_COLS: &str = "membership_id, user_id, person_id, invite_id, created_at";
const INVITE_COLS: &str = "invite_id, person_id, token_hash, agents, created_at, expires_at, used_at, revoked_at";

impl Store {
    /// A revoke: the person's devices (with their notification tickets, which phones register again), memberships and
    /// still-open invites leave the tables that grant access and wait in `suspended_*`. Doing it twice moves nothing more.
    pub fn suspend_person(&self, person: &str) -> Result<Moved> {
        let mut c = self.db.locked();
        let tx = c.transaction()?;
        let now = now_ms();
        let devices = tx.execute(
            &format!(
                "INSERT OR REPLACE INTO suspended_devices({DEVICE_COLS}) SELECT {DEVICE_COLS} FROM devices WHERE person_id = ?1"
            ),
            [person],
        )?;
        for table in ["push_tickets", "activity_tickets"] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE device_key IN (SELECT key FROM devices WHERE person_id = ?1)"),
                [person],
            )?;
        }
        tx.execute("DELETE FROM devices WHERE person_id = ?1", [person])?;
        let memberships = tx.execute(
            &format!(
                "INSERT OR REPLACE INTO suspended_memberships({MEMBERSHIP_COLS}) SELECT {MEMBERSHIP_COLS} FROM memberships WHERE person_id = ?1"
            ),
            [person],
        )?;
        tx.execute("DELETE FROM memberships WHERE person_id = ?1", [person])?;
        // An open invite cannot be accepted meanwhile: it is not in `invites`, which `proveInvite` and the cloud sync read.
        let invites = tx.execute(
            &format!(
                "INSERT OR REPLACE INTO suspended_invites({INVITE_COLS}) SELECT {INVITE_COLS} FROM invites
                 WHERE person_id = ?1 AND used_at IS NULL AND revoked_at IS NULL AND expires_at > ?2"
            ),
            params![person, now],
        )?;
        tx.execute(
            "DELETE FROM invites WHERE person_id = ?1 AND used_at IS NULL AND revoked_at IS NULL AND expires_at > ?2",
            params![person, now],
        )?;
        tx.commit()?;
        Ok(Moved { devices, memberships, invites })
    }

    /// Undo: what [`Store::suspend_person`] kept comes back as it was. An invite that has expired since stays gone; a
    /// membership whose account joined under another invite meanwhile is not put back twice.
    pub fn restore_person(&self, person: &str) -> Result<Moved> {
        let mut c = self.db.locked();
        let tx = c.transaction()?;
        let now = now_ms();
        let devices = tx.execute(
            &format!(
                "INSERT OR IGNORE INTO devices({DEVICE_COLS}) SELECT {DEVICE_COLS} FROM suspended_devices WHERE person_id = ?1"
            ),
            [person],
        )?;
        tx.execute("DELETE FROM suspended_devices WHERE person_id = ?1", [person])?;
        let memberships = tx.execute(
            &format!(
                "INSERT OR IGNORE INTO memberships({MEMBERSHIP_COLS}) SELECT {MEMBERSHIP_COLS} FROM suspended_memberships
                 WHERE person_id = ?1 AND user_id NOT IN (SELECT user_id FROM memberships)"
            ),
            [person],
        )?;
        tx.execute("DELETE FROM suspended_memberships WHERE person_id = ?1", [person])?;
        let invites = tx.execute(
            &format!(
                "INSERT OR IGNORE INTO invites({INVITE_COLS}) SELECT {INVITE_COLS} FROM suspended_invites
                 WHERE person_id = ?1 AND expires_at > ?2"
            ),
            params![person, now],
        )?;
        tx.execute("DELETE FROM suspended_invites WHERE person_id = ?1", [person])?;
        tx.commit()?;
        Ok(Moved { devices, memberships, invites })
    }

    /// Removal: what a revoke kept goes for good.
    pub fn forget_suspended(&self, person: &str) -> Result<()> {
        let c = self.db.locked();
        for table in ["suspended_devices", "suspended_memberships", "suspended_invites"] {
            c.execute(&format!("DELETE FROM {table} WHERE person_id = ?1"), [person])?;
        }
        Ok(())
    }

    /// What is kept for `person`, as counts.
    pub fn suspended_of(&self, person: &str) -> Result<Moved> {
        let c = self.db.locked();
        let n = |table: &str| -> rusqlite::Result<usize> {
            c.query_row(&format!("SELECT COUNT(*) FROM {table} WHERE person_id = ?1"), [person], |r| r.get::<_, i64>(0).map(|n| usize::try_from(n).unwrap_or(0)))
        };
        Ok(Moved {
            devices: n("suspended_devices")?,
            memberships: n("suspended_memberships")?,
            invites: n("suspended_invites")?,
        })
    }

    /// Every handle-only revoke or undo this computer knows, for the owner's other computers.
    pub fn handle_revokes(&self) -> Result<Vec<HandleRevoke>> {
        let c = self.db.locked();
        let mut st = c.prepare("SELECT handle, revoked, at FROM handle_revokes ORDER BY handle")?;
        Ok(st
            .query_map([], |r| Ok(HandleRevoke { handle: r.get(0)?, revoked: r.get::<_, i64>(1)? != 0, at: r.get(2)? }))?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Writes one handle's state when `at` is later than what is there (a repeat or an older change does nothing);
    /// whether it was written.
    pub fn put_handle_revoke(&self, h: &HandleRevoke) -> Result<bool> {
        let n = self.db.locked().execute(
            "INSERT INTO handle_revokes(handle, revoked, at) VALUES(?1, ?2, ?3)
             ON CONFLICT(handle) DO UPDATE SET revoked = ?2, at = ?3 WHERE excluded.at > handle_revokes.at",
            params![h.handle, h.revoked, h.at],
        )?;
        Ok(n > 0)
    }

    /// When `handle` last changed here, if ever.
    pub fn handle_revoke_at(&self, handle: &str) -> Option<i64> {
        self.db.locked().query_row("SELECT at FROM handle_revokes WHERE handle = ?1", [handle], |r| r.get(0)).ok()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::store::tests::temp_store;
    use crate::store::{Device, DeviceSource, Invite, Membership, Scope};
    use std::collections::BTreeMap;

    fn device(key: &str, person: &str) -> Device {
        Device {
            key: key.into(),
            name: "Phone".into(),
            platform: "ios".into(),
            source: DeviceSource::Local,
            grant_id: None,
            scopes: vec![Scope::Control],
            lease_until: None,
            created_at: 1,
            last_seen_at: None,
            person_id: Some(person.into()),
        }
    }

    fn invite(id: &str, person: &str, expires_at: i64) -> Invite {
        Invite {
            invite_id: id.into(),
            person_id: person.into(),
            token_hash: "h".into(),
            agents: BTreeMap::from([("house".to_owned(), "member".to_owned())]),
            created_at: 1,
            expires_at,
            used_at: None,
            revoked_at: None,
        }
    }

    fn member(id: &str, user: &str, person: &str) -> Membership {
        Membership {
            membership_id: id.into(),
            user_id: user.into(),
            person_id: person.into(),
            invite_id: "i-used".into(),
            created_at: 2,
        }
    }

    #[test]
    fn a_revoke_keeps_devices_memberships_and_open_invites_and_undo_gives_them_back() {
        let s = temp_store();
        s.put_device(&device("d1", "sam")).unwrap();
        s.put_device(&device("d2", "sam")).unwrap();
        s.put_device(&device("other", "mia")).unwrap();
        s.add_membership(&member("m1", "user_a", "sam")).unwrap();
        s.put_invite(&invite("open", "sam", now_ms() + 100_000)).unwrap();
        s.put_invite(&invite("old", "sam", 5)).unwrap();
        let gone = s.suspend_person("sam").unwrap();
        assert_eq!(gone, Moved { devices: 2, memberships: 1, invites: 1 });
        assert!(s.devices_of("sam").unwrap().is_empty() && s.memberships().unwrap().is_empty());
        assert!(s.invite("open").is_none(), "an open invite cannot be accepted meanwhile");
        assert_eq!(s.devices_of("mia").unwrap().len(), 1, "nobody else is touched");
        assert_eq!(s.suspend_person("sam").unwrap(), Moved::default(), "a repeat moves nothing");
        assert_eq!(s.suspended_of("sam").unwrap(), gone);
        assert_eq!(s.restore_person("sam").unwrap(), gone);
        assert_eq!(s.devices_of("sam").unwrap().len(), 2);
        assert_eq!(s.membership_of_user("user_a").unwrap().membership_id, "m1");
        assert!(s.invite("open").unwrap().open(now_ms()));
        assert_eq!(s.restore_person("sam").unwrap(), Moved::default(), "undo twice is fine");
        assert_eq!(s.suspended_of("sam").unwrap(), Moved::default());
    }

    #[test]
    fn an_invite_that_expired_while_revoked_is_not_given_back_and_removal_forgets_everything() {
        let s = temp_store();
        s.put_invite(&invite("soon", "sam", now_ms() + 50)).unwrap();
        s.put_device(&device("d1", "sam")).unwrap();
        s.suspend_person("sam").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(80));
        assert_eq!(s.restore_person("sam").unwrap(), Moved { devices: 1, memberships: 0, invites: 0 });
        s.suspend_person("sam").unwrap();
        s.forget_suspended("sam").unwrap();
        assert_eq!(s.restore_person("sam").unwrap(), Moved::default());
    }

    #[test]
    fn the_latest_change_of_a_handle_wins() {
        let s = temp_store();
        let rev = |revoked, at| HandleRevoke { handle: "telegram:7".into(), revoked, at };
        assert!(s.put_handle_revoke(&rev(true, 10)).unwrap());
        assert!(!s.put_handle_revoke(&rev(false, 5)).unwrap(), "an older change is ignored");
        assert!(!s.put_handle_revoke(&rev(false, 10)).unwrap(), "so is one at the same moment");
        assert!(s.put_handle_revoke(&rev(false, 11)).unwrap());
        assert_eq!(s.handle_revokes().unwrap(), vec![rev(false, 11)]);
    }
}
