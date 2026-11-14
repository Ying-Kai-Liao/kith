#!/usr/bin/env bash
# Model-checks rooms with people (Rooms2.tla: what a room turn reads, stamps and is reused for; RoomJobs.tla: what a revoke
# ends) with TLC. The models are the architect's (designs/2026-11-12-rooms-with-people/model), copied here with the
# variants the code is held to. Same tools and cache as verification/threads/run.sh (which this asks where they are).
#
#   verification/rooms/run.sh          every row of checks.tsv: variant x module, expected pass/fail
#   verification/rooms/run.sh --list   the rows
#   verification/rooms/run.sh --show VARIANT   one row, with TLC's full output (the counterexample)
#
# Exit 0 only when every row came out as checks.tsv expects. A row that changes means the code's rule and the model
# differ: read the invitee-access and revoke-access skills, do not edit the table.
set -euo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

if [[ ${1:-} == --list ]]; then grep -v '^#' "$HERE/checks.tsv"; exit 0; fi

{ read -r JAVA; read -r JAR; } < <("$HERE/../threads/run.sh" --tools)

one() {   # one VARIANT MODULE WANT
  local work out res states
  work=$(mktemp -d "${TMPDIR:-/tmp}/kith-verify-rooms.XXXXXX")
  mkdir -p "$work/tmp"
  out=$( ( cd "$HERE" && "$JAVA" -Djava.io.tmpdir="$work/tmp" -XX:+UseParallelGC -Xmx1g -cp "$JAR" tlc2.TLC -workers 2 \
      -deadlock -metadir "$work/states" -config "variants/$1.cfg" "$2.tla" 2>&1 ) || true )
  rm -rf "$work"
  [[ -n ${SHOW:-} ]] && echo "$out"
  if grep -q 'No error has been found' <<<"$out"; then res=pass
  elif grep -Eq 'Invariant .* is violated|Action property .* is violated|Temporal properties were violated' <<<"$out"; then res=fail
  else res=error; echo "$out" | tail -5 >&2; fi
  states=$(sed -n 's/^[0-9]* states generated, \([0-9]*\) distinct states found.*/\1/p' <<<"$out" | head -1)
  printf '%-22s %-9s %-5s %-5s %s\n' "$1" "$2" "$3" "$res" "${states:--}"
  [[ $res == "$3" ]]
}

if [[ ${1:-} == --show ]]; then
  module=$(awk -F'\t' -v v="$2" '$1==v {print $2}' "$HERE/checks.tsv" | head -1)
  [[ -n $module ]] || { echo "run.sh: no row $2 in checks.tsv" >&2; exit 2; }
  SHOW=1 one "$2" "$module" "$(awk -F'\t' -v v="$2" '$1==v {print $3}' "$HERE/checks.tsv" | head -1)" || true
  exit 0
fi

bad=0
printf '%-22s %-9s %-5s %-5s %s\n' VARIANT MODULE WANT GOT STATES
while IFS=$'\t' read -r v m want _; do
  [[ -z $v || $v == \#* ]] && continue
  one "$v" "$m" "$want" || bad=1
done < "$HERE/checks.tsv"
if (( bad )); then echo "RESULT: differs from checks.tsv"; exit 1; fi
echo "RESULT: every row as expected"
