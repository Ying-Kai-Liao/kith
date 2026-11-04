#!/usr/bin/env bash
# Which code the model covers has changed since the model was last confirmed to describe it?
#
#   verification/lease/drift.sh        compares the working tree (so also uncommitted edits) with SYNC
#
# Prints the covered files that differ and a diff stat. No output of changed files = nothing to re-read.
# After you have read the changes, updated Lease.tla / base.consts if needed, and run.sh is as expected,
# set SYNC below to the commit you checked (and in .claude/skills/holder-lease/SKILL.md).
set -euo pipefail

# The commit at which the model was confirmed to describe the files below (origin/rust-rebuild). The model describes
# the lease as designed for M3-0c; holder.rs at this commit is the `today` variant, see the code-to-model table in the skill.
SYNC=387c7b5a09ade3d45055024983ff15005e489f54

FILES=(
  core/holder/src/holder.rs           # Lease, expire_lease, watch_lease, the RENEW arm, the LEASE_LOST answer in spawn
  core/holder/src/proto.rs            # RENEW, LEASE_LOST, Renew, LeaseLost, the lease paragraph of the module doc
  core/holder/src/client.rs           # Renewer::renew, Client::renew (what a RENEW frame carries)
  core/runtime/src/holder_link.rs     # start_renewing: when and how often the core renews; what it does on LEASE_LOST
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
