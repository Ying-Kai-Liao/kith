//! M2 lane G (doors and their sweep, M2-14): the guest doors' state in the core. Experimental and owner only, served by
//! the daemon (`Core::call` itself answers NotServed: a start, an end and a sweep change state, so the daemon runs them
//! on `&mut Core`). Nothing calls them yet: `kernel/lib/kith_door.py` and the host's sweep (`serve/host/src/doors.rs`)
//! keep their own claim files until a later step switches them, so a door started the old way has no row here and a
//! core sweep cannot see it.
//!
//! A door start is refused for a person who is cut off or may not run the agent. A sweep marks doors swept and says
//! which; it kills nothing, the caller stops the boxes and confirms with `doors.end`. `people.suspend`,
//! `people.removePerson` and `people.removeBot` sweep the doors of the people they name in their own transaction and
//! answer them under `doors`. The caller's view (`kernel`, `home`, `env`) is where the agent folders are.

use super::build::*;

const HOME: Param = caller(
    "home",
    "string",
    "The caller's home folder (a daemon has its own).",
);
const ENV: Param = caller(
    "env",
    "{string}",
    "The caller's KITH_* and HQ_* variables (a daemon has its own).",
);

pub(super) const ROWS: &[Method] = &[
    door(
        "doors.list",
        "The guest doors that are starting or live, and swept ones whose box is not confirmed gone, as `{doors}`. With `--all` the closed ones kept for a day too.",
        &[sw(
            "all",
            &["--all"],
            "Include doors that ended or were confirmed gone.",
        )],
        &["doors", "ls"],
    ),
    door(
        "doors.start",
        "Starts a door for a person: the access check and the row in one step. Answers the door (`state` starting, its `version`) and `repeat` (true when this id was already starting or live, nothing written). Refuses, as `kith-auth door` does: the owner (doors are for the people an agent is shared with), a person who is cut off or may not run the agent, a Mac out of sync with the cloud, a door that is not among the doors their ticked jobs grant now (a bot's cut to its owner's); then a supervisor that is not running and an id that is a swept or ended door.",
        &[
            pos("agent", "string", true, "The agent whose door it is."),
            pos(
                "door",
                "string",
                true,
                "The door as `name@digest`, the piece a job grants.",
            ),
            pos("person", "string", true, "The person (or bot) it runs for."),
            flag(
                "folder",
                "string",
                &["--folder"],
                "The person's folder it runs on (display only).",
            ),
            flag(
                "id",
                "string",
                &["--id"],
                "The door's id, 16 hex digits (a new one when absent). A start retried with it is the same door.",
            ),
            Param {
                name: "pid",
                ty: "int",
                required: true,
                arg: crate::Arg::Flag(&["--pid"]),
                summary: "The supervisor's process (2 or more), so a crash is found by its start time.",
            },
            KERNEL,
            HOME,
            ENV,
        ],
        &["doors", "start"],
    ),
    door(
        "doors.markLive",
        "The supervisor says the box is up. Answers `{result: live}`, or `{result: lost, state}` when a sweep (or an end) got to the door first: the box must not run. Only a `starting` door at this `version` goes live.",
        &[
            pos("id", "string", true, "The door."),
            Param {
                name: "version",
                ty: "int",
                required: true,
                arg: crate::Arg::Flag(&["--version"]),
                summary: "The version `doors.start` answered.",
            },
        ],
        &["doors", "live"],
    ),
    door(
        "doors.end",
        "The box is gone. An open door becomes ended; a swept one is confirmed gone (it stays swept). Answers `{result: ended}` or `{result: swept}`. Ending twice changes nothing; refuses an id that is no door.",
        &[pos("id", "string", true, "The door.")],
        &["doors", "end"],
    ),
    door(
        "doors.sweep",
        "Marks swept every open door of the named people and their bots, every door whose supervisor is gone, and (unless `--no-recheck`) every door whose person may no longer run its agent. Answers `swept` (this call's) and `pending` (swept before, box not confirmed gone). The owner is never swept by a name, only for a gone supervisor. Kills nothing.",
        &[
            rest("persons", "People whose doors all end."),
            sw(
                "noRecheck",
                &["--no-recheck"],
                "Do not ask again whether each person may still run their door (it does by default).",
            ),
            KERNEL,
            HOME,
            ENV,
        ],
        &["doors", "sweep"],
    ),
];

const fn door(
    name: &'static str,
    summary: &'static str,
    params: &'static [Param],
    path: &'static [&'static str],
) -> Method {
    row(
        name,
        summary,
        params,
        "json",
        path,
        Served::Core,
        Stability::Experimental,
    )
    .formats(J)
}
