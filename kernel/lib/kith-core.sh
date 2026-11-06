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
# kith_cutover_refuse MODULE   exits 1 with one line when MODULE has been switched to the core on this Mac (or is part-way
# through a rollback), so KITH_CORE=python cannot hand it back to the Python. The state is one file per module,
# <machine>/core/cutover/MODULE.switch (core/runtime/src/cutover/state.rs); a missing file means unswitched. Returns
# quietly otherwise.
kith_cutover_refuse() {
  local module=$1 machine f
  [[ $module =~ ^[a-z][a-z0-9-]*$ ]] || return 0
  machine=${KITH_MACHINE:-$(python3 "$KITH/lib/kith_paths.py" --machine 2>/dev/null)}
  f=$machine/core/cutover/$module.switch
  [[ -n $machine && -e $f ]] || return 0
  # A file that says anything but stepping (or that cannot be read) counts as switched.
  if ! grep -qx 'state=stepping' "$f" 2>/dev/null; then
    echo "kith: $module has moved to the core on this Mac, so KITH_CORE=python is refused (kith rollback $module goes back)" >&2
    exit 1
  fi
  return 0
}

# kith_core_prime   looks up the program and its module list once and keeps them in shell variables, so the launch's
# several kith_core_exec calls (pkg, project-id x3) do not each spawn `kith_paths.py --bin` and `kith kernel version`.
# Subshells inherit the variables; nothing is exported. The decision is the same as without it, made earlier.
kith_core_prime() {
  _kith_core_bin=$(python3 "$KITH/lib/kith_paths.py" --bin 2>/dev/null) || _kith_core_bin=""
  _kith_core_ver=""
  [[ -z $_kith_core_bin ]] || _kith_core_ver=$("$_kith_core_bin" kernel version 2>/dev/null) || _kith_core_ver=""
  _kith_core_primed=1
}

kith_core_exec() {
  local verb=$1 module bin ver
  shift
  module=$(awk -F'\t' -v v="$verb" '$1 == v { print $2; exit }' "$KITH/lib/core-switched.txt" 2>/dev/null)
  if [[ ${KITH_CORE:-} == python ]]; then
    # After a module's cutover (kith migrate step) Python must not write its files again: refuse, never fall back.
    kith_cutover_refuse "${module:-${verb%% *}}"
    return 1
  fi
  if [[ -z $module ]]; then
    [[ ${KITH_CORE:-} == rust ]] || return 1
    module=${verb%% *}
  fi
  if [[ -n ${_kith_core_primed:-} ]]; then
    bin=$_kith_core_bin ver=$_kith_core_ver
  else
    bin=$(python3 "$KITH/lib/kith_paths.py" --bin 2>/dev/null) && [[ -n $bin ]] || return 1
    ver=$("$bin" kernel version 2>/dev/null)
  fi
  [[ -n $bin && $ver == *"\"$module\""* ]] || return 1
  # KITH_BIN names the program that was checked, so the core launch (which takes KITH_BIN as it is, once it answers
  # `memory hook`) and the router agree on one binary.
  KITH_BIN=$bin exec "$bin" kernel $verb "$@"
}

# kith_cost_ported ARGS...   succeeds when `kith-cost ARGS` is a line the Rust core answers (core/kernel/src/cost/mod.rs `run`):
# limit AGENT, sync, spent PERSON, usage --json, agent NAME --json, members AGENT, bots --json. Every other line (tag and record,
# which write the ledger, the text reports, a bare `kith cost`, -h) is Python's, so `cost` in core-switched.txt moves exactly these.
kith_cost_ported() {
  local a json=0
  for a in "$@"; do [[ $a == --json ]] && json=1; done
  case ${1:-} in
    limit|spent|members) [[ $# -ge 2 ]] ;;
    sync) return 0 ;;
    usage|bots) [[ $json == 1 ]] ;;
    agent) [[ $# -ge 2 && $json == 1 ]] ;;
    *) return 1 ;;
  esac
}

