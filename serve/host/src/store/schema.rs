//! Opening the database: tables, pragmas and schema migrations.

use super::Store;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use std::path::Path;
use std::sync::Mutex;

/// Current schema version (kv `schema`).
const SCHEMA: i64 = 8;

/// 3: authorized devices (keys, not push tickets) with their push and Live Activity
/// tickets. The old ticket-only `devices` table is dropped; phones re-register on connect.
/// 4: threads (`entries.thread_id`); instruction snapshots are kept per session, so the
/// per-bot ones go.
/// 5: invitee people: `devices.person_id` (NULL is the owner), invites, the memberships this host
/// accepted, the people it made and the audit log of what non-owners asked.
/// 6: bot guests (docs/kith-bot-guests.md 4.3): the bots, their invites and conversations, and `people.auto_bots`.
/// 7: approval cards (host card store): what a run asked for beyond its limits, and what became of it.
/// 8: a card asked from a `kith ask` run answers that ask (`cards.reply_to`, `cards.asked_by`).
fn migrate(c: &Connection) -> Result<()> {
    let version: i64 =
        c.query_row("SELECT CAST(v AS INTEGER) FROM kv WHERE k = 'schema'", [], |r| r.get(0)).optional()?.unwrap_or(0);
    if version >= SCHEMA {
        return Ok(());
    }
    if version < 3 {
        migrate_3(c)?;
    }
    if version < 4 {
        migrate_4(c)?;
    }
    if version < 5 {
        migrate_5(c)?;
    }
    if version < 6 {
        migrate_6(c)?;
    }
    if version < 7 {
        migrate_7(c)?;
    }
    migrate_8(c)
}

fn migrate_8(c: &Connection) -> Result<()> {
    c.execute_batch(
        "BEGIN;
         ALTER TABLE cards ADD COLUMN reply_to TEXT;
         ALTER TABLE cards ADD COLUMN asked_by TEXT;
         INSERT INTO kv(k, v) VALUES('schema', '8') ON CONFLICT(k) DO UPDATE SET v = '8';
         COMMIT;",
    )?;
    Ok(())
}

fn migrate_7(c: &Connection) -> Result<()> {
    c.execute_batch(
        "BEGIN;
         CREATE TABLE IF NOT EXISTS cards(
            id TEXT PRIMARY KEY, session TEXT NOT NULL, source TEXT NOT NULL CHECK (source IN ('acp','run')),
            bot_id TEXT, run_id TEXT, entry_id TEXT, person TEXT NOT NULL DEFAULT '', agent TEXT NOT NULL DEFAULT '',
            approver TEXT NOT NULL CHECK (approver IN ('owner','member')), tool TEXT NOT NULL, input TEXT NOT NULL,
            resource TEXT NOT NULL, shown TEXT NOT NULL DEFAULT '', scope TEXT NOT NULL DEFAULT 'once',
            state TEXT NOT NULL CHECK (state IN ('pending','live','parked','approved','denied','expired','withdrawn','used')),
            created_at INTEGER NOT NULL, deadline INTEGER NOT NULL, answer TEXT, allowed INTEGER, answered_by TEXT,
            answered_at INTEGER, answered_parked INTEGER NOT NULL DEFAULT 0, granted_at INTEGER, resumed_at INTEGER,
            used_by_ask INTEGER NOT NULL DEFAULT 0);
         CREATE UNIQUE INDEX IF NOT EXISTS cards_open ON cards(session, resource)
            WHERE state IN ('pending','live','parked');
         CREATE INDEX IF NOT EXISTS cards_entry ON cards(entry_id);
         CREATE INDEX IF NOT EXISTS cards_person ON cards(person, state);
         INSERT INTO kv(k, v) VALUES('schema', '7') ON CONFLICT(k) DO UPDATE SET v = '7';
         COMMIT;",
    )?;
    Ok(())
}

