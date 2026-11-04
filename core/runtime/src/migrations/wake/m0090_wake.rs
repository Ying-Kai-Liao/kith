//! Version 90: the wake queue and the waiters (docs/plans/runtime-design.md section 2; `kernel/bin/kith-wake` is the writer
//! until the switch, M3-10). Nothing writes these tables in production yet: `import/wake.rs` copies the Python's files in,
//! and the `wake.*` rows run only when something calls them.
//!
//! Tables:
//! - `wake_sessions`: one row per session that is owed something (the Python's `sessions/queue/<hash>.json`). It carries
//!   whose session it is (`agent`, `person`, `room`), the `door` (`run`: the kernel resumes it; `host`: the host takes it),
//!   the retry state (`tries`, `next`: epoch seconds before which it is not offered again) and the turn in flight, if any
//!   (`turn_run`, with the deliverer's `turn_pid` and `turn_proc_start`: a pid is matched by its start time too, and
//!   `turn_sid`, the Claude session the `[kith wake RUN]` marker is looked for in). The row goes with its last request.
//! - `wake_items`: one row per request, the dedupe key being its `id` (rule 13: an id that is here or in `wake_answered`
//!   is dropped, whoever sends it again). `state` is `queued` (no turn has it) or `taken` (turn `run` carries it; it stays
//!   until the turn commits, W2). `seq` keeps the order requests came in, which is the order a merged turn lists them.
//!   A request that left the queue is not here: `wake_answered` holds it.
//! - `wake_answered`: the ids that were answered, failed, recovered or withdrawn (`how`, as `status` reports them),
//!   kept seven days (`t`, epoch seconds), with the turn that carried each (`run`). It is `answered.json`.
//! - `waiters`: code that watches for one thing and becomes one wake request. `kind` `ci` (`repo`, `pr`), `pid` (`pid`
//!   and `proc_start`: the process must have both to count as running) or `at` (`at`, epoch seconds). `until` is the
//!   waiter's own timeout and `seen` the head commit a CI waiter saw finished on its last look.
//!
//! `format` is the `v` of the Python's file the row came from. Every step runs with foreign keys on, in its own
//! transaction, and a later step cannot rebuild a table another table points at, so no list is checked in SQL that could
//! grow (`door`, `state`, a waiter's `kind`): the code validates them. The links between the tables are in code too.

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 90,
    name: "wake",
    sql: "
        CREATE TABLE wake_sessions (
            session TEXT PRIMARY KEY,
            agent TEXT NOT NULL,
            person TEXT,
            room TEXT,
            door TEXT NOT NULL,
            tries INTEGER NOT NULL DEFAULT 0,
            next REAL NOT NULL DEFAULT 0,
            turn_run TEXT,
            turn_pid INTEGER,
            turn_proc_start TEXT,
            turn_at TEXT,
            turn_sid TEXT,
            format INTEGER NOT NULL DEFAULT 1
        ) WITHOUT ROWID;

        CREATE TABLE wake_items (
            seq INTEGER PRIMARY KEY AUTOINCREMENT,
            id TEXT NOT NULL UNIQUE,
            session TEXT NOT NULL,
            why TEXT NOT NULL DEFAULT '',
            text TEXT NOT NULL,
            at TEXT NOT NULL,
            run TEXT,
            state TEXT NOT NULL
        );
        CREATE INDEX wake_items_session ON wake_items (session, seq);
        CREATE INDEX wake_items_run ON wake_items (run) WHERE run IS NOT NULL;

        CREATE TABLE wake_answered (
            id TEXT PRIMARY KEY,
            how TEXT NOT NULL,
            t REAL NOT NULL,
            run TEXT
        ) WITHOUT ROWID;

        CREATE TABLE waiters (
            id TEXT PRIMARY KEY,
            session TEXT NOT NULL,
            agent TEXT NOT NULL,
            waits_on TEXT NOT NULL,
            kind TEXT NOT NULL,
            repo TEXT,
            pr INTEGER,
            pid INTEGER,
            proc_start TEXT,
            at REAL,
            why TEXT NOT NULL DEFAULT '',
            note TEXT NOT NULL DEFAULT '',
            created REAL NOT NULL DEFAULT 0,
            checked REAL NOT NULL DEFAULT 0,
            seen TEXT,
            until REAL NOT NULL,
            format INTEGER NOT NULL DEFAULT 1
        ) WITHOUT ROWID;
        CREATE INDEX waiters_session ON waiters (session);
    ",
};
