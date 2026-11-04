//! Version 70: what `kith migrate` imported into core.db and when (M2-8). One row per import that changed something; a
//! run that finds core.db already holding the answer writes nothing, here or anywhere.
//!
//! - `step`: which import (`authority`).
//! - `source` / `source_sha256`: the file read and the hash of its bytes, so a later run can tell the source moved on.
//! - `backup`: the `VACUUM INTO` copy of core.db taken before the first write of this import.
//! - `summary`: what went in, one line (counts, never values).
//!
//! The source is never edited or deleted, whatever this table says.

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 70,
    name: "import_log",
    sql: "
        CREATE TABLE import_log (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            step TEXT NOT NULL,
            at INTEGER NOT NULL,
            source TEXT NOT NULL,
            source_sha256 TEXT NOT NULL,
            backup TEXT,
            summary TEXT NOT NULL
        );
    ",
};
