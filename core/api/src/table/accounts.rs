//! M3-9a (account choice) and accounts-s1: the owner's view of account state and `pick`, `rows`, and the writes `out`, `clear`,
//! `usage` and `started`, over the core's database (table `account_state`, `core/runtime/src/migrations/accounts/`).
//! Experimental, owner only, not in `released.json`. Until `kith migrate step accounts` the state is `<machine>/accounts/state.json`,
//! written by `kith account out|clear|usage` and every launch: the reads answer the copy the last import left and the writes
//! are refused. After the step the core is the only writer and the CLI's account verbs call these rows (`kith kernel
//! account-state`). `account.pick` and `account.rows` start the harnesses' status commands (to see who is signed in) in the
//! core, only when the owner calls them.

use super::build::*;

pub(super) const ROWS: &[Method] = &[
    account(
        "account.state",
        "What each account is doing: its out mark, usage reading and last start, as `state.json` keeps them, from the core's database. One entry with `--entry`, else all as `{accounts}`.",
        &[flag(
            "entry",
            "string",
            &["--entry"],
            "One account id, such as `claude:work`; answers `{id, state}` (`state` is null when it has no row).",
        )],
        &["account", "state"],
    ),
    account(
        "account.pick",
        "The accounts a run of the agent would try, in order, reading the core's database for what is out or used least (`kith account pick AGENT --json`): `{candidates, skipped, all_out, harness, policy, pin}`.",
        &[
            pos("agent", "string", true, "The agent."),
            flag(
                "agentMd",
                "string",
                &["--agent-md"],
                "The agent's agent.md, when it is not in the agents folder (the harness comes from it).",
            ),
            sw(
                "deep",
                &["--deep"],
                "Ask each harness whether its login is good, not only look for the folder or the key.",
            ),
        ],
        &["account", "pick"],
    ),
    account(
        "account.rows",
        "Every account as `kith account list --json` shows it (the array of rows), with the out marks and usage read from the core's database. With `--agent`, that agent's order and where `pick` would start.",
        &[
            flag(
                "agent",
                "string",
                &["--agent"],
                "That agent's order and start.",
            ),
            sw(
                "deep",
                &["--deep"],
                "Ask each harness whether its login is good (the list always does).",
            ),
        ],
        &["account", "rows"],
    ),
    account(
        "account.out",
        "Marks an account out of usage until a time (`kith account out`): written to the core's database once `accounts` is switched, refused before.",
        &[
            pos(
                "id",
                "string",
                true,
                "The account id, such as `claude:work`.",
            ),
            flag(
                "until",
                "int",
                &["--until"],
                "Epoch seconds the limit lifts; none: an hour from now.",
            ),
            flag(
                "reason",
                "string",
                &["--reason"],
                "A short word, up to 40 letters, digits, space . _ -; default `manual`.",
            ),
            sw(
                "extend",
                &["--extend"],
                "An account already out for longer stays so.",
            ),
            flag(
                "now",
                "int",
                &["--now"],
                "The clock, epoch seconds (tests).",
            ),
        ],
        &["account", "out"],
    ),
    account(
        "account.clear",
        "Ready again (`kith account clear`): forgets the out mark and notes the time, so a host mark made before it is ignored.",
        &[
            pos("id", "string", true, "The account id."),
            flag(
                "hostUntil",
                "int",
                &["--host-until"],
                "The host's own mark standing now (epoch seconds), 0 for none.",
            ),
            flag(
                "now",
                "int",
                &["--now"],
                "The clock, epoch seconds (tests).",
            ),
        ],
        &["account", "clear"],
    ),
    account(
        "account.usage",
        "Records a usage reading for an account (`kith account usage ID --json READING`), after the same checks; answers the stored reading.",
        &[
            pos("id", "string", true, "The account id."),
            flag(
                "reading",
                "json",
                &["--reading"],
                "`{windows: [{name, percent, resets_at?}], read_at?}`.",
            ),
            flag(
                "now",
                "int",
                &["--now"],
                "The clock, epoch seconds (tests).",
            ),
        ],
        &["account", "usage"],
    ),
    account(
        "account.started",
        "Notes that a launch started on an account (`last_started`).",
        &[
            pos("id", "string", true, "The account id."),
            flag(
                "now",
                "int",
                &["--now"],
                "The clock, epoch seconds (tests).",
            ),
        ],
        &["account", "started"],
    ),
];

const fn account(
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
