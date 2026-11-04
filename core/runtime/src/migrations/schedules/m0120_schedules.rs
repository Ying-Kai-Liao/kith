//! Version 120: the run state of schedules (M4-2; docs/plans/runtime-design.md section 2). Job DEFINITIONS are not here:
//! they stay in `<agent>/jobs.json`, which versions and syncs with the agent (architect, 2026-11-04). The core indexes
//! those files and keeps what happened to each job. Nothing writes these tables from a live tick yet: `kith-sched` stays
//! the only writer of jobs.json and of its state files until M4-5, firing is M4-3, and the import reads the state files.
//!
//! - `sched_tick`: per machine (`mac`, the stable id of `<machine>/mac-id`), the last tick as `kith-sched` writes it
//!   (`tick` in state.json: a local minute, `YYYY-MM-DDTHH:MM`).
//! - `sched_fires`: one row per (agent, job, slot): the due time a job was fired for. `turn_id` is the turn the fire made
//!   (NULL until M4-3 enqueues turns), `at` when (ms). The key is what makes a fire happen once.
//! - `sched_runs`: one row per run. `at` is the start as `kith-sched` writes it (local minute text, so `ls` prints what
//!   it printed), `started_at`/`finished_at` are ms when known (NULL for an imported run). `state` is `started`,
//!   `done`, `failed` or `skipped`; `outcome` says why in a word or a line; `outbox` is the run's outbox key. No SQL
//!   foreign key to a job: a job removed from jobs.json keeps its runs, and a job with no run has no row.

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 120,
    name: "schedules",
    sql: "
        CREATE TABLE sched_tick (
            mac TEXT PRIMARY KEY,
            tick TEXT NOT NULL,
            updated_at INTEGER NOT NULL
        ) WITHOUT ROWID;

        CREATE TABLE sched_fires (
            agent TEXT NOT NULL,
            job TEXT NOT NULL,
            slot TEXT NOT NULL,
            turn_id TEXT,
            at INTEGER NOT NULL,
            PRIMARY KEY (agent, job, slot)
        ) WITHOUT ROWID;

        CREATE TABLE sched_runs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            agent TEXT NOT NULL,
            job TEXT NOT NULL,
            mac TEXT NOT NULL,
            at TEXT NOT NULL,
            state TEXT NOT NULL CHECK (state IN ('started', 'done', 'failed', 'skipped')),
            outcome TEXT,
            why TEXT,
            pid INTEGER,
            log TEXT,
            outbox TEXT,
            started_at INTEGER,
            finished_at INTEGER,
            UNIQUE (agent, job, mac, at)
        );
        CREATE INDEX sched_runs_job ON sched_runs (agent, job, id);
    ",
};
