//! Version 20: the envelope's records (docs/plans/runtime-design.md section 9b). Every column but the name and the
//! timestamps is opaque: the ciphertext is XChaCha20-Poly1305 with the name as associated data, and `key_id` is a
//! short fingerprint of the master key (never the key), so a record sealed by another machine's key is recognised
//! before any decrypt is tried.

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 20,
    name: "secret_records",
    sql: "
        CREATE TABLE secret_records (
            name TEXT PRIMARY KEY,
            key_id TEXT NOT NULL,
            nonce BLOB NOT NULL CHECK (length(nonce) = 24),
            ciphertext BLOB NOT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        ) WITHOUT ROWID;
    ",
};