# kith_cost ARGS...   `kith-cost ARGS`: the core's when `cost` is switched and the line is one it answers, else the Python.
# A subshell, because kith_core_exec execs; `KITH_PERSON_WAS=.. kith_cost ..` reaches whichever answers.
kith_cost() {
  ( kith_cost_ported "$@" && kith_core_exec cost --kernel "$KITH" "$@"; exec "$KITH/bin/kith-cost" "$@" )
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

# Held terminals (docs/plans/runtime-design.md section 4, M1-7): `kith AGENT --held` asks the core daemon to start the engine in
# the holder (terminal.open) and attaches to it; `kith attach` comes back to one. The calls go through the hidden Rust verb
# `kith kernel core-call` (owner socket only; terminal.* methods, and the people.suspend, people.undo, people.removePerson and people.removeBot that kith-auth mirrors); the keys and the screen through `kith kernel attach`.
# Exit codes of core-call: 0 answered (the people.* writes run in an embedded core when no daemon answers; the answer may be {"noCore":true}), 3 nothing ran (no daemon, or the core could not be opened), 4 refused, 5 lost after the call went out.

# kith_held_get KEY   prints a field of the JSON on stdin (a string as it is, anything else as JSON), empty when absent.
kith_held_get() {
  python3 -I -c 'import json, sys
try: v = json.load(sys.stdin).get(sys.argv[1])
except ValueError: v = None
print("" if v is None else (v if isinstance(v, str) else json.dumps(v)))' "$1"
}

# kith_held_call METHOD   the params as JSON on stdin; the answer on stdout; core-call's exit code.
kith_held_call() {
  local bin err rc=0
  bin=$(python3 "$KITH/lib/kith_paths.py" --bin 2>/dev/null) && [[ -n $bin ]] || return 3
  err=$(mktemp "${TMPDIR:-/tmp}/kith-held.XXXXXX") || return 3
  "$bin" kernel core-call "$1" --params-stdin 2>"$err" || rc=$?
  # A refusal says why; "no daemon" and a program without the verb are the caller's one line.
  if [[ $rc -eq 4 || $rc -eq 5 ]]; then sed 's/^/kith: /' "$err" >&2; fi
  rm -f "$err"
  return "$rc"
}

# kith_held_client TICKET [--observer] [--takeover]   attaches once; sets held_rc to the client's exit code. Its notes (written
# after raw mode is off) are held back until it ends so they can be read: held_note says what ended it. Exit codes: the
# engine's, 75 busy, 0 detach, 76 cut as too slow, 77 taken over, 78 holder lost, 1 any other end.
kith_held_client() {
  local ticket=$1 bin err
  shift
  held_rc=1 held_note=""
  bin=$(python3 "$KITH/lib/kith_paths.py" --bin 2>/dev/null) && [[ -n $bin ]] || { echo "kith: no kith program to attach with" >&2; return 0; }
  err=$(mktemp "${TMPDIR:-/tmp}/kith-held.XXXXXX") || return 0
  held_rc=0
  printf '%s\n' "$ticket" | "$bin" kernel attach --endpoint "$KITH_MACHINE/core/attach.sock" --ticket-stdin --tty "$@" 2>"$err" || held_rc=$?
  held_note=$(cat "$err")
  rm -f "$err"
  [[ -z $held_note ]] || printf '%s\n' "$held_note" >&2
}

# kith_held_attach SESSION TICKET [--observer] [--takeover]   attaches (with TICKET when given, else a new one from
# terminal.attach); a client cut `slow` is attached once more with a new ticket. Returns the client's exit code.
kith_held_attach() {
  local sid=$1 ticket=$2 role=owner ans rc status again=0
  shift 2
  [[ " $* " != *" --observer "* ]] || role=observer
  while :; do
    if [[ -z $ticket ]]; then
      rc=0
      ans=$(python3 -I -c 'import json, sys; print(json.dumps({"session": sys.argv[1], "role": sys.argv[2]}))' "$sid" "$role" | kith_held_call terminal.attach) || rc=$?
      if [[ $rc -ne 0 ]]; then echo "kith: the core is not answering, so there is nothing to attach to" >&2; return 1; fi
      status=$(printf '%s' "$ans" | kith_held_get status)
      if [[ $status != open ]]; then echo "kith: nothing held for $sid (it ended, or was never held here)" >&2; return 1; fi
      ticket=$(printf '%s' "$ans" | kith_held_get ticket)
    fi
    kith_held_client "$ticket" "$@"
    ticket=""
    # 76: the relay cut this client as too slow (the client's own code for it; 77 taken over, 78 holder lost).
    if [[ $held_rc -eq 76 && $again -eq 0 ]]; then again=1; continue; fi
    if [[ $held_note == *owner_present* ]]; then echo "kith: another window is attached to $sid and types in it; kith attach --takeover $sid takes over, --observer only watches" >&2; fi
    return "$held_rc"
  done
}

# kith attach [--observer] [--takeover] [AGENT|SESSION]   no word: the held terminals; a session id (or the start of one), else an
# agent's newest held terminal: attach to it.
kith_held_attach_cmd() {
  local flags=() target="" rc=0 ans sid
  while [[ $# -gt 0 ]]; do
    case $1 in
      --observer|--takeover) flags+=("$1"); shift ;;
      -h|--help) echo "usage: kith attach [--observer] [--takeover] [AGENT|SESSION]   no word lists the held terminals (kith AGENT --held starts one)"; return 0 ;;
      -*) echo "kith: unknown flag '$1' (kith attach --help)" >&2; return 1 ;;
      *) [[ -z $target ]] || { echo "kith: attach takes one agent or session" >&2; return 1; }; target=$1; shift ;;
    esac
  done
  ans=$(echo '{}' | kith_held_call terminal.list) || rc=$?
  if [[ $rc -ne 0 ]]; then echo "kith: no core daemon answers, so no terminal is held (kith serve start)" >&2; return 1; fi
  if [[ -z $target ]]; then
    printf '%s' "$ans" | python3 -I -c '
