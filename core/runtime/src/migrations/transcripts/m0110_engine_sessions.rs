//! Version 110: the engine sessions of a conversation (runtime-design section 4, "Transcripts"). Nothing writes this
//! table in production yet: `import/transcripts.rs` fills it from the session registry, and the place-or-adopt step
//! (`transcripts/`) is built and tested but not called by any start until the switch.
//!
//! One row per `(conversation, account)`. A failover starts a new engine session on the backup account (q216), so one
//! pointer on `conversations` is not enough; `conversations.engine_session`, `transcript_ptr` and `transcript_hash`
//! (m0080) stay as they are and nothing reads them: **this table is the source**.
//!
//! - `account`: the account id the session was made under (`claude`, `claude:work`; `claude` for a registry record
//!   that names none, the engine's default login). Never NULL, so it can be part of the key.
//! - `engine`, `session_id`: which engine and its id for the session (a Claude session id).
//! - `pointer`: where the engine keeps it, as the path of the session's main file
//!   (`<config dir>/projects/<slug>/<sid>.jsonl` for Claude; the sidecar folder `<sid>/` sits beside it). `''` when no
//!   file is known. Stored as text; a Windows path keeps its drive and separators as the engine wrote them.
//! - `cwd`: the folder the session ran in (the registry's `cwd`), `''` when unknown.
//! - `hash`, `size`: of the session's files as of the last read (`transcripts::hash_session`), NULL when the engine's
//!   copy was missing then. `last_read_at`: seconds, NULL when never read.
//!
//! No SQL foreign key to `conversations` (another lane's table; that link lives in code).

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 110,
    name: "engine_sessions",
    sql: "
        CREATE TABLE engine_sessions (
            conversation TEXT NOT NULL,
            account TEXT NOT NULL,
            engine TEXT NOT NULL,
            session_id TEXT NOT NULL,
            pointer TEXT NOT NULL DEFAULT '',
            cwd TEXT NOT NULL DEFAULT '',
            hash TEXT,
            size INTEGER,
            last_read_at INTEGER,
            created_at INTEGER NOT NULL,
            PRIMARY KEY (conversation, account)
        ) WITHOUT ROWID;
        CREATE INDEX engine_sessions_session ON engine_sessions (engine, session_id);
    ",
};