fn migrate_6(c: &Connection) -> Result<()> {
    c.execute_batch(
        "BEGIN;
         CREATE TABLE IF NOT EXISTS bot_guests(
            bot_id TEXT PRIMARY KEY, person_id TEXT NOT NULL UNIQUE, owner_person TEXT, name TEXT NOT NULL,
            provider TEXT NOT NULL DEFAULT 'other', agents TEXT NOT NULL,
            state TEXT NOT NULL CHECK (state IN ('invited','joining','pending','active','denied','revoked')),
            device_key TEXT, cloud_device TEXT, rate_per_hour INTEGER NOT NULL DEFAULT 60,
            concurrent INTEGER NOT NULL DEFAULT 2, created_at INTEGER NOT NULL, joined_at INTEGER, decided_at INTEGER,
            decided_by TEXT, last_seen_at INTEGER, revoked_at INTEGER, revoked_why TEXT);
         CREATE INDEX IF NOT EXISTS bot_guests_owner ON bot_guests(owner_person);
         CREATE UNIQUE INDEX IF NOT EXISTS bot_guests_device ON bot_guests(device_key)
            WHERE device_key IS NOT NULL AND revoked_at IS NULL;
         CREATE TABLE IF NOT EXISTS bot_invites(
            invite_id TEXT PRIMARY KEY, bot_id TEXT NOT NULL REFERENCES bot_guests(bot_id), token_hash TEXT NOT NULL,
            created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL, used_at INTEGER, revoked_at INTEGER);
         CREATE TABLE IF NOT EXISTS bot_conversations(
            conversation_id TEXT PRIMARY KEY, bot_id TEXT NOT NULL REFERENCES bot_guests(bot_id),
            agent TEXT NOT NULL, chat_bot_id TEXT NOT NULL, thread_root TEXT NOT NULL, external_ref TEXT,
            room_id TEXT, speaker TEXT, origin_label TEXT NOT NULL, created_at INTEGER NOT NULL,
            last_at INTEGER NOT NULL);
         CREATE UNIQUE INDEX IF NOT EXISTS bot_conv_ref ON bot_conversations(bot_id, external_ref)
            WHERE external_ref IS NOT NULL;
         CREATE INDEX IF NOT EXISTS bot_conv_bot ON bot_conversations(bot_id, last_at);
         CREATE INDEX IF NOT EXISTS bot_conv_thread ON bot_conversations(chat_bot_id, thread_root);
         ALTER TABLE people ADD COLUMN auto_bots INTEGER NOT NULL DEFAULT 0;
         INSERT INTO kv(k, v) VALUES('schema', '6') ON CONFLICT(k) DO UPDATE SET v = '6';
         COMMIT;",
    )?;
    Ok(())
}

