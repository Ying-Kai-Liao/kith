//! M4 lane: schedules' run state. Versions 120-129, one file per step (`m0120_<what>.rs` holding a
//! `pub(super) const MIGRATION`), listed in `STEPS` below in version order, first step 120, no gaps
//! (`migrations::check`).

use super::Migration;

mod m0120_schedules;

pub(super) const STEPS: &[Migration] = &[m0120_schedules::MIGRATION];
