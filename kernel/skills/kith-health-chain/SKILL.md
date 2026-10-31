---
name: kith-health-chain
description: A scheduled health check that finds a bug asks devops for the cause, asks coder for a fix, and calls the person once when the PR is open. One bug, one PR, one call.
---
# Health-check chain

`<kernel>` below is the kernel folder this skill sits in (two levels up): the same path your protocol writes before `bin/`.

The run that finds the bug is the **origin**: it owns the chain and makes the one call. devops and coder only answer it.

## Setting it up (the owner, once per service)
`<kernel>/bin/kith-sched add AGENT "*/30 * * * *" "$(<kernel>/bin/kith-chain task SERVICE --check 'COMMAND')" --id health-SERVICE`

AGENT is the one the person chats with. COMMAND is any shell command that exits non-zero with the error on its output.
The run is scheduled, so it holds the call key and an outbox only when the Kith host is up; without them the chain still
runs and the call becomes a note.

## What the run does
1. `kith-chain run SERVICE --check COMMAND`: exit 0 means healthy or already handled (the same bug signature holds until
   its PR merges or 7 days pass): end the run, send nothing. Exit 10 prints a signature `SIG` and the findings.
2. `kith-chain ask SIG devops -- "<findings, what to look at>"`, note "devops: <finding> (asked <date>)" in your own
   memory, end your turn. The answer wakes you once (also after a restart).
3. Not a bug: `kith-chain close SIG --why "..."`. Otherwise `kith-chain ask SIG coder -- "<cause, proposed fix>"`, end your turn.
4. Woken with coder's answer: `kith-chain done SIG -- "<its answer>"`. A PR link in it is kept and the person is called
   once with the bug and the link. A call that does not ring (declined, missed, busy, rate-limited, no key) leaves the
   result in your chat with them. No link: one note, no call.
5. Any step refused or failing: `kith-chain fail SIG --why "..."`. The hop and cost caps of `kith ask` end the chain the
   same way: failed, with one note saying which bug and why it stopped.

## Rules
- Findings only (C4): `kith-chain ask` cuts the brief to redacted findings (no NAME=value lines, keys, home paths,
  addresses). Never paste logs, env or keys into it, and never put them in an answer.
- Never ask a second time for a signature the ledger holds, and never call yourself: `done` calls.
- The ledger is `<machine>/chain/ledger.json`; `kith-chain seen SIG` says whether a signature holds.
