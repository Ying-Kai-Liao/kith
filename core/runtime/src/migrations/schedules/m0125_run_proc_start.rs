//! Version 125: the start stamp of an imported run's process (M4-5 handover, H3). `kith-sched` writes it beside the run's
//! pid (`procStart`); the fire's skip rule reads it to tell a run still going from a pid that was reused. NULL for a run
//! imported without one (an old state file), which falls back to "pid alive and under six hours".

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 125,
    name: "run_proc_start",
    sql: "
        ALTER TABLE sched_runs ADD COLUMN proc_start TEXT;
    ",
};
