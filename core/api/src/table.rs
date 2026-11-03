//! The method table. One row per method; `core/api/openrpc.json` is generated from it and `core/api/released.json`
//! holds it as it was at the last release.
//!
//! Three families, in this order:
//! 1. the verbs the CLI already switched to Rust (`kith paths`, `kith kernel ...`): [`Served::Core`];
//! 2. the verbs the host reads from the kernel (`serve/contract/kernel/contract.json`): `ls`, `project ls`,
//!    `AGENT --launch-json`, `new`, `ask delegations`: [`Served::Kernel`], typed from the contract;
//! 3. the kernel tools' `--json` forms (B6 in docs/plans/one-bot-model.md): [`Served::Kernel`], [`Stability::Experimental`],
//!    result `json` until their types follow.
//!
//! Until M2 only the owner may call anything. Param names are camelCase; result fields keep the kernel's own
//! snake_case, as the contract does.

use crate::{Arg, Cli, Method, Param, Principal, Served, Stability, TypeDef};

const OWNER: &[Principal] = &[Principal::Owner];

const fn pos(name: &'static str, ty: &'static str, required: bool, summary: &'static str) -> Param {
    Param {
        name,
        ty,
        required,
        arg: Arg::Pos,
        summary,
    }
}
const fn flag(
    name: &'static str,
    ty: &'static str,
    spellings: &'static [&'static str],
    summary: &'static str,
) -> Param {
    Param {
        name,
        ty,
        required: false,
        arg: Arg::Flag(spellings),
        summary,
    }
}
const fn sw(
    name: &'static str,
    spellings: &'static [&'static str],
    summary: &'static str,
) -> Param {
    Param {
        name,
        ty: "bool",
        required: false,
        arg: Arg::Switch(spellings),
        summary,
    }
}
const fn rest(name: &'static str, summary: &'static str) -> Param {
    Param {
        name,
        ty: "[string]",
        required: false,
        arg: Arg::Rest,
        summary,
    }
}
const fn words(name: &'static str, summary: &'static str) -> Param {
    Param {
        name,
        ty: "[string]",
        required: false,
        arg: Arg::Words,
        summary,
    }
}
const fn after(name: &'static str, summary: &'static str) -> Param {
    Param {
        name,
        ty: "[string]",
        required: false,
        arg: Arg::After,
        summary,
    }
}

const fn row(
    name: &'static str,
    summary: &'static str,
    params: &'static [Param],
    result: &'static str,
    path: &'static [&'static str],
    served: Served,
    stability: Stability,
) -> Method {
    Method {
        name,
        summary,
        params,
        result,
        principals: OWNER,
        cli: Cli {
            path,
            aliases: &[],
            formats: &[],
            requires: &[],
        },
        stability,
        served,
    }
}

impl Method {
    const fn alias(mut self, a: &'static [&'static [&'static str]]) -> Self {
        self.cli.aliases = a;
        self
    }
    const fn formats(mut self, f: &'static [&'static str]) -> Self {
        self.cli.formats = f;
        self
    }
    const fn requires(mut self, r: &'static [&'static str]) -> Self {
        self.cli.requires = r;
        self
    }
}

/// A switched verb (family 1).
const fn core(
    name: &'static str,
    summary: &'static str,
    params: &'static [Param],
    result: &'static str,
    path: &'static [&'static str],
) -> Method {
    row(
        name,
        summary,
        params,
        result,
        path,
        Served::Core,
        Stability::Stable,
    )
}
/// Kernel plumbing (`kith kernel ...`): switched, but not frozen. Only `paths`, `kernel.version` and the contract
/// verbs are stable; these may change shape until they have been used by a client other than the CLI.
const fn plumbing(
    name: &'static str,
    summary: &'static str,
    params: &'static [Param],
    result: &'static str,
    path: &'static [&'static str],
) -> Method {
    row(
        name,
        summary,
        params,
        result,
        path,
        Served::Core,
        Stability::Experimental,
    )
}
/// A verb the host reads from the kernel (family 2).
const fn contract(
    name: &'static str,
    summary: &'static str,
    params: &'static [Param],
    result: &'static str,
    path: &'static [&'static str],
) -> Method {
    row(
        name,
        summary,
        params,
        result,
        path,
        Served::Kernel,
        Stability::Stable,
    )
}
/// A `--json` form of a kernel tool (family 3).
const fn b6(
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
        Served::Kernel,
        Stability::Experimental,
    )
    .formats(&["--json"])
}

