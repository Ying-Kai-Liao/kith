#!/usr/bin/env bash
# Model-checks revoking a person (RevokeDoors.tla) with TLC. Same tools, same cache as verification/threads/run.sh
# (which this asks where Java and the pinned tla2tools.jar are).
#
#   verification/revoke/run.sh                          every row of checks.tsv: variant x property, expected pass/fail
#   verification/revoke/run.sh --smoke                       only the rows named in smoke.txt (CI when nothing the model covers changed)
#   verification/revoke/run.sh --show VARIANT PROPERTY   TLC's full output (the counterexample) for one row
#   verification/revoke/run.sh --list                   the rows
#   verification/revoke/run.sh --shard I/N              only shard I of N (also KITH_SHARD=I KITH_SHARDS=N), combinable with --smoke;
#                                                       shard 1 also runs the Reconcile tables. CI runs the shards as a matrix.
#   verification/revoke/run.sh --shard-check N          no TLC: fails unless the N shards together hold every row exactly once
#   KITH_TLC_HEAP=6g                                    TLC's Java heap (default 512m; the heap does not change what TLC explores)
#
# Exit 0 only when every row came out as checks.tsv expects. A row that changes exits 1: read the skill
# (.claude/skills/revoke-access/SKILL.md), don't edit the table to match.
set -euo pipefail

# --shard I/N and --shard-check N are taken off the arguments first, so the rest (--smoke, --show V P, --list) read as before.
SHARD=${KITH_SHARD:-1} SHARDS=${KITH_SHARDS:-1} SHARD_CHECK=0
args=()
while (( $# )); do
  case $1 in
    --shard) [[ ${2:-} =~ ^[0-9]+/[0-9]+$ ]] || { echo "run.sh: --shard wants I/N" >&2; exit 2; }
             SHARD=${2%/*} SHARDS=${2#*/}; shift 2 ;;
    --shard-check) [[ ${2:-} =~ ^[0-9]+$ ]] || { echo "run.sh: --shard-check wants N" >&2; exit 2; }
             SHARD_CHECK=$2; shift 2 ;;
    *) args+=("$1"); shift ;;
  esac
done
set -- ${args[@]+"${args[@]}"}
(( SHARDS >= 1 && SHARD >= 1 && SHARD <= SHARDS )) || { echo "run.sh: shard $SHARD of $SHARDS makes no sense" >&2; exit 2; }
TLC_HEAP=${KITH_TLC_HEAP:-512m}

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
  ( cd "$HERE" && "$JAVA" -Djava.io.tmpdir="$work/tmp" -XX:+UseParallelGC -Xmx$TLC_HEAP -cp "$JAR" tlc2.TLC -workers 1 -deadlock \
      -metadir "$work/states" -config "$work/RevokeDoors.cfg" RevokeDoors.tla 2>&1 ) || true
  rm -rf "$work"
}

verdict() {   # reads TLC output; prints pass | fail | error
  local out; out=$(cat)
  if grep -q 'No error has been found' <<<"$out"; then echo pass
  elif grep -Eq 'Invariant .* is violated|Action property .* is violated|Temporal properties were violated' <<<"$out"; then echo fail
  else echo error; fi
}

SMOKE=0; [[ ${1:-} == --smoke ]] && SMOKE=1

# rows -> the checks.tsv rows to run: all of them, or with --smoke only those named in smoke.txt ("VARIANT PROPERTY" per line).
rows() {
  if [[ $SMOKE == 1 ]]; then
    grep -v '^#' "$HERE/checks.tsv" | grep -v '^$' | awk -F'\t' 'NR==FNR { if ($0 !~ /^#|^$/) { split($0, w, " "); want[w[1] " " w[2]]=1 } next } (($1 " " $3) in want)' "$HERE/smoke.txt" -
  else
    grep -v '^#' "$HERE/checks.tsv" | grep -v '^$'
  fi
}

