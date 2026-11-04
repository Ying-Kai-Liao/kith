#!/usr/bin/env bash
# Which code the writer-lock models cover has changed since they were last confirmed to describe it?
#
#   verification/writer-lock/drift.sh        compares the working tree (so also uncommitted edits) with SYNC
#
# Prints the covered files that differ and a diff stat. No output of changed files = nothing to re-read.
# After you have read the changes, updated WriterLock4.tla / Holder.tla if needed, and run.sh is as expected,
# set SYNC below to the commit you checked (and in .claude/skills/writer-lock/SKILL.md).
set -euo pipefail

# The commit at which the models were confirmed to describe the files below. It is one commit of rust-rebuild:
# the one M3-0a was written against. Most of the claim code (the writer lock in the stub, the claim ids on the wire,
# the core's side of claims) does not exist yet: this pins what exists, and the files marked "(M3)" are added to
# the list as they land. Until then a green run says the DESIGN holds, not that the code follows it.
SYNC=387c7b5a09ade3d45055024983ff15005e489f54

FILES=(
  core/stub                           # the start stub: the writer lock (fd CLOEXEC, kept to the engine's exit), redeem, the engine as its child
  core/holder/src/holder.rs           # spawn (same conv+claim answers the running one; the highest claim seen), reap, ACK/KILL by id, exits kept until acked
  core/holder/src/proto.rs            # the frames: SPAWN/SPAWNED/EXITED/ACK/KILL, the ids, hello and the instance
  core/runtime/src/holder_link.rs     # the core's side: hello, the recorded pids, step 7 (end the old instance's groups), the poll
  core/runtime/src/terminal.rs        # the terminal entrance: open, claim, spawn through the holder, attach
  core/runtime/src/conversations      # (M3) the claim transaction, claim ids, redeem, park and wake, take-over; not written yet
)
paths=()
for f in "${FILES[@]}"; do paths+=("${f%% *}"); done

root=$(git rev-parse --show-toplevel)
cd "$root"
git cat-file -e "$SYNC^{commit}" 2>/dev/null || { echo "drift.sh: $SYNC is not in this clone; git fetch origin" >&2; exit 2; }
if git diff --quiet "$SYNC" -- "${paths[@]}"; then
  echo "no covered file differs from $SYNC: the models still describe them"
else
  echo "covered files that differ from $SYNC (read each change; see the skill, 'When you change covered code'):"
  git diff --stat "$SYNC" -- "${paths[@]}"
  echo
  echo "commits touching them since:"
  git log --oneline "$SYNC"..HEAD -- "${paths[@]}" || true
  exit 1
fi
