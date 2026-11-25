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
        "Opens an ask: the hop and cost-cap check and the insert of its record in ONE transaction, so no second ask passes the cap between them. Answers `{decision, record}` (the chain, hop and cross hops the record got); a `bg` ask also answers `turn` (its turn's id) and `out` (the file the host's run writes the callee's answer to), and opening the same `bg` ask again answers the same turn with `replayed`. A cap that refuses it is an error with the words `kith ask` prints. With `--check-only` nothing is written (`record` is null).",
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
            flag(
                "claimId",
                "int",
                &["--claim-id"],
                "The claim of the ask's own run, so a stop of the ask finds that claim exactly.",
            ),
            flag(
                "mode",
                "string",
                &["--mode"],
                "`fg` (the caller's own process runs the callee), `bg` (a core turn, source `ask`, that the host starts; the open makes the turn in the same transaction) or `peer` (the agent lives on another computer: no local turn, the host sends it and follows its rows; only the owner's runs, and only to a computer that is paired). Without it the record is as before the ledger.",
            ),
            flag(
                "task",
                "string",
                &["--task"],
                "What is asked, whole: the prompt of a `bg` ask's turn.",
            ),
            flag(
                "cliPid",
                "int",
                &["--cli-pid"],
                "`fg`: the caller's own process, so a caller killed with no handler is found by the tick.",
            ),
            flag(
                "cliStart",
                "string",
                &["--cli-start"],
                "`fg`: the start stamp of that process.",
            ),
            flag(
                "engine",
                "string",
                &["--engine"],
                "`bg`: the callee's engine (default: its conversation's, else claude).",
            ),
            flag(
                "peer",
                "string",
                &["--peer"],
                "`peer`: the computer the agent lives on. Refused unless this computer is paired with it now.",
            ),
            flag(
                "cont",
                "string",
                &["--cont"],
                "`peer`: the key (ask id) of the earlier ask on that computer whose session this one continues (`-c`).",
            ),
            flag(
                "originComputer",
                "string",
                &["--origin-computer"],
                "`bg`, on the callee's computer: the computer that asked. With `originKey` and `originEpoch`. The open is refused (as a refused row it can read back) unless that pairing epoch is the live one; the same key again answers the row it made.",
            ),
            flag(
                "originKey",
                "string",
                &["--origin-key"],
                "`bg`, on the callee's computer: the asking computer's id for the ask.",
            ),
            flag(
                "originEpoch",
                "int",
                &["--origin-epoch"],
                "`bg`, on the callee's computer: the pairing epoch the ask was sent under.",
            ),
            sw("checkOnly", &["--check-only"], "Decide, write nothing."),
            KERNEL,
            HOME,
            ENV,
        ],
        &["asks", "open"],
    )
    .principals(OWNER_HOST),
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
        "ask.finish",
        "The run behind an ask ended (the host's report, or a `fg` caller's own). An `open` ask that Stop did not end is settled in one transaction: with the callee's JSON answer it is `answered` (`stopped` when the callee failed, `capped` when its cost tipped the chain), without one `lost`; the cost joins the chain; and for a `bg` ask the answer (or the lost notice) becomes a mailbox row and the asker is owed the wake `ask:ID/N`, whether or not a writer holds its session. A `fg` ask prints its answer itself, so it gets no row and no wake. Anything settled already, or ended by `ask.end`, is left alone (the host's report and the tick settle once). An ask parked on a card stays open when its run ends, with an answer or without (answers `parked: true`; the cost still joins the chain, once per turn). A `turn` that is not the ask's current one settles nothing. Only a `fg` ask's own first report skips the row and wake. Answers `{id, settled, state, row}`.",
        &[
            pos("id", "string", true, "The ask."),
            flag(
                "out",
                "string",
                &["--out"],
                "The text of the out file the host read; its callee JSON is the answer (the tick reads it the same way).",
            ),
            flag(
                "result",
                "string",
                &["--result"],
                "The callee's answer text, when the caller has it already (a `fg` ask).",
            ),
            flag("cost", "json", &["--cost"], "US$ the run cost (a number), with `--result`."),
            sw("isError", &["--is-error"], "The callee said it failed, with `--result`."),
            flag(
                "session",
                "string",
                &["--session"],
                "The callee's session, kept on the record so `-c` resumes it.",
            ),
            flag(
                "turn",
                "int",
                &["--turn"],
                "The turn whose run ended: the ask is settled only if it is the ask's current turn. Omitted: the run of an ask with no turn (a fg CLI's own first run). The host's report always names its turn.",
            ),
            KERNEL,
            HOME,
            ENV,
        ],
        &["asks", "finish"],
    )
    .principals(OWNER_HOST),
    ask(
        "ask.post",
        "One row for an ask's asker (`kith-send --to ask:ID`, `kith ask --answer`): appended to the mailbox; an `answer` also owes the asker the wake `ask:ID/N` (a `progress` report waits for `--wait`). The same row again is a duplicate and wakes no one twice. An ask that was stopped or ended keeps nothing: answers `{id, stopped: true}`. Answers `{id, row}` (`row` null for a duplicate).",
        &[
            pos("id", "string", true, "The ask."),
            pos("text", "string", true, "What it says."),
            flag(
                "as",
                "string",
                &["--as"],
                "Who says it (default: the agent asked).",
            ),
            flag(
                "kind",
                "string",
                &["--kind"],
                "answer (default) or progress.",
            ),
        ],
        &["asks", "post"],
    ),
    ask(
        "ask.read",
        "`kith ask --wait`: the rows of an ask's mailbox nobody has read, marked read, with the wakes owed for them withdrawn in the same transaction (a row reaches the asker by `--wait` or by its wake, never both). An ended ask gives nothing. Answers `{id, state, rows: [{n, as, kind, text, at}]}`.",
        &[pos("id", "string", true, "The ask.")],
        &["asks", "read"],
    ),
    ask(
        "ask.park",
        "The ask waits on an approval card (replaces the `.parked` file): `ask.delegations` shows a waiting ask and every ask above it as `waiting {on: card}`. A parked ask is not settled by its run's end, with or without words (the run's cost still counts). Does nothing unless the ask is open and not ended. Answers `{id, parked}`.",
        &[
            pos("id", "string", true, "The ask."),
            pos("card", "string", true, "The card."),
        ],
        &["asks", "park"],
    )
    .principals(OWNER_HOST),
    ask(
        "ask.unpark",
        "The card's answer: the ask no longer waits on it, and the callee's resume becomes the ask's new turn (source `ask`, prompt the note, an out file of its own) in the same transaction, unless Stop ended the ask. Every later settle goes by that turn. Answers `{id, parked: false, turn, out}` (`turn` null when none was made).",
        &[
            pos("id", "string", true, "The ask."),
            flag(
                "note",
                "string",
                &["--note"],
                "What the card's answer says: the prompt of the resume turn.",
            ),
            flag(
                "session",
                "string",
                &["--session"],
                "The callee's session to resume.",
            ),
            KERNEL,
            HOME,
            ENV,
        ],
        &["asks", "unpark"],
    )
    .principals(OWNER_HOST),
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
                "Bots of the person read before an authority removal deleted their rows; a removal also records them (`open_removals`), and the bots still in `people.bot_of` are added. Refused: the owner, and a name in `people` whose `bot_of` chain does not lead to the person; a name with no row and no record is ignored.",
            ),
        ],
        &["asks", "revoke-everywhere"],
    ),
    ask(
        "ask.delegations",
        "`kith ask delegations --json`: every ask record, newest first, as `{delegations}` (money in micro-dollars, times in ms). An open ask that waits carries `waiting`: `{on: \"card\", card, since}` when it or an open ask below it is parked on a card, else `{on: \"agent\", agent, since}` when it has an open child.",
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
        "ask.peerLink",
        "A pairing with another computer starts (the host writes this before it writes the pairing): `epoch` is the live one for `computer`, and the fence every open, claim and attach of an ask from that computer reads. A newer epoch replaces the one kept, ending it first in the same transaction (as `ask.endPeer`); the same epoch again changes nothing, and an ended epoch is never made live again; an older one is refused. Answers `{computer, epoch, linked, ended, sessions, kill}`.",
        &[
            pos("computer", "string", true, "The other computer."),
            pos("epoch", "int", true, "The pairing epoch."),
        ],
        &["asks", "peer-link"],
    )
    .principals(OWNER_HOST),
    ask(
        "ask.endPeer",
        "The pairing with a computer ends (the host calls it before it removes the pairing, and again from its level-triggered reconcile). In one transaction: the fence is down, so nothing of that epoch opens, is claimed or attaches; the asks that computer sent here are ended (running ones stopped, with the claims to kill, queued and claimed ones refused, each with a row it can read); the asks sent there are cut off, each with one notice row and wake; and every stop owed to it is dropped. Safe to repeat. Answers `{computer, ended, sessions, kill}`.",
        &[pos("computer", "string", true, "The other computer.")],
        &["asks", "end-peer"],
    )
    .principals(OWNER_HOST),
    ask(
        "ask.peerApply",
        "Copies what another computer says of an ask sent there, in one transaction: each row once (the same call again adds nothing), one owed wake per row, the ask's final state and its cost (booked once). Nothing is copied once the ask was stopped, cut off or final, and its computer's final row and a cut-off notice never both reach the asker; a final state clears the stop owed. The call is the computer answering: the send counts as made and the away wait starts over. An `epoch` other than the ask's is dropped. Answers `{id, applied, rows, state, fetched}`.",
        &[
            pos("id", "string", true, "The ask on this computer."),
            pos("epoch", "int", true, "The pairing epoch the ask was sent under."),
            flag(
                "rows",
                "json",
                &["--rows"],
                "Its computer's rows after `fetched`: `[{n, as, kind, text, at}]`, `kind` answer, progress or lost.",
            ),
            flag(
                "state",
                "string",
                &["--state"],
                "The ask's state there: open, answered, capped, stopped, lost or refused. A final one ends the ask here.",
            ),
            flag("cost", "json", &["--cost"], "US$ the run and what it asked cost there, in total (read with a final state)."),
            KERNEL,
            HOME,
            ENV,
        ],
        &["asks", "peer-apply"],
    )
    .principals(OWNER_HOST),
    ask(
        "ask.peerOwed",
        "What the host owes the link, from the rows (nothing is written): `send` (asked here, not yet answered by its computer: id, key, computer, epoch, agent, task, cont, lineage), `follow` (sent and open: the last row copied, `after`) and `stop` (a stop its computer has not confirmed). Only asks whose pairing is live. Answers `{send, follow, stop}`.",
        &[],
        &["asks", "peer-owed"],
    )
    .principals(OWNER_HOST),
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
