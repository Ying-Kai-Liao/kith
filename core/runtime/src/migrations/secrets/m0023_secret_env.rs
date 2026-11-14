//! Version 23: auth.json's top-level `env` (agent -> service -> VAR -> value), which `kith-auth mcp-config` and `exec` read
//! to start a service with the agent's own settings (M2 slice S-a). Plain settings such as a path or a model name, kept as
//! JSON text so a number or a flag reads back as it was written; a secret never goes here (a profile's variable holds a
//! reference to the Keychain). `seq` keeps a service's variables in the order the file wrote them, since `mcp-config`
//! prints them in that order.
//!
//! A database that already had authority rows before this step holds an empty table that says nothing, so it gets a
//! `secret_stale` row like m0022's: the readers refuse it ("import again first") until a re-import fills the table.

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 23,
    name: "secret_env",
    sql: "
        CREATE TABLE secret_env (
            agent TEXT NOT NULL,
            service TEXT NOT NULL,
            var TEXT NOT NULL,
            value TEXT NOT NULL,
            seq INTEGER NOT NULL,
            PRIMARY KEY (agent, service, var)
        ) WITHOUT ROWID;
        INSERT OR IGNORE INTO secret_stale (reason) SELECT 'imported before secret_env' WHERE EXISTS (SELECT 1 FROM people) \
           OR EXISTS (SELECT 1 FROM memberships) OR EXISTS (SELECT 1 FROM presets) OR EXISTS (SELECT 1 FROM invites) \
           OR EXISTS (SELECT 1 FROM retired) OR EXISTS (SELECT 1 FROM agent_access);
    ",
};
