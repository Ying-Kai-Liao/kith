//! M3 (delegation and children, M3-8a): the ask tree in the core (rows of `asks`, `core/runtime/src/migrations/children/`;
//! logic in `core/runtime/src/asks/`). Experimental, owner only, not in `released.json`. Served by the daemon (a change is a
//! claim transaction, so it runs on `&mut Core`; `Core::call` itself answers NotServed). Nothing calls them yet:
//! `kernel/bin/kith-ask` stays the writer of `live/ask/*.reg` until the switch (M3-10), and the daemon runs no sweep.
//!
//! "Children" are rows of the ask tree, not processes: `ask.end` settles rows `stopped` and says which ids and sessions it
//! did; stopping the runs and withdrawing the wakes are the callers' (part b, the wake lane). The caller's view (`home`,
//! `env`, `kernel`) says where settings.json is (the chain cap) and carries the `KITH_ASK_*` and `KITH_AGENT` variables the
//! lineage reads.

use super::build::*;

const HOME: Param = caller(
    "home",
    "string",
    "The caller's home folder (a daemon has its own).",
);
const ENV: Param = caller(
    "env",
    "{string}",
    "The caller's KITH_* and HQ_* variables (`KITH_ASK_ID`, `KITH_ASK_CHAIN`/`HOP`/`CROSS`/`COST`/`FROM`, `KITH_AGENT`, `KITH_PERSON`, `KITH_VIA`, `CLAUDE_CODE_SESSION_ID`); a daemon has its own.",
);

pub(super) const ROWS: &[Method] = &[
    ask(
        "ask.open",
        "Opens an ask: the hop and cost-cap check and the insert of its record in ONE transaction, so no second ask passes the cap between them. Answers `{decision, record}` (the chain, hop and cross hops the record got). A cap that refuses it is an error with the words `kith ask` prints. With `--check-only` nothing is written (`record` is null).",
        &[
            pos("id", "string", true, "The new ask's id."),
            flag("asked", "string", &["--asked"], "The agent asked."),
            sw("crossing", &["--crossing"], "The ask goes to another Mac."),
            flag(
                "brief",
                "string",
                &["--brief"],
                "What was asked (at most 2000 characters kept).",
            ),
            flag(
                "pid",
                "int",
                &["--pid"],
                "The asking process, so a crash is found.",
            ),
            flag(
                "conversation",
                "string",
                &["--conversation"],
                "The asked agent's session of a `--bg` run, when it has one.",
            ),
            sw("checkOnly", &["--check-only"], "Decide, write nothing."),
            KERNEL,
            HOME,
            ENV,
        ],
        &["asks", "open"],
    ),
    ask(
        "ask.settle",
        "The run behind an ask ended a turn at a cost (US$): it joins its chain's cost, and a chain that reaches its cap with this turn is marked once per cap. `--state` answered or stopped settles an open ask (capped when this turn tipped the chain). Settling a settled ask only adds the cost. Answers `{id, state, cost, tipped}`; refuses an id that is no ask.",
        &[
            pos("id", "string", true, "The ask."),
            flag(
                "cost",
                "json",
                &["--cost"],
                "US$ this turn cost (a number).",
            ),
            flag(
                "state",
                "string",
                &["--state"],
                "answered or stopped; left out while the run waits on a card.",
            ),
            KERNEL,
            HOME,
            ENV,
        ],
        &["asks", "settle"],
    ),
    ask(
        "ask.capTold",
        "The host raised the chain's cap card: it is not raised again for this cap. Answers `{marked}`, the records marked.",
        &[pos(
            "chain",
            "string",
            true,
            "The chain (its root ask's id).",
        )],
        &["asks", "cap-told"],
    ),
    ask(
        "ask.end",
        "Settles `stopped` the asks whose caller is one of the sessions or persons (Stop, a revoke, a removal): an ask already delivered, answered to its caller or ended stays. Stops no process and withdraws no wake; answers `{ended, sessions}` (the ids, and the sessions owed a withdrawal) for the callers that do.",
        &[
            flag(
                "sessions",
                "[string]",
                &["--sessions"],
                "Caller sessions whose asks end.",
            ),
            flag(
                "persons",
                "[string]",
                &["--persons"],
                "People whose asks end.",
            ),
        ],
        &["asks", "end"],
    ),
    ask(
        "ask.list",
        "`kith ask delegations --json`: every ask record, newest first, as `{delegations}` (money in micro-dollars, times in ms).",
        &[KERNEL, HOME, ENV],
        &["asks", "list"],
    ),
    ask(
        "ask.record",
        "The record of one ask as `kith ask` reads it (an old registration reads as a root ask). Refuses an id that has none.",
        &[pos("id", "string", true, "The ask.")],
        &["asks", "record"],
    ),
    ask(
        "ask.limits",
        "The delegation caps: hops on one Mac, hops across Macs, the refused exit code, the default cap and `cap_usd`, the chain cap in force (null for none).",
        &[KERNEL, HOME, ENV],
        &["asks", "limits"],
    ),
];

const fn ask(
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
