//! M3-9a (account choice): the owner's read-only view of account state and `pick`, from the core's database (table `account_state`,
//! `core/runtime/src/migrations/accounts/`). Experimental, owner only, not in `released.json`. UNSWITCHED: account state is
//! still written to `<machine>/accounts/state.json` by `kith account out|clear|usage` and every launch, and read from there
//! by every run; the core writes none (only `kith migrate --accounts` copies the file in), so these rows answer the copy
//! the last import left. Nothing calls them yet. `account.pick` starts the harnesses' status commands (to see who is signed in) in the core, only when the owner calls it.

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