import datetime, json, sys
ts = json.load(sys.stdin).get("terminals", [])
if not ts: print("kith: no terminal is held"); sys.exit(0)
print("%-16s %-38s %-12s %s" % ("AGENT", "SESSION", "STARTED", "ATTACHED"))
for t in ts:
    when = datetime.datetime.fromtimestamp(t["started"]).strftime("%m-%d %H:%M") if t.get("started") else "-"
    print("%-16s %-38s %-12s %d owner, %d observer" % (t.get("agent") or "-", t["session"], when, t.get("owners", 0), t.get("observers", 0)))'
    return 0
  fi
  rc=0
  sid=$(printf '%s' "$ans" | python3 -I -c '
import json, sys
ts, w = json.load(sys.stdin).get("terminals", []), sys.argv[1]
hit = [t for t in ts if t["session"] == w]
if not hit: hit = sorted((t for t in ts if t.get("agent") == w), key=lambda t: (t.get("started", 0), (t.get("held_id") or {}).get("id", 0)))[-1:]
if not hit and len(w) >= 4: hit = [t for t in ts if t["session"].startswith(w)]
if len({t["session"] for t in hit}) > 1: sys.exit(3)
print(hit[0]["session"] if hit else "")' "$target") || rc=$?
  if [[ $rc -ne 0 ]]; then echo "kith: \"$target\" starts more than one held session; give more of the id" >&2; return 1; fi
  [[ -n $sid ]] || { echo "kith: nothing held for \"$target\" (kith attach lists what is)" >&2; return 1; }
  rc=0
  kith_held_attach "$sid" "" ${flags[@]+"${flags[@]}"} || rc=$?
  return "$rc"
}

