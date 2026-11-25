#!/bin/sh
# Builds and packs the Windows release zip, kith-<version>-windows-x64.zip. Runs in Git Bash on the CI runner.
#
#   scripts/package-windows.sh build                                 cargo build --release for each package in windows-exes.txt
#   scripts/package-windows.sh pack <version> <bin-dir> [out-dir]    check the exes, then zip them with the data
#
# Layout (what W14's install.ps1 unpacks into cli\<version>\): bin\*.exe and share\kith\ (stage-kernel.sh, Windows data
# only). Unsigned (docs/plans/windows-owner.md Q2): signing is a gate before the first public Windows release.
# Every `required` exe in windows-exes.txt must exist or the script fails; a `pending` one only warns, unless
# KITH_WINDOWS_STRICT=1 (the v* tag build), so a release never goes out with a silent gap.
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd -P)
LIST=$ROOT/scripts/windows-exes.txt
exes() { sed -e '/^[[:space:]]*#/d' -e '/^[[:space:]]*$/d' "$LIST"; }

case ${1:-} in
build)
  pkgs=$(exes | while read -r _ pkg status _; do
    # A pending program's crate may not exist yet; a required one always must.
    if [ "$status" = pending ] && ! (cd "$ROOT/serve/host" && cargo pkgid -p "$pkg" >/dev/null 2>&1); then continue; fi
    echo "-p $pkg"
  done | sort -u | tr '\n' ' ')
  echo "building: $pkgs"
  # shellcheck disable=SC2086
  (cd "$ROOT/serve/host" && cargo build --release --locked $pkgs)
  ;;
pack)
  [ $# -ge 3 ] || { sed -n '2,6p' "$0" | sed 's/^# \{0,1\}//'; exit 2; }
  VERSION=${2#v} BIN=$3 OUT=${4:-dist}
  NAME=kith-$VERSION-windows-x64
  missing='' gaps=''
  while read -r exe _ status owner; do
    [ -f "$BIN/$exe" ] && continue
    if [ "$status" = required ] || [ "${KITH_WINDOWS_STRICT:-}" = 1 ]; then missing="$missing $exe[$owner]"; else gaps="$gaps $exe[$owner]"; fi
  done <<EOL
$(exes)
EOL
  [ -z "$gaps" ] || echo "::warning::windows zip is missing pending programs:$gaps"
  [ -z "$missing" ] || { echo "::error::windows zip lacks:$missing" >&2; exit 1; }
  STAGE=$(mktemp -d "${TMPDIR:-/tmp}/kith-pack.XXXXXX")
  trap 'rm -rf "$STAGE"' EXIT
  mkdir -p "$STAGE/$NAME/bin" "$STAGE/$NAME/share/kith" "$OUT"
  exes | while read -r exe _; do
    if [ -f "$BIN/$exe" ]; then cp "$BIN/$exe" "$STAGE/$NAME/bin/$exe"; fi
  done
  sh "$ROOT/scripts/stage-kernel.sh" "$STAGE/$NAME/share/kith" "$VERSION" windows
  OUT=$(cd "$OUT" && pwd -P)
  rm -f "$OUT/$NAME.zip"
  if command -v zip >/dev/null 2>&1; then (cd "$STAGE" && zip -qr "$OUT/$NAME.zip" "$NAME")
  else (cd "$STAGE" && 7z a -tzip -bso0 "$OUT/$NAME.zip" "$NAME"); fi
  if command -v sha256sum >/dev/null 2>&1; then (cd "$OUT" && sha256sum "$NAME.zip" > "$NAME.zip.sha256")
  else (cd "$OUT" && shasum -a 256 "$NAME.zip" > "$NAME.zip.sha256"); fi
  echo "$OUT/$NAME.zip"
  ;;
*) sed -n '2,6p' "$0" | sed 's/^# \{0,1\}//'; exit 2 ;;
esac
