#!/usr/bin/env python3
"""Where Kith's things are: the one place every tool asks.

  kith_paths.py          the folders, one per line
  kith_paths.py --sh     export lines for bash: eval "$(python3 lib/kith_paths.py --sh)"
  kith_paths.py --json   the same as JSON (the Claude Code adapter)
  kith_paths.py --bin    the kith program (Rust), or nothing when this Mac has none
  kith_paths.py --guest-sandbox   exit 0 inside the sandbox of a run for anyone but the owner, else 1
  kith_paths.py --guest-tmp ID    makes that guest's Claude Code temp folder and prints it (nothing: no short one fits)
Two places, like ~/.claude for Claude Code:

  ~/kith  KITH_HOME, the Kith folder (a link to it, so it can live anywhere, an external disk included): kernel/
          (this repo) and agents/ (one folder per agent: role/ is its repo, memory/ data/ files/ its life).
          ~/hq is the old address and stays a link to the same folder
  ~/.kith this Mac's Kith, a real folder on the internal disk: what Kith has lived (user.md, projects/,
          projects.json, agents.json, private-words: the hq-state repo, ~/.kith itself) and what only this machine
          needs, never committed: secrets/ (auth.json, env), settings.json (kith_settings.py), job state, the cost
          ledger, store/ (role snapshots), logs/, cache/. ~/.hq is the old address: a real folder until
          `kith migrate` moves it, then a link to ~/.kith

Which one is this Mac's, since ~/.kith can exist long before the state moves (kith serve keeps serve/ in it):

  KITH_STATE (HQ_STATE) set        that folder
  ~/.kith/.migrated exists         ~/.kith: the marker `kith migrate` and every fresh install write
  ~/.hq is a real folder           ~/.hq: not migrated yet
  ~/.hq links to ~/.kith           ~/.kith
  ~/.kith exists (no ~/.hq)         ~/.kith  (a Mac that never had ~/.hq, kith serve's serve/ in it, or a fresh install)
  nothing yet                      state in <home>/state until install.sh makes ~/.kith

KITH_HOME (HQ_HOME) set: that folder instead of ~/kith, else ~/kith when it exists, else ~/hq. Without either link:
the Kith folder around the kernel when it sits there as kernel/, else the kernel folder. Before ~/.hq became a real
folder it was a link to the Kith folder; then the state is <home>/state, machine things are in ~/.config/kith,
logs in ~/Library/Logs/kith, caches in ~/Library/Caches/kith and snapshots in <agents>/.store (the hq-named
folders, where they exist, still win).

Paths come in two spellings, each for one job: the address (no symlinks resolved, so no spaces from a volume name
end up in prompts and Bash permission rules) and the real path (symlinks resolved), which is what Claude Code
compares file permission rules against ({{HOME_REAL}}).

Environment: every HQ_<X> variable is the old name of KITH_<X>. sync_env() makes them agree: KITH_<X> wins, the
other fills in, so the kernel reads KITH_<X> only while agent role repos that read HQ_<X> keep working. Importing
this module syncs os.environ; the bash twin is lib/kith-env.sh.
"""
import json, os, re, shlex, shutil, sys
from pathlib import Path

KERNEL = Path(__file__).resolve().parent.parent
HOME = Path.home()
DOT = HOME / '.kith'
OLD_DOT = HOME / '.hq'
MARKER = '.migrated'


# HQ_BIN (the kernel's command, which the host runs) is KITH_KERNEL_BIN: KITH_BIN already names the kith binary itself.
RENAMED = {'BIN': 'KERNEL_BIN'}


def sync_env(env=None):
    """Make KITH_<X> and the old HQ_<X> agree in env (os.environ by default): KITH_ wins, the other fills in."""
    env = os.environ if env is None else env
    back = {v: k for k, v in RENAMED.items()}
    for k in list(env):
        if k.startswith('KITH_') and env[k] != '' and k != 'KITH_BIN':
            env['HQ_' + back.get(k[5:], k[5:])] = env[k]
        elif k.startswith('HQ_'):
            new = 'KITH_' + RENAMED.get(k[3:], k[3:])
            if not env.get(new):
                env[new] = env[k]
    return env


sync_env()


def getenv(name, default=''):
    """KITH_<name>, else the old HQ_<name>, else default (name without the prefix)."""
    return os.environ.get('KITH_' + name) or os.environ.get('HQ_' + name) or default


