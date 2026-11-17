//! Version 111: the session registry's records held by the core (transcripts P1). `engine_sessions` keeps only what the
//! core needs to find a transcript; the registry's readers also need the pid, kind, label, procStart, launcher, startedAt,
//! source, resumedFrom and project, so the core keeps each record whole. Once the `transcripts` module is switched the core
//! is the only writer of `$KITH_STATE/live/terminal/<sid>.json`, and each file is a projection of its row here.
//!
//! - `sid`: the session id (the file's name, `safe_id`).
//! - `record`: the record as JSON text, the same object `kith-sessions record` writes (plus `recordedAt` when it carried one).
//! - `recorded_at`: the order key in nanoseconds since the epoch: the record's `recordedAt`, or for a record that had none
//!   (a file written by an older hook) the file's modification time when it was read.
//! - `proc_start`: the record's `procStart`, the order key's tiebreak (a string compare; NULL sorts first). A full tie is won by
//!   the record that arrives last, so no column holds the arrival.
//! - `updated_at`: seconds, when the row last changed.

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 111,
    name: "session_records",
    sql: "
        CREATE TABLE session_records (
            sid TEXT PRIMARY KEY NOT NULL,
            record TEXT NOT NULL,
            recorded_at INTEGER NOT NULL,
            proc_start TEXT,
            updated_at INTEGER NOT NULL
        ) WITHOUT ROWID;
    ",
};
