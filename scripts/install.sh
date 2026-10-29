#!/bin/sh
# Installs kith from a GitHub Release, no git or Rust needed:
#
#   curl -fsSL https://raw.githubusercontent.com/Ying-Kai-Liao/kith/main/scripts/install.sh | sh
#   curl -fsSL .../install.sh | sh -s -- --from <your-github-user>     a new Mac: brings your state and agents back
#
# Puts bin/kith and share/kith/ (the kernel, the Claude Code plugin) under ~/.kith, then runs the packaged
# install.sh: ~/kith, the kith and hq (its old name) commands in ~/.local/bin, the plugin, `kith init`. Run it again to upgrade; your
# agents (~/kith/agents) and state (~/.kith, or ~/.hq until kith migrate) are never touched. Arguments go to `kith init`.
#
# Environment:
#   KITH_VERSION          a release to install (1.2.3 or v1.2.3); default the latest
#   KITH_PREFIX           where to install; default ~/.kith
#   KITH_INSTALL_YES      never reconnect setup to the terminal (unattended; its offers are skipped)
#   KITH_REPO             GitHub repo to download from; default Ying-Kai-Liao/kith
#   KITH_INSTALL_TARBALL  a local kith-<version>-darwin-<arch>.tar.gz to install instead of downloading (tests,
#                         offline); verified against KITH_INSTALL_SHA256 or the .sha256 file beside it, if there is one
set -eu

REPO=${KITH_REPO:-Ying-Kai-Liao/kith}
PREFIX=${KITH_PREFIX:-$HOME/.kith}

say() { printf '%s\n' "$*"; }
die() { printf 'install: %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

[ "$(uname -s)" = Darwin ] || die "kith's download is for macOS. A Linux VM that only needs the client: curl -fsSL https://raw.githubusercontent.com/$REPO/main/scripts/install-guest.sh | sh. The full kith on Linux: clone https://github.com/$REPO and run ./install.sh"
case $(uname -m) in
  arm64|aarch64) ARCH=arm64 ;;
  x86_64) ARCH=x86_64 ;;
  *) die "unsupported CPU: $(uname -m)" ;;
esac
have curl || die "curl is needed"
have tar || die "tar is needed"

sha256_of() { shasum -a 256 "$1" | cut -d' ' -f1; }

WORK=$(mktemp -d "${TMPDIR:-/tmp}/kith-install.XXXXXX")
trap 'rm -rf "$WORK"' EXIT

if [ -n "${KITH_INSTALL_TARBALL:-}" ]; then
  TARBALL=$KITH_INSTALL_TARBALL
  [ -f "$TARBALL" ] || die "$TARBALL not found"
  WANT=${KITH_INSTALL_SHA256:-}
  [ -n "$WANT" ] || { [ -f "$TARBALL.sha256" ] && WANT=$(cut -d' ' -f1 < "$TARBALL.sha256"); } || true
