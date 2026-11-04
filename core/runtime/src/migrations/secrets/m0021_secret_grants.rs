//! Version 21: which profile of secrets an agent may use for a service, for the launch's keys (`secrets.launchEnv`,
//! M2-18b). Mapped from auth.json's `profiles` (profile -> VAR -> source) and `grants` (agent -> service -> profile, or
//! a list of profiles). Nothing writes them yet and the import leaves them behind: the kith-auth switch (M2-18) does.
//! No SQL foreign keys, as in the authority tables: auth.json holds a grant to a profile that does not exist, and that
//! has to read back as it was so the launch can say which profile is missing.

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 21,
    name: "secret_grants",
    sql: "
        CREATE TABLE secret_profiles (
            profile TEXT NOT NULL,
            var TEXT NOT NULL,
            source TEXT NOT NULL,
            PRIMARY KEY (profile, var)
        ) WITHOUT ROWID;
        CREATE TABLE secret_grants (
            agent TEXT NOT NULL,
            service TEXT NOT NULL,
            profile TEXT NOT NULL,
            seq INTEGER NOT NULL,
            PRIMARY KEY (agent, service, profile)
        ) WITHOUT ROWID;
    ",
};
