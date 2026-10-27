#!/bin/sh
# Writes a release's version and sha256 values into the Homebrew formula and prints the result to stdout:
#
#   scripts/update-formula.sh <version> <SHA256SUMS> [formula]     formula defaults to packaging/homebrew/kith.rb
#
# The release workflow runs it, attaches the filled kith.rb to the release and commits it to the tap repo
# with scripts/update-tap.sh (see packaging/homebrew/README.md). The URLs in the formula are built from `version`.
# Same inputs, same output.
set -eu
[ $# -ge 2 ] || { sed -n '2,8p' "$0" | sed 's/^# \{0,1\}//'; exit 2; }
VERSION=${1#v} SUMS=$2
ROOT=$(cd "$(dirname "$0")/.." && pwd -P)
FORMULA=${3:-$ROOT/packaging/homebrew/kith.rb}
sum_of() {
  s=$(awk -v f="kith-$VERSION-darwin-$1.tar.gz" '$2 == f || $2 == "*" f { print $1 }' "$SUMS")
  [ -n "$s" ] || { echo "update-formula: no checksum for kith-$VERSION-darwin-$1.tar.gz in $SUMS" >&2; exit 1; }
  echo "$s"
}
ARM=$(sum_of arm64)
INTEL=$(sum_of x86_64)
# The first sha256 line is the arm one, the second the intel one (the order in the formula).
awk -v v="$VERSION" -v arm="$ARM" -v intel="$INTEL" '
  /^  version "/ { print "  version \"" v "\""; next }
  /^ *sha256 "/ { n++; sub(/"[^"]*"/, "\"" (n == 1 ? arm : intel) "\"") }
  { print }
' "$FORMULA"
