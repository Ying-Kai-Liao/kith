//! M3 lane: transcripts held by the core. Versions 110-119, one file per step (`m0110_<what>.rs` holding a
//! `pub(super) const MIGRATION`), listed in `STEPS` below in version order, first step 110, no gaps
//! (`migrations::check`).

use super::Migration;

mod m0110_engine_sessions;
mod m0111_session_records;

pub(super) const STEPS: &[Migration] = &[
    m0110_engine_sessions::MIGRATION,
    m0111_session_records::MIGRATION,
];
