#!/usr/bin/env bash
# The model-check pass keys and drift report for CI (.github/workflows/ci.yml), from verification/model-keys.tsv.
#
#   verification/model-keys.sh keys PREFIX           one line per marker, for $GITHUB_OUTPUT: <marker>_key=PREFIX-<marker>-<hash>
#                                                    (marker with - as _); hash = sha256 of `git ls-files -s` over its inputs
#   verification/model-keys.sh hash MARKER           just the hash
#   verification/model-keys.sh drift BASE MODEL...   runs verification/MODEL/drift.sh for each MODEL (a folder name) and reports:
#                                                    covered code changed since SYNC is a ::warning naming the files this
#                                                    change touched (git diff BASE...HEAD), never a failure; a drift.sh that
#                                                    cannot run (exit 2: SYNC not in the clone) is an ::error and exit 1
#
# The hash reads the index (what is committed or staged), so a CI checkout hashes exactly its commit.
set -euo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(git -C "$HERE" rev-parse --show-toplevel)
TSV=$HERE/model-keys.tsv

rows() { grep -v '^#' "$TSV" | grep -v '^$'; }
sha256() { if command -v sha256sum >/dev/null; then sha256sum | cut -d' ' -f1; else shasum -a 256 | cut -d' ' -f1; fi; }

hash_of() {   # hash_of MARKER
  local inputs listed
  inputs=$(rows | awk -F'\t' -v m="$1" '$1 == m { print $3 }')
  [[ -n $inputs ]] || { echo "model-keys.sh: no marker $1 in model-keys.tsv" >&2; exit 2; }
  # shellcheck disable=SC2086  # the inputs are space-separated pathspecs
  listed=$(git -C "$ROOT" ls-files -s -- $inputs ':(exclude)verification/*/drift.sh')
  [[ -n $listed ]] || { echo "model-keys.sh: the inputs of $1 match no file: $inputs" >&2; exit 2; }
  printf '%s\n' "$listed" | sha256
}

# drift_files MODEL -> the paths in verification/MODEL/drift.sh's FILES list, one per line
drift_files() { sed -n '/^FILES=(/,/^)/p' "$HERE/$1/drift.sh" | sed '1d;$d' | sed -E 's/#.*//' | awk 'NF { print $1 }'; }

drift() {
  local base=$1; shift
  local zero=0000000000000000000000000000000000000000 touched="" bad=0 m out code sync skill hit
  if [[ -z $base || $base == "$zero" ]] || ! git -C "$ROOT" cat-file -e "$base^{commit}" 2>/dev/null; then
    base=$(git -C "$ROOT" merge-base origin/main HEAD 2>/dev/null || true)
  fi
  [[ -z $base ]] || touched=$(git -C "$ROOT" diff --name-only "$base...HEAD")
  summary=${GITHUB_STEP_SUMMARY:-/dev/null}
  for m in "$@"; do
    [[ -x $HERE/$m/drift.sh ]] || { echo "::error title=model drift::no verification/$m/drift.sh"; bad=1; continue; }
    skill=$(rows | awk -F'\t' -v d="verification/$m" '{ split($3, i, " ") } i[1] == d { print $2; exit }')
    code=0; out=$("$HERE/$m/drift.sh" 2>&1) || code=$?
    sync=$(sed -n 's/^SYNC=\([0-9a-f]*\).*/\1/p' "$HERE/$m/drift.sh")
    case $code in
      0) echo "$m: no covered file differs from SYNC ${sync:0:9}"
         printf '### %s\nNo covered file differs from SYNC `%s`.\n\n' "$m" "${sync:0:9}" >>"$summary" ;;
      1) hit=$(grep -Fxf <(drift_files "$m") <<<"$touched" | paste -sd, - | sed 's/,/, /g' || true)
         echo "::warning title=model drift::$m: covered code changed since SYNC ${sync:0:9}; re-read the model and bump SYNC (skill $skill). This change touched: ${hit:-none of them}"
         printf '### %s\nCovered code changed since SYNC `%s` (skill %s). This change touched: %s\n\n```\n%s\n```\n\n' \
           "$m" "${sync:0:9}" "$skill" "${hit:-none of them}" "$out" >>"$summary"
         echo "$out" ;;
      *) echo "::error title=model drift::$m: drift.sh could not run (exit $code): $out"; bad=1 ;;
    esac
  done
  return "$bad"
}

case "${1:-}" in
  keys) [[ -n ${2:-} ]] || { echo "model-keys.sh: keys wants PREFIX" >&2; exit 2; }
        rows | while IFS=$'\t' read -r m _; do echo "${m//-/_}_key=$2-$m-$(hash_of "$m")"; done ;;
  hash) hash_of "${2:?model-keys.sh: hash wants MARKER}" ;;
  drift) shift; drift "$@" ;;
  *) sed -n '2,10p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 2 ;;
esac
