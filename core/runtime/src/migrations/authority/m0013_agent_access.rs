//! Version 13: `agent_access`, the domains the owner kept on a card for every guest of an agent (top-level `agent_access`
//! of auth.json). `kith-auth` `resolve` -> `with_jobs` -> `agent_domains` adds them, with the package's `guest_domains`
//! (read from its capabilities.yaml, not stored), to a guest's `access.domains`.
//!
//! One row per agent. `domains` is the JSON list in the order the owner kept them; `since` and `by` are the JSON objects
//! `{host: ms}` and `{host: person|null}` the listing and the card use. A NULL column is a missing key; an agent with a
//! row whose `domains` list is empty is not a refusal (it keeps no domain, which `kith-auth` reads the same).
//!
//! There is no table for "just this once": that is `memberships.helper_once` (the person's or bot's own row on the
//! agent that calls the helper), which `take_once` uses up. auth.json's top-level `once` (one-time secrets, at most an
//! hour) is another thing and stays out. `bridge_held` reads kith serve's `cloud-sync.json`, as `kith-auth` does.

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 13,
    name: "agent_access",
    sql: "
        CREATE TABLE agent_access (
            agent TEXT PRIMARY KEY,
            domains TEXT,
            since TEXT,
            by TEXT
        ) WITHOUT ROWID;
    ",
};
