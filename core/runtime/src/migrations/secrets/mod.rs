//! M2 lane B: secrets envelope. Versions 20-29, one file per step (`m0020_<what>.rs` holding a `pub(super) const MIGRATION`),
//! listed in `STEPS` below in version order, first step 20, no gaps (`migrations::check`).

use super::Migration;

mod m0020_secret_records;
mod m0021_secret_grants;
mod m0022_secret_stale;
mod m0023_secret_env;

pub(super) const STEPS: &[Migration] = &[
    m0020_secret_records::MIGRATION,
    m0021_secret_grants::MIGRATION,
    m0022_secret_stale::MIGRATION,
    m0023_secret_env::MIGRATION,
];
