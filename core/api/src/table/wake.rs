//! M3 lane (wake, M3-7): the wake queue and the waiters in the core (tables in `core/runtime/src/migrations/wake/`).
//! Experimental, owner only, not in `released.json`. The core answers them (`wake::call` in kith-core, the daemon routes
//! them), but nothing calls them yet: `kernel/bin/kith-wake` stays the writer of the queue files until the switch (M3-10),
//! so the core's copy is only what `import` last read plus what a test wrote. The daemon runs no drain and fires no waiter.
//!
//! `wake.list` and `wake.status` already name the kernel's `--json` forms of kith-wake (`json_forms_more`, served by the
//! Python), so the core's are `wake.queueList` and `wake.queueStatus` until the switch retires those.
//!
//! The answers carry `lines`, what `kith-wake` prints for the same call, so a CLI that switches prints them as they were:
//! `enqueue` says `queued`, `merged` or `duplicate`; `take` the turn (`turn`, or null when nothing is owed: exit 1).
//! `person` and `via` of `wake.wait` are the caller's `KITH_PERSON` and `KITH_VIA`; a waiter is refused for a person and
//! for a `via` outside terminal, ask, schedule and wake.

use super::build::*;

const PERSON: Param = caller(
    "person",
    "string",
    "The caller's KITH_PERSON (a guest may not set waiters).",
);
const VIA: Param = caller("via", "string", "The caller's KITH_VIA (default terminal).");

const fn need(
    name: &'static str,
    ty: &'static str,
    spellings: &'static [&'static str],
    summary: &'static str,
) -> Param {
    Param {
        name,
        ty,
        required: true,
        arg: crate::Arg::Flag(spellings),
        summary,
    }
}

pub(super) const ROWS: &[Method] = &[
    wake(
        "wake.enqueue",
        "Asks for a turn of a session. Answers `result`: `queued`, `merged` (the session was already owed something: one turn carries both) or `duplicate` (the id is queued, answered or withdrawn: dropped). Refuses a person's session on the run door, and a session that is another person's or agent's.",
        &[
            pos("session", "string", true, "The session to wake."),
            need(
                "id",
                "string",
                &["--id"],
                "The request's id; one already queued, answered or withdrawn is dropped.",
            ),
            need(
                "agent",
                "string",
                &["--agent"],
                "The agent whose session it is.",
            ),
            flag(
                "person",
                "string",
                &["--person"],
                "The person the session serves (needs `--door host`).",
            ),
            flag("room", "string", &["--room"], "The room it was asked in."),
            flag(
                "door",
                "string",
                &["--door"],
                "`run` (the kernel resumes the session; the default) or `host` (the host runs the turn).",
            ),
            flag("why", "string", &["--why"], "A few words on what happened."),
            after("text", "What the session is told."),
        ],
        &["wake", "enqueue"],
    ),
    wake(
        "wake.withdraw",
        "Stop: drops what a session is still owed (not what a running turn already holds), keeping the ids so a late copy is dropped; without `--id` its waiters go too. Answers `count`.",
        &[
            pos("session", "string", true, "The session."),
            flag("id", "string", &["--id"], "Only this request."),
        ],
        &["wake", "withdraw"],
    ),
    wake(
        "wake.withdrawPerson",
        "Revoke or removal: drops what every session of these people is owed, and those sessions' waiters. Answers `count`.",
        &[rest("persons", "The people.")],
        &["wake", "withdraw-person"],
    ),
    wake(
        "wake.take",
        "The host starts a host-door turn: marks what a session is owed as carried by this holder and answers `turn` (the turn as `kith-wake take` prints it), or null when nothing is owed. Recovers turns whose holder died first (a turn whose marker is in its transcript is answered, one that is not is owed again). A session a live writer holds is not offered (W3). Creates no turn in the conversations lane yet.",
        &[
            need("holder", "int", &["--holder"], "The holder's process id."),
            flag("session", "string", &["--session"], "Only this session."),
            flag(
                "sid",
                "string",
                &["--sid"],
                "The Claude session the turn runs in (default: the session itself, when it is one).",
            ),
        ],
        &["wake", "take"],
    ),
    wake(
        "wake.done",
        "The turn ended: what it carried is answered (`--how withdrawn`: dropped, not delivered). Answers `done`: false when no turn has that run id.",
        &[
            pos("run", "string", true, "The turn."),
            flag(
                "how",
                "string",
                &["--how"],
                "How it ended (default answered).",
            ),
        ],
        &["wake", "done"],
    ),
    wake(
        "wake.back",
        "The turn did not start: what it carried is owed again, after a pause that doubles (5 s to 10 min), and after five failed tries it ends as failed. `--held` (a writer had the session) is not a failed try. Answers `done`: false when no turn has that run id.",
        &[
            pos("run", "string", true, "The turn."),
            sw(
                "held",
                &["--held"],
                "A writer held the session: owed again soon, not a failed try.",
            ),
        ],
        &["wake", "back"],
    ),
    wake(
        "wake.queueList",
        "What is owed and what is waited for, as `items` (a queue file each), `waiters` and `lines` (what `kith-wake ls` prints).",
        &[],
        &["wake", "queue-list"],
    ),
    wake(
        "wake.queueStatus",
        "Where a request is: `queued`, `running`, how it was answered (`answered`, `failed`, `withdrawn`, ...) or `unknown`, and the turn that carried it, as `{id, state, run}`.",
        &[pos("id", "string", true, "The request's id.")],
        &["wake", "queue-status"],
    ),
    wake(
        "wake.wait",
        "Wakes a session once something happened: `ci:OWNER/REPO#PR` (its checks finished, or it was merged or closed), `pid:N` (the process exited) or `at:TIME` (`+30m`, or a local `2026-10-27T15:00`). A waiter always has a timeout. Waiting twice on the same thing keeps the first waiter. Answers the waiter as `waiter`.",
        &[
            need("on", "string", &["--on"], "What to wait for."),
            flag("session", "string", &["--session"], "The session to wake."),
            flag(
                "agent",
                "string",
                &["--agent"],
                "The agent whose session it is.",
            ),
            flag(
                "why",
                "string",
                &["--why"],
                "A few words, told to the session when it wakes.",
            ),
            flag(
                "timeout",
                "string",
                &["--timeout"],
                "Give up after this long (default 6h).",
            ),
            words("note", "A note for the session, told when it wakes."),
            PERSON,
            VIA,
        ],
        &["wake", "wait"],
    ),
];

const fn wake(
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
