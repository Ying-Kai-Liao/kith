//! Family 3, continued: backup, wake, project, the kith-pkg verbs, install and schedule.

use super::build::*;
use crate::Method;

pub(super) const ROWS: &[Method] = &[
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
    b6(
        "backup.remove",
        "Take an account off the backup chain.",
        &[pos("entry", "string", true, "The entry.")],
        &["backup", "remove"],
    ),
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
    b6(
        "wake.status",
        "Where one wake stands.",
        &[pos("id", "string", true, "The wake.")],
        &["wake", "status"],
    ),
    b6(
        "project.show",
        "The project a folder is in, or the one it is.",
        &[pos("path", "string", false, "A folder (default: here).")],
        &["project"],
    ),
    b6(
        "project.notes",
        "A project's notes.",
        &[pos("id", "string", true, "The project.")],
        &["project", "notes"],
    ),
    b6(
        "pkg.versions",
        "The versions of an agent's package.",
        &[pos("agent", "string", true, "The agent.")],
        &["versions"],
    ),
    b6(
        "pkg.history",
        "The history of an agent's package, or one version of it.",
        &[
            pos("agent", "string", true, "The agent."),
            pos("version", "string", false, "A version."),
        ],
        &["history"],
    ),
    b6(
        "pkg.pin",
        "Pin an agent to a version.",
        &[
            pos("agent", "string", true, "The agent."),
            pos("version", "string", true, "The version."),
        ],
        &["pin"],
    ),
    b6(
        "pkg.unpin",
        "Follow an agent's live folder again.",
        &[pos("agent", "string", true, "The agent.")],
        &["unpin"],
    ),
    b6(
        "pkg.rollback",
        "Roll an agent back one version.",
        &[pos("agent", "string", true, "The agent.")],
        &["rollback"],
    ),
    b6(
        "pkg.restore",
        "Restore an agent's files from a ref.",
        &[
            pos("agent", "string", true, "The agent."),
            pos("ref", "string", true, "The ref."),
        ],
        &["restore"],
    ),
    b6(
        "pkg.commit",
        "Commit an agent's changes.",
        &[
            pos("agent", "string", true, "The agent."),
            flag("message", "string", &["-m"], "The message."),
        ],
        &["commit"],
    ),
    b6(
        "pkg.push",
        "Push an agent's package to its remote.",
        &[pos("agent", "string", true, "The agent.")],
        &["push"],
    ),
    b6(
        "pkg.update",
        "Update an agent's package from its remote.",
        &[pos("agent", "string", true, "The agent.")],
        &["update"],
    ),
    b6(
        "pkg.share",
        "Share an agent's package.",
        &[pos("agent", "string", true, "The agent.")],
        &["share"],
    ),
    b6(
        "pkg.deps",
        "What an agent's package depends on.",
        &[pos("agent", "string", true, "The agent.")],
        &["deps"],
    ),
    b6(
        "pkg.install",
        "Install an agent's package.",
        &[
            pos("source", "string", true, "Where from."),
            sw("yes", &["--yes"], "Install without asking."),
        ],
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
        &[
            pos("agent", "string", true, "The agent."),
            pos("id", "string", true, "The schedule."),
        ],
        &["schedule", "rm"],
    ),
    b6(
        "schedule.pause",
        "Pause a schedule.",
        &[
            pos("agent", "string", true, "The agent."),
            pos("id", "string", true, "The schedule."),
        ],
        &["schedule", "pause"],
    ),
    b6(
        "schedule.resume",
        "Resume a schedule.",
        &[
            pos("agent", "string", true, "The agent."),
            pos("id", "string", true, "The schedule."),
        ],
        &["schedule", "resume"],
    ),
    b6(
        "schedule.run",
        "Run a schedule now.",
        &[
            pos("agent", "string", true, "The agent."),
            pos("id", "string", true, "The schedule."),
        ],
        &["schedule", "run"],
    ),
    b6(
        "schedule.log",
        "What the schedules did.",
        &[words("args", "[AGENT [ID]] [COUNT], or AGENT/ID [COUNT].")],
        &["schedule", "log"],
    ),
];
