#!/bin/sh
# The holder owns processes and decides nothing (docs/plans/runtime-design.md section 1). It must link neither
# kith-core nor kith-kernel, nor any other crate of this repo: a holder that cannot see policy cannot grow any.
# The stub (the writer lock, which the holder may spawn) is held to the same rule. Checks each manifest and
# resolved dependency tree, on every platform the crate builds for.
# Run from anywhere; needs cargo and the lockfile (no network once crates are fetched).
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
fail=0

for crate in holder stub; do
  pkg="kith-$crate"
  manifest="$root/core/$crate/Cargo.toml"

  # 1. The manifest names no path dependency and no kith-* crate.
  if grep -nE '^\s*(kith[-_a-z]*)\s*=|path\s*=' "$manifest" | grep -vE '^\S*\s*(path = "src/(lib|main)\.rs"|name =)' ; then
    echo "core/$crate/Cargo.toml depends on a kith crate or a path (see lines above)" >&2
    fail=1
  fi

  # 2. The resolved tree, for the host platform and for Windows, holds no kith-* crate but the crate itself.
  for target in "" "--target x86_64-pc-windows-msvc"; do
    # shellcheck disable=SC2086
    tree=$(cd "$root/serve/host" && cargo tree --manifest-path Cargo.toml -p "$pkg" --locked --edges normal,build $target --prefix none | sort -u)
    bad=$(printf '%s\n' "$tree" | grep -E '^kith' | grep -vE "^$pkg " || true)
    if [ -n "$bad" ]; then
      echo "$pkg links other kith crates ${target:+($target)}:" >&2
      echo "$bad" >&2
      fail=1
    fi
  done
  [ "$fail" = 0 ] && echo "$pkg links no kith crate"
done
exit "$fail"
