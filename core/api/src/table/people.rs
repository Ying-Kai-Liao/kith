//! M2 lane A (authority): the owner's access to people, as `kith-auth member`, `bot`, `bots-cap`, `preset` and `invite`
//! have it today. Experimental, owner principal only, not in `released.json`; the result is `json` until the lane types
//! it. The rows the core serves (M2-2: grant, revoke, list, the bots, the invites and the presets; M2-4: add, rename and
//! remove a person, suspend and undo, clear-temp, retired; M2-3: the device access requests) are answered by the daemon, because a write needs `&mut Core`
//! (`Core::call` itself answers NotServed). `people.revoke` takes one agent away from a person (`member --rm`);
//! `people.suspend` is `kith-auth person --revoke`: they reach nothing, until `people.undo`. The access requests
//! (`requests`, `approve`, `deny`) are the host's device-pairing requests (`accessRequests`, `decideAccessRequest`).
//!
//! `people.list` is spelled `kith people access` because `kith people ls` is the people list of `kith-people` today.
//!
//! The caller's view (`kernel`, `home`, `env`) is where the agent folders are, as for `paths`: the daemon's own
//! environment is not the environment of the CLI that asked.

use super::build::*;

// TODO: a decimal string is a stand-in; `budget` needs a real type (whole cents) before people.grant goes stable.
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

const BUDGET: Param = flag(
    "budget",
    "string",
    &["--budget"],
    "At most this many dollars a calendar month of their runs of the agent (as `budget=` in `kith-auth member`).",
);

