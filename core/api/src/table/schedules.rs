//! M4 (schedules): one `Trigger` type for every schedule (cron with a per-job zone, interval, once, webhook, event; the
//! rules are in the kernel crate's schedule module). Experimental, owner only, not in `released.json`. `schedule.preview` is pure: it
//! reads no state and writes none, so the core answers it itself. The rows for the schedule store (jobs, runs) go in
//! this file when the store lane lands them.

use super::build::*;

pub(super) const ROWS: &[Method] = &[
    row(
        "schedule.preview",
        "The next fires of a trigger, as `{fires: [{at, iso}]}` (`at` in ms since the epoch): `{type: cron, expr, tz}` (tz `local` or an IANA zone), `{type: interval, every}` (seconds), `{type: once, at}`; a webhook or event has none. Refuses a spec `kith-sched` refuses, with its message.",
        &[
            pos(
                "trigger",
                "json",
                true,
                "The trigger, as JSON (`{\"type\":\"cron\",\"expr\":\"0 9 * * 1-5\",\"tz\":\"Europe/Paris\"}`).",
            ),
            flag(
                "after",
                "int",
                &["--after"],
                "Fires after this time, ms since the epoch (now by default).",
            ),
            flag(
                "n",
                "int",
                &["--n"],
                "How many fires (5 by default, at most 100).",
            ),
        ],
        "json",
        &["schedule", "preview"],
        Served::Core,
        Stability::Experimental,
    )
    .formats(J),
];
