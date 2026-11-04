//! M3 (delegation and children, M3-8a): the ask tree in the core (rows of `asks`, `core/runtime/src/migrations/children/`;
//! logic in `core/runtime/src/asks/`). Experimental, owner only, not in `released.json`. Served by the daemon (a change is a
//! claim transaction, so it runs on `&mut Core`; `Core::call` itself answers NotServed). Nothing calls them yet:
//! `kernel/bin/kith-ask` stays the writer of `live/ask/*.reg` until the switch (M3-10), and the daemon runs no sweep.
//!
//! "Children" are rows of the ask tree, not processes: `ask.end` settles a tree of rows `stopped`, stops each child's
//! conversation through the claims API (M3-8b), withdraws the wakes the callers are owed and returns the kills to send. The caller's view (`home`,
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
        "Stop, revoke, removal (`kith ask --end`): settles `stopped` the asks whose caller is one of the sessions or persons, or that are one of the roots, and every ask below them; stops what each one's conversation runs (queued and parked turns cancelled, the live claim to `ending`, an unspawned one ended); withdraws the wakes and waiters those callers are owed. An ask already delivered or answered to its caller stays. Repeating it is safe. Kills no process: answers `{ended, sessions, kill}` (the ids, the sessions withdrawn, and the writers `{claimId, pid, started}` to kill until gone).",
        &[
            flag(
                "roots",
                "[string]",
                &["--roots"],
                "Asks whose whole subtree ends.",
            ),
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
        "ask.to",
        "`kith ask --to ID`, the check before anything is queued: whether the ask still takes a follow-up. An ask that was stopped or ended refuses with `was stopped; ask anew` (the stopped fence never lifts; the Python `--to` queued the message and the wake fence dropped it). Answers `{id, ok}`; refuses an id that is no ask.",
        &[pos("id", "string", true, "The ask.")],
        &["asks", "to"],
    ),
    ask(
        "ask.revokeEverywhere",
        "The revoke order for conversations, once the authority revoke is visible to the may check: the person's claims and each of their bots' (queued and parked turns cancelled, live claims to `ending`, the revoke epoch bumped so a claim that passed its may check before is refused), THEN the asks of the person and their bots with every ask below them (`ask.end --persons`). `ask.end` alone is not a revoke. One claim transaction per principal, then one for the asks. Safe to repeat; a retry (with the same bots) finishes a crash between the steps, and a crash after the person's step leaves the bots' in-flight claims unfenced until it. On a removal pass `--bots`, read before the write: the rows are gone after. Kills no process: answers `{principals, cancelledTurns, ended, sessions, kill}`.",
        &[
            pos("person", "string", true, "The person revoked."),
            flag(
                "bots",
                "[string]",
                &["--bots"],
                "Bots of the person read before an authority removal deleted their rows; a removal also records them (`pending_revokes`), and the bots still in `people.bot_of` are added. Refused: the owner, and any name whose `bot_of` chain (current or recorded) does not lead to the person.",
            ),
        ],
        &["asks", "revoke-everywhere"],
    ),
    ask(
        "ask.delegations",
        "`kith ask delegations --json`: every ask record, newest first, as `{delegations}` (money in micro-dollars, times in ms).",
        &[KERNEL, HOME, ENV],
        &["asks", "delegations"],
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
