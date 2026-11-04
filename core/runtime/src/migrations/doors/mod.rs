//! M2 lane G: doors and their sweep. Versions 60-69, one file per step (`m0060_<what>.rs` holding a `pub(super) const MIGRATION`),
//! listed in `STEPS` below in version order, first step 60, no gaps (`migrations::check`).

use super::Migration;

mod m0060_doors;

pub(super) const STEPS: &[Migration] = &[m0060_doors::MIGRATION];
