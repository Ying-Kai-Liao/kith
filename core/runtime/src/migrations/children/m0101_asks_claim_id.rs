//! Version 101: the claim an ask's run started (M3-10c). `ask.end` stops what the ask runs through the claims API, which
//! until now matched a `--direct` run's claim by the ask's `pid`. A claim has an id of its own, and a pid can be reused or
//! shared by a respawn, so a new ask records the claim id it started and stop matches by that; a row from before the
//! column (NULL) still matches by `pid`.
//!
//! The link is code-only: `asks` is the children lane's table, `claims` the conversations lane's, and no SQL foreign key
//! crosses lanes (a claim row is pruned on its own schedule). The column is not part of the record `kith ask` prints.

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 101,
    name: "asks_claim_id",
    sql: "ALTER TABLE asks ADD COLUMN claim_id INTEGER;",
};
