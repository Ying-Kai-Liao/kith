---
name: revoke-access
description: Model-checks (TLA+/TLC) that a revoked person, and their bots, reach nothing on any of the owner's Macs, and that undo restores exactly what they had. Use when a change touches kernel/bin/kith-auth may(), cut_off, bot_may, bridge_held, door_state, owner_cut/resolve_bot or cmd_person/revoke_person, core/runtime/src/authority/ (revoke.rs person_suspend/person_remove/person_rename, the readers), the helper spawn's in-sync wait in kernel/bin/kith (and kith-permit ask_helper, and its Rust port core/kernel/src/permit/{helper,door}.rs), serve/host/src/api/people.rs revoke_person or cut_off, serve/host/src/remote/{members,cloud,relay,sync_lease,core_revokes}.rs (and the authority poll in cards/mod.rs; held/held_under, the ids a renamed person is held under; cut_off_held), the owed sweep in serve/host/src/terminal/core_holders.rs, the bot ask/thread gates in api/botguests.rs, a guest door's start or sweep (kernel/lib/kith_door.py, serve/host/src/doors.rs), the chat channels' access checks in serve/host/src/channels/ (core.rs handle/held/take/access, queue.rs work, run.rs Recheck/finish/flush, hq.rs who/resolve_in_sync), or the cloud's person revoke in serve/cloud/src/routes/invites.ts. Also use to answer "can a revoked person still get through?".
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
verification/revoke/run.sh                          # every row of checks.tsv (228 rows), then the 7 Reconcile.tla rows
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

**f9b5e509f476bb88f6203a125ae863369ba03aa9** (2026-11-17, rust-rebuild after #1013 R-rename, #1007 rooms D, #1010, #1015: every covered change since `f5b92e19` was read, see "Re-sync 2026-11-17" at the end. `Reconcile.tla` gained the rename (`Renames`, `RecByCurrent`) and four rows, two of them findings; `RoomsRename.tla` is in `verification/rooms/`; no `RevokeDoors.tla` row changed.) Earlier: **f5b92e19daddcfdfc92162abfc38be2b63dd3e4d** (2026-11-12, M2-23 S-b: the secret writers' switched path; the model gained `switched-grant`, `broken-grant-no-person`, `broken-forget-keeps-grant`; `run.sh` every row as expected.) Earlier: **0e958a1b99a5444265d2803158087462147d6911** (2026-11-06, rust-rebuild after #886/#882: the covered changes since `ff380b33` were read, see "Re-sync 2026-11-06 (3)" at the end; no revoke step, no model change.) Earlier: **ff380b3343ac78a03ca5b4ab5936a48e8d89633e** (2026-11-06, rust-rebuild after #884: every covered change since `10c4eb6c` was read, see "Re-sync 2026-11-06 (2)" at the end. Three new switches (`Switched`, `SwitchLag`, `CoreOnlyWrite`); a finding on the rollback window; no base row changed.) Earlier: **10c4eb6c012f6ad77389633bd7ecb5881054f5f4** (2026-11-06, rust-rebuild after #871: every covered change since `fb5f140f` was read against the model, see "Re-sync 2026-11-06" at the end. One new switch, `CoreGate`; no other step, check or door changed; `run.sh` gave every row as expected.) Earlier: **fb5f140f6c84bc079684575a8ce93373eba51275** (NOT moved by the F3-F6 PR: about 940 lines changed in files that hold revoke steps since this sha (`kernel/bin/kith` 437, `botguests.rs` 409, `people.rs`, `members.rs` 122, `cloud.rs` 78, `relay.rs` 33) and nobody has read them against the model yet. All 80 rows of `run.sh` passed again at `0872ea58892e` (2026-11-03, rust-rebuild with the built-in agent rule); that shows the model unchanged, not that the code still matches it. `people.rs` `local_revoke` was read: kith-auth's `--revoke --local` first, then `cut_off`. Earlier: **fb5f140f6c84bc079684575a8ce93373eba51275** (2026-10-30: the guest jobs (#428) at `89162b85` merged into the guest door (#438). The model is the union of both: #428's `HelperNoSync` and its rows with the door's `GuestDoor`, `DoorNoHold`, `DoorSplit`, `NoDoorSweep` and their rows, 44 in all; the cards' `answer` takes the person as an `Option` (None for the owner) and keeps the door's door and helper branch. `run.sh` gave every row as expected.) Earlier: `5a7898911c39` (2026-10-30: origin/main `a9db7339` merged into the guest jobs (#428), over #428's merge of #421. Main's covered changes: the `send-to` dispatch line in `kernel/bin/kith`, and bot `ask` in `botguests.rs` refusing a reused nonce for a different message (`is_retry`), after the sync wait and `roles::may_use`; neither is a revoke step. With #428's jobs and edge grants (below), rows as expected in `run.sh`.) The guest door (#438) was checked on its own branch before it merged this: `961325fb23ac` (2026-10-30: the door's review fixes. A tool installed where the box would read the home folder is refused, and `run()` kills the box whenever its claim was written. A box that would not die keeps its claim (`left`), and the sweep keeps a claim until its kill ends. This makes the code meet `DoorStart`'s one-step claim and the weakly fair `DoorSweep`. No state, row or step changed) and `ab050f9277a3` (2026-10-30: the guest door. A door starts in one locked step under the person's flock that reads `may()` and the held state and claims it (`DoorStart`), and the host's sweep ends a door whose person may no longer run it (`DoorSweep`); new rows `DoorD2`, `DoorD2c`, `DoorD1` behind `GuestDoor`); `run.sh` gave every row as expected at both. Earlier: `c6d8a1cf2752` (2026-10-30: origin/main `996b76c0` merged with the guest workbench (#421). #421's changes: the temp folder removal in `cut_off` after the bot sweep and in `revoke_person`/`--rm`, `kith-auth exec` scrubbing paths into a guest's folders, no local servers in a guest's shell; none is a revoke step, rows as expected in `run.sh`. #400's helper in-sync wait in `kernel/bin/kith` stays on every helper spawn, which #421's paths use too.) Earlier: `1f397684bb18` (main `bc3d92b8` plus the helper's in-sync wait) and `892b64dd7d2d` (the workbench without it). The jobs half of the guest workbench (#428) was checked on its own branch before it merged this: `62f5a5c7c80d` (2026-10-30: an invitee's bot gets only the jobs its owner holds, and `job`/`helper --rm` take a former person id; no state, row or door changed) and `a0b2e759d7b9` (2026-10-30: the jobs and edge grants. A tick (`job`), a helper's always (`helper`) and its once (`helper_once`) are fields of the person's own row, so revoke and undo still touch no row and `may()` still checks the revoked state before any row; the once is used up by `resolve --take-once` in the locked step that runs `may()`, so a revoke between the card and the launch wins (`kith-guest-jobs-check` "a revoke beats a once"; `kith-auth-props` break M15 fails); `run.sh` gave every row as expected at both. History:
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
the bridge's retirement; no revoke step. Each new channel check was broken in a scratch edit and its test failed.
The bot door rows (`BotDoor`, `BotDoorCut`, 2026-11-01, N2: kith-auth `door_state` cuts a bot's tick to its owner's
grants with `owner_cut`, the cut `resolve_bot` makes) were added without moving the sha: `drift.sh` then listed
covered changes from earlier commits that were not re-read for this. The same sha is in `drift.sh` (`SYNC=`). Move it only after you have
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
| `DoorD2` | D2 | Once the revoke is on a Mac, no guest door (a declared program run outside the sandbox, `kith_door`) of the person starts there. | A removed person renders on the owner's Mac. |
| `DoorD2c` | D2, R8 | Same, from the moment the cloud has the revoke, on every Mac: a Mac behind the cloud holds the start. | Same, through a Mac that hasn't heard yet. |
| `DoorD1` | D1 | A door running for a revoked person is stopped (liveness, given the host's sweep runs). | Their render keeps running, writing, after they were cut off. |
| `BotDoorCut` | I6 | An invitee's bot starts a guest door only while the invitee holds it (its own tick is cut to its owner's). | A bot ticked more than its owner runs a door the owner was never given. |
| `RetryKeepsRestored` | M3 P8 | A revoke sweep the core did not take is owed (a row in the host store, `terminal/core_holders.rs`) and retried; a retry after the revoke was undone ends nothing (`claim.sweepPerson` is a full `revoke_person`). Rows `owed-sweep`, `broken-owed-retry-revokes` (model: `OwedSweeps`, `SweepOwed`, `RetrySweep`). The model drops the row when P is not revoked now; the code drops it only if the row was owed while P was suspended in the host store and P has no suspend rows now. GAP: a revoke that left nothing to suspend, or whose `suspend_person` failed, is never dropped, so an undo then the retry ends the restored person's runs once (fails safe; fix: judge by the authority's revoked state). | A retry after an undo ends the runs the undo restored. |

