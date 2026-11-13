//! Version 82: the room basis, the per-message turn budget, the room conversation key and the warm processes
//! (designs/2026-11-14-m3-cutover section 1.5; rooms-with-people build plan, "How it fits the rebuild"). Nothing writes
//! these columns in production yet: `hostclaim.enqueue` fills them once the host passes them (rooms PR C).
//!
//! - `turns.level` is `owner` (a 1:1 turn, the default for every row that exists) or `room` (checked in code, as `source`
//!   is). A room turn's `person` is the run principal `room-<id>`; the people it speaks for are its basis, `turn_people`.
//!   `room` and `start_gen` say which room and which membership generation the turn started in.
//! - `turn_people`: the basis. `revoke_seen` is that person's revoke epoch when the turn was queued, so the enqueue and
//!   the claim's fence each refuse a turn whose basis person was revoked since. Written once with the turn.
//! - `turns.budget_root`, `budget_left`, `author`: the per-message turn budget. NULL `budget_left` is no budget (every
//!   old row, an owner-level turn). The budget is total turns of the message: `turn_budgets` holds one counter per chain
//!   (keyed by the root turn), spent in the enqueue transaction of each follow-up; `budget_left` of a turn is what the
//!   counter held when that turn was queued. `enq_key` makes an enqueue that is retried after a lost reply land on the same row,
//!   so the budget is spent once.
//! - `conversations.room`, `join_epoch`: a room-person conversation is the session of one (agent, room, join epoch); the
//!   unique index is what refuses a second one.
//! - `warm_procs`: every host engine process, idle or not, with the conversation it has loaded now, so a revoke reaches
//!   an idle one. `kill_at` is set once something decided it must die (a revoke, a host change); the row goes when the
//!   process is confirmed gone.
//!
//! The basis columns never change after the insert (triggers), so "fixed once" (R-a) is the table's rule, not a habit.

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 82,
    name: "room_basis",
    sql: "
        ALTER TABLE turns ADD COLUMN room TEXT;
        ALTER TABLE turns ADD COLUMN start_gen INTEGER;
        ALTER TABLE turns ADD COLUMN level TEXT NOT NULL DEFAULT 'owner';
        ALTER TABLE turns ADD COLUMN budget_root INTEGER;
        ALTER TABLE turns ADD COLUMN budget_left INTEGER;
        ALTER TABLE turns ADD COLUMN author TEXT;
        ALTER TABLE turns ADD COLUMN enq_key TEXT;
        CREATE UNIQUE INDEX turns_enq_key ON turns (conversation, enq_key) WHERE enq_key IS NOT NULL;

        CREATE TABLE turn_people (
            turn INTEGER NOT NULL REFERENCES turns (id),
            person TEXT NOT NULL,
            revoke_seen INTEGER NOT NULL,
            PRIMARY KEY (turn, person)
        ) WITHOUT ROWID;
        CREATE INDEX turn_people_person ON turn_people (person);

        CREATE TABLE turn_budgets (
            root INTEGER PRIMARY KEY REFERENCES turns (id),
            left INTEGER NOT NULL CHECK (left >= 0)
        );

        CREATE TRIGGER turns_basis_fixed BEFORE UPDATE OF room, start_gen, level, budget_root, budget_left, author, enq_key
            ON turns
            WHEN NEW.room IS NOT OLD.room OR NEW.start_gen IS NOT OLD.start_gen OR NEW.level IS NOT OLD.level
              OR NEW.budget_root IS NOT OLD.budget_root OR NEW.budget_left IS NOT OLD.budget_left
              OR NEW.author IS NOT OLD.author OR NEW.enq_key IS NOT OLD.enq_key
        BEGIN
            SELECT RAISE(ABORT, 'kith-core: a turn''s basis, level and budget are fixed when it is queued');
        END;
        CREATE TRIGGER turn_people_fixed BEFORE UPDATE ON turn_people
        BEGIN
            SELECT RAISE(ABORT, 'kith-core: a turn''s basis is fixed when it is queued');
        END;

        ALTER TABLE conversations ADD COLUMN room TEXT;
        ALTER TABLE conversations ADD COLUMN join_epoch INTEGER;
        CREATE UNIQUE INDEX conversations_room_epoch ON conversations (agent, room, join_epoch) WHERE room IS NOT NULL;

        CREATE TABLE warm_procs (
            pid INTEGER NOT NULL,
            started TEXT NOT NULL,
            instance TEXT NOT NULL,
            person TEXT NOT NULL DEFAULT '',
            agent TEXT NOT NULL DEFAULT '',
            conversation TEXT,
            kill_at INTEGER,
            PRIMARY KEY (pid, started)
        ) WITHOUT ROWID;
        CREATE INDEX warm_procs_person ON warm_procs (person);
        CREATE INDEX warm_procs_conversation ON warm_procs (conversation) WHERE conversation IS NOT NULL;
    ",
};
