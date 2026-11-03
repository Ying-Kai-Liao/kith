//! M2 lane A (authority): the owner's access to people, as `kith-auth member`, `bot`, `bots-cap`, `preset` and `invite`
//! have it today. Experimental, owner principal only, not in `released.json`; the result is `json` until the lane types
//! it. The rows the core serves (M2-2: grant, revoke, list, the bots, the invites and the presets; M2-3: the access
//! requests, `requests`, `approve` and `deny`) are answered by the daemon, because a write needs `&mut Core`
//! (`Core::call` itself answers NotServed).
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
        "people.requests",
        "The access requests waiting for the owner: a person asking for agents, with the preset and mode they asked for.",
        &[
            sw("all", &["--all"], "Also the ones decided or expired."),
            KERNEL,
            HOME,
            ENV,
        ],
        &["people", "requests"],
    ),
    people(
        "people.approve",
        "Approve an access request: the person gets the agents, preset and mode they asked for, and no more.",
        &[
            pos("id", "string", true, "The request."),
            sw(
                "trusted",
                &["--trusted"],
                "Allow a mode that is a free shell on this Mac.",
            ),
            KERNEL,
            HOME,
            ENV,
        ],
        &["people", "approve"],
    ),
    people(
        "people.deny",
        "Deny an access request.",
        &[pos("id", "string", true, "The request."), KERNEL, HOME, ENV],
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
