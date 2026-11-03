//! M2 lane F: run tokens. Versions 50-59, one file per step (`m0050_<what>.rs` holding a `pub(super) const MIGRATION`),
//! listed in `STEPS` below in version order, first step 50, no gaps (`migrations::check`).

use super::Migration;

pub(super) const STEPS: &[Migration] = &[];