The probes (`Never*` that must fail, and `NoBridgeRefusal`) show the good paths are reachable: the bridge serves a
guest end to end when in sync, an undo gives app turns back, a queued or held message is served once in sync, and the
cost (a guest's message is held while nothing is revoked) is real. `NeverDoorRuns` and `NeverDoorKilled` show a
member's door runs and the sweep does stop one; `NeverBotDoorRuns` and `NeverBotDoorKilled` the same for the bot's door
(stopped once the owner takes the invitee's job back).

## Coverage record (code -> model action)

| Step in the code | Code | Model action |
|---|---|---|
| deny a revoked person before any row, `*` included | `kernel/bin/kith-auth` `may()` (reads the person's own `revoked` flag; bots are cut through `may(parent)`). Assumes the owner is never revoked, kept by two writes: `revoke_person` refuses to REVOKE the owner (undo is allowed: it only clears an old owner-and-revoked row, `kith doctor` flags one; the model has no revoked owner, so this is outside it and widens nothing), and `person`/`addPerson` refuses to promote a revoked person | `MayP` (guard of every start) |
| a bot is allowed only on its own row and its owner's live may() | `kith-auth` `bot_may` (via `may(parent)`, so a revoked owner's bot is refused) | `MayB` |
| set / clear the revoked state, rows untouched | `kith-auth` `cmd_person` -> `revoke_person` (`--local`: the host's writer) | local part of `AppRevoke`/`CliRevoke`, `CloudDeliver` |
| the core's suspend and undo (M2-4): one `revoked` fact on the person in core.db, memberships, `helper_once`, bots and invites untouched; a repeat keeps the first moment; owner (revoke only: undo clears an old owner-and-revoked row), bots and unknown people refused; the temp folders of the person and their bots go after the commit | `core/runtime/src/authority/revoke.rs` `person_suspend` (rows `people.suspend`, `people.undo`), `call.rs`; the readers `Authority::may/resolve/resolve_bot/cut_off` refuse before any row. Not yet the host's path (M2-18 switches it); the cloud record and sweep are lane H | local part of `AppRevoke`/`CliRevoke`, `CloudDeliver`; `MayP`/`MayB` |
| the core's removal and rename (M2-4): ids retired, bots, rows and codes go; a rename carries a suspension | `revoke.rs` `person_remove`, `person_rename`, `person_remove_unused`, `retired` | none: a removed person is no person (`MayP` refuses the unknown); the model has no remove |
| the core's access requests (M2-3, not switched: the host's `cloud.rs` `settle` still decides): a member's device request is approved only while the person is still known and not cut off, read in the same claim transaction that ends the request; a person suspended or removed when the owner answers has the request denied and kept (never approved, never listed again; an undo afterwards does not revive it; in the core alone an undo before the answer leaves the request waiting as it was, but the host's membership sync denies a suspended person's waiting requests with either switch, so after that sync an undo does not bring them back) | `authority/requests.rs` `approve_in` (`is_member`: `secrets::cut_off` over the tables of that transaction), `deny`, `list`; `authority/tests/requests.rs` `a_refusal_that_ends_the_request_is_kept` and the parity script's "a member whose membership ended while the request waited" (replayed against `settle`) | none: the model's doors are started and cut by `may()`; an approved device is a door of the app, `RelayOK`, and the cloud's grant decides it |
| the owner's ticks and the access requests on the core (M2-17, behind `KITH_CORE_AUTHORITY=1`): `setPersonAccess` takes an agent away with `people.revoke` in the core, kith-auth untouched, an unreachable core an error; the pending requests of a person with no membership, a suspended person included, are denied on every membership sync (the cloud's "membership gone" and the host's suspension the core cannot see); the approval keeps the host's checks in front (request held, code shown, membership row, the core's device equal to the host's copy) | `serve/host/src/api/people.rs` `set_person_access` (`core_grant`, `core_revoke`), `remote/cloud.rs` `deny_without_membership`, `decide_in_core`, `file_in_core`; `api/access_core_tests.rs`; the parity scenario "suspend, then the membership sync, then undo" | none: the model's doors are cut by `may()`; a request is not a door |
| the core's run tokens die with the revoke (M2-13): tokens of the person and their bots end inside the committing core call; undo revives none (a new run mints a new token); the run-socket call re-checks under the core lock (known person, not cut off, run still live); a mint for a revoked or unknown person is refused; non-Unix refuses | `core/runtime/src/runs.rs` `after_authority_write`/`end_people`/`is_live`, `daemon.rs` (authority branch, `run.mint`, run handler), `call.rs` `run_person_allowed`/`call_run`, `authority/mod.rs` `run_allowed_in`. Not yet the host's path (`KITH_RUN_TOKENS_VIA_CORE`, off) | `Stop` (a running turn delivers nothing after the revoke); `MayP` at the start |
| terminal revoke/undo on a linked Mac goes through kith serve, refused when it can't reach the cloud | `kith-auth` `revoke_person` -> `host_call('revokePerson')`; host `api/people.rs` `revoke_person` (revoke only for the owner: an owner's undo stays on this Mac, no cloud post): cloud POST first, then kith-auth `--local` | `CliRevoke`/`CliUndo` with `CliNeedsCloud = TRUE` (no outbox) |
| the owner's revoke in the app | none: the app has no revoke/undo button yet; the host API (`revokePerson`) is what a button would call | `AppRevoke`, `AppUndo` (same code path as the terminal's) |
| the cloud's record | `serve/cloud/src/routes/invites.ts` `hostRevokePerson`/`hostUnrevokePerson` (row in `revocations`, memberships cut on every Mac of the owner, each told `cloud.changed`), `revokedOf` in host state | `cloudRevoked`, `cloudVer` |
| the relay refuses the person's app traffic from the cloud record | `revokeMembership` -> `notify` -> DO `/internal/block` (their devices are blocked, sockets closed) | `RelayOK` (app door) |
| the relay refuses their bot's traffic | not in the relay (the cloud doesn't know a bot's owner account): the host's ordered socket stands in. `relay.rs` on `cloud.changed` calls `sync_lease::changed` before reading the next frame; `botguests.rs` `ask` and `thread` wait for `sync_lease::wait`, then `roles::may_use` | `RelayOK` (bot door) |
| each Mac applies the cloud's record | `remote/cloud.rs` `pull`: `members::apply_revoked` (kith-auth `--local`), then `people::cut_off`, then the memberships; `relay.rs` `pull_state` with `sync_lease::mark`/`pulled` | `CloudDeliver` |
| in sync | `remote/sync_lease.rs`: `hub.cloud_synced` (host) and `cloud-sync.json {linked, until}` (kernel, read by kith-auth `bridge_held` for `who`/`resolve` `held`) | `InSync` (M2-10b: in the core the lease's writes `syncLease.link/unlink/changed/drop/pulled/renew` are `Host` rows, reached only on a connection that proves the host secret, which lives only in the memory of the `kith serve` whose own daemon holds the core lock (M2-10c: no file, a standalone daemon has no Host, `changed` and `drop` are sent repeatable and a failed delivery reads as not in sync); the documented owner routes (an owner terminal or agent) are refused. Remaining caveat: a dev build without the hardened runtime lets a same-user process read the serve's memory. Open item (LATER 2, not modelled): a second relay connection (a second Mac of the same owner, or a relay reconnect racing the old one) is not in the TLA+ model in `verification/revoke/`; the one-Host rule (only the serve holding the core lock is Host) covers one Mac, and the model still has one relay connection per Mac. Tested by `daemon_tests::the_owner_token_alone_is_refused_every_host_row_and_the_host_secret_is_accepted`; the model is unchanged) |
| the bridge (chat channels) takes a message | `serve/host/src/channels/core.rs` `handle`: `who` (`hq.rs` `who`, with `revoked`/`held`); `held` (non-owner) -> `held()` in the background asks `who` every `sync_poll` up to `sync_hold` (2 s / 120 s), then "Kith on this computer is catching up ..."; in sync -> `take`: `revoked` -> "Your access to kith on this computer was removed." and nothing else | `BridgeRecv` (drop / hold / start) |
| the bridge's turn starts | `channels/queue.rs` `work`, right before the batch is taken: `Core::access` -> `hq::resolve_in_sync` (resolve; while `held`, wait up to `sync_hold`, then refuse "out of sync with the Kith cloud"); the owner is not re-checked | the start in `BridgeRecv`; `HelperGate` (bridge) |
| a helper spawn | `bin/kith`, every spawn for anyone but the owner: kith-auth `resolve` (`may()`); while `held`, ask again every 2 s up to `KITH_SYNC_HOLD` (120 s), then refuse "out of sync with the Kith cloud" (exit 3), as `resolve_in_sync`; the owner is never held. A guest's helper comes through kith-permit `ask_helper` (outside the sandbox), which tells the agent to try again | `Helper` (`HelperNoSync = FALSE`) |
| delivery | app: a run's outbox (`run_outbox.rs`, #388) posts into the person's own app chat with no re-check here; their devices are revoked in `cut_off` and blocked by the relay, so it is the app door's `RelayOK`; bridge: `channels/run.rs` `Recheck::still` (via `resolve_in_sync`) at the start of `finish` (the answer) and in `flush` before posting the outbox (refused: the outbox files are deleted, nothing posted); the owner is never re-checked; bot: `botThread` gate; app: the device is revoked here and blocked by the relay | `Deliver` |
| a bot takes a room secret (`botSecretFetch`, room secrets) | `botguests.rs` `secret_fetch`: the bot's own row only (404 for anything else), member and join point, `sync_lease::wait` (503 while out of sync), then claim `pending -> taking`, `kith-auth once take`, a **re-check** that the bot is still active and a member, then `taking -> taken` as a compare-and-set that answers only if it wins; `end_bot` (via `group::left_rooms`), a room edit, `delete_bot` and the sweep end the row (`dropped`/`expired`, both `pending` and `taking`) and `once drop` the item. No new TLA+ variant: it is the model's bot `Deliver` door with the `botThread` gate's post-take re-check; the architect's own model of the ordering is `designs/2026-10-31-room-secrets/model/` (RoomSecret.tla, `OnlyCurrentMember`). Tested by `cards::room_secret_tests::a_bot_ended_between_the_claim_and_the_answer_gets_no_value` | `Deliver` (bot door) |
| stop a running turn once the revoke lands | `people.rs` `cut_off`: devices revoked, `botguests::enforce`, `stop_runs`, `sweep_claims` | `Stop` |
| a renamed person's revoke whose ids can't be read (the core down) | `people.rs` `cut_off_held` (from `local_revoke`'s failure branch, `revoke_person`'s core-down branch and `cloud.rs` `pull`): `core_revokes::held` errs -> cut by the given id and `owe_cut` (kv `revoke.owed_cut.<person>`, durable); `core_revokes::pay_owed` on every tick of the 3 s poll, under `AUTH_WRITES`: rows read under the lock, a read of the ids that answers -> `cut_off` every id, row dropped; a failed read leaves it. Every undo clears it in its own locked section (`clear_owed`, every id the person is held under): `local_revoke`'s undo (the app's, and a paired Mac's handle undo through `handle_revokes::exchange_in`) and `members::apply_revoked`'s `--unrevoke` (the cloud `pull`); `pull`'s restored gives back under `held_under`, as `local_revoke`'s undo does | `ReconcileOwed.tla`: `AppCut` (owed when the read fails), `OwedRetry`, `AppUndo` (clears the row) |
| network up / cloud down / offline | the relay connection (`relay.rs` loop: `set(false)` on every (re)connect until a pull succeeds, `renew` on each ping) | `SetNet` |
| a guest door starts | `kernel/lib/kith_door.py` `run`: under kith-auth's lock (`auth_lock`), `check` (= `kith-auth door`, `door_state`: `may()`, then `bridge_held` -> held, then the job and its tick), the claim written, then the box started, one step; ON A SWITCHED MAC (RouterDoor) `check` reads the core under the core's lock, which the revoke holds instead of kith-auth's flock: a check, then a start, and a revoke can land between (DoorD2 fails on `switched-router-door`); the guarantee is the named sweep (`CutOffSweep`, DoorCutAtSweep: one-shot under the flock, some time after the commit, triggered by the app's `cut_off` or by the host's reconcile of a core-only revoke, `remote/core_revokes.rs`) and the 60 s loop; kith-permit `run_door` asks the owner's job card first when not ticked | `DoorStart` (`DoorGate` = `MayP` and `InSync`) |
| the host's guard with `KITH_CORE_AUTHORITY=1` (OFF in every deploy, but the code takes it today) | `api/authority.rs` `core_selected` = opt-in and `gate_open(fingerprint_of(auth.json))`: the gate thread stats auth.json BEFORE each `cutover.mayServe` ask and latches the answer with that fingerprint; every `save_auth` is a new inode, so any write shuts the gate at once and the host is back on kith-auth until the core is told. kith-auth `mirror_to_core` (`people.suspend` / `people.undo`, over `kith kernel core-call`, an embedded core when no daemon runs) runs AFTER `save_auth`, one try, a failure only warns. The guard (`roles::resolve`: `may_use`, the app and bot starts, `botThread`) asks the core only while the gate is open, where core and file agree | `CoreMirror`, `GuardP`/`GuardB`/`GateOpen` (`CoreGate`; the app door's and bot door's start and the app/bot delivery re-check use them; the bridge, the helper spawn and the guest door read kith-auth directly and are unchanged) |
| a bot's guest door starts | the same `kith-auth door` for the bot: `door_state` -> `owner_cut` (one helper with `resolve_bot`): the bot's own ticks cut to what its owner's `resolve` holds, Kai's own bots uncapped; refused `owner-lacks` (kith-permit says why, asks no card; the sweep ends a running one) | `BotDoorStart` (`BotDoorGate` = `MayB`, `InSync`, the owner's tick `ptick`); `OwnerTick` |
| stop a running guest door | `kith_door.sweep` (under the same lock: every claim of the listed people, of a dead supervisor, and any whose `kith-auth door` is not ok, held excepted) from `serve/host/src/doors.rs` `sweep`: `people.rs` `cut_off`, `botguests.rs` `sweep_claims`, and `sweep_loop` every 60 s (the named sweep of a revoke on a switched Mac is `CutOffSweep`: it needs the flock, which a start holds from its check to its claim; a revoke written straight to the core runs no `cut_off` itself, so the host's poll runs it: `remote/core_revokes.rs` sees the core's revision change within about 3 s and cuts the person off here, and the loop is the backstop) | `DoorSweep` (weakly fair) |
| the core's door table (M2-14, not switched: `kith_door.py` and the host keep their claim files; a door started the old way has no row, so a core sweep cannot see it) | `core/runtime/src/doors/mod.rs` `start` ports `door_state` (`door_refusal`: the owner refused with kith-auth's words, `cut_off`, `may`, the sync lease for anyone but the owner, and the door's piece `name@digest` among the doors `resolve` grants, which for a bot is already cut to its owner's); the finer reasons (no-job, stale, not-ticked, declined, widened, owner-lacks) are one fail-closed sentence. ONE claim transaction does it and writes the `starting` row (probes of the supervisor run before the transaction); `mark_live` (`starting` to `live`, by version: a row a sweep got to first answers `lost` and the supervisor stops the box); `sweep` / `sweep_ids` (open rows of the named people and their bots, a dead supervisor by start time, rows `door_refusal` no longer allows: CAS on the version, `swept` is final); `end` | `DoorStart` (`DoorGate` = `MayP` and `InSync`; the row is the claim, so `starting` = the model's `running`), `DoorSweep`, `DoorEnd` |
| a revoke in the core marks the person's door rows swept (the rows only; stopping a running box is the host's, not yet switched) | `authority/revoke.rs` `follow_in_doors`, run by `store::run_with` in the SAME transaction as `people.suspend`, `people.removePerson`, `people.removeBot` (ids: the person and their bots, never the owner); `people.renamePerson` carries the rows; `people.undo` does nothing (swept stays swept). Same transaction rather than right after the commit: no moment where a person is revoked and a door of theirs is still open, and a crash cannot sit between the two | `DoorsSwept` in `CliRevoke`/`CloudDeliver` (`CoreDoorHook`), `DoorsCutWithRevoke`, `DoorNoResurrect` |
| the core's revoke order for conversations (M3-10d, not switched). For a removal and for a suspend this is the code (`people.suspend` writes an `open_removals` row in its transaction, runs the order after the commit, and `replay_open` finishes a crash; an undo before the order drops the row without acting); `Swept`, `OrderKept`, `AsksEnded` in `core-order` hold: the revoke epoch bumped and the claims of the person and each of their bots swept (`claims::revoke_person`, one call per principal), THEN `asks::end_tree` for the same principals; two transactions, each idempotent, so a retry finishes a crash between them; a stopped ask's conversation takes no ask or wake turn (`Error::Stopped`, never retried) and takes no `--to` (`asks::check_to`: "was stopped; ask anew") | `core/runtime/src/revoke_order.rs` `revoke_everywhere` (row `ask.revokeEverywhere`), `conversations/claims.rs` `revoke_person`/`claim_in`, `asks/mod.rs` `end_tree`/`check_to`; tests in `revoke_order/tests.rs` | `ClaimEnding` (`CoreSweep`; `SkipBots` drops B; was `ClaimSweep`, which the older notes below still name; with `KillSplit` it only moves the claim to `ending`, then `KillSent` / `KillLost` / `KillReSend`), `AskSweep` waiting for it (`AsksFirst` drops that), `OrderKept`, `Swept` |
| remove their temp folder (`<machine>/run/t/<p>`, their bots' too) | `people.rs` `cut_off` after `stop_runs` and the bot sweep (`sweep_claims`) -> kith-auth `person <p> --clear-temp`; kith-auth `revoke_person` (not `--local`) and `person --rm` -> `kith_paths.guest_tmp_remove`; a folder with no live record is emptied at the next run (`guest_tmp_ready`) | none: a cleanup, not a door. No turn starts for a revoked person, so nothing can use it; pinned by `kernel/tests/kith-guest-sandbox-check` |

**Paired computers (Mac link, phase 1)** are a new door on B: another of the owner's Macs calling with a device key
paired by a code typed on B (`remote/peers.rs`, `Principal::Peer`). Until `peerAsk` lands it starts no turn: its only
method is `peerAgents` (a list of the owner's agents), so no model action yet. Ending it: `kith unlink` on either Mac
removes the row here first (`peers::unlink`, works without the cloud) and then revokes the grant in the cloud; the
cloud ends one by no longer listing it (`peers::apply`, never adds one), and an owner change ends all of them
(`peers::drop_all`, applied in `pull` before anything else). A removed row leaves the relay's signed list of devices
(`relay.rs` `signed_acl`), which closes that key's socket. Not a person revoke: phase 1 asks carry the owner only. When
`peerAsk` starts runs for a person, its start gate (paired, same epoch, person allowed, in sync) is a new start action
here, and the brief's L5 (no run of a peer or person starts after unpair or revoke is applied) a new property.

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
  model, which has one cross-Mac person. **On a switched Mac a rename in the core** leaves the host's rows (devices,
  memberships, invites, room stays, claims) under the old id; the model still has one id because every host narrowing
  acts on every id `core_revokes::held_under` returns (R-rename, #1013): `cut_off_held` (the app's and terminal's
  `local_revoke`, the cloud's `pull`), `set_person_access`, the reconcile's `to_cut`, the "no card for a set-aside
  person" fence in `cards/mod.rs`, `local_revoke`'s undo (`restored` per id). That is checked in `Reconcile.tla` (`rename` rows) and, for room
  stays, `verification/rooms/RoomsRename.tla`. Where `held_under` cannot ask the core it falls back to the id given, and
  the cloud's undo restores only the id it names: see the findings in "Re-sync 2026-11-17".
- `InSync(m)` reads the cloud in the same step as the check. The code uses a lease: `sync_lease` ends it at once on
  `cloud.changed` and on a dropped connection, and otherwise `LEASE` (120 s) after the last ping heard. So a Mac whose
  link died silently (no close), or whose kith serve crashed, keeps its bridge serving guests for up to `LEASE` while
  a revoke made meanwhile can't reach it: a bounded window the model does not have.
- A revoke parks the person's devices, memberships and open invites (host `suspend_person`; the cloud keeps the
  membership rows stamped with the revoke's time) and the undo gives them back exactly (host `restore_person`, cloud
  `hostUnrevokePerson` re-activates the memberships and re-grants their devices under new ids; `apply_grants` follows the
  new grant id). So an undone person's app works again without a new invite (`ItemsOn`, `ItemsExact`,
  `NeverAppAfterUndo`). A removal while revoked is final (`forget_suspended`).
