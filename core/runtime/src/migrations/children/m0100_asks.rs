//! Version 100: the ask tree, one row per `kith ask` (M3-8a). Today each ask is a `live/ask/ID.reg` file that
//! `kernel/bin/kith-ask` writes under the box lock (`live/ask/.lock`); here the row is the record, and the cap check and the
//! insert of a new ask are ONE `BEGIN IMMEDIATE` transaction, which replaces that lock: no second ask can pass the cap between
//! another's check and open.
//!
//! "Children" are rows of this tree, not processes: the holder's process group already covers what a run started. Stopping a
//! whole tree (moving each child's claim to `ending`) is a later step; `ask.end` here only settles rows `stopped`.
//!
//! Column mapping (`.reg` key -> column):
//! - `reply_to.ask` -> `parent` (the ask whose run made this one), `chain` -> `chain` (the root ask of the tree; an ask that
//!   starts one is its own chain), `hop` -> `hop` (depth), `reply_to.session` -> `reply_session` (the caller's session);
//! - `asked` -> `asked`; `agent`, `person`, `via` -> `caller_agent`, `caller_person`, `caller_via`;
//! - `at`, `answered_at`, `capped_at`, `cap_card_at`, `ended_at`, `pid_at` (seconds, float) -> `*_ms` (integer ms, as the
//!   delegations view prints them); `at` is the ask's start;
//! - `cost`, `cost_base`, `cap_usd` -> same (US$, REAL). `cost` NULL is a cost that was never kept (counts as nothing);
//! - `done`, `delivered`, `woke_answer` -> same (0/1); `brief` -> `brief`; `pid` -> `pid`;
//! - `conversation` is new: the asked agent's session of a `--bg` run (today `ask-bg/ID.sid`), the `session` of the view;
//! - `extra` keeps every other key of an imported record as JSON, so nothing a `.reg` holds is lost.
//!
//! `state` is `open` (running, or parked on a card), `answered`, `capped` (answered, and its run took the chain to its cost
//! cap) or `stopped`. The chain cap itself stays in settings.json. No SQL foreign key: `parent` points at a row of this table
//! that may be gone (records are forgotten after 31 days), and `reply_session`/`conversation` point at another lane's table.

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 100,
    name: "asks",
    sql: "
        CREATE TABLE asks (
            id TEXT PRIMARY KEY,
            chain TEXT NOT NULL,
            parent TEXT,
            reply_session TEXT,
            hop INTEGER NOT NULL DEFAULT 0,
            cross_hops INTEGER NOT NULL DEFAULT 0,
            crossed INTEGER NOT NULL DEFAULT 0 CHECK (crossed IN (0, 1)),
            asked TEXT,
            caller_agent TEXT,
            caller_person TEXT,
            caller_via TEXT,
            conversation TEXT,
            cost REAL,
            cost_base REAL,
            cap_usd REAL,
            state TEXT NOT NULL CHECK (state IN ('open', 'answered', 'capped', 'stopped')),
            at_ms INTEGER,
            answered_ms INTEGER,
            capped_ms INTEGER,
            cap_card_ms INTEGER,
            ended_ms INTEGER,
            pid INTEGER,
            pid_at_ms INTEGER,
            brief TEXT,
            done INTEGER NOT NULL DEFAULT 0 CHECK (done IN (0, 1)),
            delivered INTEGER NOT NULL DEFAULT 0 CHECK (delivered IN (0, 1)),
            woke_answer INTEGER NOT NULL DEFAULT 0 CHECK (woke_answer IN (0, 1)),
            extra TEXT
        ) WITHOUT ROWID;
        CREATE INDEX asks_chain ON asks (chain);
        CREATE INDEX asks_reply_session ON asks (reply_session);
    ",
};