else
  VERSION=${KITH_VERSION:-}
  if [ -z "$VERSION" ]; then
    # /releases/latest redirects to /releases/tag/vX.Y.Z.
    url=$(curl -fsSL -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest") || die "could not look up the latest release of $REPO"
    VERSION=${url##*/}
    case $VERSION in v[0-9]*) ;; *) die "$REPO has no release yet" ;; esac
  fi
  VERSION=${VERSION#v}
  ASSET=kith-$VERSION-darwin-$ARCH.tar.gz
  BASE=https://github.com/$REPO/releases/download/v$VERSION
  say "downloading kith $VERSION ($ARCH)..."
  TARBALL=$WORK/$ASSET
  curl -fSL --progress-bar -o "$TARBALL" "$BASE/$ASSET" || die "download failed: $BASE/$ASSET"
  curl -fsSL -o "$WORK/SHA256SUMS" "$BASE/SHA256SUMS" || die "download failed: $BASE/SHA256SUMS"
  WANT=$(awk -v f="$ASSET" '$2 == f || $2 == "*" f { print $1 }' "$WORK/SHA256SUMS")
  [ -n "$WANT" ] || die "no checksum for $ASSET in SHA256SUMS"
fi

if [ -n "${WANT:-}" ]; then
  [ "$(sha256_of "$TARBALL")" = "$WANT" ] || die "checksum mismatch for $TARBALL; not installing"
  say "checksum ok"
fi

mkdir -p "$WORK/x"
tar -C "$WORK/x" -xzf "$TARBALL"
SRC=$(echo "$WORK"/x/kith-*)
[ -x "$SRC/bin/kith" ] && [ -d "$SRC/share/kith/kernel" ] || die "$TARBALL is not a kith release"

# Swap in what a release owns; anything else under share/kith (agents/, state of an older layout) stays.
# A new file is moved into place by rename, so a running kith is never half-written.
mkdir -p "$PREFIX/bin" "$PREFIX/share/kith"
cat > "$WORK/swap.sh" <<'SWAP'
set -eu
cp "$SRC/bin/kith" "$PREFIX/bin/.kith.new.$$" && chmod 755 "$PREFIX/bin/.kith.new.$$" && mv -f "$PREFIX/bin/.kith.new.$$" "$PREFIX/bin/kith"
for part in kernel .claude-plugin install.sh LICENSE NOTICE THIRD_PARTY_NOTICES.md README.md; do
  # Tarballs from before NOTICE and THIRD_PARTY_NOTICES.md shipped lack them.
  [ -e "$SRC/share/kith/$part" ] || continue
  rm -rf "$PREFIX/share/kith/$part"
  cp -R "$SRC/share/kith/$part" "$PREFIX/share/kith/$part"
done
SWAP

# The swap runs under the auth.json lock, then waits for every kith-auth / kith_settings process that started
# before it (SIGTERM after KITH_INSTALL_DRAIN_S, default 30; auth.json stays whole, its rename is atomic), then lets
# go. Waiting before the swap would be wrong: an old writer could start in the gap. The old code has no C2 check
# (no grant for someone removed), so no old writer may touch auth.json or settings.json once the new code is in.
cat > "$WORK/quiesce.py" <<'QUIESCE'
import fcntl, glob, os, plistlib, re, signal, subprocess, sys, time
locks, limit, cmd = sys.argv[1:3], float(sys.argv[3]), sys.argv[4:]
fds = []
for lock in locks:                                  # 1. both locks, always auth then settings (kith_settings writes take the second)
    os.makedirs(os.path.dirname(lock), exist_ok=True)
    fds.append(open(lock, 'a'))
    fcntl.flock(fds[-1], fcntl.LOCK_EX)
if subprocess.call(cmd) != 0:                       # 2. the swap
    sys.exit(1)
t_swap = time.time()
mine = {os.getpid()}
pid = os.getppid()                                  # our own chain is not an old writer
while pid > 1:
    mine.add(pid)
    out = subprocess.run(['ps', '-o', 'ppid=', '-p', str(pid)], capture_output=True, text=True).stdout.strip()
    pid = int(out) if out.isdigit() else 0
WRITER = re.compile(os.environ.get('KITH_QUIESCE_MATCH') or r'(^|[/\s])(kith-auth|kith_settings(\.py)?)(\s|$)')   # the env is for tests
# A long-lived process that imported the old kith_settings ignores the lock and can write until it restarts, and its
# command line need not name kith-auth or kith_settings (the bridge, the host's launcher). So also: a process with no
# terminal whose program is a file of the installed kernel. Only the program counts (argv[0], or the script an
# interpreter or launcher runs), never a path further along: an agent run holds kernel paths in its system prompt
# argument, and killing one mid-turn after 30s is wrong.
ROOTS = [r.rstrip('/') + '/' for r in os.environ.get('KITH_QUIESCE_ROOTS', '').split(os.pathsep) if r]
INTERPRETERS = re.compile(r'^(?:.*?/)?(?:python[0-9.]*|sh|bash|zsh|env|node|uv|kith-uv|hq-uv)\s+(.*)$', re.I)   # a framework python shows as .../Python, and paths have spaces


def seconds(etime):                                 # [[dd-]hh:]mm:ss
    days, _, rest = etime.rpartition('-')
    parts = [int(x) for x in rest.split(':')]
    while len(parts) < 3:
        parts.insert(0, 0)
    return int(days or 0) * 86400 + parts[0] * 3600 + parts[1] * 60 + parts[2]


def started_by_swap(etime):                         # etime has whole seconds, so allow one second of slack: a new writer that
    try:                                            # starts within ~1s after the swap counts as old, is waited for up to the
        return time.time() - seconds(etime) <= t_swap + 1   # limit and then SIGTERMed. That is the safe side; keep it.
    except ValueError:
        return False


def runs_kernel_file(command):
    starts = [command]
    m = INTERPRETERS.match(command)
    if m:
        words = m.group(1).split()
        for i in range(min(len(words), 7)):         # the first words that are not options: `kith-uv run --script <file>`
            if words[i] in ('-c', '-e'):
                break
            if not words[i].startswith('-'):
                starts.append(' '.join(words[i:]))
    return any(c.startswith(r) for c in starts for r in ROOTS)


def process_table():                                # [(pid, (pid, start), etime, tty, command)], -ww: never cut a command line
    out = subprocess.run(['ps', '-axww', '-o', 'pid=,lstart=,etime=,tty=,command='], capture_output=True, text=True).stdout
    rows = []
    for line in out.splitlines():
        f = line.split(None, 8)                     # pid, lstart (5 words), etime, tty, command
        if len(f) == 9 and f[0].isdigit():
            rows.append((int(f[0]), (int(f[0]), ' '.join(f[1:6])), f[6], f[7], f[8]))
    return rows


def old_writers():                                  # keyed by (pid, start time), so a reused pid is never taken for an old one
    found = set()
    for pid, key, etime, tty, command in process_table():
        if pid in mine:
            continue
        if (WRITER.search(command) or (tty.startswith('?') and runs_kernel_file(command))) and started_by_swap(etime):
            found.add(key)
    return found


def launchctl(*args, wait=True):
    try:
        if not wait:
            return subprocess.Popen(['launchctl', *args], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        return subprocess.run(['launchctl', *args], capture_output=True, text=True)
    except OSError:
        return None


def ps_field(field, pid):
    return subprocess.run(['ps', '-o', field + '=', '-p', str(pid)], capture_output=True, text=True).stdout.strip()


def keepalive_jobs():                               # loaded KeepAlive jobs of ours: [(label, pid)]
    jobs = []
    for plist in sorted(glob.glob(os.path.expanduser('~/Library/LaunchAgents/dev.*.plist'))):
        try:
            with open(plist, 'rb') as fh:
                data = plistlib.load(fh)
        except Exception:
            continue
        label = data.get('Label')
        if not label or not label.startswith(('dev.kith.', 'dev.hq.')) or not data.get('KeepAlive'):
            continue
        r = launchctl('print', 'gui/%d/%s' % (os.getuid(), label))
        m = re.search(r'^\s*pid = (\d+)', r.stdout, re.M) if r is not None and r.returncode == 0 else None
        if m:                                       # not loaded, or loaded but not running: nothing old to stop
            jobs.append((label, int(m.group(1))))
    return jobs


deadline = time.time() + limit                      # one deadline for the whole window, restarts and drain together
snap = old_writers()                                # 3. who is old, before anything restarts and starts new ones
mypgids = {ps_field('pgid', p) for p in mine}       # launchd kills a job's whole process group, so ours is off limits too
restarts = []
for label, jpid in keepalive_jobs():                # 4. KeepAlive jobs onto the new code
    target = 'gui/%d/%s' % (os.getuid(), label)
    if jpid in mine or ps_field('pgid', jpid) in mypgids:    # we run inside this job (kith update from the host)
        # -k now would kill the installer. A detached waiter (own session) restarts the job once the installer is done.
        print('note: %s runs this installer; it is restarted when the install ends' % label, file=sys.stderr)
        subprocess.Popen(['sh', '-c', 'while kill -0 "$0" 2>/dev/null; do sleep 1; done; launchctl kickstart -k "$1"',
                          str(os.getppid()), target], start_new_session=True, stdin=subprocess.DEVNULL,
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        continue
    etime = ps_field('etime', jpid)
    if etime and started_by_swap(etime):
        snap.add((jpid, ps_field('lstart', jpid)))
        # no wait for ready: the new process may block on our locks. kickstart -k may wait for the old one to exit, so
        # all of them are fired at once and share the deadline.
        restarts.append(launchctl('kickstart', '-k', target, wait=False))


def waiting():
    return [k for k in old_writers() if k in snap]


while (waiting() or any(r is not None and r.poll() is None for r in restarts)) and time.time() < deadline:   # 5. wait for them
    time.sleep(0.2)
for r in restarts:
    if r is not None and r.poll() is None:
        r.kill()
for pid, _ in waiting():                            # still the same process: waiting() checks the start time again
    try:
        os.kill(pid, signal.SIGTERM)
    except OSError:
        pass
for fd in fds:                                      # 4. let go
    fd.close()
QUIESCE
export SRC PREFIX
# Where this Mac keeps auth.json and settings.json is the installed kernel's own kith_paths to say (a Mac whose
# ~/.kith is not its folder keeps both under ~/.config/kith); a first install has no kernel yet: the defaults.
lock_paths() {
  AUTH_LOCK=$PREFIX/secrets/auth.json.lock SETTINGS_LOCK=$PREFIX/settings.json.lock
  lib=$PREFIX/share/kith/kernel/lib
  if have python3 && [ -f "$lib/kith_paths.py" ]; then
    found=$(python3 -c 'import sys; sys.path.insert(0, sys.argv[1]); from kith_paths import paths; p = paths(); print(p["KITH_SECRETS"] / "auth.json"); print(p["KITH_MACHINE"] / "settings.json")' "$lib" 2>/dev/null) || found=
    if [ -n "$found" ]; then
      AUTH_LOCK=$(printf '%s\n' "$found" | sed -n 1p).lock
      SETTINGS_LOCK=$(printf '%s\n' "$found" | sed -n 2p).lock
    fi
  fi
  [ -z "${KITH_AUTH:-}" ] || AUTH_LOCK=$KITH_AUTH.lock
  [ -z "${KITH_SETTINGS_LOCK:-}" ] || SETTINGS_LOCK=$KITH_SETTINGS_LOCK
}
lock_paths
# Where the installed kernel is addressed from: its own path and the links that lead to it (plists use ~/kith/kernel).
KITH_QUIESCE_ROOTS=
if [ -d "$PREFIX/share/kith/kernel" ]; then
  real=$(cd "$PREFIX/share/kith/kernel" && pwd -P)
  for c in "$PREFIX/share/kith/kernel" "$real" "$HOME/kith/kernel" "$HOME/hq/kernel" "$HOME/.kith/kernel" "${KITH_HOME:+$KITH_HOME/kernel}"; do
    [ -n "$c" ] && [ -d "$c" ] && [ "$(cd "$c" && pwd -P)" = "$real" ] && KITH_QUIESCE_ROOTS=${KITH_QUIESCE_ROOTS:+$KITH_QUIESCE_ROOTS:}$c
  done
fi
export KITH_QUIESCE_ROOTS
if have python3 && [ -d "$(dirname "$AUTH_LOCK")" ]; then
  python3 "$WORK/quiesce.py" "$AUTH_LOCK" "$SETTINGS_LOCK" "${KITH_INSTALL_DRAIN_S:-30}" sh "$WORK/swap.sh" || die "could not install kith to $PREFIX"
else
  sh "$WORK/swap.sh" || die "could not install kith to $PREFIX"   # a first install: nothing yet to lock
fi
say "installed kith to $PREFIX"

# The rest (~/kith, the commands, the plugin, kith init) is the repo's install.sh, packaged: one place for it.
rm -rf "$WORK"
# `curl | sh` leaves stdin on the script; give setup the terminal back so it can ask its questions.
if [ ! -t 0 ] && [ -z "${KITH_INSTALL_YES:-}" ] && { : </dev/tty; } 2>/dev/null; then exec </dev/tty; fi
exec "$PREFIX/share/kith/install.sh" "$@"
