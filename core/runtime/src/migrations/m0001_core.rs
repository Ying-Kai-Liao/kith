//! Version 1: the foundation M0a shipped. Never edit: a database at version 1 exists on real machines.

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 1,
    name: "core",
    sql: "
        CREATE TABLE migrations (
            version INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            applied_at INTEGER NOT NULL
        );
        CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;
        CREATE TABLE counters (name TEXT PRIMARY KEY, value INTEGER NOT NULL) WITHOUT ROWID;
        INSERT INTO counters (name, value) VALUES ('generation', 0), ('claim_id', 0);
    ",
};
