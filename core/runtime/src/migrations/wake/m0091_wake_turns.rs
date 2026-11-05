//! Version 91: which conversation turn carries a wake run (M3-10c). `wake.take` queues the run's turn in the conversations
//! lane and writes the link here in the same transaction; when the run leaves the queue (done, back, recovered, withdrawn)
//! the link says which turn to close, and is deleted. The link is code-only: `turn_id` is a row of the conversations lane's
//! `turns`, and no SQL foreign key crosses lanes. A run has at most one turn, so a replayed take finds its turn here.

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 91,
    name: "wake_turns",
    sql: "
        CREATE TABLE wake_turns (
            run TEXT PRIMARY KEY,
            session TEXT NOT NULL,
            turn_id INTEGER NOT NULL
        ) WITHOUT ROWID;
    ",
};
