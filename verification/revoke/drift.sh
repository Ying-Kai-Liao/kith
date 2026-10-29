#!/usr/bin/env bash
# Which code the revoke model covers has changed since the model was last confirmed to describe it?
#
#   verification/revoke/drift.sh        compares the working tree (so also uncommitted edits) with SYNC
#
# Prints the covered files that differ and a diff stat. No output of changed files = nothing to re-read.
# After you have read the changes, updated RevokeDoors.tla / base.consts if needed, and run.sh is as expected,
# set SYNC below to the commit you checked (and in .claude/skills/revoke-access/SKILL.md).
set -euo pipefail

# The commit at which the model was confirmed to describe the files below: the code of the revoke (PR "revoke a person").
SYNC=ab050f9277a3d0501c9fca991bb377dba5bd6728

FILES=(
  kernel/bin/kith-auth                    # may(), bot_may, cmd_person (--revoke/--unrevoke, --local), who and resolve (revoked, held)
  serve/host/src/channels/core.rs         # the bridge door (BridgeRecv): handle (held -> held()), take (revoked -> removed), access
  serve/host/src/channels/queue.rs        # work: Core::access (in sync) right before the batch is taken (the start, HelperGate)
  serve/host/src/channels/run.rs          # Recheck::still in finish (the answer) and flush (each outbox post) (Deliver)
  serve/host/src/channels/hq.rs           # Who (revoked, held), resolve (held), resolve_in_sync
  serve/host/src/channels/settings.rs     # sync_hold, sync_poll
  kernel/bin/kith                         # a helper spawn: kith-auth resolve, allowed only (Helper)
  serve/host/src/api/people.rs            # revoke_person (cloud first, then kith-auth), cut_off
  serve/host/src/api/botguests.rs         # ask, thread: the sync gate (sync_lease::wait), then may_use
  serve/host/src/remote/sync_lease.rs     # in sync or not: changed, mark/pulled, wait, the lease file kith-auth reads (held)
  serve/host/src/remote/relay.rs          # cloud.changed -> out of sync, then pull; renew on ping
  serve/host/src/remote/cloud.rs          # pull: revoked before memberships
  serve/host/src/remote/members.rs        # apply_revoked, end_access (keeps a revoked person's handle and bots)
  serve/cloud/src/routes/invites.ts       # hostRevokePerson, hostUnrevokePerson, revokedOf, acceptInvite refusal
  serve/cloud/src/routes/host.ts          # host state: revoked
  kernel/lib/kith_door.py                 # the guest door: run (lock, kith-auth door, claim, start = DoorStart), sweep (DoorSweep)
  serve/host/src/doors.rs                 # the host's door sweep: on cut_off, the bot-guest sweep and every 60 s (DoorSweep)
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
