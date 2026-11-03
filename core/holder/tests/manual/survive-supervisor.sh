#!/bin/sh
# MANUAL check (not run by CI, not run by `cargo test`): the holder and its process survive the supervisor that
# started them, under a real launchd job (macOS). Children get killed by supervisors in ways `cargo test` cannot
# reproduce: launchd kills the whole job's process group, and the app's coalition, when the job exits.
#
# It submits one throw-away job (label dev.kith.holder-survive-test) to YOUR launchd, then removes it. Run it by hand
# on a Mac you are happy to do that on:
#
#   cargo build -p kith-holder --manifest-path serve/host/Cargo.toml
#   sh core/holder/tests/manual/survive-supervisor.sh [path/to/kith-holder]
#
# Expected: "PASS". On Windows the same is proven by CI's `windows` job (the survive test starts the holder from a
# process that then exits); the real Windows supervisor (the per-user Run key) still needs a by-hand check.
set -eu

exe=${1:-serve/host/target/debug/kith-holder}
exe=$(cd "$(dirname "$exe")" && pwd)/$(basename "$exe")
dir=$(mktemp -d "${TMPDIR:-/tmp}/khs.XXXXXX")
label=dev.kith.holder-survive-test
export KITH_HOLDER_SECRET=0123456789abcdef0123456789abcdef

cleanup() {
  launchctl remove "$label" 2>/dev/null || true
  # The holder holds nothing, so its idle timer ends it; removing its folder first does no harm.
  rm -rf "$dir"
}
trap cleanup EXIT

# The job is the would-be daemon: it starts the holder detached, then exits. launchd then cleans up the job.
launchctl submit -l "$label" -o "$dir/job.out" -e "$dir/job.err" -- \
  /usr/bin/env "KITH_HOLDER_SECRET=$KITH_HOLDER_SECRET" "$exe" start --dir "$dir" --idle-secs 60

i=0
while [ ! -S "$dir/holder.sock" ] && [ "$i" -lt 100 ]; do sleep 0.1; i=$((i + 1)); done
[ -S "$dir/holder.sock" ] || { echo "FAIL: no holder socket (see $dir/job.err)"; exit 1; }

sleep 3 # launchd has reaped the job by now
pid=$(pgrep -f "kith-holder serve --dir $dir" | head -1 || true)
[ -n "$pid" ] || { echo "FAIL: the holder did not survive its job"; exit 1; }
echo "PASS: holder pid $pid outlived the launchd job; it ends itself after its idle time (60 s)"
