#!/bin/sh
# Copies the kernel tree and its companions into <share-dir> (the share/kith of an install):
#
#   scripts/stage-kernel.sh <share-dir> [version]
#
# The one place that lists what a packaged kith carries beside its binary. package-release.sh (the release
# tarball) and the Mac app's build (apps/desktop/tools/native-host.mjs) both call it, so they cannot drift.
# It stamps kernel/VERSION (one line, the release version: [version], else serve/host/Cargo.toml's): the installer
# files a release under cli/<version>/, and the host reads the CLI's version from it. A checkout has no VERSION file.
set -eu
[ $# -ge 1 ] && [ $# -le 2 ] || { sed -n '2,9p' "$0" | sed 's/^# \{0,1\}//'; exit 2; }
S=$1
ROOT=$(cd "$(dirname "$0")/.." && pwd -P)
VERSION=${2:-$(sed -n 's/^version *= *"\([^"]*\)".*/\1/p' "$ROOT/serve/host/Cargo.toml" | head -1)}
VERSION=${VERSION#v}
[ -n "$VERSION" ] || { echo "stage-kernel: no version (pass one, or serve/host/Cargo.toml has none)" >&2; exit 1; }
mkdir -p "$S"
cp -R "$ROOT/kernel" "$S/kernel"
cp -R "$ROOT/.claude-plugin" "$S/.claude-plugin"
cp "$ROOT/install.sh" "$ROOT/LICENSE" "$ROOT/README.md" "$S/"
# The NOTICE has to travel with every redistribution of the host binary.
cp "$ROOT/NOTICE" "$ROOT/THIRD_PARTY_NOTICES.md" "$S/"
# Tests and caches are for the repo, not the install.
rm -rf "$S/kernel/tests"
printf '%s\n' "$VERSION" > "$S/kernel/VERSION"
find "$S" \( -name __pycache__ -o -name .DS_Store \) -prune -exec rm -rf {} +
