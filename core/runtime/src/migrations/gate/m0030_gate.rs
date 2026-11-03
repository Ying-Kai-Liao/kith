//! Version 30: the resource gate's state that outlives a daemon restart.
//!
//! The host's gate (serve/host/src/agent/gate/mod.rs) keeps all of this in memory, because a host restart ends every
//! turn it counted. The core's holders can be processes that outlive the daemon (a run is a child of the holder, not of
//! the daemon), so the rows stay and are checked against the process on the next call: a holder whose pid is gone, or
//! now belongs to another process (start time differs), is released then.
//!
//! No SQL foreign key to another lane's table: `holder` is a run or claim id from code, never a link.

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 30,
    name: "gate",
    sql: "
        CREATE TABLE gate_config (
            key TEXT PRIMARY KEY,
            value INTEGER NOT NULL
        ) WITHOUT ROWID;
        CREATE TABLE gate_slots (
            holder TEXT PRIMARY KEY,
            rt TEXT NOT NULL,
            invitee INTEGER NOT NULL CHECK (invitee IN (0, 1)),
            state TEXT NOT NULL CHECK (state IN ('active', 'parked')),
            asks INTEGER NOT NULL DEFAULT 0 CHECK (asks >= 0),
            pid INTEGER,
            started TEXT NOT NULL DEFAULT '',
            taken_at INTEGER NOT NULL
        ) WITHOUT ROWID;
        CREATE TABLE gate_waiters (
            ord INTEGER PRIMARY KEY,
            holder TEXT NOT NULL UNIQUE,
            rt TEXT NOT NULL,
            kind TEXT NOT NULL CHECK (kind IN ('turn', 'resume')),
            invitee INTEGER NOT NULL CHECK (invitee IN (0, 1)),
            pid INTEGER,
            started TEXT NOT NULL DEFAULT ''
        );
    ",
};
