//! M3 lane: the wake queue and waiters. Versions 90-99, reserved by M3-1. One file per step (`m0090_<what>.rs` holding a
//! `pub(super) const MIGRATION`), listed in `STEPS` below in version order, first step 90, no gaps (`migrations::check`).

use super::Migration;

mod m0090_wake;
mod m0091_wake_turns;
mod m0092_wake_turn_epoch;

pub(super) const STEPS: &[Migration] = &[
    m0090_wake::MIGRATION,
    m0091_wake_turns::MIGRATION,
    m0092_wake_turn_epoch::MIGRATION,
];
