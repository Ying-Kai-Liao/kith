//! Version 102: asks on the job ledger (P2-3, design 2026-11-24-ask-ledger R1, R3, R5, R7). The row of `asks` becomes the
//! ledger entry of a job the core runs or follows, so it gains:
//! - `mode` (`fg`: the caller's own process runs the callee; `bg`: a core turn the host starts), NULL for a row from before;
//! - `task` (what was asked, whole: `brief` keeps only 2000 characters), `out_path` (where the host's run writes the callee's
//!   JSON answer), `turn` (a `bg` ask's turn in the conversations lane, code-only link), `session` (the callee's session,
//!   written when its run attaches);
//! - `parked_card` and `parked_ms` (the card the ask waits on, set while it is parked);
//! - `cli_pid` and `cli_start` (a `fg` ask's own process and its start stamp, so a caller killed without a handler is found).
//!
//! `state` gains `lost`: the run ended with no answer. SQLite cannot widen a CHECK, so the table is rebuilt (the rows are
//! copied as they are). The new table `ask_rows` is the asker's mailbox: one row per answer, progress report or lost notice,
//! numbered per ask (`n` from 1; the wake of a row is `ask:<ask>/<n>`), `read` once the asker has it by `--wait` or by a
//! wake. No SQL foreign key (lane rule): `ask` points at `asks`, whose rows are forgotten after 31 days with their rows.

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 102,
    name: "ask_ledger",
    sql: r#"
        CREATE TABLE asks_ledger (
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
            state TEXT NOT NULL CHECK (state IN ('open', 'answered', 'capped', 'stopped', 'lost')),
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
            extra TEXT,
            claim_id INTEGER,
            mode TEXT CHECK (mode IS NULL OR mode IN ('fg', 'bg')),
            task TEXT,
            out_path TEXT,
            parked_card TEXT,
            parked_ms INTEGER,
            turn INTEGER,
            cli_pid INTEGER,
            cli_start TEXT,
            session TEXT
        ) WITHOUT ROWID;
        INSERT INTO asks_ledger (id, chain, parent, reply_session, hop, cross_hops, crossed, asked, caller_agent,
            caller_person, caller_via, conversation, cost, cost_base, cap_usd, state, at_ms, answered_ms, capped_ms,
            cap_card_ms, ended_ms, pid, pid_at_ms, brief, done, delivered, woke_answer, extra, claim_id)
        SELECT id, chain, parent, reply_session, hop, cross_hops, crossed, asked, caller_agent,
            caller_person, caller_via, conversation, cost, cost_base, cap_usd, state, at_ms, answered_ms, capped_ms,
            cap_card_ms, ended_ms, pid, pid_at_ms, brief, done, delivered, woke_answer, extra, claim_id
        FROM asks;
        DROP TABLE asks;
        ALTER TABLE asks_ledger RENAME TO asks;
        CREATE INDEX asks_chain ON asks (chain);
        CREATE INDEX asks_reply_session ON asks (reply_session);
        CREATE INDEX asks_turn ON asks (turn) WHERE turn IS NOT NULL;

        CREATE TABLE ask_rows (
            ask TEXT NOT NULL,
            n INTEGER NOT NULL CHECK (n >= 1),
            "as" TEXT,
            kind TEXT NOT NULL CHECK (kind IN ('answer', 'progress', 'lost')),
            text TEXT NOT NULL,
            at_ms INTEGER NOT NULL,
            read INTEGER NOT NULL DEFAULT 0 CHECK (read IN (0, 1)),
            PRIMARY KEY (ask, n)
        ) WITHOUT ROWID;

        -- The turns whose run's cost joined the chain (0: the run of an ask with no turn): a cost is counted once per turn.
        -- Each turn made for an ask's callee, with the file its run writes the answer to: what the tick reads to book the cost
        -- of a run whose report was lost, whatever conversation the turn is on.
        CREATE TABLE ask_turns (
            ask TEXT NOT NULL,
            turn INTEGER NOT NULL,
            out_path TEXT NOT NULL,
            PRIMARY KEY (ask, turn)
        ) WITHOUT ROWID;

        CREATE TABLE ask_costs (
            ask TEXT NOT NULL,
            turn INTEGER NOT NULL,
            PRIMARY KEY (ask, turn)
        ) WITHOUT ROWID;
    "#,
};
