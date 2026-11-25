//! Version 103: asks across computers (design 2026-11-24-ask-ledger §9, A5a). A row of `asks` can now be one of two ends of
//! an ask that crosses to another computer:
//! - the caller's end: `mode = 'peer'` (no local turn), with `peer` (the computer asked), `peer_epoch` (the pairing it was
//!   opened under), `sent` (the computer answered the send), `fetched` (the last row number copied from it), `stop_owed`
//!   (a stop the computer has not confirmed yet) and `peer_seen_ms` (when it last answered for this ask: the away wait runs
//!   from it);
//! - the callee's end: an ordinary `bg` ask with `origin_computer`, `origin_key` (the caller's ask id) and `origin_epoch`,
//!   unique on (computer, key): a send that is made again finds the same row.
//!
//! `state` gains `refused` (the other computer refused it, or this one refused an ask of an ended pairing) and `cut_off` (the
//! link ended, or the computer was away past the wait). SQLite cannot widen a CHECK, so the table is rebuilt, as 102 did; a
//! row from before keeps its values and gets the defaults (no peer, nothing owed).
//!
//! `ask_peers` is the fence, one row per pairing epoch of a computer and direction (`in`: asks that computer sends here,
//! mirroring the host's `peers_in`; `out`: asks sent there, mirroring `peers_out`; two computers paired both ways hold two
//! pairings). The epoch is the host's own pairing id (TEXT, a hash with no order). A row is `live` until its pairing ends and
//! is never made live again; at most one epoch per (computer, direction) is live. It is written first when a pairing starts and
//! ends in the same transaction as the asks of its epoch (`ask.endPeer`); no SQL foreign key (lane rule).

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 103,
    name: "ask_peers",
    sql: r"
        CREATE TABLE asks_peers (
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
            state TEXT NOT NULL CHECK (state IN ('open', 'answered', 'capped', 'stopped', 'lost', 'refused', 'cut_off')),
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
            mode TEXT CHECK (mode IS NULL OR mode IN ('fg', 'bg', 'peer')),
            task TEXT,
            out_path TEXT,
            parked_card TEXT,
            parked_ms INTEGER,
            turn INTEGER,
            cli_pid INTEGER,
            cli_start TEXT,
            session TEXT,
            peer TEXT,
            peer_epoch TEXT,
            sent INTEGER NOT NULL DEFAULT 0 CHECK (sent IN (0, 1)),
            fetched INTEGER NOT NULL DEFAULT 0,
            stop_owed INTEGER NOT NULL DEFAULT 0 CHECK (stop_owed IN (0, 1)),
            peer_seen_ms INTEGER,
            origin_computer TEXT,
            origin_key TEXT,
            origin_epoch TEXT
        ) WITHOUT ROWID;
        INSERT INTO asks_peers (id, chain, parent, reply_session, hop, cross_hops, crossed, asked, caller_agent,
            caller_person, caller_via, conversation, cost, cost_base, cap_usd, state, at_ms, answered_ms, capped_ms,
            cap_card_ms, ended_ms, pid, pid_at_ms, brief, done, delivered, woke_answer, extra, claim_id, mode, task,
            out_path, parked_card, parked_ms, turn, cli_pid, cli_start, session)
        SELECT id, chain, parent, reply_session, hop, cross_hops, crossed, asked, caller_agent,
            caller_person, caller_via, conversation, cost, cost_base, cap_usd, state, at_ms, answered_ms, capped_ms,
            cap_card_ms, ended_ms, pid, pid_at_ms, brief, done, delivered, woke_answer, extra, claim_id, mode, task,
            out_path, parked_card, parked_ms, turn, cli_pid, cli_start, session
        FROM asks;
        DROP TABLE asks;
        ALTER TABLE asks_peers RENAME TO asks;
        CREATE INDEX asks_chain ON asks (chain);
        CREATE INDEX asks_reply_session ON asks (reply_session);
        CREATE INDEX asks_turn ON asks (turn) WHERE turn IS NOT NULL;
        CREATE UNIQUE INDEX asks_origin ON asks (origin_computer, origin_key) WHERE origin_computer IS NOT NULL;
        CREATE INDEX asks_peer ON asks (peer) WHERE peer IS NOT NULL;

        CREATE TABLE ask_peers (
            computer TEXT NOT NULL,
            dir TEXT NOT NULL CHECK (dir IN ('in', 'out')),
            epoch TEXT NOT NULL,
            live INTEGER NOT NULL CHECK (live IN (0, 1)),
            PRIMARY KEY (computer, dir, epoch)
        ) WITHOUT ROWID;
        CREATE UNIQUE INDEX ask_peers_live ON ask_peers (computer, dir) WHERE live = 1;
    ",
};
