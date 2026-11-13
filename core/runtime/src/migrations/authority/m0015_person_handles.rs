//! Version 15: a person's `handles` (the channel and cloud accounts `kith-auth people --json` lists as `kith` and `handles`),
//! and so what `retired` and `person --rm --unused` check before an id is free (M2-18, `people.listPeople`).
//!
//! `people.handles` is the JSON text of auth.json's `people.<id>.handles` object, platform -> user id, in the file's
//! order; the `kith` key (the cloud link) is in it, as it is in the file. NULL when the person has none or the row was
//! imported before this step: a database imported earlier has no handles until it is imported again, and answers with
//! what it has. Such a database carries `authority_settings ('handles_stale')`, which a re-import removes; `person --rm
//! --unused` refuses while it is set, since a NULL there could be a person with handles.

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 15,
    name: "person_handles",
    sql: "ALTER TABLE people ADD COLUMN handles TEXT; \
          INSERT INTO authority_settings (key, value) SELECT 'handles_stale', '1' WHERE EXISTS (SELECT 1 FROM people);",
};