- `GuestDoor` (TRUE only in the `door` variants, so the other rows keep their size): one guest door of the person,
  on m1 only, run at most once, from a live turn of theirs there (any door). The job card and the
  tick are not modeled (`kith-guest-door-check` and the host's `cards::edge_tests` check them): `DoorGate` is what
  revoke adds to them.
- `BotDoor` (TRUE only in the `bot-door` variants): one guest door of P's bot B, on m2 only, from a live turn of B.
  B's own tick always names the door; P's tick of its job (`ptick`) starts off and the owner toggles it at will.
  Kai's own (uncapped) bots are not in the model.
- Time is not modeled; a finding is an order of steps that can happen. Liveness is checked only for `DoorD1`, under
  weak fairness of `DoorSweep`: the host's 60 s loop. While kith serve is down nothing sweeps (a door started before
  then keeps running until the host is back; its first loop ends it).

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
4. **A failed revoke of a renamed person cuts only the current id** (`reconcile/rename-failcut` / `FailCutHolds`, 2026-11-17).
   See "Re-sync 2026-11-17". Closed the same day by the owed cut (`reconcile-owed` `fix` passes, `today` fails
   FailCutReached, `undo-keeps` fails NoLostUndo; see "Owed cut" at the end). 5. **An undo after a rename can leave the old id set aside** (`reconcile/rename-undo-miss`,
   accepted: fail closed).

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
| `broken-door-no-hold` | the door start's held (in sync) check (GuestDoor.tla `nohold`) | `DoorD2c` |
| `broken-door-split` | the one locked step: access read, then claim and start later (GuestDoor.tla `split`) | `DoorD2` |
| `broken-no-door-sweep` | the sweep of running doors | `DoorD1` |
| `door` (without `CoreDoorHook`) | the core's revoke sweeping the doors in its own step: a revoke leaves a window with a door still running until the sweep action | `DoorsCutWithRevoke` (fails in `door` and `bot-door`, passes in `core-door` and `core-bot-door`); row-level: the rows are swept with the revoke, a box is stopped later by the host |
| `broken-door-undo-revives` | an undo that puts a swept door row back (the core must leave swept final) | `DoorNoResurrect` |
| `broken-bot-door-no-cut` | the owner cut in a bot's door check (kith-auth `door_state` before N2) | `BotDoorCut` |
| `broken-owed-retry-revokes` | the retry's re-judge from the person's state now (it sweeps the restored person again) | `RetryKeepsRestored` |
| `broken-core-gate-stale` | the gate's fingerprint check (`gate_open_at`): the core's copy answers for the host's guard after auth.json changed and before the mirror told the core | `R1`, `R5` |
| `reconcile/broken-rename-current` (Reconcile.tla) | the reconcile's former ids (`host_ids` reading `was` in `to_cut`): a renamed person is cut by the current id only | `Narrows` |

