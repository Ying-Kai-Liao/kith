#!/bin/sh
# MANUAL check (not run by CI, not run by `cargo test`): the holder against a real launchd job (macOS). Children get
# killed by supervisors in ways `cargo test` cannot reproduce: launchd kills the whole job's process group, and the
# app's coalition, when the job exits. Run it by hand on a Mac you are happy to load two throw-away jobs on:
#
#   sh core/holder/tests/manual/survive-supervisor.sh [path/to/kith-holder]
#
# Needs a built kith-holder (CI builds it: `gh workflow run ci.yml ... -f mode=check -f packages="kith-holder"
# -f os=macos -f artifact=true`, then `gh run download <id> -n kith-macos -D <dir>`; or a local debug build).
# The binary can live anywhere; where it lives is itself something the check shows (a launchd job starts it).
#
# Part 1 (M0c): a job starts the holder DETACHED (`kith-holder start`) and exits; the holder must outlive the job.
# Part 2 (M1): the holder is the supervised root (`kith-holder supervise ... -- <core>`), as the opt-in service job
#   (KITH_SERVE_HOLDER=1) writes it. The stand-in core is a small script that records every start. Checked under the
#   throw-away label dev.kith.m1test.holder, with the holder's folder ON THE DISK THE REPO IS ON (a folder beside this
#   checkout, so it is the external disk when the checkout is):
#     a. the job's holder and core read and write that folder (macOS asks per binary for files on an external volume; a
#        job nobody has granted gets EPERM, and this reports the error);
#     b. kill the core: the holder keeps its pid and starts the core again;
#     c. kill the holder: launchd starts the job again, and the old core did not outlive it;
#     d. a folder on a disk that is not mounted: the job does not run, retries, and comes up when the disk is there.
# Both parts clean up their label, plist and folders even when a check fails. Prints one line per check and PASS or FAIL;
# a disk that is blocked prints "DISK: blocked ..." with the error and what grant fixes it, and fails part 2.
#
# On Windows the same is proven by CI's `windows` job for the detach (the survive test starts the holder from a process
# that then exits); the real supervisor (the per-user Run key starting `kith-holder.exe supervise ...`) needs the by-hand
# check described in docs/plans/runtime-design.md section 8: sign in, `kith serve install` with KITH_SERVE_HOLDER=1, end
# kith-servew.exe in Task Manager, and see it started again within the backoff, the holder's pid unchanged.
set -u

exe=${1:-serve/host/target/debug/kith-holder}
exe=$(cd "$(dirname "$exe")" && pwd)/$(basename "$exe")
root=$(cd "$(dirname "$0")/../../../.." && pwd)
uid=$(id -u)
secret=0123456789abcdef0123456789abcdef
label=dev.kith.m1test.holder
plist="$HOME/Library/LaunchAgents/$label.plist"
old_label=dev.kith.holder-survive-test
logs=$(mktemp -d "${TMPDIR:-/tmp}/khs-logs.XXXXXX") # the job's own output: on the boot disk, so it is readable whatever the disk says
data=$(mktemp -d "${M1_DATA_ROOT:-$root}/.m1test.XXXXXX") # the holder's folder: beside the checkout (M1_DATA_ROOT=/some/internal/dir for the control run)
failed=0

cleanup() {
  launchctl bootout "gui/$uid/$label" 2>/dev/null || true
  launchctl remove "$old_label" 2>/dev/null || true
  rm -f "$plist"
  # The holder's idle timer does not run in supervise mode; bootout ended the job and its process group.
  rm -rf "$data" "$logs"
}
trap cleanup EXIT
trap 'exit 1' INT TERM

ok() { echo "ok:   $*"; }
bad() { echo "FAIL: $*"; failed=1; }
wait_for() { # wait_for SECONDS COMMAND...: true when COMMAND succeeds within the time
  limit=$(($1 * 10)); shift; i=0
  while ! "$@" 2>/dev/null; do
    i=$((i + 1)); [ "$i" -lt "$limit" ] || return 1; sleep 0.1
  done
}
holder_pid() { pgrep -f "kith-holder supervise --dir $1/holder" | head -1; }
core_pid() { cat "$1/core.pid" 2>/dev/null; }
# differs OLD NEW-COMMAND...: true when the command's output is non-empty and not OLD (re-run on every try by wait_for)
differs() { old=$1; shift; new=$("$@"); [ -n "$new" ] && [ "$new" != "$old" ]; }
alive() { [ -n "${1:-}" ] && kill -0 "$1" 2>/dev/null; }

# ---- Part 1: the detached holder outlives the job that started it ----------------------------------------------------
d1=$(mktemp -d "${TMPDIR:-/tmp}/khs.XXXXXX")
launchctl submit -l "$old_label" -o "$d1/job.out" -e "$d1/job.err" -- \
  /bin/sh -c 'printf "%s\n" "$1" | "$2" start --dir "$3" --idle-secs 60' sh "$secret" "$exe" "$d1"
if wait_for 10 test -S "$d1/holder.sock"; then
  sleep 3 # launchd has reaped the job by now
  pid=$(pgrep -f "kith-holder serve --dir $d1" | head -1)
  if alive "$pid"; then ok "part 1: holder pid $pid outlived the launchd job that started it"; else bad "part 1: the holder did not survive its job"; fi
  kill "$pid" 2>/dev/null
else
  bad "part 1: no holder socket (see $(cat "$d1/job.err" 2>/dev/null))"
fi
launchctl remove "$old_label" 2>/dev/null
rm -rf "$d1"

