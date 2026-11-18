#!/usr/bin/env bash
# Which code the model covers has changed since the model was last confirmed to describe it?
#
#   verification/registry/drift.sh        compares the working tree (so also uncommitted edits) with SYNC
#
# With a difference it prints the covered files that differ from SYNC, a diff stat, untracked covered files and the commits
# touching them since, and exits 1. With none it prints one line saying the model still describes them and exits 0.
# Exit 2: SYNC is not in this clone.
# After you have read the changes, updated RegistryStep.tla / base.consts if needed, and run.sh is as expected,
# set SYNC below to the commit you checked (and in .claude/skills/registry-step/SKILL.md).
set -euo pipefail

# The commit at which the model was last read against the code below: transcripts P1 (#1040), the core side. The step's
# module (cutover/modules/transcripts.rs) is pre-P3 at it; P3 reads the model against it and moves SYNC.
SYNC=96429c4db8425fd7fb508b11fc50a3de61aa2c3f

FILES=(
  kernel/bin/kith-sessions                        # record(): the hook, Files mode and Writer mode (spool, call, delete own)
  kernel/lib/kith_registry.py                     # switched, order_key, valid, lock_at, spool, call, drain, sweep_temps (P2)
  kernel/bin/kith-sched                           # prune_terminal: skip in Writer mode, the Files-mode inbox drain
  core/runtime/src/transcripts                    # session.record (txn, per-sid file write), reconcile, drain, prune (P1)
  core/runtime/src/import/transcripts.rs          # import from files into rows
  core/runtime/src/cutover/modules/transcripts.rs # the step: preflight, import, tombstone, before_tombstone, cancel, export_back
  core/runtime/src/daemon.rs                      # the session.record dispatch, the 60 s reconcile + drain
  core/kernel/src/threads/registry.rs             # the Rust port of record (refuses under the marker)
)
paths=()
for f in "${FILES[@]}"; do paths+=("${f%% *}"); done

root=$(git rev-parse --show-toplevel)
cd "$root"
git cat-file -e "$SYNC^{commit}" 2>/dev/null || { echo "drift.sh: $SYNC is not in this clone; git fetch origin" >&2; exit 2; }
if git diff --quiet "$SYNC" -- "${paths[@]}" && [[ -z $(git ls-files --others --exclude-standard -- "${paths[@]}") ]]; then
  echo "no covered file differs from $SYNC: the model still describes them"
else
  echo "covered files that differ from $SYNC (read each change; see the skill, 'When you change covered code'):"
  git diff --stat "$SYNC" -- "${paths[@]}"
  git ls-files --others --exclude-standard -- "${paths[@]}" | sed 's/^/ new, untracked: /'
  echo
  echo "commits touching them since:"
  git log --oneline "$SYNC"..HEAD -- "${paths[@]}" || true
  exit 1
fi
