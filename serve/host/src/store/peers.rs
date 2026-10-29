//! Mac link (designs/2026-10-29-mac-link): the owner's other computers this one lets in (`peers_in`) and the ones it
//! may ask (`peers_out`). Each row is pinned to the keys of its code exchange; a changed key is a new pairing.

use super::{Store, logged, now_ms};
use crate::LockExt;
use anyhow::Result;
use rusqlite::{OptionalExtension, params};

/// A computer allowed in here: its device key reaches the peer methods only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerIn {
    pub computer_id: String,
    pub name: String,
    pub device_key: String,
    /// The pairing id from the code exchange; an ask from another epoch is refused.
    pub epoch: String,
    pub grant_id: String,
    pub paired_at: i64,
}

/// A computer this one asked to pair with (pending) or may ask (paired).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerOut {
    pub computer_id: String,
    pub name: String,
    /// Its sign key as the cloud listed it when the pairing started; the code was computed with it.
    pub sign_key: String,
    /// This computer's own key for that peer (the private half is in the vault).
    pub device_key: String,
    pub request_id: Option<String>,
    pub epoch: Option<String>,
    pub grant_id: Option<String>,
    /// Paired once a call over the pinned channel succeeded; until then pending.
    pub paired: bool,
    pub created_at: i64,
}

const IN_COLS: &str = "computer_id, name, device_key, epoch, grant_id, paired_at";
const OUT_COLS: &str = "computer_id, name, sign_key, device_key, request_id, epoch, grant_id, state, created_at";

fn row_in(r: &rusqlite::Row) -> rusqlite::Result<PeerIn> {
    Ok(PeerIn {
        computer_id: r.get(0)?,
        name: r.get(1)?,
        device_key: r.get(2)?,
        epoch: r.get(3)?,
        grant_id: r.get(4)?,
        paired_at: r.get(5)?,
    })
}

fn row_out(r: &rusqlite::Row) -> rusqlite::Result<PeerOut> {
    Ok(PeerOut {
        computer_id: r.get(0)?,
        name: r.get(1)?,
        sign_key: r.get(2)?,
        device_key: r.get(3)?,
        request_id: r.get(4)?,
        epoch: r.get(5)?,
        grant_id: r.get(6)?,
        paired: r.get::<_, String>(7)? == "paired",
        created_at: r.get(8)?,
    })
}

impl Store {
    pub fn peer_in_by_key(&self, device_key: &str) -> Option<PeerIn> {
        let c = self.db.locked();
        let row = c.query_row(&format!("SELECT {IN_COLS} FROM peers_in WHERE device_key = ?1"), [device_key], row_in);
        logged("peer in", row.optional()).flatten()
    }

    pub fn peer_in(&self, computer_id: &str) -> Option<PeerIn> {
        let c = self.db.locked();
        let row = c.query_row(&format!("SELECT {IN_COLS} FROM peers_in WHERE computer_id = ?1"), [computer_id], row_in);
        logged("peer in", row.optional()).flatten()
    }

    pub fn peers_in(&self) -> Result<Vec<PeerIn>> {
        let c = self.db.locked();
        let mut st = c.prepare(&format!("SELECT {IN_COLS} FROM peers_in ORDER BY paired_at"))?;
        Ok(st.query_map([], row_in)?.collect::<rusqlite::Result<_>>()?)
    }