On the code (dry run at the sync commit, edits dropped after):

- removing the revoked check from kith-auth `may()`: `kith-auth-revoke-check` (6 FAIL), `kith-auth-props` (shrunk to
  `[person sam, member house sam, revoke sam]`) and 5 host tests (`cargo test --bin kith revoke`: 4 of
  `api::revoke_tests` and the bot test below) fail.
- removing the held check from kith-auth `door_state`: `kith-guest-door-check` fails 2 (`kith-auth door` says ok,
  and a door starts while the Mac is out of sync).
- reading the bot's own tick without `owner_cut` in kith-auth `door_state` (2026-11-01): `kith-guest-door-check` fails
  4 (the invitee's bot may start the door after the invitee's job is taken back).
- the gate's fingerprint check in `api/authority.rs` `gate_open_at` (2026-11-06, scratch edit, dropped): `authority::gate_tests::the_gate_is_closed_until_asked_and_closes_when_auth_json_changes` fails ("a changed auth.json closes it at once").
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
| `verification/revoke/Reconcile.tla`, `reconcile/*.cfg`, `reconcile.tsv` | the host's reconcile of a revoke made in the core alone (migration blockers P1): `design` passes (132 states: TypeOK, NoLostUndo, FailCutHolds, Narrows), `broken-split-read` fails NoLostUndo (the read outside `AUTH_WRITES`: an app undo lands before the set-aside), `broken-restore-too` fails FailCutHolds (a tick that also gives back undoes a fail-closed cut). With a rename (`Renames`, 2026-11-17): `rename` passes (338 states: TypeOK, NoLostUndo, Narrows), `broken-rename-current` fails Narrows (a reconcile by the current id alone), and two findings fail on purpose: `rename-undo-miss` (NoLostUndoStrict) and `rename-failcut` (FailCutHolds). `run.sh` runs them after the main rows. Code: `remote/core_revokes.rs` (read and `suspend_person` in one step under `AUTH_WRITES`, then `cut_off`; narrowing only; only on a read that worked; never the owner; on a revision change, at start, and every 60 s), called from the 3 s authority poll in `cards/mod.rs` |
| `verification/revoke/ReconcileOwed.tla`, `reconcile-owed/*.cfg`, `reconcile-owed.tsv` | the owed cut of a renamed person's revoke whose ids could not be read (design 2026-11-17-rename-failcut, finding 1 of PR #1020): `fix` passes (497 states: TypeOK, NoLostUndo, FailCutHoldsOwed, Narrows, FailCutReached), `today` (no owed row) fails FailCutReached, `undo-keeps` (the undo leaves the row) fails NoLostUndo. `run.sh` runs them after the reconcile rows. Code: `core_revokes.rs` `owe_cut`, `pay_owed`, `clear_owed`; `people.rs` `cut_off_held` |

Re-run 2026-11-03 (review cleanup PR, rust-rebuild): `run.sh` 80 rows, every row as expected. SYNC stays at fb5f140f: the ~940 changed lines in the covered files were not read; only the F8 change to `remote/members.rs` (mirror takes AUTH_WRITES, mirrors `*` rows; a revoked person is still skipped) was read, and it adds no revoke step.

M2-14 (2026-11-04): the core's door table was read against `DoorStart`/`DoorSweep` and added to `drift.sh` and the CI filter. The model gained `CoreDoorHook` (the revoke that lands on a Mac also marks P's and B's open door rows swept in its own step; a row-level claim, boxes are stopped by the host once switched), the variants `core-door` and `core-bot-door`, and the properties `DoorsCutWithRevoke` and `DoorNoResurrect`; rows in `checks.tsv`. Tested by `core/runtime/src/doors/tests.rs` (the start against the sweep, a revoked person and a bot refused, undo leaves swept doors swept, a dead supervisor by start time, the same answers as `kith_door.py`'s sweep for all 16 combinations, threads against a revoke). SYNC is not moved.

M2-4 (2026-11-04): the core's person-level writes were read against the model: one fact, rows untouched, so no new model action; tested by `core/runtime/src/authority/tests/revoke.rs` (a property: after a suspension the readers refuse the person and every bot of theirs, for every agent; after undo they and the tables are as before). SYNC is not moved.

