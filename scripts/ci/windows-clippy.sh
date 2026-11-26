#!/usr/bin/env bash
# The Windows clippy list, in one pass. CI's `windows` job calls this natively; on a Mac it cross-checks:
#   scripts/ci/windows-clippy.sh --target x86_64-pc-windows-msvc
# This script is the source of truth for which crates the Windows job clippies: a new crate that CI builds on
# Windows is added HERE, not as a clippy line in ci.yml. Every invocation runs even when an earlier one fails,
# and the script names each failed group at the end. See docs/ci.md, "Windows clippy on a Mac".
set -u

target=""
while [ $# -gt 0 ]; do
  case "$1" in
    --target) target="${2:?--target needs a triple}"; shift 2 ;;
    --target=*) target="${1#--target=}"; shift ;;
    *) echo "usage: $0 [--target <triple>]" >&2; exit 2 ;;
  esac
done

root="$(cd "$(dirname "$0")/../.." && pwd)"
manifest="$root/serve/host/Cargo.toml"

# kith-stub joins once core/stub exists on the branch (as the CI step did).
all_targets="-p kith-holder -p kith-winsys -p kith-core -p kith-api -p kith-kernel -p kith-attach"
[ -d "$root/core/stub" ] && all_targets="$all_targets -p kith-stub"

sub=clippy
extra=""
if [ -n "$target" ]; then
  # A cross check from another OS (CI passes no target and runs natively).
  # Without a rustup proxy on PATH (the flow Macs) rust-toolchain.toml is ignored and an older stable would
  # run, showing lints CI never sees. Put the pinned toolchain's own bin first, read from the toolchain file.
  channel="$(sed -n 's/^channel *= *"\(.*\)"/\1/p' "$root/serve/rust-toolchain.toml")"
  tc="$(ls -d "$HOME/.rustup/toolchains/$channel"-* 2>/dev/null | head -1)"
  if [ -z "$tc" ]; then
    echo "pinned toolchain $channel missing from ~/.rustup/toolchains. Install: rustup toolchain install $channel --target $target" >&2
    exit 2
  fi
  export PATH="$tc/bin:$HOME/.cargo/bin:$PATH"
  command -v cargo >/dev/null 2>&1 || { echo "no cargo on PATH (install rustup)" >&2; exit 2; }
  # Host build scripts need a real C compiler; ~/.local/bin/cc can be something else on this Mac.
  [ -x /usr/bin/cc ] && export CC=/usr/bin/cc
  # C build scripts compile with clang-cl and archive with llvm-lib, which Xcode does not ship. Appended, so
  # the host's own clang stays first.
  PATH="$PATH:/opt/homebrew/opt/llvm/bin:/usr/local/opt/llvm/bin"
  for t in clang-cl llvm-lib; do
    command -v "$t" >/dev/null 2>&1 || { echo "$t missing. Install: brew install llvm" >&2; exit 2; }
  done
  if ! cargo xwin --version >/dev/null 2>&1; then
    echo "cargo-xwin missing. Install: cargo install --locked cargo-xwin" >&2
    exit 2
  fi
  if ! (cd "$root/serve" && rustc --print target-libdir --target "$target" >/dev/null 2>&1 \
        && [ -d "$(rustc --print target-libdir --target "$target")" ]); then
    echo "rust-std for $target missing. Install: rustup target add $target --toolchain $channel" >&2
    exit 2
  fi
  # Accepts the Microsoft CRT/SDK license (an approved decision); downloaded once into the xwin cache.
  export XWIN_ACCEPT_LICENSE=1
  # Its own target dir, so a Windows check never invalidates the native build cache.
  export CARGO_TARGET_DIR="${WINCLIPPY_TARGET_DIR:-$root/serve/host/target-xwin}"
  sub="xwin clippy"
  extra="--target $target"
fi

cd "$root/serve" || exit 2   # rust-toolchain.toml lives here
failed=""

run() {
  name="$1"; shift
  echo "::group::clippy $name"
  # shellcheck disable=SC2086
  if cargo $sub --manifest-path "$manifest" "$@" $extra --locked -- -D warnings; then
    echo "::endgroup::"
  else
    echo "::endgroup::"
    echo "::error title=windows clippy failed::$name"
    failed="$failed [$name]"
  fi
}

# One invocation for every crate CI clippies with --all-targets, so all their errors come out together.
# shellcheck disable=SC2086
run "all-targets crates" $all_targets --all-targets
# kith-serve is bins only (its tests are not Windows-ready).
run "kith-serve --bins" -p kith-serve --bins

if [ -n "$failed" ]; then
  echo "windows clippy FAILED in:$failed" >&2
  exit 1
fi
echo "windows clippy clean"
