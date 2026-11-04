//! M3 lane: conversations, claims, turns and reports. Versions 80-89, one file per step (`m0080_<what>.rs` holding a
//! `pub(super) const MIGRATION`), listed in `STEPS` below in version order, first step 80, no gaps
//! (`migrations::check`).

use super::Migration;

mod m0080_conversations;
#[cfg(test)]
mod tests;

pub(super) const STEPS: &[Migration] = &[m0080_conversations::MIGRATION];
