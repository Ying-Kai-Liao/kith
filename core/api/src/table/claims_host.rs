//! M3-10f: the host's door onto the writer claims (`core/runtime/src/hostclaim.rs`). Host principal only, never the owner,
//! a member or a bot (the host decides who may write; these rows only keep the one-writer rule). Experimental and
//! unswitched: nothing calls them unless the host runs with `KITH_HOST_CORE_CLAIMS=1`, which is off by default. No command
//! line reaches them.
//!
//! A conversation here is the Claude session id the host already names its claim files by. A turn claim is only made by
//! `hostclaim.turn` (`claim_next`, which picks the account); `hostclaim.claim` makes the terminal and `external` kinds.

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

/// A param the host always sends.
const fn need(name: &'static str, ty: &'static str, summary: &'static str) -> Param {
    Param {
        name,
        ty,
        required: true,
        arg: crate::Arg::Caller,
        summary,
    }
}

const fn opt(name: &'static str, ty: &'static str, summary: &'static str) -> Param {
    caller(name, ty, summary)
}

const fn host(name: &'static str, summary: &'static str, params: &'static [Param]) -> Method {
    row(
        name,
        summary,
        params,
        "json",
        &[],
        Served::Core,
        Stability::Experimental,
    )
    .principals(HOST)
    .no_cli()
}

pub(super) const ROWS: &[Method] = &[
    host(
        "hostclaim.ensure",
        "The conversation row for a session, made when it is missing (the host's terminal and turns name sessions the core may not have imported yet). Answers `{conversation}`.",
        &[
            need("conversation", "string", "The session id."),
            need("agent", "string", "The agent."),
            need("engine", "string", "The engine (claude)."),
        ],
    ),
    host(
        "hostclaim.epoch",
        "The person's revoke epoch: read it before the may check and carry it into `claim`, `turn` or `enqueue`. Answers `{epoch}`.",
        &[need("person", "string", "The claim's principal.")],
    ),
    host(
        "hostclaim.claim",
        "A terminal or `external` writer claim (step 1). Refused when a writer holds the conversation. An `external` claim names its process. Answers `{claimId, generation}`.",
        &[
            need("conversation", "string", "The session id."),
            need("kind", "string", "terminal or external."),
            need(
                "person",
                "string",
                "The principal revoke sweeps by (empty: the owner).",
            ),
            need(
                "revokeSeen",
                "int",
                "`hostclaim.epoch` read before the may check.",
            ),
            opt("pid", "int", "The external run's process."),
            opt("started", "string", "That process's start stamp."),
            opt(
                "holderInstance",
                "string",
                "The holder instance the spawn goes to.",
            ),
        ],
    ),
    host(
        "hostclaim.enqueue",
        "Queues a turn for a conversation (created when missing). Refused when the person was revoked after `revokeSeen`. Answers `{turn}`.",
        &[
            need("conversation", "string", "The session id."),
            need("agent", "string", "The agent."),
            need("engine", "string", "The engine."),
            need(
                "source",
                "string",
                "Who sent it (app, ask, wake, schedule ...).",
            ),
            need("person", "string", "The turn's principal."),
            need("prompt", "string", "The prompt."),
            need(
                "revokeSeen",
                "int",
                "`hostclaim.epoch` read before the may check.",
            ),
            opt(
                "deadline",
                "int",
                "Expire a waiting turn at this time (ms).",
            ),
        ],
    ),
    host(
        "hostclaim.turn",
        "The claim for a queued or parked turn, through `claim_next` (it picks the account and starts the session row). A stopped ask's turn is cancelled by the core and the refusal says so (`stopped`); any other refusal leaves the turn as it is, and the caller cancels it with `hostclaim.cancel` when it will not retry. Answers `{status: claimed, claimId, generation, account, fresh}` or `{status: exhausted, text}`.",
        &[
            need("conversation", "string", "The session id."),
            need("turn", "int", "The turn."),
            need("person", "string", "The turn's principal."),
            need(
                "revokeSeen",
                "int",
                "`hostclaim.epoch` read before the may check.",
            ),
            need("cwd", "string", "The run's working folder."),
            need(
                "sessionId",
                "string",
                "The id for a NEW engine session row.",
            ),
            opt(
                "holderInstance",
                "string",
                "The holder instance the spawn goes to.",
            ),
            opt("kernel", "string", "The kernel folder."),
            HOME,
            ENV,
        ],
    ),
    host(
        "hostclaim.redeem",
        "The claim's process started (step 3): `claimed` to `running` on this pid. Answers `{running, again}` or `{refused: <state>}`.",
        &[
            need("claimId", "int", "The claim."),
            need("pid", "int", "The process."),
            need("started", "string", "Its start stamp."),
            need("holderInstance", "string", "The holder instance."),
        ],
    ),
    host(
        "hostclaim.report",
        "What the process said about its claim (step 4): `done`, `exit`, `busy` or `spawned`, with the JSON `body` (`pid`, `out_of_usage`, `started`). Answers `{stored, applied}`; an unknown claim `{unknown: true}`.",
        &[
            need("claimId", "int", "The claim."),
            need("kind", "string", "done, exit, busy or spawned."),
            need("body", "string", "JSON."),
        ],
    ),
    host(
        "hostclaim.stop",
        "Stops a live claim (a take-over, a Stop): the claim goes to `ending` and stays the writer until its process is gone (`hostclaim.settle`). Answers `{kill}` (pid and start stamp to signal) or `{kill: null}`.",
        &[
            need("claimId", "int", "The claim."),
            need("reason", "string", "Why."),
            opt(
                "requeue",
                "bool",
                "Queue its turn again instead of cancelling it.",
            ),
        ],
    ),
    host(
        "hostclaim.settle",
        "Ends the `ending` claims whose process the core confirms gone. Answers `{ended: [claimId]}`.",
        &[],
    ),
    host(
        "hostclaim.show",
        "The conversation's writer, if any: `{claim: {claimId, state, kind, person, pid, started} | null}`; with `claimId`, that claim in whatever state it is.",
        &[
            opt("conversation", "string", "The session id."),
            opt("claimId", "int", "One claim."),
        ],
    ),
    host(
        "hostclaim.cancel",
        "Cancels a turn that nothing runs yet (queued, parked, or claimed with no process): what a caller does after a refused claim it will not retry. Answers `{cancelled}`.",
        &[
            need("turn", "int", "The turn."),
            need("reason", "string", "Why (logged)."),
        ],
    ),
    host(
        "hostclaim.park",
        "A queued turn waits (`lock_busy` or `terminal_live`). Answers `{parked}`.",
        &[
            need("turn", "int", "The turn."),
            need("reason", "string", "lock_busy or terminal_live."),
        ],
    ),
    host(
        "hostclaim.unpark",
        "Parked turns of the conversation go back to queued once nothing holds it. Answers `{turns}`.",
        &[
            need("conversation", "string", "The session id."),
            need("lockFree", "bool", "The writer lock could be taken."),
            need("terminalLive", "bool", "A terminal session is open."),
        ],
    ),
];
