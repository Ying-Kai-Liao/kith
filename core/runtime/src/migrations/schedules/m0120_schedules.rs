//! Version 120: the run state of schedules (M4-2; docs/plans/runtime-design.md section 2). Job DEFINITIONS are not here:
//! they stay in `<agent>/jobs.json`, which versions and syncs with the agent (architect, 2026-11-04). The core indexes
//! those files and keeps what happened to each job. Nothing writes these tables from a live tick yet: `kith-sched` stays
//! the only writer of jobs.json and of its state files until M4-5, firing is M4-3, and the import reads the state files.
//!
//! - `sched_tick`: per machine (`mac`, the stable id of `<machine>/mac-id`), the last tick as `kith-sched` writes it
//!   (`tick` in state.json: a local minute, `YYYY-MM-DDTHH:MM`).
//! - A SLOT names one due time of one job: a UTC instant (`2026-11-04T00:00:00Z`, whole minutes) or `manual:<id>` for a
//!   run started by hand. Never local-minute text, which repeats at a clock change and means nothing on another Mac.
//! - `sched_fires`: one row per (agent, job, slot), the decision made for that slot, so a slot is decided once. `outcome`
//!   is `fired` or `skipped:<reason>` (checked in code, so a new reason needs no table rebuild). `turn_id` is the turn the
//!   fire made (NULL until M4-3 enqueues turns and for a skip), `at` when it was decided (ms).
//! - `sched_runs`: one row per run, keyed on (agent, job, mac, slot) and linked to its fire by the slot and to its turn by
//!   `turn_id`. `state` is validated in code (`started`, `done`, `failed`, `skipped`), not by a CHECK, since the set
//!   grows with firing. `outcome` says why in a word or a line; `outbox` is the run's outbox key. `at` is the start as
//!   `kith-sched` printed it (local minute text), kept only so `ls` prints what it printed; `started_at`/`finished_at`
//!   are ms when known (NULL for an imported run). No SQL foreign key to a job or a fire: a job removed from jobs.json
//!   keeps its runs, and a job with no run has no row.

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
            outcome TEXT NOT NULL,
            turn_id TEXT,
            at INTEGER NOT NULL,
            PRIMARY KEY (agent, job, slot)
        ) WITHOUT ROWID;

        CREATE TABLE sched_runs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            agent TEXT NOT NULL,
            job TEXT NOT NULL,
            mac TEXT NOT NULL,
            slot TEXT NOT NULL,
            turn_id TEXT,
            at TEXT NOT NULL,
            state TEXT NOT NULL,
            outcome TEXT,
            why TEXT,
            pid INTEGER,
            log TEXT,
            outbox TEXT,
            started_at INTEGER,
            finished_at INTEGER,
            UNIQUE (agent, job, mac, slot)
        );
        CREATE INDEX sched_runs_job ON sched_runs (agent, job, id);
    ",
};
