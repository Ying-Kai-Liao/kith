#!/usr/bin/env bash
# Model-checks "one writer per thread" (Threads.tla) with TLC, one command, no global install.
#
#   verification/threads/run.sh                  every row of checks.tsv: variant x property, expected pass/fail
#   verification/threads/run.sh --show VARIANT PROPERTY   TLC's full output (the counterexample) for one row
#   verification/threads/run.sh --list           the rows
#
# Exit 0 only when every row came out as checks.tsv expects. A row that changes (a finding fixed, or a new break)
# exits 1: that is the signal to read the skill (.claude/skills/thread-writers/SKILL.md), not to edit the table.
#
# Tools (pinned, sha256-checked, cached under ${XDG_CACHE_HOME:-$HOME/.cache}/kith-verify/; nothing else on disk):
#   tla2tools.jar  TLA+ tools v1.7.4 (MIT), https://github.com/tlaplus/tlaplus/releases/tag/v1.7.4
#   Temurin JRE 21.0.9+10 (GPLv2+Classpath Exception), only when no Java 11+ is on PATH
set -euo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
CACHE=${XDG_CACHE_HOME:-$HOME/.cache}/kith-verify

TLA_VERSION=v1.7.4
TLA_SHA256=936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88
TLA_URL=https://github.com/tlaplus/tlaplus/releases/download/$TLA_VERSION/tla2tools.jar

JRE_TAG='jdk-21.0.9%2B10'
JRE_DIR='jdk-21.0.9+10-jre'
jre_asset() {   # prints "<file> <sha256>" for this machine
  case "$(uname -s)-$(uname -m)" in
    Darwin-arm64)  echo "OpenJDK21U-jre_aarch64_mac_hotspot_21.0.9_10.tar.gz 1f7f6506b598e85d7d8ff8b36563d98657d2d81b16bfca3cd242d7906cfbd11b" ;;
    Darwin-x86_64) echo "OpenJDK21U-jre_x64_mac_hotspot_21.0.9_10.tar.gz 945abc49249f1e89a2a6a008d70d63dc42b25bbe5e1711ff97aeec70063008d2" ;;
    Linux-x86_64)  echo "OpenJDK21U-jre_x64_linux_hotspot_21.0.9_10.tar.gz aeab55d064a1a27a3744b0880b9b414077b4ed2b1790817eea3df60aec946431" ;;
    Linux-aarch64) echo "OpenJDK21U-jre_aarch64_linux_hotspot_21.0.9_10.tar.gz 1d041073c65e834bdb4da732485a54ff829859dcd1549e7992f15bd73341be29" ;;
    *) echo "run.sh: no pinned JRE for $(uname -s)-$(uname -m); put Java 11+ on PATH" >&2; return 1 ;;
  esac
}

sha256_of() { if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi; }

fetch() {   # fetch URL DEST SHA256
  local tmp="$2.part.$$"
  curl -fsSL --retry 3 -o "$tmp" "$1"
  if [[ $(sha256_of "$tmp") != "$3" ]]; then rm -f "$tmp"; echo "run.sh: sha256 mismatch for $1" >&2; exit 2; fi
  mv "$tmp" "$2"
}

java_major() { "$1" -version 2>&1 | sed -n 's/.* version "\([0-9]*\).*/\1/p' | head -1; }

find_java() {
  local j=""
  if [[ $(uname -s) == Darwin ]]; then
    # /usr/bin/java is a stub on a Mac without a JDK; java_home fails there.
    if h=$(/usr/libexec/java_home 2>/dev/null) && [[ -x $h/bin/java ]]; then j=$h/bin/java; fi
  else
    j=$(command -v java 2>/dev/null || true)
  fi
  if [[ -n $j ]] && [[ $(java_major "$j") =~ ^[0-9]+$ ]] && (( $(java_major "$j") >= 11 )); then echo "$j"; return; fi
  read -r file sha < <(jre_asset)
  local root=$CACHE/jre-21.0.9+10
  j=$(find "$root" -path '*/bin/java' -type f 2>/dev/null | head -1 || true)
  if [[ -z $j ]]; then
    mkdir -p "$root.tmp.$$"
    fetch "https://github.com/adoptium/temurin21-binaries/releases/download/$JRE_TAG/$file" "$root.tmp.$$/$file" "$sha"
    tar -xzf "$root.tmp.$$/$file" -C "$root.tmp.$$"
    rm -f "$root.tmp.$$/$file"
    rm -rf "$root"; mv "$root.tmp.$$" "$root"
    j=$(find "$root" -path '*/bin/java' -type f | head -1)
  fi
  echo "$j"
}

setup() {
  mkdir -p "$CACHE"
  JAR=$CACHE/tla2tools-$TLA_VERSION.jar
  [[ -f $JAR ]] || fetch "$TLA_URL" "$JAR" "$TLA_SHA256"
  JAVA=$(find_java)
}

