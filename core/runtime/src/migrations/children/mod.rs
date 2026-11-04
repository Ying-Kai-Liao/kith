//! M3 lane: delegation and children. Versions 100-104, reserved by M3-1; no step yet. One file per step
//! (`m0100_<what>.rs` holding a `pub(super) const MIGRATION`), listed in `STEPS` below in version order, first step
//! 100, no gaps (`migrations::check`).

use super::Migration;

pub(super) const STEPS: &[Migration] = &[];
