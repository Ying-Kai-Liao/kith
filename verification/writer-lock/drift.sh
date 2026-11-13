#!/usr/bin/env bash
# Which code the writer-lock models cover has changed since they were last confirmed to describe it?
#
#   verification/writer-lock/drift.sh        compares the working tree (so also uncommitted edits) with SYNC
#
# Prints the covered files that differ and a diff stat. No output of changed files = nothing to re-read.
# After you have read the changes, updated WriterLock4.tla / Holder.tla if needed, and run.sh is as expected,
# set SYNC below to the commit you checked (and in .claude/skills/writer-lock/SKILL.md).
set -euo pipefail

# Confirmed at 1720d27e (M3-12, 2026-11-14): every covered change since the old pin (73a03656) was read against the
# models (verification/writer-lock/results4.md, "Added by M3-12"). Move SYNC only after the next re-read (skill, "When you
# change covered code").
SYNC=1720d27eef3ac4f1208b94fa7c61f268a9e824b4

FILES=(
  core/stub                           # the start stub: the writer lock (fd CLOEXEC, kept to the engine's exit), redeem, the engine as its child
  core/holder/src/holder.rs           # spawn (same conv+claim answers the running one; the highest claim seen), reap, ACK/KILL by id, exits kept until acked
  core/holder/src/proto.rs            # the frames: SPAWN/SPAWNED/EXITED/ACK/KILL, the ids, hello and the instance
  core/runtime/src/holder_link.rs     # the core's side: hello, the recorded pids, step 7 (end the old instance's groups), the poll
  core/runtime/src/terminal.rs        # the terminal entrance: open, claim, spawn through the holder, attach
  core/runtime/src/conversations      # (M3-2) claims.rs: the claim transaction, claim ids, redeem CAS, reports, reconcile, revoke, new holder instance (park and wake, take-over: later)
  core/runtime/src/asks/mod.rs        # (M3-10b1, Claims4.tla) end_tree: the stopped-ask stop; its fence is in claims.rs claim_in
  core/runtime/src/wake/queue.rs      # (M3-10b1, Claims4.tla) give_back / release: a wake given back after a stop
  core/runtime/src/wake/turns.rs      # (M3-10c, Claims4.tla) the wake's turn: start_in parks terminal_live, a run's end closes its turn
  core/runtime/src/import/claims.rs  # (M3-10b3) the claim files into core.db: only live claimers, never a second live row on a conversation
  core/runtime/src/kill_resend.rs     # (M3-12, Claims4 SendKill) the daemon's own tick re-sends the kill of `ending` claims; victims and the dead leader's group
  core/runtime/src/hostclaim.rs       # (M3-12, M3-10f) the host's claim rows: one claims-API call each, refusals as data
  core/runtime/src/hostclaim_tests.rs # (M3-12) the same, real-core tests: a claim left ending, a hello under the host's instance name
  core/runtime/src/holder_link_tests.rs # (M3-12) the holder link's tests (the link itself is holder_link.rs)
  core/kernel/src/procstart.rs        # (M3-12, K2) orphan_group, same_process, comparable, claim_victims: what "the process is gone" means
  core/procstart                      # (M3-12) the stamp crate behind it (Linux b<boot_id>:<ticks>, unknown cross-form)
  serve/host/src/terminal             # (M3-12, M3-10f) core_claims.rs (the host's turn claims: HOST_INSTANCE, the claim file as the writer with the core row as its shadow), claim.rs, sync.rs, registry.rs
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
