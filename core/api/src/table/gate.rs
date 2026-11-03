//! M2 lane D (resource gate): the slots a turn takes before it starts. Experimental and owner only, served by the
//! daemon (`Core::call` itself answers NotServed: the gate changes state, so the daemon runs it on `&mut Core`).
//! Nothing calls them yet: the host's own gate still decides until lane H switches its callers. The sync lease (M2-10)
//! adds its rows here later.

use super::build::*;
use crate::Method;

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
        "Asks for a slot: `granted`, or `waiting` with a position. Asking again for the same holder changes nothing.",
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
                "How many slots there are (0 or more). Remembered for later calls.",
            ),
            sw(
                "invitee",
                &["--invitee"],
                "An invited person's turn: behind the owner's, and few at once.",
            ),
            flag(
                "pid",
                "int",
                &["--pid"],
                "The holder's process, so a crash frees the slot.",
            ),
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
];
