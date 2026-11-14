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
            opt(
                "room",
                "string",
                "A room conversation: the conversation of (agent, room, joinEpoch), made when missing. A second one for the same key is refused.",
            ),
            opt("joinEpoch", "int", "The room's join epoch (with `room`)."),
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
        "Queues a turn for a conversation (created when missing). Refused when the person was revoked after `revokeSeen`, or, for a room turn, when a `basis` person was revoked after their `epochs` entry (`revoked`), the turn has no basis, a bot's post opens a chain, or the conversation belongs to another join epoch (`other`), or the chain's turn budget is spent (`budget`). A room turn's basis, room, gen, level, author and budget are fixed here and never change. `key` makes a retry after a lost reply answer the same turn without spending the budget again. Answers `{turn}`.",
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
            opt(
                "level",
                "string",
                "`owner` (default) or `room`: a room turn speaks for the people of its basis.",
            ),
            opt("basis", "[string]", "The people the turn speaks for."),
            opt(
                "epochs",
                "{int}",
                "`hostclaim.epoch` of each basis person, read before the may check.",
            ),
            opt("room", "string", "The room."),
            opt(
                "gen",
                "int",
                "The room's membership generation at the start.",
            ),
            opt(
                "joinEpoch",
                "int",
                "The room's join epoch: which conversation it goes to.",
            ),
            opt(
                "author",
                "string",
                "Whose post made the turn: a person, `bot:<id>` or an agent name.",
            ),
            opt(
                "budget",
                "int",
                "A person's post: the follow-up rounds the chain may have.",
            ),
            opt(
                "budgetFrom",
                "int",
                "A bot's or agent's turn: the turn that caused it. It spends one of that turn's rounds.",
            ),
            opt(
                "key",
                "string",
                "The caller's id for this enqueue, so a retry is not counted twice.",
            ),
        ],
    ),
    host(
        "hostclaim.proc",
        "A host engine process, idle or not, with a session it has loaded: registers it or adds one more session to it (one core row per process and session; a process loads its agent's main session and its chat's threads), so a revoke reaches an idle one. `revokeSeen` is required for any person but the owner; refused (`revoked`) when stale. Answers `{registered}`.",
        &[
            need("pid", "int", "The process."),
            need("started", "string", "Its start stamp."),
            need("instance", "string", "The host instance that runs it."),
            opt(
                "person",
                "string",
                "Whose process it is (a guest's principal).",
            ),
            opt("agent", "string", "The agent."),
            opt(
                "conversation",
                "string",
                "A session it has loaded. Leave out for a process with none yet.",
            ),
            opt(
                "revokeSeen",
                "int",
                "`hostclaim.epoch` of `person`, read before the process was started.",
            ),
        ],
    ),
    host(
        "hostclaim.procGone",
        "The process exited: forget it, unless a kill is owed for it (a revoke or host change marked it): then it stays until `hostclaim.settle` finds it gone. With `conversation`, only that session is forgotten (the process stays known, with its other sessions). Answers `{gone}`.",
        &[
            need("pid", "int", "The process."),
            need("started", "string", "Its start stamp."),
            opt(
                "conversation",
                "string",
                "Forget only this session of the process (it unloaded or forked it away).",
            ),
        ],
    ),
    host(
        "hostclaim.adopt",
        "This host start's instance takes over: every claim and process of another host instance is ended (a claim never redeemed at once and its turn given back; one with a process goes to `ending`, its process to be killed until gone). Answers `{ended, kill}`.",
        &[need(
            "instance",
            "string",
            "`kith-serve-host/<pid>-<start stamp>`.",
        )],
    ),
    host(
        "hostclaim.turn",
        "The claim for a queued or parked turn, through `claim_next` (it starts the session row on the account: the core picks it once accounts are switched; until then it is the `account` the host sends). A stopped ask's turn is cancelled by the core and the refusal says so (`stopped`); any other refusal leaves the turn as it is, and the caller cancels it with `hostclaim.cancel` when it will not retry. Answers `{status: claimed, claimId, generation, account, fresh}` or `{status: exhausted, text}`.",
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
            opt(
                "account",
                "string",
                "The account the host's warm process runs as. Used and recorded while accounts are not switched (the core does not pick then); ignored once the core owns account state.",
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
        "The conversation's writer, if any: `{claim: {claimId, state, kind, person, pid, started, holderInstance} | null}`; with `claimId`, that claim in whatever state it is.",
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