    /// Lets a computer in, replacing an older pairing of the same computer (its key and epoch go with it).
    pub fn put_peer_in(&self, p: &PeerIn) -> Result<()> {
        let mut c = self.db.locked();
        let tx = c.transaction()?;
        tx.execute("DELETE FROM peers_in WHERE computer_id = ?1", [&p.computer_id])?;
        tx.execute(
            &format!("INSERT INTO peers_in({IN_COLS}) VALUES(?1, ?2, ?3, ?4, ?5, ?6)"),
            params![p.computer_id, p.name, p.device_key, p.epoch, p.grant_id, p.paired_at],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// False when it wasn't there.
    pub fn remove_peer_in(&self, computer_id: &str) -> Result<bool> {
        Ok(self.db.locked().execute("DELETE FROM peers_in WHERE computer_id = ?1", [computer_id])? > 0)
    }

    pub fn peer_out(&self, computer_id: &str) -> Option<PeerOut> {
        let c = self.db.locked();
        let row =
            c.query_row(&format!("SELECT {OUT_COLS} FROM peers_out WHERE computer_id = ?1"), [computer_id], row_out);
        logged("peer out", row.optional()).flatten()
    }

    pub fn peers_out(&self) -> Result<Vec<PeerOut>> {
        let c = self.db.locked();
        let mut st = c.prepare(&format!("SELECT {OUT_COLS} FROM peers_out ORDER BY created_at"))?;
        Ok(st.query_map([], row_out)?.collect::<rusqlite::Result<_>>()?)
    }

    /// Starts a pairing with a computer: pending, with no epoch or grant yet. A pairing already there is replaced.
    pub fn start_peer_out(&self, computer_id: &str, name: &str, sign_key: &str, device_key: &str) -> Result<()> {
        self.db.locked().execute(
            &format!(
                "INSERT INTO peers_out({OUT_COLS}) VALUES(?1, ?2, ?3, ?4, NULL, NULL, NULL, 'pending', ?5)
                 ON CONFLICT(computer_id) DO UPDATE SET name = ?2, sign_key = ?3, device_key = ?4, request_id = NULL,
                   epoch = NULL, grant_id = NULL, state = 'pending', created_at = ?5"
            ),
            params![computer_id, name, sign_key, device_key, now_ms()],
        )?;
        Ok(())
    }

    pub fn set_peer_out_request(&self, computer_id: &str, request_id: &str) -> Result<()> {
        self.db
            .locked()
            .execute("UPDATE peers_out SET request_id = ?2 WHERE computer_id = ?1", params![computer_id, request_id])?;
        Ok(())
    }

    /// The other computer said yes: its grant and the epoch. Still pending until a pinned call succeeds.
    pub fn set_peer_out_granted(&self, computer_id: &str, grant_id: &str, epoch: &str) -> Result<()> {
        self.db.locked().execute(
            "UPDATE peers_out SET grant_id = ?2, epoch = ?3 WHERE computer_id = ?1 AND state = 'pending'",
            params![computer_id, grant_id, epoch],
        )?;
        Ok(())
    }

    /// Pending → paired, only for the grant the pinned call was made under. False when that pairing is gone.
    pub fn set_peer_out_paired(&self, computer_id: &str, grant_id: &str) -> Result<bool> {
        Ok(self.db.locked().execute(
            "UPDATE peers_out SET state = 'paired' WHERE computer_id = ?1 AND grant_id = ?2",
            params![computer_id, grant_id],
        )? > 0)
    }

    pub fn remove_peer_out(&self, computer_id: &str) -> Result<bool> {
        Ok(self.db.locked().execute("DELETE FROM peers_out WHERE computer_id = ?1", [computer_id])? > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::temp_store;
    use super::*;

    fn peer(cid: &str, dk: &str, epoch: &str) -> PeerIn {
        PeerIn {
            computer_id: cid.into(),
            name: "laptop".into(),
            device_key: dk.into(),
            epoch: epoch.into(),
            grant_id: "pgr_1".into(),
            paired_at: 1,
        }
    }

    #[test]
    fn a_new_pairing_of_a_computer_replaces_its_key_and_epoch() {
        let s = temp_store();
        s.put_peer_in(&peer("c1", "k1", "e1")).unwrap();
        s.put_peer_in(&peer("c1", "k2", "e2")).unwrap();
        assert_eq!(s.peer_in_by_key("k1"), None, "the old key is no one's");
        assert_eq!(s.peer_in_by_key("k2").unwrap().epoch, "e2");
        assert!(s.remove_peer_in("c1").unwrap());
        assert_eq!(s.peers_in().unwrap(), Vec::<PeerIn>::new());
    }

    #[test]
    fn a_key_belongs_to_one_computer() {
        let s = temp_store();
        s.put_peer_in(&peer("c1", "k1", "e1")).unwrap();
        assert!(s.put_peer_in(&peer("c2", "k1", "e2")).is_err());
        assert_eq!(s.peer_in_by_key("k1").unwrap().computer_id, "c1");
    }

    #[test]
    fn an_outgoing_pairing_is_paired_only_under_its_own_grant() {
        let s = temp_store();
        s.start_peer_out("c1", "mini", "sk", "dk").unwrap();
        s.set_peer_out_granted("c1", "pgr_1", "e1").unwrap();
        assert!(!s.set_peer_out_paired("c1", "pgr_0").unwrap());
        assert!(!s.peer_out("c1").unwrap().paired);
        assert!(s.set_peer_out_paired("c1", "pgr_1").unwrap());
        let p = s.peer_out("c1").unwrap();
        assert!(p.paired && p.epoch.as_deref() == Some("e1"));
        // Starting again forgets the old grant and epoch.
        s.start_peer_out("c1", "mini", "sk", "dk2").unwrap();
        let p = s.peer_out("c1").unwrap();
        assert!(!p.paired && p.grant_id.is_none() && p.epoch.is_none());
    }
}
