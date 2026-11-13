//! Version 80: conversations, claims, turns and reports (runtime-design section 4, the start protocol). Nothing writes
//! these tables in production yet: the claim code is M3-2 on, and terminal.rs keeps the claim files until M3-11.
//!
//! Tables:
//! - `conversations`: one row per conversation. The engine session is a pointer (`engine_session`, `transcript_ptr`:
//!   a Claude session is a directory, not a file) and the hash the core last read (`transcript_hash`, section 4
//!   "Transcripts"). `account` is the account the conversation runs on; there is no mid-life move (q216-q219), a failover
//!   starts a new claim on another account and records it on that claim.
//! - `claims`: the writer claim. `claim_id` is the id from `counters.claim_id` (`Core::next_claim_id`), so it is never
//!   reused. `kind`: `terminal` (the holder's TUI), `turn` (a headless prompt) or `external` (a `--direct` run, with
//!   `pid` and `pid_started` so a reused pid is never mistaken for the writer). `state`: `queued` (a take-over
//!   waiting for the current writer to be gone; at most one per conversation, `claims_one_queued`), `claimed` (held,
//!   spawn sent), `running` (redeemed), `ending` (a revoke, stop or take-over sent the kill and the process is not yet
//!   gone: still the writer, persists across restarts, the kill is re-sent until the holder reports it gone, step 6),
//!   `ended` (gone). At most one claim per conversation is `claimed`, `running` or `ending` (the partial unique index
//!   `claims_one_writer`: the row itself refuses a second writer). `pid` and `pid_started` (text, as `lock::process_start`
//!   gives it) are the writer's process: an external run's own (required, CHECK), or for terminal and turn claims the
//!   stub's, recorded by the redeem (M3-2); `person` and `turn` say whose work it is.
//!   `generation` is the core's at the claim; `holder_instance` is the holder the spawn went to (step 7).
//! - `turns`: the queue of work. A turn names its current `claim_id` (NULL while queued or parked). `parked` turns wait
//!   for the writer lock (`parked_reason` = `lock_busy`, `terminal_live`); `deadline` expires a scheduled run (q218).
//! - `reports`: what the holder or stub told the core about a claim (`spawned`, `busy`, `done`, `exit`; the set is
//!   checked in code, so a new kind needs no table rebuild). Keyed by
//!   `(claim_id, kind)` and kept until `acked_at` is set, never overwritten: `INSERT OR IGNORE` makes a retried report
//!   idempotent, a `busy` cannot replace an unacked `done`, and a report for an old claim id lands under that id, never on
//!   the current claim's. The core acts only on reports whose claim id is the conversation's current claim. Precedence when
//!   several are unacked for one claim: `done`, then `exit`, then `busy`, then `spawned` (the first that applies wins,
//!   the rest are acked with it). `claim_id` has no SQL foreign key, so a report for an unknown claim id is a no-op the
//!   code rejects, not an error.
//!
//! Every step runs with foreign keys on, in its own transaction, so a later step cannot rebuild `claims` (`turns` points
//! at it). The value lists checked in SQL are therefore only those that cannot grow (`claims.state`, `claims.kind`,
//! `turns.state`); `turns.source`, `turns.parked_reason` and `reports.kind` are validated in code.
//!
//! How the start protocol (section 4) maps to rows, each step in one `Core::claim_tx`:
//! 1. Claim: re-check may (other lanes' tables, in code), `next_claim_id`, `INSERT claims (state 'claimed', generation)`;
//!    `UPDATE turns SET claim_id, state 'claimed'`. The partial index fails the insert when a writer exists.
//! 2. Spawn: after commit; `claims.holder_instance` is set in the step-1 transaction.
//! 3. Redeem (the stub, through the core): CAS `claims` 'claimed' -> 'running' (only if still current), recording
//!    `running_at`, `pid`, `pid_started` (and the holder instance); `turns` 'running'.
//!    A busy lock: `claims` -> 'ended' (`end_reason` 'busy'), `turns` -> 'parked' (`lock_busy`, `claim_id` NULL).
//!    Revoke or stop: `claims` -> 'ending' (the kill is re-sent until gone, then 'ended') and the turn dropped or requeued
//!    in the same transaction.
//! 4. Exit: `reports` row kept; the ack sets `reports.acked_at`, ends the claim and marks the turn 'done' or 'queued'.
//! 5. Reconcile: after the generation bump, one transaction ends the claims the holder does not list and requeues their
//!    turns; claims are refused until it commits (the check is in code, the generation guard is `claim_tx`).
//! 6. Revoke: `turns` of the person in 'queued' / 'parked' are cancelled, live claims 'ending' (`claims.person`).
//! 7. A different holder: `claims` with the old `holder_instance` and state 'claimed' / 'running' / 'ending' are the ones
//!    to end (index `claims_holder`); the pids to kill are the claim's recorded `pid`/`pid_started` (the redeem's), plus
//!    `meta.holder_procs` for processes with no redeem, matched by pid and start time. A running claim the new instance
//!    does not list goes to 'ending', never to 'ended', until that pid is confirmed gone; a 'claimed' one is ended by
//!    CAS so a late redeem fails.
//!
//! No SQL foreign key to another lane's table (people, agents, cards): those links live in code. The tables here point
//! at each other, which is the same lane.

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 80,
    name: "conversations",
    sql: "
        CREATE TABLE conversations (
            id TEXT PRIMARY KEY,
            agent TEXT NOT NULL,
            engine TEXT NOT NULL,
            engine_session TEXT,
            transcript_ptr TEXT,
            transcript_hash TEXT,
            account TEXT,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );
        CREATE INDEX conversations_agent ON conversations (agent);

        CREATE TABLE claims (
            claim_id INTEGER PRIMARY KEY,
            conversation TEXT NOT NULL REFERENCES conversations (id),
            generation INTEGER NOT NULL,
            kind TEXT NOT NULL CHECK (kind IN ('terminal', 'turn', 'external')),
            state TEXT NOT NULL CHECK (state IN ('queued', 'claimed', 'running', 'ending', 'ended')),
            holder_instance TEXT,
            person TEXT NOT NULL DEFAULT '',
            turn INTEGER,
            pid INTEGER,
            pid_started TEXT,
            account TEXT,
            end_reason TEXT,
            created_at INTEGER NOT NULL,
            claimed_at INTEGER,
            running_at INTEGER,
            ended_at INTEGER,
            CHECK (kind <> 'external' OR (pid IS NOT NULL AND pid_started IS NOT NULL))
        );
        CREATE UNIQUE INDEX claims_one_writer ON claims (conversation) WHERE state IN ('claimed', 'running', 'ending');
        CREATE UNIQUE INDEX claims_one_queued ON claims (conversation) WHERE state = 'queued';
        CREATE INDEX claims_conversation ON claims (conversation, claim_id);
        CREATE INDEX claims_holder ON claims (holder_instance) WHERE state IN ('claimed', 'running', 'ending');

        CREATE TABLE turns (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            conversation TEXT NOT NULL REFERENCES conversations (id),
            claim_id INTEGER REFERENCES claims (claim_id),
            person TEXT NOT NULL DEFAULT '',
            source TEXT NOT NULL,
            prompt TEXT NOT NULL DEFAULT '',
            state TEXT NOT NULL CHECK (state IN
                ('queued', 'claimed', 'running', 'parked', 'done', 'failed', 'cancelled', 'expired')),
            parked_reason TEXT,
            parked_at INTEGER,
            deadline INTEGER,
            requeues INTEGER NOT NULL DEFAULT 0,
            queued_at INTEGER NOT NULL,
            started_at INTEGER,
            finished_at INTEGER,
            CHECK ((state = 'parked') = (parked_reason IS NOT NULL))
        );
        CREATE INDEX turns_queue ON turns (conversation, id) WHERE state IN ('queued', 'parked');
        CREATE INDEX turns_claim ON turns (claim_id) WHERE claim_id IS NOT NULL;

        CREATE TABLE reports (
            claim_id INTEGER NOT NULL,
            kind TEXT NOT NULL,
            body TEXT NOT NULL DEFAULT '{}',
            received_at INTEGER NOT NULL,
            acked_at INTEGER,
            PRIMARY KEY (claim_id, kind)
        ) WITHOUT ROWID;
        CREATE INDEX reports_unacked ON reports (claim_id) WHERE acked_at IS NULL;
    ",
};
