//! Version 15: a person's `handles` (the channel and cloud accounts `kith-auth people --json` lists as `kith` and `handles`),
//! and so what `retired` and `person --rm --unused` check before an id is free (M2-18, `people.listPeople`).
//!
//! `people.handles` is the JSON text of auth.json's `people.<id>.handles` object, platform -> user id, in the file's
//! order; the `kith` key (the cloud link) is in it, as it is in the file. NULL when the person has none or the row was
//! imported before this step: a database imported earlier has no handles until it is imported again, and answers with
//! what it has.

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 15,
    name: "person_handles",
    sql: "ALTER TABLE people ADD COLUMN handles TEXT;",
};