def state_dir():
    """(this Mac's Kith folder, whether it is a real machine folder rather than the old link to the Kith folder)."""
    given = getenv('STATE').strip()
    if given:
        d = Path(os.path.expanduser(given))
        # <home>/state is what the old link layout exports as KITH_STATE; children must not mistake it for a choice
        return d, not (d.name == 'state' and (d.parent / 'kernel').is_dir())
    if (DOT / MARKER).exists():
        return DOT, True
    if OLD_DOT.is_dir() and not OLD_DOT.is_symlink():
        return OLD_DOT, True
    if OLD_DOT.is_symlink() and OLD_DOT.is_dir():
        if DOT.is_dir() and OLD_DOT.resolve() == DOT.resolve():
            return DOT, True
        return OLD_DOT, False
    return DOT, DOT.is_dir()   # nothing yet: the old link layout (state in <home>/state) until install.sh makes ~/.kith


def _first(*candidates):
    return next((c for c in candidates if c.exists()), candidates[0])


def paths():
    dot, mine = state_dir()   # mine: this Mac's folder, not a link to the Kith folder
    given = getenv('HOME').strip()
    linked = HOME / 'kith'
    old_linked = HOME / 'hq'
    # Without either link: the Kith folder around the kernel when it sits there as kernel/, else the kernel folder.
    fallback = KERNEL.parent if KERNEL.name == 'kernel' else KERNEL
    home = (Path(os.path.expanduser(given)) if given else linked if linked.is_dir()
            else old_linked if old_linked.is_dir() else dot if dot.is_symlink() and dot.is_dir() else fallback)
    out = {'KITH_HOME': home, 'KITH_STATE': dot if mine else home / 'state', 'KITH_AGENTS': home / 'agents'}
    out.update({k + '_REAL': Path(os.path.realpath(v)) for k, v in list(out.items())})
    if mine:
        out.update(KITH_MACHINE=dot, KITH_SECRETS=dot / 'secrets', KITH_LOGS=dot / 'logs', KITH_CACHE=dot / 'cache',
                   KITH_STORE=dot / 'store')
    else:
        config = _first(HOME / '.config/kith', HOME / '.config/hq')
        out.update(KITH_MACHINE=config, KITH_SECRETS=config,
                   KITH_LOGS=_first(HOME / 'Library/Logs/kith', HOME / 'Library/Logs/hq'),
                   KITH_CACHE=_first(HOME / 'Library/Caches/kith', HOME / 'Library/Caches/hq'),
                   KITH_STORE=home / 'agents' / '.store')
    return out


def launcher():
    """uv with Full Disk Access, for a repo on an external disk: kith-uv, or hq-uv where it was never renamed."""
    bin_dir = paths()['KITH_MACHINE'] / 'bin'
    return _first(bin_dir / 'kith-uv', bin_dir / 'hq-uv')


def _has_hook(binary):
    """Does this kith program answer `kith memory hook`? An older one doesn't, and a hook that can't run checks nothing."""
    import subprocess
    try:
        r = subprocess.run([binary, 'memory', 'hook', '--help'], capture_output=True, text=True, timeout=10)
    except (OSError, subprocess.SubprocessError):
        return False
    return r.returncode == 0 and 'phase' in r.stdout.lower()


def kith_bin():
    """The kith program (the Rust host, which also answers `kith memory check|log|diff|undo|forget|hook`): KITH_BIN, else
    a kith on PATH that isn't the kernel's bash entry, else the host kith serve installed, else one built in this
    kernel's checkout (CARGO_TARGET_DIR or serve/host/target); the first that has `kith memory hook`. A dev build never
    beats an installed host by accident (CARGO_TARGET_DIR is set in sessions started from the repo): KITH_DEV_BIN=1 puts
    the build right after KITH_BIN. '' when there is none."""
    own = KERNEL / 'bin' / 'kith'
    target = Path(os.environ.get('CARGO_TARGET_DIR') or KERNEL.parent / 'serve/host/target')
    built = [target / 'dev-app/kith', target / 'release/kith', target / 'debug/kith']
    installed = [shutil.which('kith'), HOME / '.kith/serve/bin/kith-serve', HOME / '.kith/serve-main/bin/kith-serve']
    explicit = os.environ.get('KITH_BIN')
    candidates = [explicit, *built, *installed] if os.environ.get('KITH_DEV_BIN') == '1' else [explicit, *installed, *built]
    for c in candidates:
        # Any kernel bash entry is out, not only this kernel's own: ~/.local/bin/kith may be another checkout's.
        if c and os.access(c, os.X_OK) and Path(c).is_file() and not (own.exists() and os.path.samefile(c, own)) \
                and Path(os.path.realpath(c)).parts[-3:] != ('kernel', 'bin', 'kith') and _has_hook(str(c)):
            return str(c)
    return ''


def job_installed(name):
    """Is the background job `name` (sched, backup, live, bridge, ...) installed, under dev.kith.<name> or the old dev.hq.<name>?"""
    return any((HOME / f'Library/LaunchAgents/dev.{p}.{name}.plist').exists() for p in ('kith', 'hq'))


