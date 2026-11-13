//! Version 31: the cloud sync lease (M2-10), one row.
//!
//! `cloud` is `unknown` until something says otherwise, and unknown never counts as held. `off` is the cloud turned off
//! on purpose (nothing waits on it); `linked` is the cloud on, where `synced` and `until` (ms) say whether this Mac has
//! pulled the cloud's state on a live connection lately. `changes` counts the cloud's "something changed" notices, so a
//! pull that started before one cannot end the wait. `at` is the latest clock reading any change saw: a clock that
//! steps back below it never extends a lease.

use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 31,
    name: "sync_lease",
    sql: "
        CREATE TABLE sync_lease (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            cloud TEXT NOT NULL CHECK (cloud IN ('unknown', 'linked', 'off')),
            synced INTEGER NOT NULL CHECK (synced IN (0, 1)),
            until INTEGER NOT NULL CHECK (until >= 0),
            changes INTEGER NOT NULL CHECK (changes >= 0),
            at INTEGER NOT NULL CHECK (at >= 0)
        );
    ",
};
