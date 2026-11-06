//! Version 11: what the owner's writes (M2-2) keep beyond what the access decision reads. m0010 is not edited; this
//! step only adds.
//!
//! - `memberships.access_by` and `access_since`: who approved each "always" entry on a row and when (JSON objects keyed
//!   `kind:value`, as `kith-auth` keeps them). The decision does not read them; taking a person off an agent, or
//!   demoting them from `member`, drops the entries they approved (`drop_self_approved`), so the write has to.
//! - `people.provider`: the platform a bot guest comes from (`kith-auth bot ... provider=`). Same shape as `name`.
//! - `retired`: the ids of removed people. `kith-auth bot` refuses an id in it; nothing here writes to it yet (removing a
//!   person is M2-4), but the check is the owner's write rule and `kith migrate` (M2-8) fills it from auth.json.
//! - `invites`: the one-time `/link` codes (`kith-auth invite`): the code, the person it binds, when it stops
//!   working (seconds since the epoch, as auth.json keeps it). Every write drops the ones that ran out, as
//!   `kith-auth`'s save does.
//!
//! No SQL foreign keys, as in m0010: an invite for a person who is gone must read back as it was.

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 11,
    name: "authority_write",
    sql: "
        ALTER TABLE memberships ADD COLUMN access_by TEXT;
        ALTER TABLE memberships ADD COLUMN access_since TEXT;
        ALTER TABLE people ADD COLUMN provider TEXT;
        CREATE TABLE retired (
            id TEXT PRIMARY KEY,
            at INTEGER
        ) WITHOUT ROWID;
        CREATE TABLE invites (
            code TEXT PRIMARY KEY,
            person TEXT NOT NULL,
            expires REAL NOT NULL
        ) WITHOUT ROWID;
    ",
};
