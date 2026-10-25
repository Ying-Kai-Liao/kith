#!/usr/bin/env bash
# Model-checks the order of a secret card's steps (Secrets.tla) with TLC. Same tools, same cache as
# verification/threads/run.sh (which this asks where Java and the pinned tla2tools.jar are).
#
#   verification/secrets/run.sh          every row of checks.tsv: variant x property, expected pass/fail
#   verification/secrets/run.sh --list   the rows
#
# Exit 0 only when every row came out as checks.tsv expects.
set -euo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

# variant -> "Exclusive ClaimBlocksClose"
switches() {
  case $1 in
    code) echo "TRUE TRUE" ;;
    no-slot) echo "FALSE TRUE" ;;
    no-claim) echo "TRUE FALSE" ;;
    *) echo "run.sh: unknown variant $1" >&2; exit 2 ;;
  esac
}

if [[ ${1:-} == --list ]]; then grep -v '^#' "$HERE/checks.tsv"; exit 0; fi

{ read -r JAVA; read -r JAR; } < <("$HERE/../threads/run.sh" --tools)

one() {   # one VARIANT KIND NAME WANT
  local work out res
  work=$(mktemp -d "${TMPDIR:-/tmp}/kith-verify-secrets.XXXXXX")
  read -r ex blocks < <(switches "$1")
  mkdir -p "$work/tmp"
  {
    echo "CONSTANTS"
    echo "Exclusive = $ex"
    echo "ClaimBlocksClose = $blocks"
    echo "SPECIFICATION Spec"
    echo "$2 $3"
  } > "$work/Secrets.cfg"
  out=$( ( cd "$HERE" && "$JAVA" -Djava.io.tmpdir="$work/tmp" -XX:+UseParallelGC -Xmx512m -cp "$JAR" tlc2.TLC -workers 1 \
      -deadlock -metadir "$work/states" -config "$work/Secrets.cfg" Secrets.tla 2>&1 ) || true )
  rm -rf "$work"
  if grep -q 'No error has been found' <<<"$out"; then res=pass
  elif grep -Eq 'Invariant .* is violated|Action property .* is violated|Temporal properties were violated' <<<"$out"; then res=fail
  else res=error; echo "$out" | tail -5 >&2; fi
  local states; states=$(sed -n 's/^[0-9]* states generated, \([0-9]*\) distinct states found.*/\1/p' <<<"$out" | head -1)
  printf '%-10s %-10s %-22s %-5s %-5s %s\n' "$1" "$2" "$3" "$4" "$res" "${states:--}"
  [[ $res == "$4" ]]
}

bad=0
printf '%-10s %-10s %-22s %-5s %-5s %s\n' VARIANT KIND PROPERTY WANT GOT STATES
while IFS=$'\t' read -r v k p want; do
  one "$v" "$k" "$p" "$want" || bad=1
done < <(grep -v '^#' "$HERE/checks.tsv" | grep -v '^$')
if (( bad )); then echo "RESULT: differs from checks.tsv"; exit 1; fi
echo "RESULT: every row as expected"
