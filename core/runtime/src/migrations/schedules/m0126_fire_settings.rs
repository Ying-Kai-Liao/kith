//! Version 126: the settings a scheduled run starts with, fixed on the fire row when the turn is made (M4-5 package C):
//! `{"model", "effort", "harness"}` as JSON, each key only when the job set it. NULL for a fire made before this, whose run
//! reads the job's current values. The turn already carries the prompt and the person.

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 126,
    name: "fire_settings",
    sql: "
        ALTER TABLE sched_fires ADD COLUMN settings TEXT;
    ",
};
