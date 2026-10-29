#!/bin/sh
# Packs a built kith binary and the kernel into the release tarball the installer and the Homebrew formula use.
#
#   scripts/package-release.sh <version> <arch> <path-to-kith-binary> [out-dir]
#
# kith-<version>-darwin-<arch>.tar.gz holding bin/kith (and bin/kith-tts, the local voice helper, when KITH_TTS_BIN names
# a built one: arm64 only) and share/kith/{kernel,.claude-plugin,install.sh,LICENSE,
# NOTICE,THIRD_PARTY_NOTICES.md,README.md}: the layout the binary's kernel lookup expects (kernel at ../share/kith/kernel). Also writes the
# tarball's .sha256 next to it.
set -eu
[ $# -ge 3 ] || { sed -n '2,8p' "$0" | sed 's/^# \{0,1\}//'; exit 2; }
VERSION=${1#v} ARCH=$2 BIN=$3 OUT=${4:-dist}
ROOT=$(cd "$(dirname "$0")/.." && pwd -P)
NAME=kith-$VERSION-darwin-$ARCH
STAGE=$(mktemp -d "${TMPDIR:-/tmp}/kith-pack.XXXXXX")
trap 'rm -rf "$STAGE"' EXIT
mkdir -p "$STAGE/$NAME/bin" "$STAGE/$NAME/share/kith" "$OUT"
install -m 755 "$BIN" "$STAGE/$NAME/bin/kith"
# The host looks for kith-tts next to itself; Apple silicon only, so the x86_64 tarball has none.
[ -z "${KITH_TTS_BIN:-}" ] || install -m 755 "$KITH_TTS_BIN" "$STAGE/$NAME/bin/kith-tts"
sh "$ROOT/scripts/stage-kernel.sh" "$STAGE/$NAME/share/kith"
tar -C "$STAGE" -czf "$OUT/$NAME.tar.gz" "$NAME"
(cd "$OUT" && shasum -a 256 "$NAME.tar.gz" > "$NAME.tar.gz.sha256")
echo "$OUT/$NAME.tar.gz"
