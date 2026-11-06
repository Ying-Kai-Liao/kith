//! Version 14 (13 is `m0013_agent_access`): the removals whose revoke order has not finished (M2-18c).
//!
//! `person --rm` deletes the person's and their bots' `people` rows in one write, so a walk of `people.bot_of` after it
//! finds no bot, and a crash between that write and the revoke order would leave the person's and the bots' claims and
//! asks alive. The removal therefore also writes a row here, in the same transaction, for EVERY removal (a person with
//! no bots too: their own in-flight claims need the order as well). The row means "removed, order not finished":
//! - the core runs the order itself right after the removal commits, and deletes the row only after its last step;
//! - the core runs it again for every row left (a crash, an error mid-order) at start and on the supervisor's pass.
//!   It is level-triggered: each pass re-reads every row, and a row goes only after its effect.
//! - while a row names an id (as the person or as a bot), `people.addPerson` and `people.addBot` refuse that id, in
//!   their own transaction, so a recorded name can not vouch for someone who joined again.
//!
//! Columns:
//! - `person`: the id removed (no SQL foreign key: the row is gone from `people`).
//! - `bots`: a JSON array of the bots' ids (the whole transitive set, possibly empty), in the order the removal found them.
//! - `at`: milliseconds since the epoch, for whoever finds a row that never clears.

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 14,
    name: "open_removals",
    sql: "
        CREATE TABLE open_removals (
            person TEXT PRIMARY KEY,
            bots TEXT NOT NULL,
            at INTEGER NOT NULL
        ) WITHOUT ROWID;
    ",
};
