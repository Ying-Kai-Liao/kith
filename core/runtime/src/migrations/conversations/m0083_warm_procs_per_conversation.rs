//! Version 83: `warm_procs` is one row per (process, conversation) it has loaded (P7a, designs/2026-11-14-m3-cutover
//! section 1.3, architect MUST 2). One host process loads several sessions (the agent's main one and its chat's threads),
//! so a single `(pid, started)` row could name only one and a revoke or an app take-over would miss the others.
//!
//! `conversation` is `''` for a process that has loaded none (it was `NULL`). Every old row migrates as its single
//! conversation. `kill_at` is still per row but is always set on every row of a process together (the writers mark by
//! process), so the kill re-send and `settle` read the process, not the row. m0082 is not edited.

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 83,
    name: "warm_procs_per_conversation",
    sql: "
        CREATE TABLE warm_procs_new (
            pid INTEGER NOT NULL,
            started TEXT NOT NULL,
            instance TEXT NOT NULL,
            person TEXT NOT NULL DEFAULT '',
            agent TEXT NOT NULL DEFAULT '',
            conversation TEXT NOT NULL DEFAULT '',
            kill_at INTEGER,
            PRIMARY KEY (pid, started, conversation)
        ) WITHOUT ROWID;
        INSERT INTO warm_procs_new (pid, started, instance, person, agent, conversation, kill_at)
            SELECT pid, started, instance, person, agent, COALESCE(conversation, ''), kill_at FROM warm_procs;
        DROP TABLE warm_procs;
        ALTER TABLE warm_procs_new RENAME TO warm_procs;
        CREATE INDEX warm_procs_person ON warm_procs (person);
        CREATE INDEX warm_procs_conversation ON warm_procs (conversation) WHERE conversation <> '';
    ",
};
