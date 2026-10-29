---
name: revoke-access
description: Model-checks (TLA+/TLC) that a revoked person, and their bots, reach nothing on any of the owner's Macs, and that undo restores exactly what they had. Use when a change touches kernel/bin/kith-auth may(), cut_off, bot_may, bridge_held or cmd_person/revoke_person, the helper spawn's in-sync wait in kernel/bin/kith (and kith-permit ask_helper), serve/host/src/api/people.rs revoke_person or cut_off, serve/host/src/remote/{members,cloud,relay,sync_lease}.rs, the bot ask/thread gates in api/botguests.rs, the chat channels' access checks in serve/host/src/channels/ (core.rs handle/held/take/access, queue.rs work, run.rs Recheck/finish/flush, hq.rs who/resolve_in_sync), or the cloud's person revoke in serve/cloud/src/routes/invites.ts. Also use to answer "can a revoked person still get through?".
---

# Revoke access: a revoked person reaches nothing, anywhere

Revoking a person (architect's design `designs/revoke-person.md`, rules R1-R9) is one fact, a `revoked` state on the
person in kith-auth, that must win over every row on every Mac, put their bots to sleep, and stop a running turn from
reaching them; undo must give back exactly what they had. Whether that holds depends on the order of steps between
the owner, two Macs, the Kith cloud, the person's doors (app, bridge, cloud bot) and a running turn. So it is checked
with TLA+ (TLC), not with tests: the failures need two Macs and a lost cloud link at the right moment, which tests do
not reach. The tests (below) check each step on the code; the model checks that the steps add up.

The model is `verification/revoke/RevokeDoors.tla` (round 2 of the design model; round 1, `Revoke.tla`, stays with the
design). This skill is its only documentation.

## Run it

```
verification/revoke/run.sh                          # every row of checks.tsv (35 rows, about a minute)
verification/revoke/run.sh --show VARIANT PROPERTY   # TLC's full output (the counterexample) for one row
verification/revoke/drift.sh                        # covered files changed since the sync commit?
```

Same pinned tools as `verification/threads/run.sh` (it is asked for them with `--tools`): `tla2tools.jar` v1.7.4,
Temurin JRE 21 only when no Java 11+ is present, cached under `${XDG_CACHE_HOME:-$HOME/.cache}/kith-verify/`.
A run composes `base.consts` (the code as built) with `variants/<v>.set` (the switches a variant changes), then one
property. Exit 0 only when every row came out as `checks.tsv` expects.

The tests of each step on the code (all in CI):

```
kernel/tests/kith-auth-revoke-check      # kith-auth: may/bot_may/who/resolve, rows kept, undo exact, R9 refusal
python3 kernel/tests/kith-auth-props.py  # random op sequences incl. revoke/unrevoke (may() vs a reference)
cd serve/host && cargo test revoke        # host: api/revoke_tests.rs, bot_guests_tests.rs (sync gate, dormant bots)
cd serve/host && cargo test --bin kith channels   # host: channels/tests.rs (revoked reply, held wait, delivery re-check)
cd serve/cloud && npx vitest run test/invites.test.ts   # cloud: every Mac of the owner, accept refused
```

## Sync commit

**1f397684bb18** (2026-10-29: origin/main `bc3d92b8` plus the helper's in-sync wait in `kernel/bin/kith`, finding 2).
Between `3eb9f9a50c25` (2026-10-28, the channels' revoke checks) and it, the covered files changed by the invite link's
web origin (`people.rs`, #376), the iPhone's answer to members' access requests, the owner-approved proven device and
idle web devices (`cloud.rs`, #382/#383/#387; `pull` still applies revoked before memberships), and in `kernel/bin/kith`
self-update, the guest sandbox's real paths, `KITH_ASK_ID`, the run outbox (#388) and `ask_helper` for guests (#380,
the `Helper` door from a guest's run): no revoke step, except that #380 made the helper's missing in-sync check
reachable from a guest's turn, now fixed. Before that, `3eb9f9a50c25` was origin/main `b241d7bb` plus the channels'
revoke checks, which took over the retired bridge's doors: the revoked reply, the held wait, the turn-start and
delivery re-checks. First written at `48af5a4eafb8`
(the revoke-a-person code, "kith serve: a revoke goes through the cloud first ..."). Between the two, the covered files
changed by contract and type refactors (cloud HTTP and host RPC), the grants ledger, `bots-cap --json` and budgets, the
host's owner pin in `pull` (revoked is still applied before memberships), the claim refactor in `kernel/bin/kith`, and
the bridge's retirement; no revoke step. Each new channel check was broken in a scratch edit and its test failed. The
same sha is in `drift.sh` (`SYNC=`). Move it only after you have
confirmed that the model still describes the covered code (see "When you change covered code").

## What is promised, and the harm if not

| Property (RevokeDoors.tla) | Rule | Plain sentence | Harm |
|---|---|---|---|
| `R1` | R1 | After a revoke reaches a Mac, no turn for the person or their bot starts there, through any door. | The person keeps using the agent after the owner cut them off. |
| `R2` | R2 | The revoked state wins over every row: hand-given, host-made, `*`. | Agents shared with everyone stay open to them. |
| `R4` | R3, R4 | Revoke and undo never touch rows; after undo the person has exactly what they had. | Undo gives back less; the owner rebuilds access by hand. |
| `R5` | R5 | Once the revoke is on a Mac, a running turn there delivers nothing and spawns no helper. | A reply reaches someone already removed. |
| `R5c` | R5, R7 | Same, from the moment the cloud has the revoke, on every Mac. | Same, through another Mac. |
| `R6` | R6, R9 | Once the owner revoked on any Mac (or the app), no Mac starts a turn for them or their bot, until undo. | They get in through another Mac's bridge. |
| `R6c` | R6, R7 | Same, from the moment the cloud has the revoke, through any door. | Same. |
| `R6cDown` | R8 | ... including while a Mac has internet but no Kith cloud (the bridge still gets Telegram). | Same, for as long as the cloud is unreachable. |
| `R6cWake` | R8 | ... including a message Telegram queued while the Mac slept, handed over on wake. | Same, the moment the Mac wakes. |
| `BotBelowOwner` | I6 | A bot is never allowed where its owner is not. | Their bot keeps working for them. |
| `SyncAgrees` | | A Mac at the cloud's version agrees with the cloud (the lemma R6c rests on). | |
| `NeverBridgeInCloudDown` | R8 | The cost, proved: the bridge serves no guest while the Mac can't reach the cloud. | |

The probes (`Never*` that must fail, and `NoBridgeRefusal`) show the good paths are reachable: the bridge serves a
guest end to end when in sync, an undo gives app turns back, a queued or held message is served once in sync, and the
cost (a guest's message is held while nothing is revoked) is real.

## Coverage record (code -> model action)

| Step in the code | Code | Model action |
|---|---|---|
| deny a revoked person before any row, `*` included | `kernel/bin/kith-auth` `may()` (calls `cut_off` right after the owner check) | `MayP` (guard of every start) |
| a bot is allowed only on its own row and its owner's live may() | `kith-auth` `bot_may` (via `may(parent)`, so a revoked owner's bot is refused) | `MayB` |
| set / clear the revoked state, rows untouched | `kith-auth` `cmd_person` -> `revoke_person` (`--local`: the host's writer) | local part of `AppRevoke`/`CliRevoke`, `CloudDeliver` |
| terminal revoke/undo on a linked Mac goes through kith serve, refused when it can't reach the cloud | `kith-auth` `revoke_person` -> `host_call('revokePerson')`; host `api/people.rs` `revoke_person`: cloud POST first, then kith-auth `--local` | `CliRevoke`/`CliUndo` with `CliNeedsCloud = TRUE` (no outbox) |
| the owner's revoke in the app | none: the app has no revoke/undo button yet; the host API (`revokePerson`) is what a button would call | `AppRevoke`, `AppUndo` (same code path as the terminal's) |
| the cloud's record | `serve/cloud/src/routes/invites.ts` `hostRevokePerson`/`hostUnrevokePerson` (row in `revocations`, memberships cut on every Mac of the owner, each told `cloud.changed`), `revokedOf` in host state | `cloudRevoked`, `cloudVer` |
| the relay refuses the person's app traffic from the cloud record | `revokeMembership` -> `notify` -> DO `/internal/block` (their devices are blocked, sockets closed) | `RelayOK` (app door) |
| the relay refuses their bot's traffic | not in the relay (the cloud doesn't know a bot's owner account): the host's ordered socket stands in. `relay.rs` on `cloud.changed` calls `sync_lease::changed` before reading the next frame; `botguests.rs` `ask` and `thread` wait for `sync_lease::wait`, then `roles::may_use` | `RelayOK` (bot door) |
| each Mac applies the cloud's record | `remote/cloud.rs` `pull`: `members::apply_revoked` (kith-auth `--local`), then `people::cut_off`, then the memberships; `relay.rs` `pull_state` with `sync_lease::mark`/`pulled` | `CloudDeliver` |
| in sync | `remote/sync_lease.rs`: `hub.cloud_synced` (host) and `cloud-sync.json {linked, until}` (kernel, read by kith-auth `bridge_held` for `who`/`resolve` `held`) | `InSync` |
| the bridge (chat channels) takes a message | `serve/host/src/channels/core.rs` `handle`: `who` (`hq.rs` `who`, with `revoked`/`held`); `held` (non-owner) -> `held()` in the background asks `who` every `sync_poll` up to `sync_hold` (2 s / 120 s), then "Kith on this computer is catching up ..."; in sync -> `take`: `revoked` -> "Your access to kith on this computer was removed." and nothing else | `BridgeRecv` (drop / hold / start) |
| the bridge's turn starts | `channels/queue.rs` `work`, right before the batch is taken: `Core::access` -> `hq::resolve_in_sync` (resolve; while `held`, wait up to `sync_hold`, then refuse "out of sync with the Kith cloud"); the owner is not re-checked | the start in `BridgeRecv`; `HelperGate` (bridge) |
| a helper spawn | `bin/kith`, every spawn for anyone but the owner: kith-auth `resolve` (`may()`); while `held`, ask again every 2 s up to `KITH_SYNC_HOLD` (120 s), then refuse "out of sync with the Kith cloud" (exit 3), as `resolve_in_sync`; the owner is never held. A guest's helper comes through kith-permit `ask_helper` (outside the sandbox), which tells the agent to try again | `Helper` (`HelperNoSync = FALSE`) |
| delivery | app: a run's outbox (`run_outbox.rs`, #388) posts into the person's own app chat with no re-check here; their devices are revoked in `cut_off` and blocked by the relay, so it is the app door's `RelayOK`; bridge: `channels/run.rs` `Recheck::still` (via `resolve_in_sync`) at the start of `finish` (the answer) and in `flush` before posting the outbox (refused: the outbox files are deleted, nothing posted); the owner is never re-checked; bot: `botThread` gate; app: the device is revoked here and blocked by the relay | `Deliver` |
| stop a running turn once the revoke lands | `people.rs` `cut_off`: devices revoked, `botguests::enforce`, `stop_runs`, `sweep_claims` | `Stop` |
| network up / cloud down / offline | the relay connection (`relay.rs` loop: `set(false)` on every (re)connect until a pull succeeds, `renew` on each ping) | `SetNet` |

A check and the act it guards are separate actions in the model (`Start`, `Helper`, `Deliver`). In the code the
channels check in `handle`, queue in the inbox, and start in `queue.rs` `work`: that is why `work` re-checks (in
sync), and the answer and each outbox flush re-check again.

**The bridge door is now the host's chat channels** (`serve/host/src/channels/`). The Python bridge
(`kernel/bin/kith-bridge`, `kernel/lib/kith_bridge/core.py`, its `kith-bridge-revoke-check`) was removed 2026-10-27;
its three checks (the revoked reply, the held wait, the delivery re-check) were ported to the channels as the rows
above say, with the same semantics, so the model's `BridgeRecv`, `HelperGate` (bridge) and `Deliver` are unchanged.
The model's "bridge" is this door.

## Bounds and assumptions

- 2 Macs, 1 person, 1 bot, one turn per (Mac, person or bot), revoke and undo at most once, rows fixed (m1:
  hand-given and `*`; m2: hand-given and host-made), all Macs online at the start.
- **The person has one id on every Mac: their Kith account** (the `kith` handle, a `user_...` id). The cloud's
  `revocations` table and host state are keyed by it, and `apply_revoked` matches people by it. A person known only by
  a bridge handle (no Kith account) is revoked on that Mac only, without the cloud (`everywhere: false`): outside the
  model, which has one cross-Mac person.
- `InSync(m)` reads the cloud in the same step as the check. The code uses a lease: `sync_lease` ends it at once on
  `cloud.changed` and on a dropped connection, and otherwise `LEASE` (120 s) after the last ping heard. So a Mac whose
  link died silently (no close), or whose kith serve crashed, keeps its bridge serving guests for up to `LEASE` while
  a revoke made meanwhile can't reach it: a bounded window the model does not have.
- Memberships are cut on revoke (the app door is the relay's device block). Undo restores kith-auth rows, bridge
  access and bots; the person's app needs a new invite. The model's app door after undo (`NeverAppAfterUndo`) is the
  re-invited app.
- Time is not modeled; a finding is an order of steps that can happen. Liveness is not checked.

## Findings on the design (rows that fail on purpose)

1. **A terminal revoke that works offline breaks R6** (`terminal-offline` / `R6`): the other Macs never hear of it,
   so a fully in-sync Mac serves the person. Hence R9: on a cloud-linked Mac a terminal revoke or undo needs the
   cloud, and is refused with a message when it can't be reached (`api/people.rs` `revoke_person`). `app-only` (no
   terminal revoke) passes too.
2. **A helper that reads only `may()` breaks R5c** (`broken-helper-no-sync` / `R5c`): a guest's turn on a Mac that
   has not yet pulled a revoke made elsewhere spawns a helper for them; `R5` still holds, since local `may()` catches
   the revoke once it is on the Mac. `kernel/bin/kith` read only `allowed` from `resolve` until 2026-10-29, which #380
   (`ask_helper` for guests) made reachable; it now waits while `held`, as the channels' `resolve_in_sync` does.
3. The probes under `code` and `app-only` (see above) are reachable, as they should be.

## How we know it can fail

| Variant (`variants/*.set`) | Removed | Fails |
|---|---|---|
| `broken-star` | the revoked check before the `*` row | `R1`, `R2` |
| `broken-delete-rows` | "revoke never touches rows" | `R4` |
| `broken-no-deliver-recheck` | the re-check at delivery | `R5` |
| `broken-bot-ignores-owner` | bot_may's live owner check | `BotBelowOwner` |
| `broken-no-relay-check` | the relay's / socket order's refusal | `R6c`, `R5c` |
| `broken-bridge-no-sync` | the bridge's in-sync wait | `R6c`, `R6cDown`, `R6cWake`, `R5c`, `NeverBridgeInCloudDown` |
| `broken-helper-no-sync` | the helper spawn's in-sync wait (`bin/kith`) | `R5c` (`R5` passes) |
| `terminal-offline` | R9 (terminal revoke needs the cloud) | `R6` |

On the code (dry run at the sync commit, edits dropped after):

- removing the revoked check from kith-auth `may()`: `kith-auth-revoke-check` (6 FAIL), `kith-auth-props` (shrunk to
  `[person sam, member house sam, revoke sam]`) and 5 host tests (`cargo test --bin kith revoke`: 4 of
  `api::revoke_tests` and the bot test below) fail.
- removing the `sync_lease::wait` in bot `ask`: `bot_guests_tests` `a_bot_ask_waits_for_the_cloud...` fails (an ask
  while out of sync is not refused "syncing").
- removing the in-sync wait in `kernel/bin/kith` (2026-10-29, at `1f397684`): `kith-guest-helper-check` fails 4
  checks (a held guest's helper runs, and is not told it is out of sync).

## When you change covered code

1. Run `verification/revoke/drift.sh`; read each diff it lists, including new code that writes the revoked state or
   rows, or that starts a turn or delivers by a new door (a new door is a new action in the model).
2. Decide per change: no effect; the model no longer matches (a step split in two, a check moved, a new door); or the
   promise changed on purpose (cite it). Update `RevokeDoors.tla` / `base.consts` from the code, not from what makes
   a run pass.
3. Run `run.sh`. A row that differs is drift (replay the counterexample against the code; a step the code cannot take
   means fix the model) or a bug (every step can happen: report it with the steps in the code's words and the harm;
   do not weaken the property).
4. A pass on changed code may have lost the ability to fail: break the new check in a scratch edit, see a test or a
   run fail, drop the edit. Add a broken variant for a new check.
5. Only then set `SYNC` in `drift.sh` and the sync commit above to the commit you checked.

## Map of the folder

| File | What |
|---|---|
| `verification/revoke/RevokeDoors.tla` | the model; switches (CONSTANTS) choose the code as built and its broken variants |
| `verification/revoke/base.consts` | the switches for the code as built |
| `verification/revoke/variants/*.set` | each variant's overrides |
| `verification/revoke/checks.tsv` | variant, kind, property, expected verdict |
| `verification/revoke/run.sh`, `drift.sh` | run every row; what changed since the sync commit |
