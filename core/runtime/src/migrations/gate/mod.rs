//! M2 lane D: resource gate and sync lease. Versions 30-39, one file per step (`m0030_<what>.rs` holding a `pub(super) const MIGRATION`),
//! listed in `STEPS` below in version order, first step 30, no gaps (`migrations::check`).

use super::Migration;

mod m0030_gate;
mod m0031_sync_lease;

pub(super) const STEPS: &[Migration] = &[m0030_gate::MIGRATION, m0031_sync_lease::MIGRATION];
