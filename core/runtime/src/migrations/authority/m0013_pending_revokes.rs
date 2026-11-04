//! Version 13: the bots a person's removal dropped (M2-18c), kept until the revoke order has run.
//!
//! `person --rm` deletes the person's and their bots' `people` rows in one write, so a walk of `people.bot_of` after it
//! finds no bot, and a crash between that write and `ask.revokeEverywhere` would leave the bots' claims and asks alive.
//! The write therefore also puts the transitive bot set here, in its own transaction; the revoke order reads it, and
//! deletes the row when it has finished. A removal written before this step has no record: the order falls back to the
//! bots still in `people.bot_of`.
//! - `person`: the id removed (no SQL foreign key: the row is gone from `people`).
//! - `bots`: a JSON array of the bots' ids (the whole transitive set), in the order the removal found them.
//! - `at`: milliseconds since the epoch, for whoever finds a row that never cleared.

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 13,
    name: "pending_revokes",
    sql: "
        CREATE TABLE pending_revokes (
            person TEXT PRIMARY KEY,
            bots TEXT NOT NULL,
            at INTEGER NOT NULL
        ) WITHOUT ROWID;
    ",
};
