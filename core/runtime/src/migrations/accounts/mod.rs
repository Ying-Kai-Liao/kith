//! M3 lane: account choice. Versions 105-109, reserved by M3-1; no step yet. One file per step
//! (`m0105_<what>.rs` holding a `pub(super) const MIGRATION`), listed in `STEPS` below in version order, first step
//! 105, no gaps (`migrations::check`).

use super::Migration;

pub(super) const STEPS: &[Migration] = &[];
