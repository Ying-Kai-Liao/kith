//! Version 22: a database imported before lane B's tables were filled. Such a database has people but no profiles, grants or
//! variables, which does not mean the file had none: `retired` and `person --rm --unused` could not see a login an id
//! owns or a grant made for it, and a rollback would take the empty tables for the truth and drop the pre-cutover copy's.
//!
//! `secret_stale` holds one row ('imported before `secret_grants`') on a database that already had authority rows (people, memberships, presets, invites, retired or `agent_access`) when this step ran,
//! and none on a new one. A re-import (`kith migrate --authority --force`) empties it with the tables it fills. The
//! snapshot shows it as `secrets_stale`; `--rm --unused` and `retired` refuse while it is set, and `export_back` keeps
//! the old copy's `profiles` and `grants` instead of the core's empty ones. Like `handles_stale` (m0015), but a table of
//! this lane's own, since a step changes only its own lane's tables.

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 22,
    name: "secret_stale",
    sql: "
        CREATE TABLE secret_stale (reason TEXT PRIMARY KEY) WITHOUT ROWID;
        INSERT INTO secret_stale (reason) SELECT 'imported before secret_grants' WHERE EXISTS (SELECT 1 FROM people) \
           OR EXISTS (SELECT 1 FROM memberships) OR EXISTS (SELECT 1 FROM presets) OR EXISTS (SELECT 1 FROM invites) \
           OR EXISTS (SELECT 1 FROM retired) OR EXISTS (SELECT 1 FROM agent_access);
    ",
};