# compose VARIANT KIND NAME -> a cfg on stdout: base.consts with the variant's overrides, then the property.
compose() {
  local v=$1 kind=$2 name=$3 set=$HERE/variants/$1.set
  [[ -f $set ]] || { echo "run.sh: no variants/$v.set" >&2; exit 2; }
  echo CONSTANTS
  awk -F' *= *' 'NR==FNR { if ($0 ~ /^[A-Za-z]/) o[$1]=$0; next } /^[A-Za-z]/ { print ($1 in o) ? o[$1] : $0 }' "$set" "$HERE/base.consts"
  echo "SPECIFICATION Spec"
  echo "$kind $name"
}

run_tlc() {   # run_tlc VARIANT KIND NAME -> TLC's output on stdout
  local work; work=$(mktemp -d "${TMPDIR:-/tmp}/kith-verify.XXXXXX")
  compose "$1" "$2" "$3" > "$work/Threads.cfg"
  # TLC unpacks its standard modules into java.io.tmpdir; parallel runs must not share one.
  mkdir -p "$work/tmp"
  ( cd "$HERE" && "$JAVA" -Djava.io.tmpdir="$work/tmp" -XX:+UseParallelGC -Xmx512m -cp "$JAR" tlc2.TLC -workers 1 -deadlock \
      -metadir "$work/states" -config "$work/Threads.cfg" Threads.tla 2>&1 ) || true
  rm -rf "$work"
}

verdict() {   # reads TLC output; prints pass | fail | error
  local out; out=$(cat)
  if grep -q 'No error has been found' <<<"$out"; then echo pass
  elif grep -Eq 'Invariant .* is violated|Action property .* is violated|Temporal properties were violated' <<<"$out"; then echo fail
  else echo error; fi
}

case "${1:-}" in
  --one)   # internal: one row, used for the parallel run
    setup; shift
    out=$(run_tlc "$1" "$2" "$3")
    res=$(verdict <<<"$out")
    states=$(sed -n 's/^[0-9]* states generated, \([0-9]*\) distinct states found.*/\1/p' <<<"$out" | head -1)
    [[ $res != error ]] || echo "run.sh: TLC gave no verdict for $1 $3: $(grep -m1 -i 'error\|exception' <<<"$out")" >&2
    echo "$1|$3|$4|$res|${states:--}"
    [[ $res == "$4" ]]
    exit $? ;;
  --show)
    setup; kind=$(awk -F'\t' -v v="$2" -v n="$3" '$1==v && $3==n {print $2}' "$HERE/checks.tsv" | head -1)
    [[ -n $kind ]] || { echo "run.sh: no row $2 $3 in checks.tsv" >&2; exit 2; }
    run_tlc "$2" "$kind" "$3"; exit 0 ;;
  --tools)  # internal: where the pinned Java and TLA+ tools are, for the other models (verification/secrets/run.sh)
    setup; echo "$JAVA"; echo "$JAR"; exit 0 ;;
  --list)  grep -v '^#' "$HERE/checks.tsv"; exit 0 ;;
esac

setup
echo "tla2tools $TLA_VERSION, java $(java_major "$JAVA") ($JAVA)"
jobs=$(( $(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4) > 4 ? 4 : $(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4) ))
res=$(mktemp "${TMPDIR:-/tmp}/kith-verify-res.XXXXXX")
start=$(date +%s)
grep -v '^#' "$HERE/checks.tsv" | grep -v '^$' \
  | tr '\t\n' '\0\0' \
  | xargs -0 -n 4 -P "$jobs" "${BASH_SOURCE[0]}" --one >"$res" 2>&1 || true

bad=0
printf '%-26s %-34s %-6s %-6s %s\n' VARIANT PROPERTY WANT GOT STATES
while IFS='|' read -r v p want got st; do
  mark=ok; [[ $want == "$got" ]] || { mark=MISMATCH; bad=1; }
  printf '%-26s %-34s %-6s %-6s %-9s %s\n' "$v" "$p" "$want" "$got" "$st" "$([[ $mark == ok ]] || echo "<-- $mark")"
done < <(sort "$res")
rows=$(grep -vc '^#\|^$' "$HERE/checks.tsv" || true)
[[ $(wc -l <"$res") -eq $rows ]] || { echo "run.sh: only $(wc -l <"$res") of $rows rows reported (TLC or setup failed); see: run.sh --show VARIANT PROPERTY"; cat "$res" | grep -v '|' || true; bad=1; }
rm -f "$res"
echo "$rows rows in $(( $(date +%s) - start ))s"
if (( bad )); then echo "RESULT: differs from checks.tsv"; exit 1; fi
echo "RESULT: every row as expected"