pub(super) const ROWS: &[Method] = &[
    people(
        "people.grant",
        "Let a person use an agent, with their own settings on top of the agent's.",
        &[
            pos(
                "person",
                "string",
                true,
                "The person (`*` is everyone kith knows).",
            ),
            pos(
                "agent",
                "string",
                true,
                "The agent, or agents separated by commas.",
            ),
            flag(
                "preset",
                "string",
                &["--preset"],
                "A named bundle of permissions.",
            ),
            flag("mode", "string", &["--mode"], "The permission mode."),
            BUDGET,
            flag(
                "deny",
                "string",
                &["--deny"],
                "Tools to refuse, separated by commas.",
            ),
            flag("model", "string", &["--model"], "The model their runs use."),
            flag(
                "helpers",
                "string",
                &["--helpers"],
                "The helpers their runs may call, separated by commas.",
            ),
            sw(
                "trusted",
                &["--trusted"],
                "Allow a mode that is a free shell on this Mac.",
            ),
            KERNEL,
            HOME,
            ENV,
        ],
        &["people", "grant"],
    ),
    people(
        "people.revoke",
        "Take a person's use of an agent away.",
        &[
            pos("person", "string", true, "The person."),
            pos(
                "agent",
                "string",
                true,
                "The agent, or agents separated by commas.",
            ),
            KERNEL,
            HOME,
            ENV,
        ],
        &["people", "revoke"],
    ),
    people(
        "people.list",
        "Who may use which agent, with their preset, mode and budget.",
        &[
            flag("agent", "string", &["--agent"], "Only this agent."),
            flag("person", "string", &["--person"], "Only this person."),
            KERNEL,
            HOME,
            ENV,
        ],
        &["people", "access"],
    ),
    people(
        "people.may",
        "Whether a person may use an agent right now, and why (the kernel's `may`: a removed access beats every row, then their own, then everyone's), with whether they are the owner and the preset they hold.",
        &[
            pos("person", "string", true, "The person."),
            pos("agent", "string", true, "The agent."),
            KERNEL,
            HOME,
            ENV,
        ],
        &["people", "may"],
    ),
    people(
        "people.resolve",
        "What a run of an agent gets for a person (the kernel's `resolve`): whether it may run, their settings over the agent's, the domains the agent's package declares for every guest and the ones the owner kept, and `held`. With --take-once and --via, a launch refused only for want of a \"just this once\" grant is allowed by one and the grant is used up in the same transaction.",
        &[
            pos("agent", "string", true, "The agent."),
            flag(
                "person",
                "string",
                &["--person"],
                "The person (default: the one the caller acts for).",
            ),
            flag(
                "via",
                "string",
                &["--via"],
                "The agent whose membership the run rides on (a helper call).",
            ),
            sw(
                "takeOnce",
                &["--take-once"],
                "Count a \"just this once\" grant on the caller's row, and use it up when it lets the launch through.",
            ),
            KERNEL,
            HOME,
            ENV,
        ],
        &["people", "resolve"],
    ),
    people(
        "people.bridgeHeld",
        "Whether the channels hold this person now: kith serve serves anyone but the owner only while this Mac is in sync with the cloud (read from kith serve's cloud-sync file).",
        &[
            pos("person", "string", true, "The person."),
            KERNEL,
            HOME,
            ENV,
        ],
        &["people", "bridge-held"],
    ),
    people(
        "people.addBot",
        "Add a bot that belongs to a person (it is never more than its owner's).",
        &[
            pos(
                "id",
                "string",
                true,
                "The bot's id (lowercase letters, digits and -).",
            ),
            flag("of", "string", &["--of"], "The person it belongs to."),
            flag("name", "string", &["--name"], "Its name."),
            flag(
                "provider",
                "string",
                &["--provider"],
                "The platform it comes from.",
            ),
            KERNEL,
            HOME,
            ENV,
        ],
        &["people", "bot", "add"],
    ),
    people(
        "people.removeBot",
        "Remove a bot, with its memberships.",
        &[pos("id", "string", true, "The bot."), KERNEL, HOME, ENV],
        &["people", "bot", "remove"],
    ),
    people(
        "people.botsCap",
        "The most all the owner's bots may spend together a calendar month; with no amount, what it is now.",
        &[
            pos(
                "amount",
                "string",
                false,
                "Dollars a month (a number above 0), or `off`.",
            ),
            KERNEL,
            HOME,
            ENV,
        ],
        &["people", "bots-cap"],
    ),
    people(
        "people.invite",
        "A one-time `/link` code for a person, to send to the kith bot on any platform.",
        &[
            pos("person", "string", true, "The person."),
            flag(
                "minutes",
                "int",
                &["--minutes"],
                "How long it works (15 by default).",
            ),
            KERNEL,
            HOME,
            ENV,
        ],
        &["people", "invite"],
    ),
    people(
        "people.invites",
        "The `/link` codes that have not run out.",
        &[KERNEL, HOME, ENV],
        &["people", "invites"],
    ),
    people(
        "people.removeInvite",
        "Take a `/link` code back before it is used.",
        &[pos("code", "string", true, "The code."), KERNEL, HOME, ENV],
        &["people", "invite-remove"],
    ),
    people(
        "people.preset",
        "Define a named bundle of permissions, or change one.",
        &[
            pos("name", "string", true, "The preset."),
            flag("mode", "string", &["--mode"], "The permission mode."),
            flag(
                "deny",
                "string",
                &["--deny"],
                "Tools to refuse, separated by commas.",
            ),
            sw(
                "trusted",
                &["--trusted"],
                "Allow a mode that is a free shell on this Mac.",
            ),
            KERNEL,
            HOME,
            ENV,
        ],
        &["people", "preset"],
    ),
    people(
        "people.removePreset",
        "Remove a preset. Rows that name it wait, refused, until it is defined again.",
        &[
            pos("name", "string", true, "The preset."),
            KERNEL,
            HOME,
            ENV,
        ],
        &["people", "preset-remove"],
    ),
    people(
        "people.addPerson",
        "Add a person, or change their name, language or time zone, or make them the owner.",
        &[
            pos(
                "id",
                "string",
                true,
                "The person's id (lowercase letters, digits and -).",
            ),
            flag("name", "string", &["--name"], "Their name."),
            flag("lang", "string", &["--lang"], "Their language."),
            flag("tz", "string", &["--tz"], "Their time zone."),
            sw("owner", &["--owner"], "Make them the owner (there is one)."),
            KERNEL,
            HOME,
            ENV,
        ],
        &["people", "person", "add"],
    ),
    people(
        "people.renamePerson",
        "Give a person a new id. Their memberships, owned accounts and invites come along; the old id stays as an alias.",
        &[
            pos("id", "string", true, "The person's id now."),
            pos("to", "string", true, "The new id."),
            KERNEL,
            HOME,
            ENV,
        ],
        &["people", "person", "rename"],
    ),
    people(
        "people.removePerson",
        "Remove a person with their bots, memberships and codes, and retire the id. Their memory and files stay.",
        &[
            pos("id", "string", true, "The person."),
            sw(
                "unused",
                &["--unused"],
                "Only if they never had anything; the id is then free again.",
            ),
            KERNEL,
            HOME,
            ENV,
        ],
        &["people", "person", "remove"],
    ),
    people(
        "people.suspend",
        "Cut a person off from everything on this computer, and their bots with them. Their rows are kept; `people undo` brings it all back.",
        &[pos("id", "string", true, "The person."), KERNEL, HOME, ENV],
        &["people", "suspend"],
    ),
    people(
        "people.undo",
        "Bring back a person cut off by `people suspend`, with exactly the access they had.",
        &[pos("id", "string", true, "The person."), KERNEL, HOME, ENV],
        &["people", "undo"],
    ),
    people(
        "people.clearTemp",
        "Remove the temp folder of a person's runs, and of their bots' (their runs must be stopped).",
        &[pos("id", "string", true, "The person."), KERNEL, HOME, ENV],
        &["people", "person", "clear-temp"],
    ),
    people(
        "people.retired",
        "Why a new person or bot can't take this id (removed, an old id of someone, or what a removal left); nothing when it can.",
        &[pos("id", "string", true, "The id."), KERNEL, HOME, ENV],
        &["people", "retired"],
    ),
    people(
        "people.requests",
        "The devices asking this computer for access, waiting for the owner (with the code to compare once the device has shown it).",
        &[
            sw(
                "members",
                &["--members"],
                "Only members' requests (what the owner's other devices see).",
            ),
            KERNEL,
            HOME,
            ENV,
        ],
        &["people", "requests"],
    ),
    people(
        "people.approve",
        "Approve a device's access request (it needs the device's code first). Answers the device to add.",
        &[
            pos("id", "string", true, "The request."),
            sw(
                "members",
                &["--members"],
                "Answer as one of the owner's other devices: members' requests only.",
            ),
            KERNEL,
            HOME,
            ENV,
        ],
        &["people", "approve"],
    ),
    people(
        "people.deny",
        "Deny a device's access request.",
        &[
            pos("id", "string", true, "The request."),
            sw(
                "members",
                &["--members"],
                "Answer as one of the owner's other devices: members' requests only.",
            ),
            KERNEL,
            HOME,
            ENV,
        ],
        &["people", "deny"],
    ),
];

/// A row the core serves (through the daemon): owner only, `--json` form, experimental.
const fn people(
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
