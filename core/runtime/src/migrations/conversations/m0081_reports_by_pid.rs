//! Version 81: reports are keyed by `(claim_id, kind, pid)`. With `(claim_id, kind)` alone, the exit of a process that was
//! not the claim's writer (a respawn that lost the lock) was kept and acked, and the real writer's own exit of the same
//! kind was then ignored by the key, leaving the claim `running` for ever. `pid` is the process the report names (0 when
//! it names none), so a retry of the same report is still idempotent and a report about another process is its own row.

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 81,
    name: "reports_by_pid",
    sql: "
        CREATE TABLE reports_new (
            claim_id INTEGER NOT NULL,
            kind TEXT NOT NULL,
            pid INTEGER NOT NULL DEFAULT 0,
            body TEXT NOT NULL DEFAULT '{}',
            received_at INTEGER NOT NULL,
            acked_at INTEGER,
            PRIMARY KEY (claim_id, kind, pid)
        ) WITHOUT ROWID;
        INSERT INTO reports_new (claim_id, kind, pid, body, received_at, acked_at)
            SELECT claim_id, kind, CASE WHEN json_valid(body) THEN COALESCE(json_extract(body, '$.pid'), 0) ELSE 0 END, body, received_at, acked_at FROM reports;
        DROP TABLE reports;
        ALTER TABLE reports_new RENAME TO reports;
        CREATE INDEX reports_unacked ON reports (claim_id) WHERE acked_at IS NULL;
    ",
};
