#!/bin/sh
# Copies the kernel tree and its companions into <share-dir> (the share/kith of an install):
#
#   scripts/stage-kernel.sh <share-dir>
#
# The one place that lists what a packaged kith carries beside its binary. package-release.sh (the release
# tarball) and the Mac app's build (apps/desktop/tools/native-host.mjs) both call it, so they cannot drift.
set -eu
[ $# -eq 1 ] || { sed -n '2,7p' "$0" | sed 's/^# \{0,1\}//'; exit 2; }
S=$1
ROOT=$(cd "$(dirname "$0")/.." && pwd -P)
mkdir -p "$S"
cp -R "$ROOT/kernel" "$S/kernel"
cp -R "$ROOT/.claude-plugin" "$S/.claude-plugin"
cp "$ROOT/install.sh" "$ROOT/LICENSE" "$ROOT/README.md" "$S/"
# The NOTICE has to travel with every redistribution of the host binary.
cp "$ROOT/NOTICE" "$ROOT/THIRD_PARTY_NOTICES.md" "$S/"
# Tests and caches are for the repo, not the install.
rm -rf "$S/kernel/tests"
find "$S" \( -name __pycache__ -o -name .DS_Store \) -prune -exec rm -rf {} +
