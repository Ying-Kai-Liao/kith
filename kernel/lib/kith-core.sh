# kith-core.sh: the router from `kith <verb>` to the Rust core (docs/plans/core-design.md section 3). Sourced by bin/kith,
# which has set $KITH (the kernel folder).
#
#   kith_core_exec VERB [ARGS...]    execs `<kith program> kernel VERB ARGS...` and so never returns, when
#                                    - KITH_CORE is not `python`, and
#                                    - VERB is in lib/core-switched.txt (`verb<TAB>module`), or KITH_CORE=rust, and
#                                    - this Mac has a kith program that lists the module in `kith kernel version`.
#                                    Otherwise it returns 1 and the caller runs the Python, silently.
#
# lib/core-switched.txt is read here at run time, never baked into the program, so a git pull and an older program
# cannot disagree about what is switched. Removing a line is the rollback.
kith_core_exec() {
  local verb=$1 module bin
  shift
  [[ ${KITH_CORE:-} == python ]] && return 1
  module=$(awk -F'\t' -v v="$verb" '$1 == v { print $2; exit }' "$KITH/lib/core-switched.txt" 2>/dev/null)
  if [[ -z $module ]]; then
    [[ ${KITH_CORE:-} == rust ]] || return 1
    module=${verb%% *}
  fi
  bin=$(python3 "$KITH/lib/kith_paths.py" --bin 2>/dev/null) && [[ -n $bin ]] || return 1
  "$bin" kernel version 2>/dev/null | grep -q "\"$module\"" || return 1
  exec "$bin" kernel $verb "$@"
}

# kith_launch_json_flag ARGS...   succeeds when --launch-json stands among the leading flags of `kith AGENT ARGS` (the flag
# loop of bin/kith: a flag with a value skips it, `--`, a word or a task that starts with a dash ends the flags).
kith_launch_json_flag() {
  while [[ $# -gt 0 ]]; do
    case $1 in
      --launch-json) return 0 ;;
      --hold|--resume|--account|--backup|-p|--project|-m|--model|-e|--effort|-H|--harness|--permission-mode) shift 2 || return 1 ;;
      --print|-r|-c|--continue|--bypass) shift ;;
      *) return 1 ;;
    esac
  done
  return 1
}