# kith_held_launch AGENT SESSION RESUMED_SESSION CLAUDE_ARGS...   the interactive launch of `kith AGENT --held`: the same assembled
# claude line as the direct path, handed to terminal.open instead of exec'd here. Ends the shell (exit) when the terminal was
# opened, the session is busy, or the call may have run; returns 1 for "run it here (--direct)", with one line saying why.
#
# The writer claim. A --resume took the session's claim for this shell in `resolve`; terminal.open takes it itself, so it is given
# back first, and only when a holder is up (terminal.list says link up): with no daemon or no holder, the common fallback, the
# claim is never let go. If open then answers direct anyway (no stub, a remote holder), or the call is refused, the claim is taken
# again before running here. What is left is the gap between the give-back and open's own take: a third writer taking the claim
# there makes open answer busy (75), never a second engine. An atomic hand-over inside terminal.open is the proper fix (Later).
kith_held_launch() {
  local agent=$1 sid=$2 rsid=$3 params ans rc=0 status ticket note why="" released=0
  shift 3
  if [[ ! $sid =~ ^[a-z0-9._-]+$ ]]; then why="this conversation has no id the lock can use"
  else
    if [[ -n $rsid ]]; then
      ans=$(echo '{}' | kith_held_call terminal.list) || rc=$?
      if [[ $rc -ne 0 || $(printf '%s' "$ans" | kith_held_get link) != up ]]; then why="holder not reachable"
      elif python3 "$KITH/bin/kith-sessions" release "$rsid"; then released=1
      else why="the session's claim is not this shell's to hand over"
      fi
      rc=0
      # Tests only: something that happens in the gap between the give-back and open (a third writer).
      [[ -z ${KITH_HELD_GAP_HOOK:-} || $released -eq 0 ]] || eval "$KITH_HELD_GAP_HOOK"
    fi
    if [[ -z $why ]]; then
      params=$(python3 -I -c '
import json, os, sys
# The window and the run: nothing that names this terminal window or a run token goes to a terminal that outlives both
# (the core drops the token names too; kith_env_sync mirrors KITH_ names as HQ_ ones, so both spellings are listed here).
drop = {"TERM_SESSION_ID", "ITERM_SESSION_ID", "TMUX", "TMUX_PANE", "SSH_TTY", "WINDOWID", "COLUMNS", "LINES", "SHLVL", "_"}
for p in ("KITH_", "HQ_"):
    drop |= {p + n for n in ("RUN_TOKEN", "RUN_SOCKET", "ASK_TOKEN", "ASK_SOCKET")}
env = {k: v for k, v in os.environ.items() if k not in drop and not k.startswith(("KITH_RUN_", "HQ_RUN_", "KITH_CALL_", "HQ_CALL_"))}
size = None
for fd in (2, 0, 1):
    try: size = os.get_terminal_size(fd); break
    except OSError: pass
if size is None:
    try:
        fd = os.open("/dev/tty", os.O_RDONLY); size = os.get_terminal_size(fd); os.close(fd)
    except OSError: pass
p = {"agent": sys.argv[1], "session": sys.argv[2], "argv": ["claude"] + sys.argv[3:], "cwd": os.getcwd(), "env": env}
if size: p["cols"], p["rows"] = size.columns, size.lines
print(json.dumps(p))' "$agent" "$sid" "$@")
      ans=$(printf '%s' "$params" | kith_held_call terminal.open) || rc=$?
      case $rc in
        0) ;;
        5) echo "kith: the call may have started it; kith attach $sid joins it if it is held" >&2; exit 1 ;;   # not repeated
        4) why="the core refused it" ;;   # nothing ran; the refusal was printed
        *) why="holder not reachable" ;;
      esac
    fi
    if [[ -z $why ]]; then
      status=$(printf '%s' "$ans" | kith_held_get status)
      note=$(printf '%s' "$ans" | kith_held_get note)
      case $status in
        busy) echo "kith: that conversation is already open in another writer${note:+ ($note)}; kith attach $sid joins it if it is held" >&2; exit 75 ;;
        open)
          ticket=$(printf '%s' "$ans" | kith_held_get ticket)
          if [[ ! -t 0 || ! -t 1 ]]; then
            # No terminal on this side: the engine is held and runs with nobody attached (no raw mode, nothing read).
            echo "kith: $agent is held as session $sid; attach with: kith attach $sid"
            exit 0
          fi
          rc=0
          kith_held_attach "$sid" "$ticket" || rc=$?
          # The way back, unless the engine itself ended: a detach, or the attach failing while the terminal runs on.
          if [[ $held_note == *"keeps running"* || ( $rc -ne 0 && $rc -ne 75 && $rc -lt 79 && -n $held_note ) ]]; then
            echo "kith: reattach with: kith attach $sid" >&2
          fi
          exit "$rc" ;;
        *) why=${note%%: use --direct*}; why=${why:-holder not reachable} ;;
      esac
    fi
  fi
  if [[ -t 0 && -t 1 ]]; then echo "kith: ${why}; running here (--direct)" >&2
  else echo "kith: ${why}; an interactive session needs a terminal to run here" >&2; fi
  [[ $released -eq 0 ]] || python3 "$KITH/bin/kith-sessions" terminal "$rsid" "$$" || exit 1
  return 1
}
