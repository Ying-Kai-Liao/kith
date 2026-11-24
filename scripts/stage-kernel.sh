#!/bin/sh
# Copies the kernel tree and its companions into <share-dir> (the share/kith of an install):
#
#   scripts/stage-kernel.sh <share-dir> [version]
#
# The one place that lists what a packaged kith carries beside its binary. package-release.sh (the release
# tarball) and the Mac app's build (apps/desktop/tools/native-host.mjs) both call it, so they cannot drift.
# It stamps kernel/VERSION (one line, the release version: [version], else serve/host/Cargo.toml's): the installer
# files a release under cli/<version>/, and the host reads the CLI's version from it. A checkout has no VERSION file.
# With `windows` as the third argument (the Windows zip, package-windows.sh) only the data is staged: no kernel/bin and no
# kernel/lib (the Python and shell programs) and no POSIX install.sh.
set -eu
[ $# -ge 1 ] && [ $# -le 3 ] || { sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'; exit 2; }
S=$1
WINDOWS=
[ "${3:-}" = windows ] && WINDOWS=1
ROOT=$(cd "$(dirname "$0")/.." && pwd -P)
VERSION=${2:-$(sed -n 's/^version *= *"\([^"]*\)".*/\1/p' "$ROOT/serve/host/Cargo.toml" | head -1)}
VERSION=${VERSION#v}
[ -n "$VERSION" ] || { echo "stage-kernel: no version (pass one, or serve/host/Cargo.toml has none)" >&2; exit 1; }
mkdir -p "$S"
if [ -n "$WINDOWS" ]; then
  # What the Windows zip leaves out compared with the Mac tarball, and what stands in (docs/plans/windows-owner.md 4.11).
  # Later packages must not assume these files exist on Windows:
  #   kernel/bin   the `kith` router and every kith-* script. A verb the host owns runs in Rust; any other answers
  #                "not on Windows yet: <verb> (retire-python Pn-m)" from the W17 table. `project-id` ships as
  #                bin\project-id.exe (W17). No bash fallback.
  #   kernel/lib   the Python and shell libs (kith_*.py, kith-core.sh, kith-env.sh, core-switched.txt, ...). Nothing on
  #                Windows yet: each is ported by its own retire-python item (P3-*, P4-*, P5-*), and until then its
  #                verb is a named refusal. The few data files (starters.json, user.md, *.gitignore) are not staged
  #                either; a package that needs one moves it into kernel/ data first.
  #   kernel/tests the repo's tests.
  #   install.sh   the POSIX installer; Windows uses scripts/install.ps1 (W14).
  # Also never copy kernel/bin: it holds symlinks (hq -> kith-*) that Git Bash on Windows cannot create.
  mkdir -p "$S/kernel"
  for d in "$ROOT"/kernel/*; do
    case $(basename "$d") in bin | lib | tests) ;; *) cp -R "$d" "$S/kernel/" ;; esac
  done
else
  cp -R "$ROOT/kernel" "$S/kernel"
fi
cp -R "$ROOT/.claude-plugin" "$S/.claude-plugin"
cp "$ROOT/LICENSE" "$ROOT/README.md" "$S/"
[ -n "$WINDOWS" ] || cp "$ROOT/install.sh" "$S/"
# The NOTICE has to travel with every redistribution of the host binary.
cp "$ROOT/NOTICE" "$ROOT/THIRD_PARTY_NOTICES.md" "$S/"
# Tests and caches are for the repo, not the install.
rm -rf "$S/kernel/tests"
[ -z "$WINDOWS" ] || rm -rf "$S/kernel/bin" "$S/kernel/lib"
printf '%s\n' "$VERSION" > "$S/kernel/VERSION"
find "$S" \( -name __pycache__ -o -name .DS_Store \) -prune -exec rm -rf {} +
