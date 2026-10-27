#!/usr/bin/env bash
# Which code the secret-card model covers has changed since the model was last confirmed to describe it?
#
#   verification/secrets/drift.sh        compares the working tree (so also uncommitted edits) with SYNC
#
# Prints the covered files that differ and a diff stat. No output of changed files = nothing to re-read.
# After you have read the changes, updated Secrets.tla / run.sh if needed, and run.sh is as expected,
# set SYNC below to the commit you checked (and in .claude/skills/secret-cards/SKILL.md).
set -euo pipefail

# The commit at which the model was confirmed to describe the files below: origin/main after the secret cards
# (host d8713608, kernel 7863e2e9) and the limit cards (adcfb2f7, a branch after the secret routing) were merged.
SYNC=ed2ceb6356599bbf65756709b3b91966c9ff8cc5

FILES=(
  serve/host/src/cards/secret.rs    # provide, respond, deny, grant, Slot, write_key, open_card, list_keys, revoke_key
  serve/host/src/store/cards.rs     # card_claim, card_claim_release, card_saved, card_answer, cards_sweep, card_withdraw
  serve/host/src/cards/mod.rs       # sweep, answer, delivered, withdraw_entry, withdraw_person (closers of a card)
  serve/host/src/api/cards.rs       # provideSecret, listKeys, revokeKey (owner only)
  serve/host/src/api/bots.rs        # respondPermission: a secret card goes to secret::respond, before any other answer
  serve/host/src/api/roles.rs       # guard: nobody but the owner answers a secret card
  kernel/bin/kith-auth              # set --stdin-json --grant (the Write step), grant, keys, revoke --profile
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
