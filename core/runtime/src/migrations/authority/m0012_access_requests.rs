//! Version 12: the device-pairing access requests (M2-3), as `serve/host/src/remote/cloud.rs` keeps them today: a
//! device of an account asking this computer for access, waiting for the owner's yes or no. m0010 and m0011 are not
//! edited; this step only adds.
//!
//! One row per request. The host keeps them in memory only (`Cloud.requests`), so there is nothing on a Mac to import:
//! after a restart the cloud lists each request again with the host's nonce set, and the host denies it (its nonce is
//! gone). The host's SAS secrets (`commit`, `host_nonce`) stay the host's, in memory; the row holds what the owner
//! sees and what the decision needs.
//! - `id`: the cloud's request id.
//! - `device_key`: the device's public key as the cloud gives it (base64 of 32 bytes), `device_name`, `platform` and
//!   `email` as the device claimed them.
//! - `person`: the invited person the account is on this computer; NULL is the owner's own account. No SQL foreign
//!   key, as in m0010: a person removed or suspended while the request waits must read back as it was, and be refused
//!   when approved.
//! - `code`: the six-digit code the owner compares, once the device revealed a nonce that matches its commit; NULL
//!   before.
//! - `status`: `pending`, `approved` or `denied`. "Expired" is not stored: it is a pending request whose
//!   `expires_at` has passed, so no clock has to run for it.
//! - `created_at`, `expires_at`, `decided_at`: milliseconds since the epoch.

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 12,
    name: "access_requests",
    sql: "
        CREATE TABLE access_requests (
            id TEXT PRIMARY KEY,
            device_key TEXT NOT NULL,
            device_name TEXT NOT NULL,
            platform TEXT NOT NULL,
            email TEXT,
            person TEXT,
            code TEXT,
            status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'approved', 'denied')),
            created_at INTEGER NOT NULL,
            expires_at INTEGER NOT NULL,
            decided_at INTEGER
        ) WITHOUT ROWID;
    ",
};