const KERNEL: Param = flag(
    "kernel",
    "string",
    &["--kernel"],
    "The kernel folder (default: the one this program finds).",
);
const J: &[&str] = &["--json"];

/// Types the switched verbs add to the contract's.
pub static EXTRA_TYPES: &[TypeDef] = &[
    TypeDef {
        name: "KernelVersion",
        doc: "`kith kernel version`: the interface version and the modules this build answers.",
        fields: &[("abi", "int"), ("modules", "[string]")],
        may_omit: &[],
    },
    TypeDef {
        name: "Engines",
        doc: "`kith kernel engines --json`: the coding engines, installed or not, as the engine table describes them.",
        fields: &[("engines", "[json]")],
        may_omit: &[],
    },
    TypeDef {
        name: "LaunchPrompt",
        doc: "`kith kernel launch prompt --json`: the system prompt a run gets.",
        fields: &[("prompt", "string")],
        may_omit: &[],
    },
];

pub static METHODS: &[Method] = &[
    // 1. Switched verbs.
    core("paths", "Where Kith's folders are.", &[KERNEL], "{string}", &["paths"])
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
    row(
        "kernel.coreMigrate",
        "Makes (or migrates) this Mac's core database and releases it.",
        &[],
        "string",
        &["kernel", "core-migrate"],
        Served::Core,
        Stability::Experimental,
    ),
    // 2. What the host reads from the kernel (serve/contract/kernel/contract.json).
    contract("agents.list", "The agents on this Mac.", &[], "[Agent]", &["ls"]).requires(J),
    contract("projects.list", "The registered projects.", &[], "Projects", &["project", "ls"]).requires(J),
    contract(
        "agents.launch",
        "How to start an agent (run in the folder the run works in; `KITH_PERSON` makes it an invited person's run).",
        &[
            pos("agent", "string", true, "The agent."),
            flag("backup", "string", &["--backup"], "Launch a backup chain entry instead of the agent's own login."),
            flag("project", "string", &["-p"], "Load that project's context."),
        ],
        "Launch",
        &[],
    )
    .requires(&["--launch-json"]),
    contract(
        "agents.create",
        "Make an agent.",
        &[
            pos("name", "string", true, "Lowercase letters, digits, dashes."),
            pos("description", "string", true, "One line, no ': '."),
            flag("instructions", "string", &["--instructions"], "A file holding the first draft of agent.md's opening paragraph."),
        ],
        "NewAgent",
        &["new"],
    )
    .requires(J),
    contract("delegations.list", "The delegation records on this Mac.", &[], "Delegations", &["ask", "delegations"]).requires(J),
    // 3. The kernel tools' `--json` forms (B6).
    b6("auth.list", "Who holds which grants (no secret values).", &[], &["auth", "ls"]),
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
        &[pos("agent", "string", true, "The agent."), pos("service", "string", true, "The service.")],
        &["auth", "revoke"],
    ),
    b6("doctor", "The health checks of this install.", &[], &["doctor"]),
    b6("remote.list", "The computers linked as remotes.", &[], &["remote", "ls"]),
    b6("remote.add", "Link a remote computer.", &[words("args", "NAME and the ssh target.")], &["remote", "add"]),
    b6("remote.remove", "Unlink a remote.", &[pos("name", "string", true, "The remote.")], &["remote", "rm"]),
    b6("ask.list", "The asks in flight and recent.", &[], &["ask", "ls"]),
    b6("bug.list", "The bug reports kept on this Mac.", &[], &["bug", "list"]),
    b6("bug.show", "One bug report.", &[pos("id", "string", true, "The report.")], &["bug", "show"]),
    b6(
        "bug.create",
        "Write a bug report.",
        &[words("title", "The title."), flag("details", "string", &["--details"], "What happened.")],
        &["bug"],
    ),
    b6("backup.list", "The backup chain.", &[], &["backup", "list"]),
    b6(
        "backup.add",
        "Add an account to the backup chain.",
        &[
            pos("entry", "string", true, "The harness or harness:account."),
            flag("at", "int", &["--at"], "Where in the chain."),
            flag("agent", "string", &["--agent"], "Only for this agent."),
        ],
        &["backup", "add"],
    ),
    b6("backup.remove", "Take an account off the backup chain.", &[pos("entry", "string", true, "The entry.")], &["backup", "remove"]),
    row(
        "update.self",
        "Whether this program is the newest, or update it.",
        &[sw("check", &["--check"], "Only look.")],
        "json",
        &["update", "self"],
        Served::Kernel,
        Stability::Experimental,
    )
    .formats(J),
    b6("wake.list", "The wakes waiting.", &[], &["wake", "ls"]),
    b6("wake.status", "Where one wake stands.", &[pos("id", "string", true, "The wake.")], &["wake", "status"]),
    b6("project.show", "The project a folder is in, or the one it is.", &[pos("path", "string", false, "A folder (default: here).")], &["project"]),
    b6("project.notes", "A project's notes.", &[pos("id", "string", true, "The project.")], &["project", "notes"]),
    b6("pkg.versions", "The versions of an agent's package.", &[pos("agent", "string", true, "The agent.")], &["versions"]),
    b6(
        "pkg.history",
        "The history of an agent's package, or one version of it.",
        &[pos("agent", "string", true, "The agent."), pos("version", "string", false, "A version.")],
        &["history"],
    ),
    b6(
        "pkg.pin",
        "Pin an agent to a version.",
        &[pos("agent", "string", true, "The agent."), pos("version", "string", true, "The version.")],
        &["pin"],
    ),
    b6("pkg.unpin", "Follow an agent's live folder again.", &[pos("agent", "string", true, "The agent.")], &["unpin"]),
    b6("pkg.rollback", "Roll an agent back one version.", &[pos("agent", "string", true, "The agent.")], &["rollback"]),
    b6(
        "pkg.restore",
        "Restore an agent's files from a ref.",
        &[pos("agent", "string", true, "The agent."), pos("ref", "string", true, "The ref.")],
        &["restore"],
    ),
    b6(
        "pkg.commit",
        "Commit an agent's changes.",
        &[pos("agent", "string", true, "The agent."), flag("message", "string", &["-m"], "The message.")],
        &["commit"],
    ),
    b6("pkg.push", "Push an agent's package to its remote.", &[pos("agent", "string", true, "The agent.")], &["push"]),
    b6("pkg.update", "Update an agent's package from its remote.", &[pos("agent", "string", true, "The agent.")], &["update"]),
    b6("pkg.share", "Share an agent's package.", &[pos("agent", "string", true, "The agent.")], &["share"]),
    b6("pkg.deps", "What an agent's package depends on.", &[pos("agent", "string", true, "The agent.")], &["deps"]),
    b6(
        "pkg.install",
        "Install an agent's package.",
        &[pos("source", "string", true, "Where from."), sw("yes", &["--yes"], "Install without asking.")],
        &["install"],
    ),
    b6(
        "schedule.list",
        "The scheduled runs.",
        &[pos("agent", "string", false, "Only this agent's.")],
        &["schedule", "ls"],
    ),
    b6(
        "schedule.add",
        "Schedule a run.",
        &[
            pos("agent", "string", true, "The agent."),
            words("spec", "The five cron fields, then the prompt."),
            flag("id", "string", &["--id"], "The schedule's id."),
        ],
        &["schedule", "add"],
    ),
    b6(
        "schedule.remove",
        "Remove a schedule.",
        &[pos("agent", "string", true, "The agent."), pos("id", "string", true, "The schedule.")],
        &["schedule", "rm"],
    ),
    b6(
        "schedule.pause",
        "Pause a schedule.",
        &[pos("agent", "string", true, "The agent."), pos("id", "string", true, "The schedule.")],
        &["schedule", "pause"],
    ),
    b6(
        "schedule.resume",
        "Resume a schedule.",
        &[pos("agent", "string", true, "The agent."), pos("id", "string", true, "The schedule.")],
        &["schedule", "resume"],
    ),
    b6(
        "schedule.run",
        "Run a schedule now.",
        &[pos("agent", "string", true, "The agent."), pos("id", "string", true, "The schedule.")],
        &["schedule", "run"],
    ),
    b6(
        "schedule.log",
        "What the schedules did.",
        &[words("args", "[AGENT [ID]] [COUNT], or AGENT/ID [COUNT].")],
        &["schedule", "log"],
    ),
];

/// The row called `name`.
pub fn method(name: &str) -> Option<&'static Method> {
    METHODS.iter().find(|m| m.name == name)
}
