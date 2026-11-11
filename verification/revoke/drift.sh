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
SYNC=f5b92e19daddcfdfc92162abfc38be2b63dd3e4d

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
  serve/host/src/doors.rs                 # the host's door sweep: on cut_off, the bot-guest sweep and every 60 s (DoorSweep)
  core/runtime/src/authority/revoke.rs   # M2-4: person_suspend (the revoked fact, rows untouched), person_remove, person_rename
  core/runtime/src/doors/mod.rs           # M2-14: the door table (start, mark_live, sweep, sweep_ids, end: DoorStart, DoorSweep)
  core/runtime/src/doors/call.rs          # M2-14: the doors.* rows
  core/runtime/src/migrations/doors/m0060_doors.rs  # M2-14: the doors table
  core/runtime/src/revoke_order.rs        # M3-10d/M2-18c: the order (claims of P and B, then the asks), open_removals, replay_open (ClaimSweep, AskSweep)
  core/runtime/src/authority/call.rs      # which writes run the order (a removal does; a suspend does not yet), the open-removal join refusal
  core/runtime/src/authority/card.rs      # C-a/C-b: the always, helper and job writes of the router (AlwaysWrite, AlwaysRemove, Demote's approver rule)
  core/runtime/src/authority/secret_writes.rs  # S-b: the secret grant for agent@P (GrantWrite: need_person in the one locked step)
  serve/host/src/api/authority.rs         # the host's gate for KITH_CORE_AUTHORITY=1: fingerprint latch (CoreGate, GuardP)
  core/runtime/src/import/authority.rs    # M2-18c: record_suspends (a revoke the file has and the core missed gets the open_removals row), record_removals
  core/runtime/src/authority/mod.rs       # the readers: snapshot (revoked), may/resolve/cut_off over secrets
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