# shard_rows -> this shard's rows. The slow liveness rows (heavy, below) are dealt round-robin first, the rest after them,
# so they spread; with more than one shard the single slowest row (switched-always-kill NoLiveProc, ~25 min alone) gets the
# last shard to itself. Deterministic: depends only on the row order and N.
shard_rows() {
  rows | awk -F'\t' -v me="$SHARD" -v n="$SHARDS" '
    { r[NR] = $0; key = $1 " " $3
      if (n > 1 && key == "switched-always-kill NoLiveProc") pin[NR] = 1
      else if (key ~ /-kill NoLiveProc$|^switched-always|^core-order Swept$|^asks |^switched-core-door/) heavy[NR] = 1 }
    END {
      m = (n > 1) ? n - 1 : 1; k = 0
      for (i = 1; i <= NR; i++) if (heavy[i]) to[i] = k++ % m + 1
      for (i = 1; i <= NR; i++) if (!heavy[i] && !pin[i]) to[i] = k++ % m + 1
      for (i = 1; i <= NR; i++) { if (pin[i]) to[i] = n; if (to[i] == me) print r[i] } }'
}

if (( SHARD_CHECK )); then   # the guard: a later edit to the split can't silently drop or double a row
  got=$(for ((i = 1; i <= SHARD_CHECK; i++)); do SHARD=$i SHARDS=$SHARD_CHECK shard_rows; done | sort)
  [[ $(rows | sort) == "$got" ]] || { echo "run.sh: the $SHARD_CHECK shards do not hold every row exactly once" >&2; exit 1; }
  for ((i = 1; i <= SHARD_CHECK; i++)); do echo "shard $i/$SHARD_CHECK: $(SHARD=$i SHARDS=$SHARD_CHECK shard_rows | wc -l | tr -d ' ') rows"; done
  echo "shard check ok: $(rows | wc -l | tr -d ' ') rows in $SHARD_CHECK shards, each exactly once"; exit 0
fi

case "${1:-}" in
  --list) grep -v '^#' "$HERE/checks.tsv"; exit 0 ;;
esac

{ read -r JAVA; read -r JAR; } < <("$HERE/../threads/run.sh" --tools)

if [[ $SMOKE == 1 ]]; then
  [[ $(grep -vc '^#\|^$' "$HERE/smoke.txt") -eq $(rows | wc -l) ]] || { echo "run.sh: smoke.txt names a row that is not in checks.tsv" >&2; exit 2; }
fi

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
done < <(shard_rows)
echo "$(shard_rows | wc -l) rows in $(( $(date +%s) - start ))s"
# The host's reconcile of a core-only revoke (Reconcile.tla, 132 states) and the owed cut of a renamed person's failed
# revoke (ReconcileOwed.tla, 497 states): one config per row, all of them every run.
run_reconcile() {   # run_reconcile MODULE DIR CFG -> TLC's output on stdout
  local work; work=$(mktemp -d "${TMPDIR:-/tmp}/kith-verify-reconcile.XXXXXX")
  mkdir -p "$work/tmp"
  ( cd "$HERE" && "$JAVA" -Djava.io.tmpdir="$work/tmp" -XX:+UseParallelGC -Xmx$TLC_HEAP -cp "$JAR" tlc2.TLC -workers 1 -deadlock \
      -metadir "$work/states" -config "$HERE/$2/$3.cfg" "$1.tla" 2>&1 ) || true
  rm -rf "$work"
}
for table in Reconcile:reconcile ReconcileOwed:reconcile-owed; do
  (( SHARD == 1 )) || break   # the two small tables ride with shard 1
  module=${table%%:*} dir=${table#*:}
  printf '%-26s %-5s %-5s %s\n' "$(tr '[:lower:]-' '[:upper:] ' <<<"$dir")" WANT GOT STATES
  while IFS=$'\t' read -r cfg want _; do
    out=$(run_reconcile "$module" "$dir" "$cfg")
    got=$(verdict <<<"$out")
    states=$(sed -n 's/^[0-9]* states generated, \([0-9]*\) distinct states found.*/\1/p' <<<"$out" | head -1)
    [[ $got != error ]] || echo "run.sh: TLC gave no verdict for $dir $cfg: $(grep -m1 -i 'error\|exception' <<<"$out")" >&2
    mark=""; [[ $got == "$want" ]] || { mark="<-- MISMATCH"; bad=1; }
    printf '%-26s %-5s %-5s %-9s %s\n' "$cfg" "$want" "$got" "${states:--}" "$mark"
  done < <(grep -v '^#' "$HERE/$dir.tsv" | grep -v '^$')
done
if (( bad )); then echo "RESULT: differs from checks.tsv"; exit 1; fi
echo "RESULT: every row as expected"
