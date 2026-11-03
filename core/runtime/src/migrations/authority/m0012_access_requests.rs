//! Version 12: the access requests (M2-3): a person asking the owner for use of agents, waiting for a yes or a no.
//! m0010 and m0011 are not edited; this step only adds.
//!
//! One row per request. The columns are plain on purpose (M2-8 fills the table from the host's own store):
//! - `id`: the request's name, chosen by whoever makes it (the owner's `approve` and `deny` name it).
//! - `person`: the id in `people` it is for. No SQL foreign key, as in m0010: a request whose person was removed
//!   or revoked must read back as it was, and be refused when approved.
//! - `agents`: JSON array of agent ids, in the order asked. `preset` and `mode` are what they asked for, NULL when
//!   they asked for neither (a plain `guest`, `default`).
//! - `via`: where it came from, free text for the owner to read (`invite:ABC234`, `cloud`); nothing reads it.
//! - `status`: `pending`, `approved` or `denied`. "Expired" is not stored: it is a pending request whose `expires_at`
//!   has passed, so no clock has to run for it.
//! - `created_at`, `expires_at`, `decided_at`: milliseconds since the epoch.

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 12,
    name: "access_requests",
    sql: "
        CREATE TABLE access_requests (
            id TEXT PRIMARY KEY,
            person TEXT NOT NULL,
            agents TEXT NOT NULL,
            preset TEXT,
            mode TEXT,
            via TEXT,
            status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'approved', 'denied')),
            created_at INTEGER NOT NULL,
            expires_at INTEGER NOT NULL,
            decided_at INTEGER
        ) WITHOUT ROWID;
    ",
};
