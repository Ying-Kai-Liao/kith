//! M3 lane: conversations, claims, turns and reports. Versions 80-89, one file per step (`m0080_<what>.rs` holding a
//! `pub(super) const MIGRATION`), listed in `STEPS` below in version order, first step 80, no gaps
//! (`migrations::check`).

use super::Migration;

mod m0080_conversations;
mod m0081_reports_by_pid;
mod m0082_room_basis;
#[cfg(test)]
mod tests;

pub(super) const STEPS: &[Migration] = &[
    m0080_conversations::MIGRATION,
    m0081_reports_by_pid::MIGRATION,
    m0082_room_basis::MIGRATION,
];
