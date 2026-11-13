//! Version 40: the card store, mapped from the host's `cards` table in serve.db (`serve/host/src/store/schema.rs`
//! `migrate_7` plus its later `ALTER`s, read by `serve/host/src/store/cards.rs`). A later `kith migrate` (M2-8) copies
//! a row across column by column; nothing writes this table in production yet (the host's callers switch in lane H).
//!
//! Column mapping (host -> core):
//! - `id`, `session`, `source`, `bot_id`, `run_id`, `entry_id`, `person`, `agent`, `approver`, `tool`, `input`,
//!   `resource`, `shown`, `scope`, `state`, `created_at`, `deadline`, `answer`, `allowed`, `answered_by`, `answered_at`,
//!   `answered_parked`, `granted_at`, `resumed_at`, `applied_at`, `used_by_ask`, `reply_to`, `asked_by`,
//!   `answerable_by`, `in_grants`: same name, same type, same meaning. Times are milliseconds since the epoch, as in the
//!   host (`now_ms`), so an import copies them unchanged. `allowed` and the flags are 0/1; the host's own CHECKs on
//!   `source`, `approver` and `state` are kept.
//! - The unique index `cards_open` (one open card per session and resource, A6) and the two lookup indexes carry over.
//!
//! New in the core, NULL (or empty) for an imported card:
//! - `options`: the answers the card offers, as JSON text `[{"optionId": "...", "kind": "allow_once"}, ...]`. The host
//!   keeps these on the card's entry (`entry.data.options`), not on the card; a card raised in the core carries them so
//!   `cards.answer` can tell a yes from a no with no entry. NULL means the plain `allow` / `deny` / `cancel` answers.
//! - `claim_by`, `claim_until`, `claim_pid`, `claim_started`: the at-most-one-answerer claim. The host takes a claim by
//!   pushing `deadline` out (`card_claim`) and keeps the holder in memory (secret.rs `Slot`); the core records the
//!   holder, so a second answerer is refused by the row itself and the claim outlives neither its time nor its process.
//!   `deadline` stays the card's own, and the sweep reads `MAX(deadline, claim_until)` while a claim is held.
//!
//! Not mapped: the host's other tables a card points to (`devices` for a person's answer, `entries`); no SQL foreign
//! key to another lane's table, the links live in code.

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 40,
    name: "cards",
    sql: "
        CREATE TABLE cards (
            id TEXT PRIMARY KEY,
            session TEXT NOT NULL,
            source TEXT NOT NULL CHECK (source IN ('acp', 'run')),
            bot_id TEXT,
            run_id TEXT,
            entry_id TEXT,
            person TEXT NOT NULL DEFAULT '',
            agent TEXT NOT NULL DEFAULT '',
            approver TEXT NOT NULL CHECK (approver IN ('owner', 'member')),
            tool TEXT NOT NULL,
            input TEXT NOT NULL,
            resource TEXT NOT NULL,
            shown TEXT NOT NULL DEFAULT '',
            scope TEXT NOT NULL DEFAULT 'once',
            state TEXT NOT NULL CHECK (state IN
                ('pending', 'live', 'parked', 'approved', 'denied', 'expired', 'withdrawn', 'used')),
            created_at INTEGER NOT NULL,
            deadline INTEGER NOT NULL,
            answer TEXT,
            allowed INTEGER CHECK (allowed IN (0, 1)),
            answered_by TEXT,
            answered_at INTEGER,
            answered_parked INTEGER NOT NULL DEFAULT 0 CHECK (answered_parked IN (0, 1)),
            granted_at INTEGER,
            resumed_at INTEGER,
            applied_at INTEGER,
            used_by_ask INTEGER NOT NULL DEFAULT 0 CHECK (used_by_ask IN (0, 1)),
            reply_to TEXT,
            asked_by TEXT,
            answerable_by TEXT,
            in_grants INTEGER NOT NULL DEFAULT 0 CHECK (in_grants IN (0, 1)),
            options TEXT,
            claim_by TEXT,
            claim_until INTEGER,
            claim_pid INTEGER,
            claim_started TEXT NOT NULL DEFAULT '',
            CHECK ((claim_by IS NULL) = (claim_until IS NULL))
        ) WITHOUT ROWID;
        CREATE UNIQUE INDEX cards_open ON cards (session, resource)
            WHERE state IN ('pending', 'live', 'parked');
        CREATE INDEX cards_entry ON cards (entry_id);
        CREATE INDEX cards_person ON cards (person, state);
    ",
};
