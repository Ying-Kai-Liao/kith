#!/bin/sh
# The host unit tests that run real kernel files, listed in host-kernel-tests.txt.
#   filter  print a cargo-nextest filterset (-E) that selects exactly those tests
#   check   fail when a file under serve/host reads the kernel and is not listed (CI drift guard)
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
list="$here/host-kernel-tests.txt"
tab=$(printf '\t')

rows() { grep -v '^[[:space:]]*#' "$list" | grep -v '^[[:space:]]*$'; }

case "${1:-}" in
filter)
  rows | awk -F"$tab" '{ printf "%stest(/^%s/)", (n++ ? " + " : ""), $2 } END { print "" }'
  ;;
check)
  bad=0
  for f in $(rows | cut -f1); do
    [ -f "$root/serve/host/$f" ] || { echo "listed but missing: serve/host/$f"; bad=1; }
  done
  # Test code that reaches the kernel folder by path, or builds the api Rig (which runs the real kith-auth).
  # The Rig itself, tests/ (host-e2e) and the wire crate (host_core covers serve/contract/kernel) are not unit-test users.
  cd "$root/serve/host"
  for f in $(grep -rlE 'CARGO_MANIFEST_DIR.*kernel|rig::Rig|room_secret_tests::' src --include='*.rs' | sort); do
    # wake.rs only holds the helpers the listed resume and revoke tests call.
    # stepped_e2e_tests.rs is #[ignore]d: only the host-build step runs it, with the built kith (never a skip).
    case "$f" in src/api/rig.rs|src/wake.rs|src/api/stepped_e2e_tests.rs) continue ;; esac
    rows | cut -f1 | grep -qxF "$f" || { echo "serve/host/$f reads the kernel but is not in scripts/ci/host-kernel-tests.txt"; bad=1; }
  done
  [ "$bad" = 0 ] && echo "host-kernel-tests: the list covers every host test that reads the kernel"
  exit "$bad"
  ;;
*)
  echo "usage: $0 filter|check" >&2
  exit 2
  ;;
esac
