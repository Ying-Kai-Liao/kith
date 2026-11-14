//! M3 (conversations): the owner's view of conversations and their writer claims (runtime-design section 4; tables in
//! `core/runtime/src/migrations/conversations/`). Experimental, owner only, not in `released.json`; the result is `json`
//! until the lane types it. Nothing serves these yet (`Core::call` answers NotServed, the caller falls back): M3-1 only
//! reserves the area so the later M3 lanes add their rows here without touching a shared list. Rows for the wake queue,
//! children and accounts go in their own area files when those lanes land.

use super::build::*;

pub(super) const ROWS: &[Method] = &[
    conversation(
        "conversation.list",
        "The conversations, newest first, with the engine, the account and whether a writer holds each.",
        &[],
        &["conversations", "list"],
    ),
    conversation(
        "conversation.show",
        "One conversation in full: its engine session pointer, the transcript hash, its live claim and its queued or parked turns. Refuses an id that is no conversation.",
        &[pos("id", "string", true, "The conversation.")],
        &["conversations", "show"],
    ),
    conversation(
        "claim.list",
        "The writer claims: the live ones (queued, claimed, running) and the recent ended ones, as `{live, recent}`.",
        &[],
        &["claims", "list"],
    ),
    external(
        "claim.external",
        "A kernel taker's writer claim on a conversation (`kith-sessions claim|terminal|resolve`, a `--direct` run) once the claims are the core's: kind `external`, naming the run's process (pid and start stamp) and the person kith-auth resolved (empty: the owner). Idempotent for the same process. Ends dead externals and settles stopped idle processes first; an idle warm process holding the conversation is marked for a kill and answers `waiting` until it is gone. A person's claim needs `revokeSeen` (`claim.epoch`, read before the may check). Answers `{claimId, generation}`, or `{refused: held | takeover | waiting | revoked | agent, why}`; a session id outside a-z0-9._- is refused. Serves only once the claims are switched.",
        &[
            caller("conversation", "string", "The session id."),
            caller(
                "pid",
                "int",
                "The run's process (the launcher's shell, which then execs the engine).",
            ),
            caller("started", "string", "That process's start stamp."),
            caller(
                "person",
                "string",
                "The principal revoke sweeps by (empty: the owner).",
            ),
            caller(
                "agent",
                "string",
                "The agent (required for a conversation the core does not know yet; a different one than the conversation's is refused).",
            ),
            caller(
                "engine",
                "string",
                "The engine of such a conversation (default claude).",
            ),
            caller(
                "revokeSeen",
                "int",
                "The person's `claim.epoch` read before the caller's may check (required for a person; the owner's default is read now).",
            ),
        ],
    ),
    external(
        "claim.epoch",
        "How many times a principal was revoked. A person's claim carries the value read BEFORE the may check that let the run in (`revokeSeen`), so a revoke committed after it refuses the claim. Answers `{epoch}`.",
        &[caller(
            "person",
            "string",
            "The principal (empty: the owner).",
        )],
    ),
    external(
        "claim.release",
        "Gives back the owner's external claims of one process (pid and start stamp): a claim already ended, or none, answers `{ended: []}`, so a retry is harmless. Never touches a turn's, a terminal's or a person's claim (a person's ends when its process is gone, or by a revoke). Answers `{ended}`.",
        &[
            caller("pid", "int", "The process."),
            caller("started", "string", "Its start stamp."),
            caller("conversation", "string", "Only this conversation's claim."),
        ],
    ),
    external(
        "claim.sweepPerson",
        "Revoke or removal with no host running: ends the live claims of these principals (queued ones end at once, live ones go to `ending`). The owner is refused. Answers `{kill: [{claimId, pid, started}]}` for the caller to signal; the rows end once the processes are gone.",
        &[caller(
            "persons",
            "[string]",
            "The principals (a person and their bots).",
        )],
    ),
];

const fn external(name: &'static str, summary: &'static str, params: &'static [Param]) -> Method {
    row(
        name,
        summary,
        params,
        "json",
        &[],
        Served::Core,
        Stability::Experimental,
    )
    .principals(OWNER_HOST)
    .no_cli()
}

const fn conversation(
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
