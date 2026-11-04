//! M3 lane: delegation and children. Versions 100-104, reserved by M3-1. One file per step (`m0100_<what>.rs` holding a
//! `pub(super) const MIGRATION`), listed in `STEPS` below in version order, first step 100, no gaps
//! (`migrations::check`).

use super::Migration;

mod m0100_asks;

pub(super) const STEPS: &[Migration] = &[m0100_asks::MIGRATION];
