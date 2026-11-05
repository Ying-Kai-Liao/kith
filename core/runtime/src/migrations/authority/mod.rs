//! M2 lane A: authority (people, grants, access requests, revoke). Versions 10-19, one file per step (`m0010_<what>.rs` holding a `pub(super) const MIGRATION`),
//! listed in `STEPS` below in version order, first step 10, no gaps (`migrations::check`).

use super::Migration;

mod m0010_authority_read;
mod m0011_authority_write;
mod m0012_access_requests;
mod m0013_agent_access;
mod m0014_open_removals;

pub(super) const STEPS: &[Migration] = &[
    m0010_authority_read::MIGRATION,
    m0011_authority_write::MIGRATION,
    m0012_access_requests::MIGRATION,
    m0013_agent_access::MIGRATION,
    m0014_open_removals::MIGRATION,
];
