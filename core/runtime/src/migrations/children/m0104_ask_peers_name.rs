//! Version 104: the name of a computer an ask can go to (design 2026-11-24-ask-ledger §9, A5b). `kith ask AGENT@MINI` names a
//! computer the way its owner knows it; the pairing's `ask_peers` row held only the computer's id. The host writes the name with
//! the epoch (`ask.peerLink`'s `name`), so the core can turn the name of a paired computer into its id when a peer ask opens. A row
//! from before has none (NULL) and is found by its id only.
//!
//! `boot` names the core process that last had the pairing confirmed by the host (`ask.peerLink`, which the host's reconcile
//! repeats): a pairing not confirmed in this process starts no ask until it is (the host may have ended it while the core was
//! away, and its reconcile has not yet run).

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 104,
    name: "ask_peers_name",
    sql: "ALTER TABLE ask_peers ADD COLUMN name TEXT; ALTER TABLE ask_peers ADD COLUMN boot TEXT;",
};