M2-11b (2026-11-04): the core's card store follows a revoke. `authority/revoke.rs` `follow_in_cards`, run by `store::run_with` in the SAME transaction as `people.suspend`, `people.removePerson` and `people.removeBot`, withdraws every open card (pending, live, parked) and unused once-grant (approved) whose `person` is the person or one of their bots (the ids the write names; never the owner, whose cards carry an empty person), via `cards::withdraw_people`. A card whose claim is held is withdrawn too and its claim dropped (the host's `cards_withdraw_person` does the same; the core's by-id `withdraw` waits for a claim, a revoke does not). Undo calls nothing: access comes back, withdrawn cards stay withdrawn, so `cards::raise` can never hand a pre-revoke yes to a session after an undo. Difference from the host, on purpose: none in which states move; the host's `cards_park_bot` (a bot's process is gone) is unrelated to a revoke and is not ported here. Model: `CardsOn` (one card on m1 asked before any revoke), `CardsAfter` in `CliRevoke`/`CliUndo`/`CloudDeliver`/`Remove`, properties `CardsCut` and `CardNoResurrect`, variants `cards`, `broken-card-no-withdraw` (fails `CardsCut`), `broken-card-undo-revives` (fails `CardsCut`, `CardNoResurrect`), probes `NeverCardUsed`, `NeverCardWithdrawn`. Tested by `core/runtime/src/authority/tests/cards.rs` (person and bot cards withdrawn, a late answer refused, undo revives none, removal and bot removal, a property over random suspend/undo/ask/answer/pickup sequences). SYNC is not moved.

Re-run 2026-11-05 (M2-3 requests fix, flow/rust-folder-rules-10-m2-3-requests): `verification/revoke/run.sh` 104 rows in 542 s, RESULT: every row as expected (the table has grown past the 80 named above); `kernel/tests/kith-auth-revoke-check` and `kernel/tests/kith-auth-props` pass. The access-request row (M2-3) now holds for bots and the owner (never members), and the parity script keeps `suspend` (parity) apart from `membership_gone` (the cloud drops the membership while the person stays in kith-auth: the core cannot see it, an M2-17 MUST in `authority/requests.rs`). SYNC stays at fb5f140f: `drift.sh` still lists many changed covered files that nobody has read against the model.

## Re-sync 2026-11-06 (SYNC moved to 10c4eb6c, rust-rebuild)

Every covered file changed since `fb5f140f` was read; `drift.sh` also lists `revoke_order.rs`, `authority/call.rs` and `api/authority.rs` now. Classified:

**The model has one more switch.** `CoreGate` (FALSE in `base.consts`, as deployed): with `KITH_CORE_AUTHORITY=1` the host's request guard asks the core's copy of the revoked fact (`crev`) while the gate is open. The code can take it today, so it is modeled, but the switch is OFF in every deploy and the base rows are unchanged. Rows: `core-gate` (R1, R5, R5c, R6, R6c pass; probes `NeverCoreRevoked`, `NeverGateShut` fail), `broken-core-gate-stale` (R1, R5 fail).  Why the same-step parity read is sound: `fingerprint_of` covers auth.json only, and the core's revoked fact changes only through `mirror_to_core` after `save_auth` under the same flock; the code takes the fingerprint before the ask (a write during the ask drops the answer) and stats per request. A core-only write (a hand-run `core-call`, or core-side writers once #875 lands) would leave the latch open and out of step for up to `GATE_MAX_AGE` (6 s), which the model does not have; `GATE_MAX_AGE` only guards against a hung gate thread and does not narrow that window. After the switch, kith-auth fails closed on a tombstoned auth.json (`kith_tombstone`, exit 3): more refusals, no new door. The host-gate PR (#875, `authority::switched()`, per-event re-check) was not merged when this was written; re-read `api/authority.rs` when it is.

**Read, no effect on the model:**
- kith-auth: `builtin()` agents refused for everyone but the owner (a refusal after the revoked check); tombstone fail-closed in `load_auth`/`save_auth`; `mirror_to_core` for `person --revoke/--unrevoke/--rm` and `bot --rm` (after `save_auth`, no queue); `sweep_kernel` (ends delegations, wakes and writer claims with no kith serve); `member --rm` needs no agent folder; a revoked person can't be made owner; undo allowed on the owner; `agent-access` domains (applied after `may()`); `door_state` uses `owner_cut` (already `BotDoorCut`). `revoke_person` no longer deletes the person's open `/link` codes: `link` refuses a revoked person's code (`cut_off`), `invite` refuses to make one, undo gives them back while they last. That is the model's `ItemsOn` invites, with the check moved from the revoke to the link.
- Host `people.rs`: `cut_off` is `suspend_person`, `enforce`, `stop_runs`, `sweep_asks`, then `sweep_claims` (which also sweeps the person's guest doors; the direct `doors::sweep` call went). `local_revoke` is shared by the owner's call, the cloud's pull and a peer's handle revoke. The host ends the asks before sweeping claims; the model's `OrderKept`/`Swept` rows are for the core's order (`CoreSweep`), and the `asks` rows, which describe this host path, pass with the asks unordered.
- `members.rs`: `apply_revoked` returns `cut` and `restored`; removal (`end_access`) forgets what the revoke kept and sweeps asks and claims (`Remove`, `NoOrphan`).
- `cloud.rs` `pull`: same order (revoked before memberships); `restored` and the re-granted device are the undo (`ItemsUndo`); the rest is access requests and Mac link.
- `invites.ts`, `host.ts`: undo re-activates the memberships and grants; `peerState` is Mac link.
- Mac link: still no start. `peerAsk` has not landed (`PEER_METHODS` are `peerAgents` and `peerRevokes`; a peer acts for no person); `peerRevokes` is the handle-revoke exchange (`HandleSync`).
- Bot rooms (`botAsk` with `room`, `botPost`, `botRoomRead`, room secrets): only the computer owner's own bot guests can be room members (`is_owners_active_bot`: no `owner_person`), so an invitee's bot gets "No such room" from all three; each takes `sync_lease::wait`. No door for a revoked person (the owner is never revoked).
- `channels/`: the wake withdrawn by a Stop and the account chain are not revoke steps. `kernel/bin/kith`: the in-sync wait is unchanged; `--held` is for the owner's terminal only.
- Core: `follow_in_doors`/`follow_in_cards` (already `CoreDoorHook`, `CardsOn`), the snapshot reads (the same `may` rule), the import recording removals (#867: an `open_removals` row for each person the new auth.json lacks, then `replay_open`).

**Revoke order and `open_removals` (M2-18c).** A removal writes its `open_removals` row in its own transaction, runs the order, and deletes the row last; `replay_open` finishes the rest at start and on the supervisor's pass. That is the fairness `Swept`, `AsksEnded` and `WakesWithdrawn` assume (`WF` of `ClaimSweep`/`AskSweep`) for a removal. The joins refused while a row names an id have no model action (the model has no re-add). The model's `Remove` only starts from a revoked person: removing a person who was never revoked (rows gone, the same order) is covered by the core's tests, not by the model.

**Finding (a gap to close before M2-23, harmless today):** the core runs `revoke_everywhere` (claims of P and B, then the ask tree) only after a removal (`authority/call.rs`) and for the RPC `ask.revokeEverywhere`, which nothing in the kernel or the host calls. `people.suspend` (what kith-auth mirrors for a revoke) does not run it: the core's claim epoch is not bumped and its queued, parked and live claims and asks of a suspended person are not ended by the core. Today the stop of a revoke is the host's `cut_off` and kith-auth's `sweep_kernel`, and deployed turns do not run on the core's claims, so nothing is reachable. Once turns do, a revoke not followed by `revoke_everywhere` leaves a live turn of the revoked person running (harm: a reply reaches a removed person), and the model's `Swept` row, which assumes `CoreSweep` follows every revoke, would not describe the code. The plan log (2026-11-05: "the suspend replay (the architect models that first)") lists it as open. Not fixed here: it needs a record like `open_removals` for a suspend.

## Re-sync 2026-11-06 (2) (SYNC moved to ff380b33, rust-rebuild after #884)

Every covered change since `10c4eb6c` was read (`git diff 10c4eb6c origin/rust-rebuild -- <covered files>`); the new covered file is `core/runtime/src/import/authority.rs` (`record_suspends`). Classified:

**New in the model: three switches**, all FALSE in `base.consts`, none changes a base row. `Switched` (the host after `kith migrate step authority`), `SwitchLag` (broken), `CoreOnlyWrite` + `CoreSideShutsGate`; `crev[m]` is now `[v, side]` (the value, and whether a core-only write left the gate's latch open on it).
- **Switched host (#875).** `core_selected()` is `switched() || (opted_in() && gate_open(..))`: switched, the gate is not asked, the core is the only store, and `run_auth` turns every host kith-auth call into the `people.*` method, so a revoke is one write of one fact (`CrevAfter`: `crev` follows `revoked` in the same step; no `CoreMirror`). Rows `switched`: `TypeOK`, `CoreInStep`, R1, R5, R5c, R6, R6c pass; probe `NeverCoreRevoked` fails (reachable). The latch and its window do not exist switched. Core up or down: a core that does not answer gives no access (events drop, `owner_call` errors, the tombstone is never read through kith-auth), and a tombstone read error counts as switched (fail closed): more refusals, no new door. A switched `local_revoke` that fails runs `cut_off` anyway: the revoke stored nothing, so the model has no step for it (a stutter); the extra stop is a stronger stop, which no property forbids, and the owner is told to run it again.
- **Core-side writer outside the flock (#876's note).** Unswitched, a write of the core's REVOKED FACT that does not touch auth.json (a hand-run `kith kernel core-call people.undo`) leaves the gate's latch open on a copy that differs from the file until the gate thread's next ask (`GATE_EVERY` 2 s; `GATE_MAX_AGE` 6 s only if that thread hangs). `core-only-write` R1 FAILS (CliRevoke, CoreMirror, CoreSide undo, AppStart: the app door starts for a person the file has revoked): that is the window, modeled. `core-only-write-parity` R1 passes (a gate that compares the core to the file per request has none). Is it a real gap before M2-23? Not through any production path: unswitched, the host's own writes go through kith-auth (`run_auth` is kith-auth while unswitched), whose mirror is under the flock and moves the fingerprint; `mirrors_to_core` covers agent rows (`core_revoke`), not the revoked fact; the import writes before the host reads. Only a hand-run core-call has it, with `KITH_CORE_AUTHORITY` OFF in every deploy. Switched it cannot exist. The access-request writes (`decide_in_core`, `deny_or_queue`, `fileRequest` in cloud.rs; `core_grant`/`core_revoke` in people.rs ~:118-128) are core-only as well but write agent rows and requests, never this fact, so they are not this window.
- **FINDING: the rollback window (`broken-switch-lag`: R6, R6c and `CoreInStep` fail; trace CliRevoke, BridgeRecv).** `switched()` is cached for `GATE_EVERY` (2 s) with no stat (`AT_MS`), so for up to 2 s after `kith rollback authority` puts auth.json back, the host still thinks it is switched. A revoke made then (`local_revoke`, which CliRevoke stands for) goes through `run_auth` to the core only. The file, now kith-auth's again, never gets it, and kith-auth's doors read that file at once. The origin Mac serves the person while the cloud has the revoke: `Publish` marks it in sync (`InSync` holds), so the sync wait does not hold the bridge or the doors on this Mac (R6c fails here, not only R6). The real heal is the next pull after the cache expires: `apply_revoked` (members.rs ~:296-317) compares lists with no version check and writes the flag to kith-auth. A Mac with no cloud link never heals; `cut_off` still stops what runs. The window is narrower than a bare 2 s: the rollback takes the core's lifetime lock and the host has no embedded fallback (`core_front.rs` ~:91-117), so it opens only once the daemon is back up and before the host's cache refreshes. Real all the same, and a rollback is the case the cutover is built to survive. Not fixed here (no production code in this PR); worker `rust-folder-rules-18-rollback-lag` builds the fix. **The fix is not a re-stat alone** (check-then-act against `export_back`) **and not a host restart** (only if the rollback stops the host first): the host's switched-route writes carry a marker, and the core refuses them inside the write's own transaction unless the switch phase is Switched (the rollback writes RollingBack before `export_back`, under the lifetime lock). The re-stat on each `run_auth` write is the cheap half. **The reverse lag:** a revoke made in the 2 s after a switch goes to kith-auth, which exits 3 on the tombstone, so `local_revoke` fails and `cut_off` never runs (it runs only when `switched()` already reads true): fail closed for the fact (nothing stored) but nothing is stopped. The same fix covers it. The model has no step for it (a revoke that is not stored is a stutter), so no row.

**Scope of the `Switched` rows.** CliRevoke and CliUndo stand for the host's `local_revoke` (app or cloud). The terminal's `kith person --revoke` exits 3 on a tombstone and `person` is not in `kernel/lib/core-switched.txt`, so a switched Mac with no app reachable has no terminal revoke: M2-18 gate 1, still open. The rows run on `switched` (the full code set: R1, R2, R4, R5, R5c, R6, R6c, R6cDown, R6cWake, SyncAgrees, BotBelowOwner, NeverBridgeInCloudDown) and on `switched-door`, `switched-cards`, `switched-order` (the hooks combined with it; the three are separate runs, not one product).

**Open gates found while reading (not model steps):** (a) `run_auth_noted` keeps only `lines` of the core's answer (authority.rs ~:641-644), so a switched `people.suspend` loses the answer's `kill` and `revokePending`. (b) Switched, `end_access` cannot clear a kith handle (`locked_run` refuses, members.rs ~:64-72) and `export_back` takes the other keys from the pre-cutover copy, so a rollback may bring back a handle for someone whose membership ended: for `invitee-access`.

**Suspend order (#880), confirmed against the model.** `people.suspend` records `open_removals` in its transaction (`record_suspend`, no bots), then `run_open` after the commit: claims of the person and bots (`revoke_person` each), then `end_tree` for the asks. That is `ClaimSweep` before `AskSweep` (`OrderKept` row `core-order`, `broken-asks-first` fails it, `broken-bots-skipped` fails `Swept`), repeated by `replay_open` at start and on the supervisor's pass (the weak fairness `Swept`/`AsksEnded` assume). The undo drop is `ClaimSweep`/`AskSweep` requiring `revoked[m]`: a person whose access is back is not swept (`run_open` deletes the row without acting). One race is not modeled: an undo landing between `run_open`'s read and the claims sweep still sweeps the person once; the direction is a turn stopped too many, never one let through. The skill's 2026-11-06 finding (1) (a suspend did not run the order) is closed by the code. Not in the model: the open row's refusals (`addPerson`/`addBot` of a new id, `renamePerson`, `removeBot` while a row names the person), the import's `record_suspends` (the same row, run after the import's commit); no re-add or rename action exists.

**Read, no model step:**
- `kernel/bin/kith`: `kith set permission|call-model|limit|at-limit` goes to the core's settings module when switched (#870/#837 lane): settings, no access fact.
- `kith-auth`: `member --rm` mirrors `people.revoke` (#872) after `save_auth`; agent rows, not the revoked fact (the model's rows stay out of it). `MIRROR_DONE`/`MIRROR_METHOD` additions only.
- `people.rs`: every kith-auth call is `run_auth` (unswitched = the old call, switched = core); `set_person_access` makes no second copy when switched (`on_core` false); `local_revoke`: see above (cut_off on a failed switched revoke); `cut_off` clears the temp folder through `run_auth`.
- `botguests.rs`: `new_bot`, `undo_auth`, `end_bot_noted` use `run_auth`/`run_auth_noted`; the order of steps in `end_bot` is unchanged.
- `members.rs`: `locked_run` refuses a handle bind/unbind when switched (the core has no method): `end_access` runs its sweeps before that call and only logs its failure; a switched join cannot set the kith handle (fewer doors). `auth_people` and `apply_revoked` use `run_auth`.
- `cloud.rs`: `mirrors()` is `mirrors_to_core()` (true when switched too).
- `authority/call.rs`: the open-removal refusals now read `joining` and the bot's lineage inside the write's transaction (not a step), and the suspend record is the `suspendedPerson` key; `revoke_order.rs`: `record_suspend`, `run_open`.

`verification/revoke/run.sh` (under `mcp__flow__test_slot`): every row as expected, new rows included. `drift.sh` shows no changed covered file at ff380b33.

## Re-sync 2026-11-06 (3) (SYNC moved to 0e958a1b, rust-rebuild after #886/#882)

The only covered file that changed since `ff380b33` is `people.rs`; `roles.rs` and `remote/account.rs` are not in drift.sh's list but #882 touched them, so they were read too. None is a revoke step:
- `people.rs` `make_invite`: the owner check ("that is you") uses `account::person_of(Owner)` instead of `who`. A lookup of who the owner is; no revoked fact, no claim, no door. Matches the existing `Switched` row (the owner id comes from the core's owner row).
- `remote/account.rs` `who_owner`: when switched, reads the core's `people --json` owner row instead of `kith-auth who` (which exits 3 on the tombstone). Same lookup, same fact; the read-only side of the `Switched` switch.
- `api/roles.rs` `owner_name`: the owner's display name from the core when switched ("Waiting for <owner>"). Display only.

`verification/revoke/run.sh` (under `mcp__flow__test_slot`, `nice`): every row as expected. `drift.sh` shows no changed covered file at 0e958a1b.

## M2-23 gate: the kill after the commit (2026-11-07, flow/rust-folder-rules-18-m2-23-kill-resend)

Finding from the #939 review: on a switched host the core commits a revoked person's claims to `ending` and answers a `kill` list; the host signals once, on a detached task (`run_auth_noted` -> `end_kills` -> `end_claimed`). A host restart, a failed spawn or an unreadable stamp lost that signal for good. The model split `ClaimSweep` into `ClaimEnding` (the core's commit) and `KillSent` (the signal), with `KillLost` (host crash between them, process left `stuck`) and `KillReSend` (the supervisor pass), behind new switches `KillSplit`, `HostCrash`, `ReSendKills`, and added the liveness property `NoLiveProc` (a revoked person's live or `ending` process is eventually gone). Rows: `core-kill-split` (no crash, passes), `core-kill-resend` (crash plus re-send, passes), `broken-no-kill-resend` (crash, no re-send: `NoLiveProc` fails). The old one-step `ClaimSweep` is `ClaimEnding` with `KillSplit` off, so every earlier row is unchanged.

Code: `core/runtime/src/kill_resend.rs` `pass`, run by the daemon's own tick (every 5 s, `daemon.rs`, no holder link needed). It signals every `ending` claim with a kill's end reason (not `done`/`exit`) whose holder instance is the serve host's, none, or the connected local holder's (a remote, down or failing holder adds no owner; an older local holder's claims are left to the next hello's `reconcile`), when `procstart::comparable` + `same_process` say the pid is the recorded process: `procstart::claim_victims` (descendants, the claimer, its group when `pgid == pid`), SIGTERM first, SIGKILL on a later pass after the grace, no sleep; a dead leader whose group is held and whose stamp is from this boot has its group signalled (#919's rule, `procstart::orphan_group`). Model: `link` / `LinkFlaps` / `ReSendNeedsLink` make the link a variable; `broken-resend-needs-link` (the re-send inside the holder link) fails `NoLiveProc`, `core-kill-resend` (the re-send reads no link) passes; its state space is too big for run.sh's 512 MB heap once the link flaps too, so the flap is only in the broken row. Tests: `daemon_tests.rs` (`an_ending_claim_*`, `host_and_no_instance_*`, `a_process_that_ignores_term_*`, `a_dead_leaders_group_*`, `an_unstamped_*`). `kill_resend.rs` is not in `drift.sh`'s list; SYNC is not moved.

## A nameless report before the revoke (2026-11-07, L1012, coder/20261107-revoke-ending-rows)

Finding (architect, M1 of the kill re-send): a nameless `done`/`exit` report (`claims.rs` `apply`, no pid named) moves a live claim to `ending` with that reason while its process may still run. `revoke_person` listed that claim's kill but rewrote `end_reason = 'revoked'` only on `claimed`/`running` rows, so the row kept `done`; once the host lost its one signal, `kill_resend::pass` skipped the row (its reason is not a kill's) and the guest's process lived on. Fix: in the same transaction, `revoke_person` sets `end_reason = 'revoked'` on the person's rows that are already `ending`. Model: `Report` (switch `NamelessReport`, needs `KillSplit`) moves a live turn to `reported`; `ClaimEnding` sweeps `reported` too, to `ending` (the code) or, with the broken switch `RevokeKeepsReason`, to `rending`, whose lost kill leaves `rstuck`, which `KillReSend` never touches. Rows: `core-kill-nameless` (`TypeOK`, `OrderKept`, `NoLiveProc` pass), `broken-revoke-keeps-reason` (`NoLiveProc` fails: running, reported, revoke, kill lost, stuck). Both switches are off in `base.consts`, so every earlier row is unchanged. Test: `daemon_tests.rs` `a_revoke_after_a_nameless_done_is_re_sent`. `claims.rs` is not in `drift.sh`'s list; SYNC is not moved.

## The always of an escape card behind the router (2026-11-12, C-a)

On the authority tombstone `kernel/bin/kith-auth access ... [--by ID]` is one call of the core (`people.access`, `core/runtime/src/authority/card.rs`), which checks and writes in ONE locked transaction. The model has it as `AlwaysWrite(m, w)`, guarded by `May(m, w)` (the core's own fact under `RouterDoor`), for P's row and B's (B only while P's entry is there), a self-approval only while P is a member (`mem`); `Demote(m)` drops the entries P approved in the same step (`drop_self_approved`, M3); `AlwaysRemove` takes an entry as written with no check. Switches (all FALSE in `base.consts`, the older rows unchanged): `AlwaysOn`, `RouterDoor`, and the breaks `AlwaysNoMay`, `AlwaysSplit` (check, then write in a later step), `AlwaysBotNoCut`, `DemoteKeeps`. Rows: `switched-always` (TypeOK, R2, R4, BotBelowOwner, SyncAgrees, `AlwaysOnlyIfMay`, `AlwaysBotBelowOwner`, `ApproverDemoted`, R1, R5, R5c, R6, R6c), `switched-always-kill` (the same hooks as `core-kill-split`: NoLiveProc, OrderKept), and the breaks `broken-always-no-may` / `broken-always-split` (`AlwaysOnlyIfMay` fails), `broken-demote-keeps` (`ApproverDemoted` fails), `broken-always-bot-above` (`AlwaysBotNoCut` drops the owner's-entry clause of the write step, so the write-time oracle `botAbove` is set and `AlwaysBotBelowOwner` fails; the pass rows check that no step writes B's always while P's entry is absent). The always is written on m1 only (`AlwaysMacs`) to keep the state space small; the entry carries only who approved it, not a path. Code: `authority/card.rs` `access_grant`, `approver_of`, `access_remove`; `call.rs` `removal_only` (a removal goes through on a damaged record), `routed_write`. The host's cards poll no longer watches only auth.json's change time (`cards::authority_stamp`, `people.revision`): display lag only, `may_answer` re-checks live, so no row.

On the authority tombstone `kernel/bin/kith-auth access ... [--by ID]` is one call of the core (`people.access`, `core/runtime/src/authority/card.rs`), which checks and writes in ONE locked transaction. The model has it as `AlwaysWrite(m, w)`, guarded by `May(m, w)` (the core's own fact under `RouterDoor`), for P's row and B's (B only while P's entry is there), a self-approval only while P is a member (`mem`); `Demote(m)` drops the entries P approved in the same step (`drop_self_approved`, M3); `AlwaysRemove` takes an entry as written with no check. Switches (all FALSE in `base.consts`, the older rows unchanged): `AlwaysOn`, `RouterDoor`, and the breaks `AlwaysNoMay`, `AlwaysSplit` (check, then write in a later step), `DemoteKeeps`. Rows: `switched-always` (TypeOK, R2, R4, BotBelowOwner, SyncAgrees, `AlwaysOnlyIfMay`, `AlwaysBotBelowOwner`, `ApproverDemoted`, R1, R5, R5c, R6, R6c), `switched-always-kill` (the same hooks as `core-kill-split`: NoLiveProc, OrderKept), and the breaks `broken-always-no-may` / `broken-always-split` (`AlwaysOnlyIfMay` fails), `broken-demote-keeps` (`ApproverDemoted` fails), `broken-always-bot-above` (`AlwaysBotBelowOwner` fails). The always is written on m1 only (`AlwaysMacs`) to keep the state space small; the entry carries only who approved it, not a path. Code: `authority/card.rs` `access_grant`, `approver_of`, `access_remove`; `call.rs` `removal_only` (a removal goes through on a damaged record), `routed_write`. The host's cards poll no longer watches only auth.json's change time (`cards::authority_stamp`, `people.revision`): display lag only, `may_answer` re-checks live, so no row.

## The secret grant for a person's own account (2026-11-12, S-b)

`kith-auth grant AGENT@P SERVICE PROFILE` on the tombstone is `people.secretGrant` (`core/runtime/src/authority/secret_writes.rs`):
`need_person(P)` is read in the same locked step as the write, so a grant cannot land for an id that was just removed (an id
taken again by someone else would inherit it), and the removal (`forget`) takes the grants made for P. The model has it as
`GrantWrite` (`GrantOn`, base `switched-grant`) with the oracle `grbad` and the invariant `NoGrantForGone`; `broken-grant-no-person`
(the guard dropped) and `broken-forget-keeps-grant` (the removal leaves them) must fail. The other secret writers touch nothing a
revoke reads (profiles, grants of agents, env); `people.secretRevoke` only narrows, so it is a `removal_only` write.

## The router on the tombstone (2026-11-12, M2-23 lift)

With auth.json a tombstone kith-auth has no file: `resolve`, `may`, `door_state`/`door` and `exec` ask the core (`people.resolve`,
`people.may`, ...), and the C/S-b writes (the always, the helper, the grant) read the core's fact in their own locked step. The model
says it with `RouterDoor` (needs `Switched`): `MayP` is then `~crev[m].v /\ rows[m] # {}`, the core's copy, which `CrevAfter` keeps equal
to `revoked[m]` in the same step. It adds no action; the claim is that every start of a turn, a door or a helper reads that one fact.
`RouterStale` is the break: the router answers from the file it replaced (frozen at the switch: the rows it had, never revoked).
The `switched-router*` pass rows are the switched rows under a new name: under `Switched`, `crev` follows `revoked` in the same step, so
`MayP` is the same predicate. They prove the properties hold; the sensitivity proof is the breaks. "Every router path reads that one fact
in the step it acts" is an assumption TLC does not test; the router-check's byte-for-byte parity does. `RouterStale` reads live `rows`,
not rows frozen at the switch, which is a weaker break than described, and still fails.
**The door row is the code since L1** (`621e9b528`, re-read 2026-11-17): `door` is in kith-auth's ROUTES (`route_secret_read('door')`,
kith-auth's own `door_state` over the core's tables), so on the tombstone `kith_door.run` checks under its flock while the read is
under the core's lock, and a core revoke takes the core's lock only: a check, then a claim, which is `RouterDoor`'s two-step start;
`DoorD2` fails on `switched-router-door` by design and the named sweep (`CutOffSweep`, from `cut_off` or the reconcile) plus the 60 s
loop end the door (`DoorD1`, `DoorCutAtSweep` pass). `kith_door.py`'s docstring says the same.
Rows (checks.tsv, "the router"): `switched-router` (TypeOK, CoreInStep, R2, R4, SyncAgrees, BotBelowOwner, R1, R5, R5c, R6, R6c, R6cDown,
R6cWake pass), `switched-router-door` (DoorD2, DoorD2c, DoorD1, DoorsCutWithRevoke pass), `switched-router-kill` (NoLiveProc passes),
and the breaks `broken-router-stale` (R2, R1, R6 fail) and `broken-router-stale-door` (DoorD2 fails). Two smoke rows
(`switched-router R1`, `broken-router-stale R1`). The code side is `kernel/tests/kith-auth-router-check` on homes stepped for real
(`kith migrate step authority`, no hand-written tombstone any more) and its section 4: a rollback after the core's own writes gives a
file that answers what the core answered. SYNC is not moved: the lift (runtime refusals in `cutover/authority.rs`) adds no revoke step.

Migration blockers P1 (2026-11-13, rust-rebuild): `Reconcile.tla` and its three rows were ported from the architect's design (`verification/revoke/reconcile.tsv`). A terminal revoke on a stepped Mac is `kith kernel core-call people.suspend` (`kith people suspend` does not exist and `kith-auth person --revoke` exits 3); the host half follows in the next poll. An undo is always from the app. The verdicts of the other rows are unchanged: the reconcile is the trigger the `switched-router-door` variant's `CutOffSweep` already stood for. `cards/mod.rs` and `remote/core_revokes.rs` are in `drift.sh`. SYNC is not moved.

## Room turns (2026-11-14, rooms with people, PR C)
A revoke (and a removal, and an agent taken from a person) ends every room turn whose basis holds the person:
`Hub::end_room_turns_of` from `api/people.rs` `cut_off` (which the core's reconcile calls too, per held id), then the sweep of
claims, helpers and doors also names the room persons the person has run as (`room_person_ids_of`). A revoke writes no room
row, so the undo gives the room back as it was. The models are in `verification/rooms/` (`run.sh`): `jobs-fixed` passes,
`jobs-revokekeepsturn` (the revoke leaves the turn running, 53 states) and `revokedeletes` (33) fail on purpose.

### Room doors (2026-11-15, rooms with people, PR D)
A room person's door (`room-<id>`) is a claim over its basis: `kith-auth door --basis a,b` ANDs `door_state` over the people
(a definite no beats `held`), `kith_door.run` writes the claim with `basis`, `gen` and `level` in the same locked step, and
`sweep --person p` (and its recheck) reaches every claim whose basis holds p; a room claim with no usable basis is ended by any
sweep (a room id alone in `--person` ends none, on purpose; after an untick the 60 s recheck ends it). No card: a room's door that a basis person lacks is refused, naming them. Rows in `verification/rooms/checks.tsv`:
`jobs-switched` passes (the switched start/sweep order of #975, the sweep owed until it runs), `jobs-switchednolock` and
`jobs-switchedsweepperson` fail on purpose, `jobs-switchedwindow` fails on purpose (the window Kai accepted: a commit between a
switched start's check and its claim), and `jobs-liveonly`, `jobs-snaponly`, `jobs-split`, `jobs-joinnarrows`,
`jobs-sweepperson` and `doors-stampland` fail on purpose. Tested by `kernel/tests/kith-guest-door-check` (room section) and
`kith-auth-props.py` (`room_door_properties`).

## Re-sync 2026-11-17 (SYNC moved to f9b5e509, rust-rebuild after #1013, #1007, #1010, #1015)

Every covered change since `f5b92e19` was read (`git diff f5b92e19 f9b5e509 -- <drift.sh's files>`, 14 files, about 1,040
lines). The sections above (P1 reconcile, P8 owed sweeps, room turns C, room doors D, the router) already described part of
it without moving SYNC; this pass read all of it. `drift.sh` gains `terminal/core_holders.rs`. Classified:

**Covered by a model row (new or existing):**
- **R-rename (#1013): a person is one identity across every id the core lists** (`core_revokes::held`/`held_under`:
  `host_ids` of their `listPeople` row, current id plus `was`; `Held::first` = `was[0]`). Every host narrowing loops over
  them: `local_revoke` and the cloud `pull` call `cut_off_held` (was `cut_off(id)`), `local_revoke`'s undo restores under each (the cloud's undo does not: finding 3),
  `set_person_access` runs `enforce`, `stop_runs` and the room-turn end per id, the card fence ("no card for a set-aside
  person", `cards/mod.rs`) checks every id, and the reconcile's `to_cut` cuts the current id and each former one (keyed by
  `revoked_at`, so a new revoke is cut again). `RevokeDoors.tla` keeps one id (see "Bounds"); the rename is in
  `Reconcile.tla` (`Renames`): `rename` passes, `broken-rename-current` fails Narrows. Tests:
  `core_revokes::tests::a_renamed_then_revoked_person_is_cut_under_every_id_the_host_may_hold`,
  `the_first_id_is_the_oldest_former_one_and_never_moves_on_a_rename`,
  `api/core_revokes_tests.rs` `a_person_renamed_then_revoked_in_the_core_is_set_aside_under_the_old_id`,
  `api/room_rename_tests.rs`. Room stays across a rename (joins under the first id, "already in" and leave over every id,
  the readers keyed on the stay's id): `verification/rooms/RoomsRename.tla` (rows `rename-*`; the code is `rename-code`).
- **A revoke with the core down cuts locally** (`revoke_person`, `f88017e3d`): when `auth_person` cannot read who is who
  (a switched Mac whose core is not running), an invited person known to the host (never the owner or a bot) is `cut_off`
  here and the owner told it is not complete. The fact is not stored: `Reconcile.tla`'s `AppRevokeFails` (fail-closed cut,
  `FailCutHolds`); in `RevokeDoors.tla` a stutter plus a stronger stop. Test: `api/stepped_e2e_tests.rs`.
- **The kernel launch's revoke epoch** (`kernel/bin/kith`, P5/P9: `cad67a6d7`, `dd52f5a25`): once the claims are the
  core's, a non-owner's revoke epoch is read BEFORE kith-auth decides they may run and travels with the claim
  (`KITH_REVOKE_SEEN`); a revoke between the two refuses the claim in its transaction (`claims.rs` `revoke_seen`), and a
  guest run the core cannot record does not start (`proc` exit 9 or any failure while `claims_core`). That keeps the
  model's start (`MayP` and the claim, one step) true against a revoke. Modelled in writer-lock (`c4-epochleak`); tested
  by `kernel/tests/kith-sessions-core-check`. The bash launch also refuses `room-` persons (fewer starts).
- **The guest door on the tombstone** (L1, `621e9b528`): `door` routed; now the code of `switched-router-door` (see "The
  router on the tombstone"). **Room doors over a basis** (#1007): "Room doors" above.
- **The tick on the tombstone** (`people.job` with `tick`, `card.rs` `job_tick`): `own_row` checks the person, the owner,
  the own row and `may()` in the same locked step as the write: the `AlwaysWrite` shape (`AlwaysOnlyIfMay`), no row of its
  own. Tests: `authority/tests/card.rs` `a_job_is_ticked_as_the_owner_saw_it_*`.
- **Owed sweeps** (`botguests::sweep_claims` -> `core_holders::sweep`, P8): `RetryKeepsRestored` (above).
- **Room-turn cards withdrawn with the revoke** (`cards::withdraw_room_person`, `283820001`, `b1581438b`) and stopped asks'
  cards (`withdraw_asks`): the host side of `CardsCut`; tested by `api/room_withdraw_tests.rs` (`a_revoke_withdraws_the_card_of_a_running_room_turn_and_only_that_one`, after a restart, a parked turn).

**Read, no revoke step:** the core refusing new `room-` person, rename, invite and bot ids (`revoke.rs` `check_not_room`,
`write.rs`; kith-auth `room_prefix_msg`; the host's `slug` gives `p-room-...`); kith-auth's `APP_ONLY` people writes exiting
3 on the tombstone (a terminal revoke there is `kith kernel core-call people.suspend`, as P1 says); `owner`/`share`/`browser`
through the core (`secret_writes.rs` `secret_owner`/`secret_share`/`secret_browser`: profiles, owner-only; `secretShare
--rm` a removal); `people.link` and `/link` through the core when switched (`hq.rs`; the link refuses a cut-off person,
invitee-access 37); `people.listPeople` `withWas`; `mcp-names`/`ls`/`connectors` routes; `helper-mcp` reading the core's
tables; the host's `resolved` bundle (`consumeAccess`) and the test rig's opt-in split (`TEST_OPT_IN`); invite rooms on
`createInvite` and `proveInvite` (widening only for a person the live `may_use` allows, invitee-access); the off-turn
lock-alone stub flag (thread-writers).

**Findings (not fixed here: no production code in this PR):**
1. **A failed revoke of a renamed person cuts only the current id** (`reconcile/rename-failcut`, FailCutHolds fails:
   AppRevokeFails, rename, AppCut with the read falling back). On a switched Mac where the core cannot answer, both the
   core-down branch of `revoke_person` (`cut_off(id)`) and `local_revoke`'s failure branch (`cut_off_held`, whose
   `held_under` falls back to the id given) set aside and stop only what the host holds under the CURRENT id. A renamed
   person's devices, memberships, invites, claims and room stays are under the old id, so their runs there keep running and
   their devices stay live. The core shows no revoke (the write failed), so the reconcile never cuts the old id either.
   While the core is down the switched guard refuses new starts, but a running turn under the old id is not stopped,
   against the comment's "the host's half is fail-closed"; and once the core is back it shows no revoke and the host
   set aside only the current id, so the old id's devices work again until the owner revokes a second time (they are
   told the revoke is not complete). `cut_off_held`'s doc (`people.rs` ~:714) says the core's poll cuts the old ids: it
   does not (no revoke to see). Proposed fix (architect, `designs/2026-11-17-rename-failcut/`, model-checked, not yet
   agreed): a durable "owed cut" row when the ids cannot be read, paid by the 3 s poll under `AUTH_WRITES` once they can,
   cleared by every undo route; the core-down branch of `revoke_person` uses `cut_off_held` too.
2. **An undo after a rename can leave the old id set aside** (`reconcile/rename-undo-miss`, NoLostUndoStrict fails;
   NoLostUndo with `missed` passes): the undo's `held_under` runs after the core commit; if that read fails the host
   restores only the current id. Fail closed (narrower); the owner undoes again (idempotent in the core). Accepted.
3. **An undo that arrives through the cloud restores only the id the cloud names** (`cloud.rs` `pull`:
   `restored(hub, &person)` for `changed.restored`, while the cut above it is `cut_off_held` and `local_revoke`'s undo
   loops over `held_under`). After a rename on a switched Mac, an undo made on another Mac leaves this Mac's rows under the
   old id set aside, every time, not only when a read fails: the `missed` state of finding 2 without the fallback. Fail
   closed; the owner's undo in this Mac's app restores every id. Fix: the same `held_under` loop in `pull`.
4. `cut_off_held` and the cloud `pull` cut each id with a separate `cut_off`, one after another: the stop is per id, not one
   step. No property is about the order of ids; a revoke is never narrower for it.

`verification/revoke/run.sh` and `verification/rooms/run.sh` (under `mcp__flow__test_slot`, `nice`): every row as expected,
new rows included. `drift.sh` shows no changed covered file at f9b5e509.

## Owed cut of a renamed person's failed revoke (2026-11-17, flow/rename-owed-cut, #1022)
Closes findings 1 and 3 of the re-sync above (PR #1020), design 2026-11-17-rename-failcut (`ReconcileOwed.tla`, rows in `reconcile-owed.tsv`): see the
code table's last row. Finding 3: the cloud `pull`'s restored now gives back under
`held_under`, as `local_revoke`'s undo does. `revoke_person`'s core-down branch calls `cut_off_held` too, but it is taken
only for an id the host holds a person row under; a revoke by a new id (`samuel`, whose host rows are under `sam`) with
the core down is refused before any cut, so the owed cut is reached from `local_revoke`'s failure branch (the person
read, then the core write failed) and from a revoke by the old id. Tested by `api/core_revokes_tests.rs`
(`a_renamed_persons_revoke_without_the_core_cuts_the_old_id_once_the_core_is_back`,
`the_app_revoke_with_the_core_down_owes_the_cut_too`, `an_undo_before_the_owed_cut_is_paid_restores_and_clears_it`).
SYNC is not moved.

## Refusal words, unread undo, owed cut of an undone person (2026-11-17, flow/rebuild-followups-revoke-words, #1029)
Follow-ups 1 and 2 of #1022. Model: `ReconcileOwed.tla` gained `UndoBlind`/`UndoMarks` and a `mark` variable; rows `fix-blind`
(pass, 961 states) and `undo-blind` (fails NoLostUndo). Code: `core_revokes.rs` takes an order number from a durable counter
(`next_seq`, kv `revoke.seq`, never a clock) for each owed row (`revoke.owed_cut.<p>`) and each undo marker
(`revoke.undone.<p>`). An undo whose read of the ids fails (`clear_owed` or `restore_held`) writes the marker; `pay_owed`
reads the ids once and drops a row unpaid only when a marker on one of them is newer than the row, else pays (a revoke that
failed leaves no marker, so it is paid: a guard on the person's revoked flag would drop it, as the core shows it unrevoked).
`restore_held` is the one give-back of an undo (`local_revoke`, the cloud `pull`); on a failed read it also warns, writes an
`undo.unread` audit row and a note (kv `revoke.undo_unread.<p>`) shown as `undoNote` on the `people` row under any id the person
is held under, cleared by a later `restore_held` that reads, or by a new revoke (`cut_off_held`). `revoke_person` words every
refusal (`unreadable`: a revoke by the new id with the core down says it was NOT revoked, nothing was cut, start the core and
revoke again; `unfinished`). The apps do not render `undoNote` yet. Tests: `api/core_revokes_tests.rs`
(`a_revoke_of_the_new_id_with_the_core_down_says_it_did_not_happen_and_what_to_do`,
`an_undo_that_cannot_read_the_ids_leaves_a_note_until_a_later_undo_reads_them`,
`an_owed_cut_older_than_an_undo_that_could_not_read_the_ids_is_dropped_not_paid`), `remote/core_revokes.rs` unit tests.

## Guest doors on the core's rows (2026-11-19, M2(b), flow/core-followups-doors)

On a switched Mac a person's own guest door is a row of the core's `doors` table (`kernel/lib/kith_door_core.py`, called by `kith_door.py`
`run` and `sweep_report`; the host's `doors.rs` still shells `kith door sweep`). The model splits what the old rows folded into one action:
`gdoor` is the ROW (`idle` -> `running` open -> `killed` swept -> `reaped` confirmed gone, or `done` ended), `gbox` is the BOX (`no`, `up`, `gone`,
`never`). New switches (all FALSE in `base.consts`): `CoreDoorStart` (needs `RouterDoor`, `GuestDoor`, `CoreDoorHook`: `DoorStart` is then ONE
step, the core's `doors.start`, so `DoorD2` holds on the row; the box starts later with no check, `BoxStart`; the supervisor's markLive and the
sweeper's kill are `BoxKill`; its `doors.end` is `RowEnd`/`Reap`), `CoreSlow` (a core call with no answer is a step that changes nothing, in both
forms: `DoorStartTimeout` before the commit, `StartLost` after it with the reply lost; `DoorSweepTimeout`), and the breaks `TimeoutOpen` (a start
with no answer starts the box with no row), `TimeoutKills` (a recheck with no answer ends a live door of an allowed person) and `ReapLive` (the
sweeper confirms a swept row gone before the box started). Properties: `NoOrphanBox` (a running box always has its row, open or swept and not
reaped), `BoxD1` (a box up for a revoked person stops, liveness), `BoxD2` (the box starts only while not revoked: FAILS by design on
`switched-core-door`, bounded by `BoxKill`), `LiveDoorKept` (a running door ends only because the person is revoked or has no row), and
`DoorNoResurrect` now lets a swept row go to `reaped`. Rows: `switched-core-door` (TypeOK, DoorD2, DoorD2c, DoorD1, DoorNoResurrect,
DoorsCutWithRevoke, DoorCutAtSweep, NoOrphanBox, BoxD1 pass; BoxD2 fails), `switched-core-door-slow` (DoorD2, DoorD2c, DoorD1, BoxD1, NoOrphanBox,
LiveDoorKept pass), and `broken-door-timeout-open` / `broken-door-reap-live` (`NoOrphanBox` fails), `broken-door-timeout-kills` (`LiveDoorKept`
fails). Not in the model: a room door keeps the file path (its row would have no basis), so `switched-router-door` stays the row for it; a door
started before the switch has a claim file and no row, and the file's sweep (named person, gone supervisor, `left`, recheck) ends it.

The rule for a call with no answer, in the code's words: a start with no answer starts nothing and ends the id it may have written; a sweep's
recheck with no answer leaves running doors alone (`{"unknown": true}`, the host asks again in 10 s); a named person, a gone supervisor and a
definite `revoked` end a door with no answer from the core; the sweeper calls `doors.end` on a swept row only when its supervisor is gone.
`kith-auth door` with no answer (a timeout, a core that did not answer) is state `unknown` and ends no running door (it ended it before).
Bound left: an untick that `kith-auth` cannot judge leaves a door up to `RUN_LIMIT_S` (3600 s). Tests: `kernel/tests/kith-door-core-check`
(a stub core: answers, refuses, down, hung; scratch edits of each rule were run and fail it). SYNC is not moved (the other covered changes since it
have not been read).
