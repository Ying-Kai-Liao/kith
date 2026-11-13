//! Version 124: an index on when a slot was decided (M4-5a), so pruning `sched_fires` by age does not scan the table.
//!
//! The fire prunes rows older than two weeks once an hour (`schedules::fire::prune_fires`): a delivery's row
//! (`event:d<hash>`) stays sixty days for redelivery dedupe, a job's latest timed slot and a row whose turn has not
//! finished stay for ever. No data changes here.

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 124,
    name: "fires_at",
    sql: "
        CREATE INDEX sched_fires_at ON sched_fires (at);
    ",
};
