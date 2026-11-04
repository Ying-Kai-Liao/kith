//! Version 123: two things a job's own index row remembers (M4-4a).
//!
//! - `first_seen` (ms): when the job was first indexed. An interval that never fired counts from it. It was the time the
//!   agent's whole file was last indexed (`sched_index.at`), so editing or pausing any other job in the file pushed back
//!   this one's first fire. Set at insert, never changed by an update. Rows from before this step take their file's
//!   `sched_index.at`, the anchor they had, so nothing shifts.
//! - `fail_slot` / `fail_count`: the slot a tick could not fire and in how many ticks in a row. At the limit the fire
//!   writes a `skipped:error` row for the slot so one broken job stops holding the watermark for every job. Kept in the
//!   row so a restart between ticks does not start the count again.

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 123,
    name: "first_seen",
    sql: "
        ALTER TABLE sched_jobs ADD COLUMN first_seen INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE sched_jobs ADD COLUMN fail_slot TEXT;
        ALTER TABLE sched_jobs ADD COLUMN fail_count INTEGER NOT NULL DEFAULT 0;

        UPDATE sched_jobs SET first_seen = coalesce(
            (SELECT at FROM sched_index WHERE sched_index.agent = sched_jobs.agent), 0);
    ",
};
