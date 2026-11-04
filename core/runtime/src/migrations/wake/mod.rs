//! M3 lane: the wake queue and waiters. Versions 90-99, reserved by M3-1; no step yet. One file per step
//! (`m0090_<what>.rs` holding a `pub(super) const MIGRATION`), listed in `STEPS` below in version order, first step 90,
//! no gaps (`migrations::check`).

use super::Migration;

pub(super) const STEPS: &[Migration] = &[];
