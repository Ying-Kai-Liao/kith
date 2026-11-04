//! M3 lane: the wake queue and waiters. Versions 90-99, reserved by M3-1. One file per step (`m0090_<what>.rs` holding a
//! `pub(super) const MIGRATION`), listed in `STEPS` below in version order, first step 90, no gaps (`migrations::check`).

use super::Migration;

mod m0090_wake;

pub(super) const STEPS: &[Migration] = &[m0090_wake::MIGRATION];
