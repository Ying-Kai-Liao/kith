#!/usr/bin/env bash
# Which code the model covers has changed since the model was last confirmed to describe it?
#
#   verification/lease/drift.sh        compares the working tree (so also uncommitted edits) with SYNC
#
# Prints the covered files that differ and a diff stat. No output of changed files = nothing to re-read.
# After you have read the changes, updated Lease.tla / base.consts if needed, and run.sh is as expected,
# set SYNC below to the commit you checked (and in .claude/skills/holder-lease/SKILL.md).
set -euo pipefail

# The commit at which the model was confirmed to describe the files below. Since M3-0c the code is the `fixed` variant,
# see the code-to-model table in the skill.
SYNC=0cac896b9a556eaab4c3080ccea1e2141ecf1fb4

FILES=(
  core/holder/src/holder.rs           # Lease, expire_lease, watch_lease, the PING and RENEW arms, the LEASE_LOST answer in spawn
  core/holder/src/proto.rs            # PING, PONG, RENEW, LEASE_LOST, Renew, LeaseLost, Exit::lapsed, the lease paragraph of the module doc
  core/holder/src/client.rs           # Renewer::ping, the reader thread's PONG answer (what a RENEW frame carries)
  core/holder/src/clock.rs            # lease_clock: which OS clock, and that it counts sleep
  core/runtime/src/holder_link.rs     # start_renewing: when and how often the core pings; the belief precondition
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
