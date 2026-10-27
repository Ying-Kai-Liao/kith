#!/bin/sh
# Writes a release's filled formula into the Homebrew tap as Formula/kith.rb, through the GitHub API (gh):
#
#   scripts/update-tap.sh <version> <kith.rb>
#
# The release workflow runs it after publishing, with GH_TOKEN set to the HOMEBREW_TAP_TOKEN secret. Nothing is
# committed when the tap already has this exact file.
#
# Environment:
#   KITH_TAP         the tap repo; default Ying-Kai-Liao/homebrew-kith
#   KITH_TAP_BRANCH  the branch to commit to; default the tap's default branch. A branch that doesn't exist yet is
#                    made from the default branch (for a pull request instead of a direct commit)
set -eu
[ $# -eq 2 ] || { sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'; exit 2; }
VERSION=${1#v} FORMULA=$2
TAP=${KITH_TAP:-Ying-Kai-Liao/homebrew-kith}
[ -f "$FORMULA" ] || { echo "update-tap: $FORMULA not found" >&2; exit 1; }
grep -q "^  version \"$VERSION\"$" "$FORMULA" || { echo "update-tap: $FORMULA is not version $VERSION" >&2; exit 1; }
if grep -q '"0\{64\}"' "$FORMULA"; then
  echo "update-tap: $FORMULA still has placeholder checksums (fill it with scripts/update-formula.sh)" >&2; exit 1
fi

DEFAULT=$(gh api "repos/$TAP" --jq .default_branch)
BRANCH=${KITH_TAP_BRANCH:-$DEFAULT}
if ! gh api "repos/$TAP/branches/$BRANCH" --silent 2>/dev/null; then
  base=$(gh api "repos/$TAP/git/ref/heads/$DEFAULT" --jq .object.sha)
  gh api -X POST "repos/$TAP/git/refs" -f ref="refs/heads/$BRANCH" -f sha="$base" --silent
fi

NEW=$(base64 < "$FORMULA" | tr -d '\n')
if CUR=$(gh api "repos/$TAP/contents/Formula/kith.rb?ref=$BRANCH" --jq '.sha + " " + .content' 2>/dev/null); then
  SHA=${CUR%% *}
  [ "$(printf '%s' "${CUR#* }" | tr -d '\n' | base64 -d)" = "$(cat "$FORMULA")" ] && { echo "$TAP: Formula/kith.rb is already $VERSION"; exit 0; }
  gh api -X PUT "repos/$TAP/contents/Formula/kith.rb" -f message="kith $VERSION" -f content="$NEW" -f branch="$BRANCH" -f sha="$SHA" --silent
else
  gh api -X PUT "repos/$TAP/contents/Formula/kith.rb" -f message="kith $VERSION" -f content="$NEW" -f branch="$BRANCH" --silent
fi
echo "$TAP: Formula/kith.rb is now $VERSION on $BRANCH"
