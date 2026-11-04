//! M2 lane C: data import bookkeeping. Versions 70-79, one file per step (`m0070_<what>.rs` holding a `pub(super) const MIGRATION`),
//! listed in `STEPS` below in version order, first step 70, no gaps (`migrations::check`).

use super::Migration;

mod m0070_import_log;

pub(super) const STEPS: &[Migration] = &[m0070_import_log::MIGRATION];
