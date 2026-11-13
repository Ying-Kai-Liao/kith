//! M2 lane D (resource gate): the slots a turn takes before it starts, and (M2-10) the cloud sync lease. Experimental
//! and owner only, served by the daemon (`Core::call` itself answers NotServed: both change state, so the daemon runs
//! them on `&mut Core`). Nothing calls them yet: the host's own gate and lease still decide until lane H switches its
//! callers.

use super::build::*;
use crate::{Arg, Method};

pub(super) const ROWS: &[Method] = &[
    row(
        "gate.list",
        "The gate's slots and the line, in the order the line is kept. Frees holders whose process is gone first.",
        &[],
        "json",
        &["gate", "list"],
        Served::Core,
        Stability::Experimental,
    ),
    row(
        "gate.take",
        "Asks for a slot. Answers `granted`, `waiting` (with a position), `parked` (gave its slot up, not in line) or `gone` (holds nothing). Asking again for the same holder changes nothing.",
        &[
            pos(
                "holder",
                "string",
                true,
                "The run or claim that wants the slot.",
            ),
            flag(
                "rt",
                "string",
                &["--rt"],
                "The actor it belongs to (display only; the holder when absent).",
            ),
            flag(
                "max",
                "int",
                &["--max"],
                "How many slots there are (1 or more). Remembered; later takes may leave it out.",
            ),
            sw(
                "invitee",
                &["--invitee"],
                "An invited person's turn: behind the owner's, and few at once.",
            ),
            Param {
                name: "pid",
                ty: "int",
                required: true,
                arg: Arg::Flag(&["--pid"]),
                summary: "The holder's process (2 or more), so a crash frees the slot.",
            },
        ],
        "json",
        &["gate", "take"],
        Served::Core,
        Stability::Experimental,
    ),
    row(
        "gate.release",
        "Gives a slot or a place in line back. Releasing a holder that holds nothing does nothing.",
        &[pos("holder", "string", true, "The run or claim.")],
        "json",
        &["gate", "release"],
        Served::Core,
        Stability::Experimental,
    ),
    row(
        "gate.park",
        "The holder waits on another bot's reply: its slot is given up. Nested asks share one parking.",
        &[pos("holder", "string", true, "The run or claim.")],
        "json",
        &["gate", "park"],
        Served::Core,
        Stability::Experimental,
    ),
    row(
        "gate.unpark",
        "One ask is back; the last one puts the holder in line again, ahead of turns that have not started.",
        &[pos("holder", "string", true, "The run or claim.")],
        "json",
        &["gate", "unpark"],
        Served::Core,
        Stability::Experimental,
    ),
    row(
        "syncLease.status",
        "Whether this Mac is in sync with the cloud. Answers `cloud` (`unknown`, `linked` or `off`), `synced`, `held` (what a non-owner's claim needs: the cloud is off, or linked with the lease unexpired), `until` (ms) and `mark`.",
        &[],
        "json",
        &["sync-lease", "status"],
        Served::Core,
        Stability::Experimental,
    ),
    row(
        "syncLease.link",
        "The cloud is on: the lease decides from now on, and it is not held until a pull says so. Linking again changes nothing.",
        &[],
        "json",
        &["sync-lease", "link"],
        Served::Core,
        Stability::Experimental,
    )
    .principals(HOST)
    .no_cli(),
    row(
        "syncLease.unlink",
        "The cloud is off on purpose: nothing waits on it, and any lease is gone.",
        &[],
        "json",
        &["sync-lease", "unlink"],
        Served::Core,
        Stability::Experimental,
    )
    .principals(HOST)
    .no_cli(),
    row(
        "syncLease.mark",
        "Where the cloud's changes stand. Take it before a pull reads the cloud's state, and give it back to `syncLease.pulled`.",
        &[],
        "json",
        &["sync-lease", "mark"],
        Served::Core,
        Stability::Experimental,
    )
    .principals(OWNER_HOST),
    row(
        "syncLease.changed",
        "The cloud said its state changed: the lease ends at once, until a pull that started after this succeeds. Answers the new `mark`.",
        &[],
        "json",
        &["sync-lease", "changed"],
        Served::Core,
        Stability::Experimental,
    )
    .principals(HOST)
    .no_cli(),
    row(
        "syncLease.drop",
        "The connection to the cloud dropped or is being made: the lease ends. Dropping when nothing is held does nothing.",
        &[],
        "json",
        &["sync-lease", "drop"],
        Served::Core,
        Stability::Experimental,
    )
    .principals(HOST)
    .no_cli(),
    row(
        "syncLease.pulled",
        "A pull of the cloud's state succeeded. Answers `held: true` when the lease now runs, `held: false` when it does not: the cloud changed since the mark, the cloud is not linked, or the clock is behind its last reading.",
        &[
            pos(
                "mark",
                "int",
                true,
                "The `mark` taken before the pull read the cloud's state.",
            ),
            flag(
                "ms",
                "int",
                &["--ms"],
                "How long the lease runs, 1 to 3600000 (120000 when absent).",
            ),
        ],
        "json",
        &["sync-lease", "pulled"],
        Served::Core,
        Stability::Experimental,
    )
    .principals(HOST)
    .no_cli(),
    row(
        "syncLease.renew",
        "The relay was heard: runs a held lease out again. Answers `renewed: true`, or `renewed: false` when nothing is held to renew (never acquired, lost, expired or the clock is behind): only a pull acquires.",
        &[flag(
            "ms",
            "int",
            &["--ms"],
            "How long the lease runs from now, 1 to 3600000 (120000 when absent).",
        )],
        "json",
        &["sync-lease", "renew"],
        Served::Core,
        Stability::Experimental,
    )
    .principals(HOST)
    .no_cli(),
];
