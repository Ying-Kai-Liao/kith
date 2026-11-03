//! M2 lane E: cards. Versions 40-49, one file per step (`m0040_<what>.rs` holding a `pub(super) const MIGRATION`),
//! listed in `STEPS` below in version order, first step 40, no gaps (`migrations::check`).

use super::Migration;

pub(super) const STEPS: &[Migration] = &[];
