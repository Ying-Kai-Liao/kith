//! Version 92: the revoke epoch of the person a wake run's turn was made for, read when the turn was queued (S4 R4.4). A run
//! whose turn was cancelled by a revoke has no claim to say why, so the tick compares this with the person's epoch now: a
//! moved epoch means the run was fenced and ends `withdrawn`, not given back. Null for a link made before this step.

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 92,
    name: "wake_turn_epoch",
    sql: "ALTER TABLE wake_turns ADD COLUMN revoke_seen INTEGER;",
};
