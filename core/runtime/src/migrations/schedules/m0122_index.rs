//! Version 122: the index over the agents' `jobs.json` (M4-3). The definitions stay in the files (m0120); the core keeps
//! what it last read of each, so a fire decides on one row inside its transaction, not on a file.
//!
//! - `sched_index`: per agent, the hash of the `jobs.json` text the rows below were made from, and when (ms). A fire
//!   reads the file (under `<agent>.jobs.lock`), compares the hash with this row and re-indexes in its own transaction
//!   when they differ.
//! - `sched_jobs`: one row per job of that file: `def` (the job as JSON), `paused` (0/1), `person` (the job's person
//!   when it names one; the owner's runs have none). Written only by the re-index, which also cancels the turns of a
//!   job that is paused or gone.

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 122,
    name: "index",
    sql: "
        CREATE TABLE sched_index (
            agent TEXT PRIMARY KEY,
            hash TEXT NOT NULL,
            at INTEGER NOT NULL
        ) WITHOUT ROWID;

        CREATE TABLE sched_jobs (
            agent TEXT NOT NULL,
            job TEXT NOT NULL,
            def TEXT NOT NULL,
            paused INTEGER NOT NULL DEFAULT 0,
            person TEXT,
            PRIMARY KEY (agent, job)
        ) WITHOUT ROWID;
    ",
};
