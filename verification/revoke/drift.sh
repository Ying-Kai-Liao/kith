#!/usr/bin/env bash
# Which code the revoke model covers has changed since the model was last confirmed to describe it?
#
#   verification/revoke/drift.sh        compares the working tree (so also uncommitted edits) with SYNC
#
# Prints the covered files that differ and a diff stat. No output of changed files = nothing to re-read.
# After you have read the changes, updated RevokeDoors.tla / base.consts if needed, and run.sh is as expected,
# set SYNC below to the commit you checked (and in .claude/skills/revoke-access/SKILL.md).
set -euo pipefail

# The commit at which the model was confirmed to describe the files below (see the skill's "Sync commit").
SYNC=a8465ad5c020e4996a26b8fa852eb995cb96127e

FILES=(
  kernel/bin/kith-auth                    # may(), bot_may, cmd_person (--revoke/--unrevoke, --local), who and resolve (revoked, held)
  serve/host/src/channels/core.rs         # the bridge door (BridgeRecv): handle (held -> held()), take (revoked -> removed), access
  serve/host/src/channels/queue.rs        # work: Core::access (in sync) right before the batch is taken (the start, HelperGate)
  serve/host/src/channels/run.rs          # Recheck::still in finish (the answer) and flush (each outbox post) (Deliver)
  serve/host/src/channels/hq.rs           # Who (revoked, held), resolve (held), resolve_in_sync
  serve/host/src/channels/settings.rs     # sync_hold, sync_poll
  kernel/bin/kith                         # a helper spawn: kith-auth resolve, waits while held (Helper)
  serve/host/src/api/people.rs            # revoke_person (cloud first, then kith-auth), cut_off
  serve/host/src/api/botguests.rs         # ask, thread: the sync gate (sync_lease::wait), then may_use
  serve/host/src/remote/sync_lease.rs     # in sync or not: changed, mark/pulled, wait, the lease file kith-auth reads (held)
  serve/host/src/remote/relay.rs          # cloud.changed -> out of sync, then pull; renew on ping
  serve/host/src/remote/cloud.rs          # pull: revoked before memberships
  serve/host/src/remote/members.rs        # apply_revoked, end_access (keeps a revoked person's handle and bots)
  serve/cloud/src/routes/invites.ts       # hostRevokePerson, hostUnrevokePerson, revokedOf, acceptInvite refusal
  serve/cloud/src/routes/host.ts          # host state: revoked
  serve/host/src/run_outbox.rs            # a run's outbox into the person's app chat (the app door: the relay's block)
  kernel/lib/kith_door.py                 # the guest door: run (lock, kith-auth door, claim, start = DoorStart), sweep (DoorSweep)
  kernel/lib/kith_door_core.py            # M2(b): a person's door on a switched Mac: doors.start/markLive/end/sweep, an unknown answer (CoreDoorStart, CoreSlow, BoxStart, Reap)
  serve/host/src/doors.rs                 # the host's door sweep: on cut_off, the bot-guest sweep and every 60 s (DoorSweep)
  core/runtime/src/authority/revoke.rs   # M2-4: person_suspend (the revoked fact, rows untouched), person_remove, person_rename
  core/runtime/src/doors/mod.rs           # M2-14: the door table (start, mark_live, sweep, sweep_ids, end: DoorStart, DoorSweep)
  core/runtime/src/doors/call.rs          # M2-14: the doors.* rows
  core/runtime/src/migrations/doors/m0060_doors.rs  # M2-14: the doors table
  core/runtime/src/revoke_order.rs        # M3-10d/M2-18c: the order (claims of P and B, then the asks), open_removals, replay_open (ClaimSweep, AskSweep)
  core/runtime/src/authority/call.rs      # which writes run the order (a removal does; a suspend does not yet), the open-removal join refusal
  core/runtime/src/authority/card.rs      # C-a/C-b: the always, helper and job writes of the router (AlwaysWrite, AlwaysRemove, Demote's approver rule)
  serve/host/src/remote/core_revokes.rs   # P1: the host's reconcile of a revoke made in the core alone (Reconcile.tla); held/held_under (R-rename)
  serve/host/src/terminal/core_holders.rs # P8: the owed revoke sweep and its retry (RetryKeepsRestored)
  serve/host/src/cards/mod.rs             # P1: the 3 s authority poll that calls it
  core/runtime/src/authority/secret_writes.rs  # S-b: the secret grant for agent@P (GrantWrite: need_person in the one locked step)
  serve/host/src/api/authority.rs         # the host's gate for KITH_CORE_AUTHORITY=1: fingerprint latch (CoreGate, GuardP)
  core/runtime/src/import/authority.rs    # M2-18c: record_suspends (a revoke the file has and the core missed gets the open_removals row), record_removals
  core/runtime/src/authority/mod.rs       # the readers: snapshot (revoked), may/resolve/cut_off over secrets
  core/kernel/src/secrets/authority.rs    # P1-15 M1: the Rust launch's reader on the tombstone: may/resolve are people.may/people.resolve asked in process, mcp-config/exec read people.secretsView; no answer is exit 3, never the replaced file (RouterDoor, RouterStale)
  core/kernel/src/access.rs               # W22a: the access rules the core and the Rust kith-auth both call (one copy, WI14): canonical, owner, is_owner, parent_of, cut_off (MayP/MayB refuse a revoked person and their bots), held (bridge_held), take_once (the once used up in the locked step); secrets.rs re-exports them
  core/kernel/src/access/rules.rs         # W22a: Rules::may, resolve (with the bot cut to its owner's), who, bridge_held over one auth.json snapshot (revoked is checked before any row)
  core/kernel/src/access/jobs.rs          # W22a: an agent's jobs and doors read from capabilities.yaml (what a tick, a helper's always and once can grant)
  core/kernel/src/access/packages.rs      # W22a: the package facts the rules ask for (where an agent's package is, its jobs and capabilities)
  core/kernel/src/launch/plan.rs          # P1-12 R0: who the run is for, with the sync hold (a person is resolved only while this Mac is in sync with the cloud), in Rust now
  core/kernel/src/launch/exec.rs          # P1-12 R3a: a one-shot run in this process: the revoke epoch is read BEFORE the plan resolves the person (I1), `proc` before the engine (I5), KITH_REVOKE_SEEN with every claim
  core/kernel/src/launch/start.rs         # R3a: the claim and the guest's once grants after it, then the engine (the pid the revoke sweep finds)
  core/kernel/src/secrets.rs              # the Rust kith-auth both launches read: Rules::may (revoked before any row), resolve, bridge_held (held), --take-once in the locked step
  core/kernel/src/threads/writer.rs       # Taker: epoch (claim.epoch), proc, core_take's revokeSeen (the claim a revoke after the epoch refuses: I1, I5)
  core/kernel/src/permit/helper.rs        # ask_helper (a guest's helper: kith ask outside the sandbox, the launch's in-sync wait decides)
  core/kernel/src/permit/door.rs          # run_door (the job card, then kith_door.run: DoorStart)
  kernel/lib/kith-core.sh                 # kith_core_exec / kith_oneshot_flag: which launch (bash body or launch-exec) runs a one-shot
  kernel/lib/kith_people_access.py        # kith people suspend|undo on a switched Mac: the host's revokePerson (cloud first, R9), never a raw core call
  core/kernel/src/launch/terminal.rs      # R3b: the terminal run: `proc` before the engine (exec.rs), the terminal claim for this pid, then `threads run` in process (the pid the revoke sweep finds)
  core/kernel/src/launch/held.rs          # R3b: --held (the owner's own terminal only): release the resumed claim, then terminal.open
)
paths=()
for f in "${FILES[@]}"; do paths+=("${f%% *}"); done

root=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
cd "$root"
git cat-file -e "$SYNC^{commit}" 2>/dev/null || { echo "drift.sh: $SYNC is not in this clone; git fetch origin" >&2; exit 2; }
if git diff --quiet "$SYNC" -- "${paths[@]}"; then
  echo "no covered file differs from $SYNC: the model still describes them"
else
  echo "covered files that differ from $SYNC (read each change; see the skill, 'When you change covered code'):"
  git diff --stat "$SYNC" -- "${paths[@]}"
  echo
  echo "commits touching them since:"
  git log --oneline "$SYNC"..HEAD -- "${paths[@]}" || true
  exit 1
fi
