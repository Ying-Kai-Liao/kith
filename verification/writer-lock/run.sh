#!/usr/bin/env bash
# Model-checks the writer lock (WriterLock4.tla, round 4) and the holder's claim rules (Holder.tla) with TLC. Same
# tools, same cache as verification/threads/run.sh (which this asks where Java and the pinned tla2tools.jar are).
#
#   verification/writer-lock/run.sh                       the fast rows of checks.tsv: model x cfg, expected pass/fail
#   verification/writer-lock/run.sh --smoke               only the fast rows named in smoke.txt (CI when nothing the model covers changed)
#   verification/writer-lock/run.sh --all                 also the slow rows (a few of them run 15-30 min each)
#   verification/writer-lock/run.sh --show MODULE CFG     TLC's full output (the counterexample) for one row
#   verification/writer-lock/run.sh --list                the rows
#
# Exit 0 only when every row came out as checks.tsv expects: pass, or fail on the named invariant (`temporal` = a
# PROPERTY). A row that changes exits 1: read the skill (.claude/skills/writer-lock/SKILL.md), don't edit the table.
set -euo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

case "${1:-}" in
  --list) grep -v '^#' "$HERE/checks.tsv"; exit 0 ;;
esac

{ read -r JAVA; read -r JAR; } < <("$HERE/../threads/run.sh" --tools)

run_tlc() {   # run_tlc MODULE CFG SPEED -> TLC's output on stdout
  local work; work=$(mktemp -d "${TMPDIR:-/tmp}/kith-verify-wl.XXXXXX")
  # Fast rows run in parallel, one worker each, as the architect ran them; a slow row runs alone with all cores.
  local workers=1 heap=1g
  if [[ $3 == slow ]]; then workers=auto; heap=3g; fi
  # TLC unpacks its standard modules into java.io.tmpdir; parallel runs must not share one.
  mkdir -p "$work/tmp"
  ( cd "$HERE" && "$JAVA" -Djava.io.tmpdir="$work/tmp" -XX:+UseParallelGC -Xmx$heap -cp "$JAR" tlc2.TLC -workers "$workers" -deadlock \
      -metadir "$work/states" -config "$2.cfg" "$1.tla" 2>&1 ) || true
  rm -rf "$work"
}

verdict() {   # reads TLC output; prints pass | fail:<invariant or temporal> | error
  local out; out=$(cat)
  if grep -q 'No error has been found' <<<"$out"; then echo pass
  elif inv=$(sed -n 's/^Error: Invariant \([A-Za-z0-9_]*\) is violated.*/\1/p' <<<"$out" | head -1) && [[ -n $inv ]]; then echo "fail:$inv"
  elif grep -Eq 'Action property .* is violated' <<<"$out"; then echo "fail:action"
  elif grep -Eq 'Temporal properties were violated' <<<"$out"; then echo "fail:temporal"
  else echo error; fi
}

want_of() { if [[ $1 == pass ]]; then echo pass; else echo "fail:$2"; fi; }   # want_of EXPECTED FAILS

if [[ ${1:-} == --one ]]; then   # internal: one row, used for the parallel run
  shift   # MODULE CFG EXPECTED FAILS SPEED
  out=$(run_tlc "$1" "$2" "$5")
  got=$(verdict <<<"$out")
  states=$(sed -n 's/^[0-9]* states generated, \([0-9]*\) distinct states found.*/\1/p' <<<"$out" | head -1)
  [[ $got != error ]] || echo "run.sh: TLC gave no verdict for $2: $(grep -m1 -i 'error\|exception' <<<"$out")" >&2
  echo "$2|$(want_of "$3" "$4")|$got|${states:--}"
  exit 0
fi

if [[ ${1:-} == --show ]]; then
  speed=$(awk -F'\t' -v m="$2" -v c="$3" '$1==m && $2==c {print $5}' "$HERE/checks.tsv" | head -1)
  [[ -n $speed ]] || { echo "run.sh: no row $2 $3 in checks.tsv" >&2; exit 2; }
  run_tlc "$2" "$3" "$speed"; exit 0
fi

all=0; [[ ${1:-} == --all ]] && all=1
smoke=0; [[ ${1:-} == --smoke ]] && smoke=1
jobs=$(( $(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4) > 4 ? 4 : $(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4) ))
res=$(mktemp "${TMPDIR:-/tmp}/kith-verify-wl-res.XXXXXX")
start=$(date +%s)
rows() {
  if (( smoke )) && [[ $1 == fast ]]; then   # smoke.txt: one cfg name per line; a name not in checks.tsv is an error, not a skipped row
    grep -v '^#' "$HERE/checks.tsv" | grep -v '^$' | awk -F'\t' -v s="$1" 'NR==FNR { if ($0 !~ /^#|^$/) want[$1]=1; next } $5 == s && ($2 in want)' "$HERE/smoke.txt" -
  else
    grep -v '^#' "$HERE/checks.tsv" | grep -v '^$' | awk -F'\t' -v s="$1" '$5 == s'
  fi
}
if (( smoke )); then
  [[ $(grep -vc '^#\|^$' "$HERE/smoke.txt") -eq $(rows fast | wc -l) ]] || { echo "run.sh: smoke.txt names a cfg that is not a fast row of checks.tsv" >&2; exit 2; }
fi

echo "tla2tools v1.7.4, java ($JAVA)"
rows fast | tr '\t\n' '\0\0' | xargs -0 -n 5 -P "$jobs" "${BASH_SOURCE[0]}" --one >"$res" 2>&1 || true
if (( all )); then
  while IFS=$'\t' read -r m c e f s; do "${BASH_SOURCE[0]}" --one "$m" "$c" "$e" "$f" "$s" >>"$res" 2>&1 || true; done < <(rows slow)
fi

bad=0; n=0
printf '%-34s %-24s %-24s %s\n' CFG WANT GOT STATES
while IFS='|' read -r c want got st; do
  n=$((n+1)); mark=""; [[ $want == "$got" ]] || { mark="<-- MISMATCH"; bad=1; }
  printf '%-34s %-24s %-24s %-11s %s\n' "$c" "$want" "$got" "$st" "$mark"
done < <(sort "$res")
expect=$(( $(rows fast | wc -l) + (all ? $(rows slow | wc -l) : 0) ))
(( n == expect )) || { echo "run.sh: only $n of $expect rows reported (TLC or setup failed); see: run.sh --show MODULE CFG"; grep -v '|' "$res" || true; bad=1; }
rm -f "$res"
(( all )) || echo "(slow rows skipped: $(rows slow | wc -l); run with --all)"
echo "$n rows in $(( $(date +%s) - start ))s"
if (( bad )); then echo "RESULT: differs from checks.tsv"; exit 1; fi
echo "RESULT: every row as expected"
