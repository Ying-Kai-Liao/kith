//! Family 1: the verbs the CLI already switched to Rust (`kith paths`, `kith kernel ...`): `Served::Core`.

use super::build::*;
use crate::{Arg, Method};

pub(super) const ROWS: &[Method] = &[
    core(
        "paths",
        "Where Kith's folders are.",
        &[
            KERNEL,
            caller("home", "string", "The caller's home folder (a daemon has its own)."),
            caller("env", "{string}", "The caller's KITH_* and HQ_* variables (a daemon has its own)."),
        ],
        "{string}",
        &["paths"],
    )
        .alias(&[&["kernel", "paths"]])
        .formats(&["--json", "--sh"]),
    core("kernel.version", "The interface version and modules of this build.", &[], "KernelVersion", &["kernel", "version"]),
    plumbing("kernel.engines.list", "The coding engines an agent can run on.", &[], "Engines", &["kernel", "engines"])
        .formats(J),
    plumbing(
        "kernel.engines.install",
        "How to install an engine on this computer.",
        &[pos("id", "string", true, "The engine's id.")],
        "string",
        &["kernel", "engines", "install"],
    )
    .formats(J),
    plumbing(
        "kernel.engines.login",
        "How to sign in to an engine.",
        &[pos("id", "string", true, "The engine's id.")],
        "string",
        &["kernel", "engines", "login"],
    )
    .formats(J),
    plumbing(
        "kernel.engines.launch",
        "How to start an engine for a run: its ACP program, role prompt delivery, whether it serves guests.",
        &[pos("id", "string", true, "The engine's id.")],
        "json",
        &["kernel", "engines", "launch"],
    )
    .formats(J),
    plumbing(
        "kernel.launch.prompt",
        "The system prompt a run of the agent gets.",
        &[
            pos("agent", "string", true, "The agent."),
            flag("person", "string", &["--person"], "Whom the run is for (default: the owner)."),
            flag("project", "string", &["--project"], "The project the run is in."),
            flag("pkg", "string", &["--pkg"], "The pinned package folder."),
            KERNEL,
            flag("cwd", "string", &["--cwd"], "The working folder shown in the prompt."),
            flag("date", "string", &["--date"], "The date shown in the prompt."),
        ],
        "LaunchPrompt",
        &["kernel", "launch", "prompt"],
    )
    .formats(J),
    plumbing(
        "kernel.launch.settings",
        "The settings a run of the agent starts with.",
        &[
            pos("agent", "string", true, "The agent."),
            flag("person", "string", &["--person"], "Whom the run is for."),
            flag("mode", "string", &["--mode"], "The permission mode."),
            flag("pkg", "string", &["--pkg"], "The pinned package folder."),
            sw("snapshot", &["--snapshot"], "Read the pinned package, not the live folder."),
            KERNEL,
            flag("project", "string", &["--project"], "The project the run is in."),
            flag("cwd", "string", &["--cwd"], "The working folder."),
            flag("harness", "string", &["--harness"], "The engine the run is on."),
            flag("kithBin", "string", &["--kith-bin"], "The kith program the run's helpers call."),
            flag("pluginsJson", "string", &["--plugins-json"], "A file listing the plugins."),
            sw("terminal", &["--terminal"], "The run is in a terminal."),
            flag("apiHelper", "string", &["--api-helper"], "The API key helper."),
            flag("onceId", "string", &["--once-id"], "The one-shot grant id."),
            flag("resolve", "string", &["--resolve"], "A file holding kith-auth's answer."),
        ],
        "json",
        &["kernel", "launch", "settings"],
    )
    .formats(J),
    plumbing(
        "kernel.launch.mcp",
        "The MCP servers a run of the agent starts with.",
        &[
            pos("agent", "string", true, "The agent."),
            flag("person", "string", &["--person"], "Whom the run is for."),
            flag("pkg", "string", &["--pkg"], "The pinned package folder."),
            sw("snapshot", &["--snapshot"], "Read the pinned package, not the live folder."),
            KERNEL,
            flag("granted", "string", &["--granted"], "A file listing the granted connectors."),
            flag("resolve", "string", &["--resolve"], "A file holding kith-auth's answer."),
        ],
        "json",
        &["kernel", "launch", "mcp"],
    )
    .formats(J),
    plumbing(
        "kernel.launch.whole",
        "The whole launch of an agent (`AGENT --launch-json [flags]`, the flags passed through).",
        &[KERNEL, rest("args", "The agent, then the launch flags.")],
        "Launch",
        &["kernel", "launch"],
    ),
    plumbing(
        "kernel.accounts.backups",
        "The backup chain of an agent (`kith AGENT --backups-json`).",
        &[
            pos("agent", "string", true, "The agent."),
            pos("agentMd", "string", true, "The agent's agent.md."),
            KERNEL,
        ],
        "json",
        &["kernel", "accounts", "backups-json"],
    ),
    plumbing(
        "kernel.accounts.launchEnv",
        "Shell assignments for a backup chain entry; refused (exit 4) when it can't run.",
        &[
            pos("agent", "string", true, "The agent."),
            pos("entry", "string", true, "The backup entry."),
            sw("record", &["--record"], "The launch is real: note `last_started`."),
            KERNEL,
        ],
        "string",
        &["kernel", "accounts", "launch-env"],
    ),
    plumbing(
        "kernel.accounts.mainEnv",
        "`main_id=ID` plus the assignments of the agent's main account.",
        &[
            pos("agent", "string", true, "The agent."),
            pos("agentMd", "string", true, "The agent's agent.md."),
            sw("state", &["--state"], "Start on `pick`'s first candidate."),
            sw("record", &["--record"], "The launch is real: note `last_started`."),
            flag("prefer", "string", &["--prefer"], "The account a handed-on session was made under."),
            KERNEL,
        ],
        "string",
        &["kernel", "accounts", "main-env"],
    ),
    plumbing(
        "kernel.accounts.sessionRun",
        "The harness, model and effort a session started with.",
        &[
            pos("sid", "string", true, "The session."),
            sw("sh", &["--sh"], "Shell assignments."),
            KERNEL,
        ],
        "json",
        &["kernel", "accounts", "session-run"],
    ),
    plumbing(
        "kernel.accounts.sessionAccount",
        "The account id a session was made under.",
        &[pos("sid", "string", true, "The session."), KERNEL],
        "string",
        &["kernel", "accounts", "session-account"],
    ),
    plumbing(
        "kernel.accounts.list",
        "Each extra account and whether it is logged in.",
        &[flag("agent", "string", &["--agent"], "That agent's order."), KERNEL],
        "json",
        &["kernel", "accounts", "list"],
    )
    .alias(&[&["kernel", "accounts", "ls"]])
    .formats(J),
    plumbing("kernel.delegation.limits", "How far a chain of asks may go.", &[KERNEL], "json", &["kernel", "delegation", "limits"]),
    plumbing("kernel.delegation.cap", "The chain cost cap in dollars (`none` for no cap).", &[KERNEL], "string", &["kernel", "delegation", "cap"]),
    plumbing(
        "kernel.delegation.record",
        "The delegation record of one ask, as the code reads it.",
        &[pos("id", "string", true, "The ask's id."), KERNEL],
        "json",
        &["kernel", "delegation", "record"],
    ),
    plumbing(
        "kernel.delegation.check",
        "Whether the ask running now may start another.",
        &[
            flag("id", "string", &["--id"], "The ask's id (default: a probe)."),
            sw("crossing", &["--crossing"], "The ask goes to an agent on another Mac."),
            KERNEL,
        ],
        "json",
        &["kernel", "delegation", "check"],
    ),
    plumbing(
        "kernel.delegation.parked",
        "Whether an ask waits on a card.",
        &[pos("id", "string", true, "The ask's id."), KERNEL],
        "bool",
        &["kernel", "delegation", "parked"],
    ),
    plumbing(
        "kernel.secrets",
        "The read half of kith-auth: `resolve` and `may`, with its output and exit codes.",
        &[KERNEL, rest("args", "`resolve AGENT [--person ID] ...` or `may ID AGENT ...`.")],
        "string",
        &["kernel", "secrets"],
    ),
    row(
        "kernel.threads",
        "The writer claim of a session and the session registry (kith-sessions' claim half).",
        &[KERNEL, rest("args", "The verb and its arguments.")],
        "string",
        &["kernel", "threads"],
        Served::Core,
        Stability::Experimental,
    ),
    plumbing(
        "kernel.runOnce",
        "One task on one engine over ACP: the answer.",
        &[
            pos("engine", "string", true, "The engine."),
            flag("model", "string", &["--model"], "The model."),
            flag("cwd", "string", &["--cwd"], "The folder the engine works in."),
            flag("mcpConfig", "string", &["--mcp-config"], "MCP servers for the run, as JSON or a file."),
            sw("approve", &["--approve"], "Allow the tools the engine asks to use."),
            flag("timeout", "int", &["--timeout"], "Seconds the whole run may take (default 600)."),
            flag("acpCommand", "string", &["--acp-command"], "Start this ACP command line instead (tests)."),
            after("task", "The task, after `--`."),
        ],
        "json",
        &["kernel", "run-once"],
    )
    .formats(J),
    plumbing(
        "kernel.coreStatus",
        "The daemon, its owner socket and the holder, or why there is no daemon, and the verb that recovers a core that is down.",
        &[],
        "json",
        &["kernel", "core-status"],
    )
    .formats(J),
    row(
        "kernel.coreDaemon",
        "Runs the daemon front alone in the foreground (what `kith serve` starts inside itself); ends when stdin closes.",
        &[],
        "string",
        &["kernel", "core-daemon"],
        Served::Core,
        Stability::Experimental,
    ),
    row(
        "kernel.coreMigrate",
        "Makes (or migrates) this Mac's core database and releases it.",
        &[],
        "string",
        &["kernel", "core-migrate"],
        Served::Core,
        Stability::Experimental,
    ),
    row(
        "terminal.open",
        "Starts an agent's engine in a held terminal (the holder runs it under the start stub, so it outlives the window) and returns a single-use attach ticket; `direct` when the holder is not reachable, `busy` when another writer has the session.",
        &[
            held_param("agent", "string", true, "The agent."),
            held_param("session", "string", true, "The session id: the writer claim and the stub's lock key."),
            held_param("argv", "[string]", true, "The assembled engine launch (program first)."),
            held_param("cwd", "string", true, "The folder the engine works in."),
            held_param("env", "{string}", false, "The launch's environment; run tokens and their sockets are dropped."),
            held_param("cols", "int", false, "Terminal width (default 80)."),
            held_param("rows", "int", false, "Terminal height (default 24)."),
        ],
        "json",
        &["kernel", "terminal-open"],
        Served::Core,
        Stability::Experimental,
    ),
    row(
        "terminal.attach",
        "A new single-use attach ticket for a held terminal that is running (or ended a moment ago and is still kept): how a client comes back after a detach, a cut or a lost link. `gone` when the core no longer knows it.",
        &[
            held_param("heldId", "json", false, "The held terminal: {instance, id}, as `terminal.open` and `terminal.list` give it."),
            held_param("session", "string", false, "The session id, instead of heldId."),
            held_param("role", "string", false, "`owner` (default) or `observer`."),
        ],
        "json",
        &["kernel", "terminal-attach"],
        Served::Core,
        Stability::Experimental,
    ),
    row(
        "terminal.list",
        "The held terminals the core knows (held id, session, agent if known, pid, when it started, how many owners and observers are attached, size). An ended terminal is not listed.",
        &[],
        "json",
        &["kernel", "terminal-list"],
        Served::Core,
        Stability::Experimental,
    ),
];

/// A param only a client that knows the launch can fill (never typed): like `caller`, but it may be required.
const fn held_param(
    name: &'static str,
    ty: &'static str,
    required: bool,
    summary: &'static str,
) -> Param {
    Param {
        name,
        ty,
        required,
        arg: Arg::Caller,
        summary,
    }
}
