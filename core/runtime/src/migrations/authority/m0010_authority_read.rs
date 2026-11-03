//! Version 10: the tables the access decision reads (`may`, `resolve`, `resolve_bot`, `cut_off`), mapped from the
//! tables of auth.json that `kernel/bin/kith-auth` reads for it: `people`, `members`, `presets` and `owner_bots_budget`.
//! Nothing writes them yet (M2-2 does) and nothing outside the tests reads them (M2-8 imports auth.json).
//!
//! A column is NULL when auth.json has no such key; the reader leaves the key out then, so a missing field takes the
//! default `kith-auth` gives it. Lists and objects are JSON text, kept in the order auth.json has them (the order
//! decides which helper is listed first). `people` keeps its rows in insertion order (`seq`), which is the order of
//! auth.json's `people` object: `canonical` and `owner` take the first match.
//!
//! No SQL foreign keys: auth.json holds a bot whose owner is gone, a row for an id that is no person, a revoked
//! person's rows. All of that must read back as it was.
//!
//! Left out of this step, with who needs them:
//! - Read by the access decision in `kith-auth` but NOT yet here: top-level `agent_access` (the domains the owner kept
//!   for every guest of an agent; Python `resolve` -> `with_jobs` -> `agent_domains` adds them, plus the package's
//!   `guest_domains`, to `access.domains`). A later lane A step adds that table. `kith_kernel::secrets::with_jobs`
//!   does not add either yet, so `Rules::resolve` already differs from Python there: a follow-up for the rules, not
//!   for these tables.
//! - Not read by the decision: `profiles`, `grants`, `env` (secrets, lane B), `invites` (access requests, M2-3),
//!   `retired` and `once` (written by M2-2), and a person's `handles`, `root`, `provider`, `kith`, `agents` (channel
//!   and cloud links that `kith-people` lists and the host keeps). `revoked` is kept as a flag and its time
//!   (`revoked.at`); other keys inside a `revoked` object are read by nothing.

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 10,
    name: "authority_read",
    sql: "
        CREATE TABLE people (
            seq INTEGER PRIMARY KEY AUTOINCREMENT,
            id TEXT NOT NULL UNIQUE,
            name TEXT,
            lang TEXT,
            tz TEXT,
            owner INTEGER NOT NULL DEFAULT 0 CHECK (owner IN (0, 1)),
            bot_of TEXT,
            revoked INTEGER NOT NULL DEFAULT 0 CHECK (revoked IN (0, 1)),
            revoked_at INTEGER,
            was TEXT
        );
        CREATE TABLE memberships (
            agent TEXT NOT NULL,
            person TEXT NOT NULL,
            preset TEXT,
            mode TEXT,
            trusted INTEGER CHECK (trusted IN (0, 1)),
            deny TEXT,
            model TEXT,
            env TEXT,
            helpers TEXT,
            budget TEXT,
            access TEXT,
            jobs TEXT,
            helper_once TEXT,
            PRIMARY KEY (agent, person)
        ) WITHOUT ROWID;
        CREATE TABLE presets (
            name TEXT PRIMARY KEY,
            mode TEXT,
            trusted INTEGER CHECK (trusted IN (0, 1)),
            deny TEXT
        ) WITHOUT ROWID;
        CREATE TABLE authority_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;
    ",
};