# ---- Part 2: the holder as the supervised root -----------------------------------------------------------------------
# The stand-in core: records its pid and start, reads the folder (probe) and writes it, then waits.
core='echo $$ > "$1/core.pid" && echo started >> "$1/core.starts" && cat "$1/probe" > /dev/null && exec sleep 3600'

write_plist() { # write_plist FOLDER
  mkdir -p "$HOME/Library/LaunchAgents"
  cat > "$plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$label</string>
  <key>ProgramArguments</key><array>
    <string>$exe</string><string>supervise</string><string>--dir</string><string>$1/holder</string>
    <string>--secret-file</string><string>$1/secret</string><string>--backoff-ms</string><string>200:1000</string>
    <string>--</string><string>/bin/sh</string><string>-c</string><string>$core</string><string>sh</string><string>$1</string>
  </array>
  <key>EnvironmentVariables</key><dict><key>PATH</key><string>/usr/bin:/bin:/usr/sbin:/sbin</string></dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ThrottleInterval</key><integer>2</integer>
  <key>ProcessType</key><string>Interactive</string>
  <key>StandardOutPath</key><string>$logs/job.out</string>
  <key>StandardErrorPath</key><string>$logs/job.err</string>
</dict>
</plist>
EOF
}

# The probe and the secret are made by this script (the disk is readable to it); the JOB must read and write the folder.
printf '%s' "$secret" > "$data/secret"; chmod 600 "$data/secret"
echo probe > "$data/probe"
write_plist "$data"
launchctl bootout "gui/$uid/$label" 2>/dev/null
launchctl bootstrap "gui/$uid" "$plist" || { bad "launchctl bootstrap failed"; exit 1; }

# a. the job reads and writes the folder
if wait_for 20 test -S "$data/holder/holder.sock" && wait_for 5 test -s "$data/core.starts"; then
  ok "DISK: works: the launchd-started holder made its folder, socket and log under $data, and its core read and wrote it"
else
  echo "DISK: blocked or failed: the job did not come up on $data within 20 s. Its stderr:"
  sed 's/^/        /' "$logs/job.err" 2>/dev/null | tail -8
  echo "      Empty stderr and a holder process with 0 s of CPU and no open files means the job is BLOCKED in its first open of the"
  echo "      folder, with no error (seen on macOS 26: the access waits on a consent no one can give a background job)."
  echo "      'Operation not permitted' (EPERM) means the same refusal, answered. Either way macOS blocks $exe from this volume (TCC, 'Files and"
  echo "      Folders > Removable Volumes' / 'Full Disk Access'). The grant that fixes it: System Settings > Privacy & Security >"
  echo "      Full Disk Access, add THIS binary (grants are per binary; a rebuilt or moved binary needs it again), or run the job"
  echo "      behind a launcher that already has it (the 'launcher' setting of the service, hq-uv today). Not changed here."
  bad "part 2: the job cannot use the folder"
  exit 1
fi

h1=$(holder_pid "$data"); c1=$(core_pid "$data")
[ -n "$h1" ] && alive "$c1" && ok "the job runs: holder pid $h1, core pid $c1" || bad "holder or core not running (holder '$h1', core '$c1')"

# b. kill the core: the holder keeps its pid and the core comes back
kill -9 "$c1" 2>/dev/null
if wait_for 20 differs "$c1" core_pid "$data" && alive "$(core_pid "$data")" && [ "$(holder_pid "$data")" = "$h1" ]; then
  ok "killed the core ($c1): the holder kept pid $h1 and started the core again as $(core_pid "$data")"
else
  bad "after killing the core: holder '$(holder_pid "$data")' (was $h1), core '$(core_pid "$data")' (was $c1)"
fi

# c. kill the holder: launchd starts the job again; the old core must not linger
c2=$(core_pid "$data")
kill -9 "$h1" 2>/dev/null
if wait_for 30 differs "$h1" holder_pid "$data" && wait_for 10 differs "$c2" core_pid "$data"; then
  h2=$(holder_pid "$data")
  sleep 1
  if alive "$c2"; then bad "the old core $c2 outlived its killed holder"; else ok "killed the holder ($h1): launchd started the job again (holder $h2); the old core $c2 did not outlive it"; fi
else
  bad "launchd did not bring the job back after the holder was killed"
fi

# d. a folder on a disk that is not mounted
launchctl bootout "gui/$uid/$label" 2>/dev/null
wait_for 10 sh -c "! pgrep -f \"kith-holder supervise --dir $data/holder\" >/dev/null"
gone="/Volumes/m1test-not-mounted-$$"
write_plist "$gone"
: > "$logs/job.err"
launchctl bootstrap "gui/$uid" "$plist"
sleep 6
if [ -z "$(holder_pid "$gone")" ] && [ -s "$logs/job.err" ]; then
  ok "disk not mounted: the job does not run, and says so: $(head -1 "$logs/job.err")"
else
  bad "disk not mounted: expected no holder and a message, got holder '$(holder_pid "$gone")'"
fi
# launchd keeps retrying (KeepAlive): a job that comes up once the folder exists is what a login before the disk mounts needs.
mkdir -p "$gone" 2>/dev/null && { printf '%s' "$secret" > "$gone/secret"; chmod 600 "$gone/secret"; echo probe > "$gone/probe"; }
if [ -d "$gone" ]; then
  if wait_for 30 differs "" holder_pid "$gone"; then ok "the job came up once the folder existed (launchd retried)"; else bad "the job did not come up after the folder appeared"; fi
  launchctl bootout "gui/$uid/$label" 2>/dev/null
  rm -rf "$gone"
else
  echo "note: /Volumes is not writable here, so the retry-after-mount step is not exercised"
fi

if [ "$failed" = 0 ]; then echo PASS; else echo FAIL; exit 1; fi
