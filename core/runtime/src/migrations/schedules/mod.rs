//! M4 lane: schedules' run state. Versions 120-129, one file per step (`m0120_<what>.rs` holding a
//! `pub(super) const MIGRATION`), listed in `STEPS` below in version order, first step 120, no gaps
//! (`migrations::check`).

use super::Migration;

mod m0120_schedules;
mod m0121_watermark;
mod m0122_index;
mod m0123_first_seen;

pub(super) const STEPS: &[Migration] = &[
    m0120_schedules::MIGRATION,
    m0121_watermark::MIGRATION,
    m0122_index::MIGRATION,
    m0123_first_seen::MIGRATION,
];
