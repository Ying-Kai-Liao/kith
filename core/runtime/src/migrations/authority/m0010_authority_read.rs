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
//! Left out on purpose, because the access decision never reads them: auth.json `profiles`, `grants`, `env` (secrets,
//! lane B), `invites` (access requests, M2-3), `retired` (ids never handed out again, a write rule: M2-2), `once`
//! (used up by a launch, M2-2), and a person's `handles`, `root`, `provider`, `kith`, `agents` (channel and cloud
//! links that `kith-people` lists and the host keeps). `revoked` is kept as a flag and its time (`revoked.at`); any
//! other key inside a `revoked` object is not read by anything.

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
