//! M2 lane A (authority): the owner's access to people, as `kith-auth member` has it today. Experimental stubs, owner
//! principal only, served by nobody in the core yet: `Core::call` answers NotServed and the caller falls back to
//! `kith-auth`/`kith-people`. The result is `json` until the lane types it. Not in `released.json`.
//!
//! `people.list` is spelled `kith people access` because `kith people ls` is the people list of `kith-people` today.

use super::build::*;

// TODO: a decimal string is a stand-in; `budget` needs a real type (whole cents) before people.grant goes stable.
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
        ],
        &["people", "revoke"],
    ),
    people(
        "people.list",
        "Who may use which agent, with their preset, mode and budget.",
        &[
            flag("agent", "string", &["--agent"], "Only this agent."),
            flag("person", "string", &["--person"], "Only this person."),
        ],
        &["people", "access"],
    ),
    people(
        "people.requests",
        "The access requests waiting for the owner.",
        &[],
        &["people", "requests"],
    ),
    people(
        "people.approve",
        "Approve an access request.",
        &[pos("id", "string", true, "The request.")],
        &["people", "approve"],
    ),
    people(
        "people.deny",
        "Deny an access request.",
        &[pos("id", "string", true, "The request.")],
        &["people", "deny"],
    ),
];

/// An M2 stub: owner only, `--json` form, experimental, still answered by the Python kernel.
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
        Served::Kernel,
        Stability::Experimental,
    )
    .formats(J)
}
