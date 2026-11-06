//! Version 121: the fire watermark (M4-3). `sched_tick.tick` (m0120) holds `kith-sched`'s local-minute text, which
//! repeats when the clocks go back and means nothing on another Mac, so the fire cannot move it. The watermark is a
//! UTC instant in ms, per machine, and only ever moves forward (`max` in code).
//!
//! No data is converted here: turning a local minute into an instant needs the zone database, which SQL does not
//! have. The import writes the watermark beside the tick it copies, and a machine that was imported before this step
//! gets its watermark from `sched_tick` the first time the fire reads it (`schedules::fire::watermark_in`), which
//! converts the text the way the import does. `sched_tick` stays as the import left it, for `schedule.jobs`.

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 121,
    name: "watermark",
    sql: "
        CREATE TABLE sched_watermark (
            mac TEXT PRIMARY KEY,
            at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        ) WITHOUT ROWID;
    ",
};
