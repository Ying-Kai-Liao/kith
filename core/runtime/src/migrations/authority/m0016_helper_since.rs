//! Version 16: `memberships.helper_since`, when each helper on a person's (or bot's) row was allowed (the JSON object
//! `{helper: ms}` of auth.json's `members.<agent>.<person>.helper_since`). The grants ledger shows it as the helper's
//! "since" and `kith-auth helper` writes it; before this step the import dropped it, so on a switched core every helper's
//! "since" read null and a rollback lost it (S-b, from the C review).
//!
//! NULL is a missing key. A database that already had memberships when this step ran was imported without the column
//! (`authority_settings ('helper_since_stale')`): its rows may have lost a "since", so a rollback keeps the old copy's
//! until a re-import fills the column and removes the mark, as for `handles_stale` (m0015).

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 16,
    name: "helper_since",
    sql: "ALTER TABLE memberships ADD COLUMN helper_since TEXT; \
          INSERT INTO authority_settings (key, value) SELECT 'helper_since_stale', '1' WHERE EXISTS (SELECT 1 FROM memberships);",
};
