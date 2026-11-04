//! Version 21: which profile of secrets an agent may use for a service (`secrets.serviceKeys`, M2-18b). Mapped from
//! auth.json's `profiles` (profile -> VAR -> source, plus the profile's `person`, `with` and `browser`) and `grants`
//! (agent -> service -> profile, or a list of profiles). Nothing writes them yet and the import leaves them behind: the
//! kith-auth switch (M2-18) does.
//!
//! - `secret_grants.agent` is the grants key as auth.json spells it: `coder` for the agent's own grant, `coder@<person>`
//!   for a grant made for one person's runs (looked up first). A list of profiles keeps its order in `seq`.
//! - `secret_profiles` is one row per profile, including those with no variables (a browser profile, a login service's):
//!   `person` is its owner as `kith-auth`'s
//!   `owner_of` answers it, which the M2-18 import (and later the `kith-auth` writers) must store: the named person; the
//!   owner's id (or 'the owner' when none is named) for an unmarked profile of a personal service (login, browser, or
//!   `"personal": true`); NULL only for '*' (shared on purpose) or a service that is not personal. A change to a
//!   service's `personal` setting re-derives the column. The core has no service specs, so it cannot work this out
//!   itself. `lent` is a JSON list of people it is lent to (`with`), `browser` its browser.
//! - `secret_vars` is one row per variable of a profile.
//!
//! No SQL foreign keys, as in the authority tables: auth.json holds a grant to a profile that does not exist, and that
//! has to read back as it was so the launch can say which profile is missing.

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 21,
    name: "secret_grants",
    sql: "
        CREATE TABLE secret_profiles (
            profile TEXT PRIMARY KEY,
            person TEXT,
            lent TEXT,
            browser TEXT
        ) WITHOUT ROWID;
        CREATE TABLE secret_vars (
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