fn migrate_5(c: &Connection) -> Result<()> {
    c.execute_batch(
        "BEGIN;
         ALTER TABLE devices ADD COLUMN person_id TEXT;
         ALTER TABLE devices ADD COLUMN proven INTEGER NOT NULL DEFAULT 0;
         CREATE TABLE IF NOT EXISTS invites(
            invite_id TEXT PRIMARY KEY, person_id TEXT NOT NULL, token_hash TEXT NOT NULL,
            agents TEXT NOT NULL, created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL,
            used_at INTEGER, revoked_at INTEGER);
         CREATE TABLE IF NOT EXISTS people(
            person_id TEXT PRIMARY KEY, name TEXT NOT NULL, agents TEXT NOT NULL DEFAULT '{}',
            projects TEXT NOT NULL DEFAULT '[]', made TEXT NOT NULL DEFAULT '[]', created_at INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS memberships(
            membership_id TEXT PRIMARY KEY, user_id TEXT NOT NULL, person_id TEXT NOT NULL,
            invite_id TEXT NOT NULL, created_at INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS audit(
            id INTEGER PRIMARY KEY AUTOINCREMENT, at INTEGER NOT NULL, person_id TEXT NOT NULL,
            device_key TEXT NOT NULL, method TEXT NOT NULL, target TEXT, allowed INTEGER NOT NULL);
         CREATE INDEX IF NOT EXISTS audit_at ON audit(at);
         INSERT INTO kv(k, v) VALUES('schema', '5') ON CONFLICT(k) DO UPDATE SET v = '5';
         COMMIT;",
    )?;
    Ok(())
}

fn migrate_4(c: &Connection) -> Result<()> {
    c.execute_batch(
        "BEGIN;
         ALTER TABLE entries ADD COLUMN thread_id TEXT;
         CREATE INDEX IF NOT EXISTS entries_thread ON entries(bot_id, thread_id, seq);
         DELETE FROM kv WHERE k GLOB 'context.*';
         INSERT INTO kv(k, v) VALUES('schema', '4') ON CONFLICT(k) DO UPDATE SET v = '4';
         COMMIT;",
    )?;
    Ok(())
}

fn migrate_3(c: &Connection) -> Result<()> {
    c.execute_batch(
        "BEGIN;
         DROP TABLE IF EXISTS devices;
         CREATE TABLE devices(
            key TEXT PRIMARY KEY, name TEXT NOT NULL, platform TEXT NOT NULL,
            source TEXT NOT NULL, grant_id TEXT, scopes TEXT NOT NULL,
            lease_until INTEGER, created_at INTEGER NOT NULL, last_seen_at INTEGER);
         CREATE TABLE IF NOT EXISTS push_tickets(
            ticket TEXT PRIMARY KEY, device_key TEXT NOT NULL, push_key TEXT, ctx TEXT, name TEXT,
            created_at INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS activity_tickets(
            ticket TEXT PRIMARY KEY, bot_id TEXT NOT NULL, device_key TEXT NOT NULL, created_at INTEGER NOT NULL);
         INSERT INTO kv(k, v) VALUES('schema', '3') ON CONFLICT(k) DO UPDATE SET v = '3';
         COMMIT;",
    )?;
    Ok(())
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let c = Connection::open(path)?;
        c.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA secure_delete=ON;
             CREATE TABLE IF NOT EXISTS kv(k TEXT PRIMARY KEY, v TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS bots(
                id TEXT PRIMARY KEY, rev INTEGER NOT NULL, deleted INTEGER NOT NULL DEFAULT 0,
                config TEXT NOT NULL, session_id TEXT, read_rev INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE IF NOT EXISTS entries(
                seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT UNIQUE NOT NULL, bot_id TEXT NOT NULL,
                rev INTEGER NOT NULL, kind TEXT NOT NULL, turn INTEGER NOT NULL, data TEXT NOT NULL,
                created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
             CREATE INDEX IF NOT EXISTS entries_rev ON entries(rev);
             CREATE INDEX IF NOT EXISTS entries_bot ON entries(bot_id, seq);",
        )?;
        migrate(&c)?;
        kith_mem::schema::migrate(&c)?;
        let secret_key = if path == Path::new(":memory:") {
            let mut key = vec![0; 32];
            getrandom::fill(&mut key).map_err(|_| anyhow::anyhow!("random key unavailable"))?;
            Some(zeroize::Zeroizing::new(key))
        } else {
            None
        };
        Ok(Self { db: Mutex::new(c), connector_lock: Mutex::new(()), secret_key: Mutex::new(secret_key) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_five_adds_people_and_keeps_existing_devices_as_the_owners() {
        let dir = std::env::temp_dir().join(format!("kith-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("t.db");
        {
            let s = Store::open(&db).unwrap();
            drop(s);
            let c = Connection::open(&db).unwrap();
            // Back to a version-4 database: no person column, no people tables.
            c.execute_batch(
                "DROP TABLE invites; DROP TABLE people; DROP TABLE memberships; DROP TABLE audit;
                 ALTER TABLE devices DROP COLUMN proven; ALTER TABLE devices DROP COLUMN person_id;
                 ALTER TABLE cards DROP COLUMN reply_to; ALTER TABLE cards DROP COLUMN asked_by;
                 INSERT INTO devices VALUES('old', 'Phone', 'ios', 'local', NULL, '[\"control\"]', NULL, 1, NULL);
                 UPDATE kv SET v = '4' WHERE k = 'schema';",
            )
            .unwrap();
        }
        let s = Store::open(&db).unwrap();
        assert_eq!(s.kv_get("schema").as_deref(), Some("8"));
        assert_eq!(s.device("old").unwrap().person_id, None, "an old device is the owner's");
        assert!(s.invites().unwrap().is_empty() && s.people().unwrap().is_empty());
    }

    #[test]
    fn migrations_replace_the_old_ticket_table_and_add_threads() {
        let dir = std::env::temp_dir().join(format!("kith-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("t.db");
        {
            let c = Connection::open(&db).unwrap();
            c.execute_batch(
                "CREATE TABLE kv(k TEXT PRIMARY KEY, v TEXT NOT NULL);
                 CREATE TABLE devices(ticket TEXT PRIMARY KEY, name TEXT, created_at INTEGER NOT NULL);
                 INSERT INTO devices VALUES('old', 'iPhone', 1);",
            )
            .unwrap();
        }
        let s = Store::open(&db).unwrap();
        assert!(s.devices().unwrap().is_empty());
        assert_eq!(s.kv_get("schema").as_deref(), Some("8"));
        assert!(s.thread("b", "root", 10).unwrap().is_empty(), "entries have a thread column");
        drop(s);
        let s = Store::open(&db).unwrap();
        assert!(s.devices().unwrap().is_empty(), "reopening keeps the new table");
    }
}
