//! Version 123: two things a job's own index row remembers (M4-4a).
//!
//! - `first_seen` (ms): when the job was first indexed. An interval that never fired counts from it. It was the time the
//!   agent's whole file was last indexed (`sched_index.at`), so editing or pausing any other job in the file pushed back
//!   this one's first fire. Set at insert, never changed by an update. Rows from before this step take their file's
//!   `sched_index.at`, the anchor they had, so nothing shifts.
//!   Nullable with no default: a row inserted without it (an import, a test rig) falls back to the watermark instead of
//!   counting from 1970. A row with no index row (none can exist from `sync_in`) stays NULL.
//! - `fail_minute` / `fail_count`: the tick minute of the job's last counted failure and how many ticks in a row its fire
//!   failed, whatever the slot. At the limit each failing tick writes a `skipped:error` row so one broken job stops
//!   holding the watermark for every job. Kept in the row so a restart between ticks does not start the count again.
//!
//! This migration is unreleased and was edited in place (the failure columns were `fail_slot`, and `first_seen` had
//! `DEFAULT 0`): a local core that already applied it needs its `sched_jobs` columns redone or the core.db rebuilt.

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 123,
    name: "first_seen",
    sql: "
        ALTER TABLE sched_jobs ADD COLUMN first_seen INTEGER;
        ALTER TABLE sched_jobs ADD COLUMN fail_minute INTEGER;
        ALTER TABLE sched_jobs ADD COLUMN fail_count INTEGER NOT NULL DEFAULT 0;

        UPDATE sched_jobs SET first_seen =
            (SELECT at FROM sched_index WHERE sched_index.agent = sched_jobs.agent);
    ",
};
