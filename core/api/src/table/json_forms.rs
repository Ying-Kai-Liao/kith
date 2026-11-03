//! Family 3: the kernel tools' `--json` forms (B6 in docs/plans/one-bot-model.md), with the switched `kith kernel
//! core-*` plumbing that sits among them.

use super::build::*;
use crate::Method;

pub(super) const ROWS: &[Method] = &[
    b6(
        "auth.list",
        "Who holds which grants (no secret values).",
        &[],
        &["auth", "ls"],
    ),
    b6(
        "auth.granted",
        "The agents granted a service, or what an agent was granted.",
        &[
            pos("service", "string", false, "The service."),
            flag("forAgent", "string", &["--for"], "Limit to this agent."),
        ],
        &["auth", "granted"],
    ),
    b6(
        "auth.grant",
        "Grant an agent a scope of a service.",
        &[
            pos("agent", "string", true, "The agent."),
            pos("service", "string", true, "The service."),
            pos("scope", "string", true, "service/item."),
        ],
        &["auth", "grant"],
    ),
    b6(
        "auth.revoke",
        "Take a service back from an agent.",
        &[
            pos("agent", "string", true, "The agent."),
            pos("service", "string", true, "The service."),
        ],
        &["auth", "revoke"],
    ),
    b6(
        "doctor",
        "The health checks of this install.",
        &[],
        &["doctor"],
    ),
    b6(
        "remote.list",
        "The computers linked as remotes.",
        &[],
        &["remote", "ls"],
    ),
    b6(
        "remote.add",
        "Link a remote computer.",
        &[words("args", "NAME and the ssh target.")],
        &["remote", "add"],
    ),
    b6(
        "remote.remove",
        "Unlink a remote.",
        &[pos("name", "string", true, "The remote.")],
        &["remote", "rm"],
    ),
    b6(
        "ask.list",
        "The asks in flight and recent.",
        &[],
        &["ask", "ls"],
    ),
    b6(
        "bug.list",
        "The bug reports kept on this Mac.",
        &[],
        &["bug", "list"],
    ),
    b6(
        "bug.show",
        "One bug report.",
        &[pos("id", "string", true, "The report.")],
        &["bug", "show"],
    ),
    b6(
        "bug.create",
        "Write a bug report.",
        &[
            words("title", "The title."),
            flag("details", "string", &["--details"], "What happened."),
        ],
        &["bug"],
    ),
    plumbing(
        "run.mint",
        "A run token for one run of an agent, for a person in a conversation (and the claim it holds, when it holds one). The token is shown once; it works on the run socket only for a peer that descends from the process bound with run.bind. Tokens live in the daemon's memory and are gone when it restarts.",
        &[
            caller("agent", "string", "The agent the run is of."),
            caller(
                "person",
                "string",
                "The invited person or bot guest the run is for.",
            ),
            caller(
                "conversation",
                "string",
                "The conversation the run continues.",
            ),
            caller(
                "claim",
                "string",
                "The writer claim the run holds (kept in the identity; claims themselves are M3).",
            ),
            caller("room", "string", "The room, when the run is in one."),
            caller("project", "string", "The project the run was launched in."),
        ],
        "json",
        &["kernel", "core-run", "mint"],
    ),
    plumbing(
        "run.bind",
        "Binds a run's token to the process the run was started as; until then no request resolves. The process's start time is kept, so a reused pid does not revive the token.",
        &[
            caller("runId", "string", "The run."),
            caller("pid", "int", "The process id."),
        ],
        "json",
        &["kernel", "core-run", "bind"],
    ),
    plumbing(
        "run.end",
        "Ends a run's token (the run's process is gone, or it was revoked). A call already running finishes as the run it was.",
        &[caller("runId", "string", "The run.")],
        "json",
        &["kernel", "core-run", "end"],
    ),
];
