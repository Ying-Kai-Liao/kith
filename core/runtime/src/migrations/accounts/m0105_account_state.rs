//! Version 105: what each account is doing right now, one row per entry id (M3-9a). Today this is
//! `<machine>/accounts/state.json` (`{"version": 1, "accounts": {"claude:work": {...}}}`), written by `kith account
//! out|clear|usage` (Python) and by a launch's `last_started` (Python and Rust), under the flock on `state.json.lock`.
//!
//! NOTHING WRITES THIS TABLE IN PRODUCTION YET. The core owns account choice per run (runtime-design section 2), but every
//! writer (out, clear, usage, `last_started`, `--extend`, `own`) has to switch together, or `pick` would read a copy that the
//! file's writers do not update. Until then the rows come from `kith migrate --accounts` (import) and the tests, and the
//! readers (`account.state`, `account.pick`) are not wired to a run.
//!
//! At the switch, a `CoreState` read error must fail loudly: today it reads as empty state (the file's "unreadable is no
//! state" rule), which would hide every out mark and put a run on an account that is out.
//!
//! Column mapping (state.json entry -> row). The columns carry NO type affinity on purpose, so an integer stays an
//! integer and a float a float exactly as the file had them (Python's `float(v)` readers treat both the same, but the
//! stored value should read back as it was):
//! - the key -> `id`;
//! - `out_until` (epoch seconds, the account is out until then; past or 0 counts as clear), `reason`, `since` (when it was
//!   marked), `cleared_at` (set by `clear`; an out mark whose `since` is not after it is ignored by the host's marks),
//!   `cleared_host_until` (the host's own `out_until` at the clear), `last_started` (epoch seconds of the last launch):
//!   same names. A NULL is a key the entry did not have;
//! - `usage` -> `usage_json`, the whole `usage` object as JSON text: `{"read_at": ..., "windows": [{"name", "percent",
//!   "resets_at"}]}`. A JSON column, not a child table: the readers take the object whole (`usage_level` the fullest
//!   window of a fresh `read_at`; the listing re-renders it) and nothing queries a window on its own;
//! - `extra_json`: every other key of the entry, and any known key whose value is not the plain type above (a string
//!   `out_until`, a `null`, a bool), as one JSON object, so reading the row back gives the entry exactly as the file had
//!   it. The readers that would trip on such a value (`pyfloat` of a bool...) then trip the same way in both stores.
//!
//! For 9b (failover, not here): a running claim never moves to another account (q216). Failover happens only after a
//! turn ends on a usage limit: the ack transaction marks the account out here, the turn is requeued, and a NEW claim
//! starts from the top of the chain on the next account, at most once per entry. The one-writer rule is per
//! conversation, so a main account coming back (`clear`, or `out_until` passing) while the backup runs does not take
//! the conversation back: `claims_one_writer` refuses a second claim while the backup's is live.

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 105,
    name: "account_state",
    sql: "
        CREATE TABLE account_state (
            id TEXT PRIMARY KEY CHECK (id <> ''),
            out_until,
            reason,
            since,
            cleared_at,
            cleared_host_until,
            last_started,
            usage_json TEXT CHECK (usage_json IS NULL OR json_valid(usage_json)),
            extra_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(extra_json))
        ) WITHOUT ROWID;
    ",
};