def retire_old_job(name):
    """Boot out and delete dev.hq.<name>: its replacement dev.kith.<name> is being installed (never both: two ticks)."""
    import subprocess
    plist = HOME / f'Library/LaunchAgents/dev.hq.{name}.plist'
    if plist.exists():
        subprocess.run(['launchctl', 'bootout', f'gui/{os.getuid()}/dev.hq.{name}'], capture_output=True)
        plist.unlink(missing_ok=True)


def guest_sandbox():
    """Whether this process is inside the OS sandbox kernel/bin/kith puts around the run of anyone but the owner. That
    sandbox closes the home folder to reads and nothing of the owner's ever does, so the answer comes from the OS, not
    the environment: a guest's shell that drops KITH_PERSON (or names the owner in it) is still told apart from the
    owner's. Any doubt is a guest."""
    import pwd
    try:
        with os.scandir(pwd.getpwuid(os.getuid()).pw_dir):
            return False
    except (OSError, KeyError):
        return True


def guest_tmp(pid):
    """Claude Code's own temp folder for a guest's runs: <machine>/run/t/<pid>, or an 8-hex hash of the id when that is
    too long. Claude Code keeps it for its commands only if <dir>/claude-<uid> fits in 44 bytes (it puts sockets there)
    and otherwise falls back to /tmp, which a guest's sandbox closes. None when even the hash is too long. The real
    path: the sandbox matches resolved paths, and the length is of the path Claude Code is given."""
    import hashlib
    base = os.path.join(os.path.realpath(paths()['KITH_MACHINE']), 'run', 't')
    for name in (pid, hashlib.sha256(pid.encode()).hexdigest()[:8]):
        if len(os.fsencode(f'{base}/{name}/claude-{os.getuid()}')) <= 44:
            return os.path.join(base, name)
    return None


def _tmp_record(d):
    """Whose run/t/<name> is: run/t-who/<name>, out of the guest's reach (only run/t/<name> is theirs to write)."""
    return Path(os.path.dirname(d) + '-who') / os.path.basename(d)


def _rm(d):
    import subprocess
    subprocess.run(['/bin/rm', '-rf', '--', d], capture_output=True)   # rm -rf removes a link inside, never what it names


def guest_tmp_ready(pid):
    """Makes the guest's temp folder for a run and returns it (None as guest_tmp). Only this Mac account reads run/t and
    each folder in it (0700). A folder whose record names someone else (another id with the same hash) or no one (a
    removal that didn't finish, a former person of this id) is emptied first."""
    d = guest_tmp(pid)
    if not d:
        return None
    rec = _tmp_record(d)
    for p in (os.path.dirname(d), str(rec.parent)):
        os.makedirs(p, mode=0o700, exist_ok=True)
        os.chmod(p, 0o700)
    try:
        theirs = rec.read_text() == pid
    except OSError:
        theirs = False
    if not theirs or os.path.islink(d):
        _rm(d)
        rec.write_text(pid)
    os.makedirs(d, mode=0o700, exist_ok=True)
    os.chmod(d, 0o700)
    return d


def guest_tmp_remove(pid):
    """Removes the guest's temp folder once their runs are stopped (a revoke, a removal): the record first, so whatever a
    late write left is emptied before their next run. A folder recorded as someone else's (same hash) stays."""
    d = guest_tmp(pid)
    if not d:
        return
    rec = _tmp_record(d)
    try:
        if rec.read_text() != pid:
            return
    except OSError:
        pass
    rec.unlink(missing_ok=True)
    _rm(d)


def with_old_names(p):
    """p plus every KITH_<X> under its old name HQ_<X>, for agents' tools that still read those."""
    return {**p, **{'HQ_' + k[5:]: v for k, v in p.items()}}


if __name__ == '__main__':
    p = {k: str(v) for k, v in with_old_names(paths()).items()}
    arg = sys.argv[1] if len(sys.argv) > 1 else ''
    if arg == '--sh':
        print('\n'.join('export %s=%s' % (k, shlex.quote(v)) for k, v in p.items()))
    elif arg == '--bin':
        print(kith_bin())
    elif arg == '--guest-sandbox':
        sys.exit(0 if guest_sandbox() else 1)
    elif arg == '--guest-tmp':
        pid = sys.argv[2] if len(sys.argv) > 2 else ''
        if not re.fullmatch(r'[a-z][a-z0-9-]*', pid):
            sys.exit('kith_paths.py: --guest-tmp <person id>')
        print(guest_tmp_ready(pid) or '')
    elif arg == '--json':
        print(json.dumps(p))
    else:
        print('\n'.join('%-15s %s' % kv for kv in p.items()))
