//! Version 60: the guest doors' state, one row per door (M2-14). Today the state is a file per door under
//! `<machine>/run/<claim>.json` (`kernel/lib/kith_door.py`: `run` writes it, `sweep` reads every one), guarded by
//! kith-auth's flock. Here the row IS the claim: the start and the sweep both change this one row, each in a
//! `BEGIN IMMEDIATE` transaction, and the change goes by `version`, so a start that lost the race to a sweep finds the
//! row swept and starts nothing.
//!
//! Column mapping (`kith_door.py` claim file -> row):
//! - `claim` -> `id` (16 hex); `agent`, `door`, `person`: same; `pid` -> `pid`, `procStart` -> `proc_start` (the
//!   supervisor, so a dead one is found by its start time, never by its pid alone); `left` is the state `swept` with
//!   `reaped = 0` (the sweep asked for it to end and the box has not said it is gone).
//! - `folder` is new (the person's folder the door runs on); display only.
//!
//! `state`: `starting` (the row is written, the box is not up yet), `live` (the supervisor says the box runs),
//! `ended` (it ended by itself), `swept` (the core ordered it stopped: its person may no longer run it, its supervisor
//! is gone, or the person was cut off). `swept` and `ended` are final: nothing moves a row out of them, so an undo of
//! a revoke never brings a swept door back. `reaped` says the box is confirmed gone after a sweep.
//!
//! No SQL foreign key to the people table (another lane's): the link lives in code.

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 60,
    name: "doors",
    sql: "
        CREATE TABLE doors (
            id TEXT PRIMARY KEY,
            agent TEXT NOT NULL,
            door TEXT NOT NULL,
            person TEXT NOT NULL,
            folder TEXT NOT NULL DEFAULT '',
            pid INTEGER NOT NULL CHECK (pid > 1),
            proc_start TEXT NOT NULL DEFAULT '',
            state TEXT NOT NULL CHECK (state IN ('starting', 'live', 'ended', 'swept')),
            version INTEGER NOT NULL DEFAULT 0,
            reaped INTEGER NOT NULL DEFAULT 0 CHECK (reaped IN (0, 1)),
            why TEXT NOT NULL DEFAULT '',
            started_at INTEGER NOT NULL,
            changed_at INTEGER NOT NULL
        ) WITHOUT ROWID;
        CREATE INDEX doors_person ON doors (person, state);
    ",
};
