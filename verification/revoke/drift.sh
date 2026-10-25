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
SYNC=SYNC_COMMIT

FILES=(
  kernel/bin/kith-auth                    # may(), bot_may, cmd_person (--revoke/--unrevoke, --local), who and resolve (revoked, held)
  kernel/lib/kith_bridge/core.py          # handle (revoked, held), held, access, run (spawn re-check), still, flush (deliver)
  serve/host/src/api/people.rs            # revoke_person (cloud first, then kith-auth), cut_off
  serve/host/src/api/botguests.rs         # ask, thread: the sync gate (sync_lease::wait), then may_use
  serve/host/src/remote/sync_lease.rs     # in sync or not: changed, mark/pulled, wait, the bridge's lease file
  serve/host/src/remote/relay.rs          # cloud.changed -> out of sync, then pull; renew on ping
  serve/host/src/remote/cloud.rs          # pull: revoked before memberships
  serve/host/src/remote/members.rs        # apply_revoked, end_access (keeps a revoked person's handle and bots)
  serve/cloud/src/routes/invites.ts       # hostRevokePerson, hostUnrevokePerson, revokedOf, acceptInvite refusal
  serve/cloud/src/routes/host.ts          # host state: revoked
)
paths=()
for f in "${FILES[@]}"; do paths+=("${f%% *}"); done

root=$(git rev-parse --show-toplevel)
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
