#!/usr/bin/env bash
# Model-checks revoking a person (RevokeDoors.tla) with TLC. Same tools, same cache as verification/threads/run.sh
# (which this asks where Java and the pinned tla2tools.jar are).
#
#   verification/revoke/run.sh                          every row of checks.tsv: variant x property, expected pass/fail
#   verification/revoke/run.sh --show VARIANT PROPERTY   TLC's full output (the counterexample) for one row
#   verification/revoke/run.sh --list                   the rows
#
# Exit 0 only when every row came out as checks.tsv expects. A row that changes exits 1: read the skill
# (.claude/skills/revoke-access/SKILL.md), don't edit the table to match.
set -euo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

# compose VARIANT KIND NAME -> a cfg on stdout: base.consts with the variant's overrides, then the property.
compose() {
  local set=$HERE/variants/$1.set
  [[ -f $set ]] || { echo "run.sh: no variants/$1.set" >&2; exit 2; }
  echo CONSTANTS
  awk -F' *= *' 'NR==FNR { if ($0 ~ /^[A-Za-z]/) o[$1]=$0; next } /^[A-Za-z]/ { print ($1 in o) ? o[$1] : $0 }' "$set" "$HERE/base.consts"
  echo "SPECIFICATION Spec"
  echo "$2 $3"
}

run_tlc() {   # run_tlc VARIANT KIND NAME -> TLC's output on stdout
  local work; work=$(mktemp -d "${TMPDIR:-/tmp}/kith-verify-revoke.XXXXXX")
  compose "$1" "$2" "$3" > "$work/RevokeDoors.cfg"
  mkdir -p "$work/tmp"
  ( cd "$HERE" && "$JAVA" -Djava.io.tmpdir="$work/tmp" -XX:+UseParallelGC -Xmx512m -cp "$JAR" tlc2.TLC -workers 1 -deadlock \
      -metadir "$work/states" -config "$work/RevokeDoors.cfg" RevokeDoors.tla 2>&1 ) || true
  rm -rf "$work"
}

verdict() {   # reads TLC output; prints pass | fail | error
  local out; out=$(cat)
  if grep -q 'No error has been found' <<<"$out"; then echo pass
  elif grep -Eq 'Invariant .* is violated|Action property .* is violated|Temporal properties were violated' <<<"$out"; then echo fail
  else echo error; fi
}

case "${1:-}" in
  --list) grep -v '^#' "$HERE/checks.tsv"; exit 0 ;;
esac

{ read -r JAVA; read -r JAR; } < <("$HERE/../threads/run.sh" --tools)

if [[ ${1:-} == --show ]]; then
  kind=$(awk -F'\t' -v v="$2" -v n="$3" '$1==v && $3==n {print $2}' "$HERE/checks.tsv" | head -1)
  [[ -n $kind ]] || { echo "run.sh: no row $2 $3 in checks.tsv" >&2; exit 2; }
  run_tlc "$2" "$kind" "$3"; exit 0
fi

bad=0
start=$(date +%s)
printf '%-26s %-24s %-5s %-5s %s\n' VARIANT PROPERTY WANT GOT STATES
while IFS=$'\t' read -r v k p want; do
  out=$(run_tlc "$v" "$k" "$p")
  got=$(verdict <<<"$out")
  states=$(sed -n 's/^[0-9]* states generated, \([0-9]*\) distinct states found.*/\1/p' <<<"$out" | head -1)
  [[ $got != error ]] || echo "run.sh: TLC gave no verdict for $v $p: $(grep -m1 -i 'error\|exception' <<<"$out")" >&2
  mark=""; [[ $got == "$want" ]] || { mark="<-- MISMATCH"; bad=1; }
  printf '%-26s %-24s %-5s %-5s %-9s %s\n' "$v" "$p" "$want" "$got" "${states:--}" "$mark"
done < <(grep -v '^#' "$HERE/checks.tsv" | grep -v '^$')
echo "$(grep -vc '^#\|^$' "$HERE/checks.tsv") rows in $(( $(date +%s) - start ))s"
if (( bad )); then echo "RESULT: differs from checks.tsv"; exit 1; fi
echo "RESULT: every row as expected"
