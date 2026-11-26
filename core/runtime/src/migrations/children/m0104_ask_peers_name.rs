//! Version 104: the name of a computer an ask can go to (design 2026-11-24-ask-ledger §9, A5b). `kith ask AGENT@MINI` names a
//! computer the way its owner knows it; the pairing's `ask_peers` row held only the computer's id. The host writes the name with
//! the epoch (`ask.peerLink`'s `name`), so the core can turn the name of a paired computer into its id when a peer ask opens. A row
//! from before has none (NULL) and is found by its id only.

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 104,
    name: "ask_peers_name",
    sql: "ALTER TABLE ask_peers ADD COLUMN name TEXT;",
};
