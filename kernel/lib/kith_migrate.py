"""kith migrate: move a live hq install to the Kith names, reversibly. The CLI is bin/kith-migrate; kith doctor reads state()
and report() from here.

What moves (each step is written to the manifest, ~/.kith/migrate-manifest.json, as it finishes, so a crash can be resumed
by running migrate again or reversed with --undo):

  backup    ~/.hq copied (cp -a) to ~/.kith-migrate-backup-<id>/dot-hq, plus a copy of every file below that gets changed
  stop      the dev.hq.* jobs booted out (nothing runs while the state moves)
  move      every entry of ~/.hq moved into ~/.kith (they are on one disk: a rename, .git included), ~/.kith/.migrated
            written, ~/.hq replaced by a link to ~/.kith. A name in both folders other than an empty folder: nothing moves
  exclude   ~/.kith/.git/info/exclude names what was in ~/.kith before (serve/, ...), so kith backup never commits it
  home      ~/kith made a link to what ~/hq leads to; ~/hq stays
  bin       ~/.local/bin/kith and hq lead to ~/kith/kernel/bin/kith
  launcher  ~/.kith/bin/hq-uv renamed to kith-uv (Full Disk Access is per path, so it is granted again anyway); the
            launcher in ~/.kith/serve/config.json follows it
  jobs      each dev.hq.<x> job replaced by dev.kith.<x>: sched and backup through their own installers, the others by
            rewriting the plist; dev.kith.serve is only touched when its plist names ~/.hq or hq-uv
  plugin    Claude Code: marketplace kith added, kith@kith installed, hq@hq (and the hq marketplace) removed
  settings  ~/.hq paths in ~/.claude/settings.json and ~/.kith/settings.json become ~/.kith paths

Everything outside the machine's own folders goes through $HOME, `launchctl` and `claude` from PATH, so a test points all
three at a throwaway place. The backup is never deleted.
"""
import hashlib, json, os, plistlib, re, shutil, subprocess, sys, time
from pathlib import Path

HOME = Path.home()
OLD = HOME / '.hq'
NEW = HOME / '.kith'
MARKER = NEW / '.migrated'
MANIFEST = NEW / 'migrate-manifest.json'
LA = HOME / 'Library/LaunchAgents'
CLAUDE = HOME / '.claude'
BIN = HOME / '.local/bin'
KERNEL = Path(__file__).resolve().parent.parent
UID = os.getuid()
# the paths kith_paths exports for the state as it is *now*; a child that inherits them would not see the move
DERIVED = {p + x + s for p in ('KITH_', 'HQ_') for x in ('STATE', 'MACHINE', 'SECRETS', 'LOGS', 'CACHE', 'STORE', 'AGENTS')
           for s in ('', '_REAL')} | {'KITH_HOME_REAL', 'HQ_HOME_REAL'}


class Stop(Exception):
    pass


def say(text):
    print(f'  {text}')


def sh(*args, env=None):
    return subprocess.run(list(map(str, args)), capture_output=True, text=True, env=env)


def clean_env():
    return {k: v for k, v in os.environ.items() if k not in DERIVED}


def real_dir(p):
    return p.is_dir() and not p.is_symlink()


def digest(p):
    return hashlib.sha256(Path(p).read_bytes()).hexdigest()


def copy(src, dst):
    """cp -a: links stay links, modes and times stay."""
    Path(dst).parent.mkdir(parents=True, exist_ok=True)
    r = sh('cp', '-a', src, dst)
    if r.returncode:
        raise Stop(f'cp -a {src} {dst} failed: {r.stderr.strip()}')


def link_target(p):
    """Where the link p leads, as an absolute path."""
    t = os.readlink(p)
    return t if os.path.isabs(t) else os.path.normpath(os.path.join(os.path.dirname(p), t))


# ---- state ------------------------------------------------------------------------------------------------------------

def load():
    try:
        return json.loads(MANIFEST.read_text())
    except (OSError, ValueError):
        return None


def save(m):
    tmp = MANIFEST.with_suffix('.tmp')
    tmp.write_text(json.dumps(m, indent=1))
    tmp.replace(MANIFEST)
    b = Path(m['backup'])
    if b.is_dir():
        (b / 'manifest.json').write_text(json.dumps(m, indent=1))   # a second copy, for an undo that cannot find the first


def state():
    """unmigrated, migrated, partial (a crashed migrate: run it again, or --undo), fresh (no ~/.hq at all) or other
    (~/.hq is the older link to the Kith folder: not ours to move)."""
    old_dir = real_dir(OLD)
    linked = OLD.is_symlink() and NEW.is_dir() and OLD.resolve() == NEW.resolve()
    m = load()
    if m and m.get('status') == 'in-progress':
        return 'partial'
    if m and m.get('status') == 'done':
        return 'migrated' if linked else 'partial'
    if MARKER.exists():
        return 'partial' if old_dir else 'migrated'   # both a marker and a real ~/.hq, and no manifest: not ours to resume
    if old_dir:
        return 'unmigrated'
    if OLD.is_symlink() or OLD.exists():
        return 'linked' if linked else 'other'
    return 'fresh'


def leftovers():
    """Names from before the rename that are still in use as more than an alias."""
    out = []
    if real_dir(OLD):
        out.append(f'{OLD} is still a real folder (the state lives there)')
    for p in sorted(LA.glob('dev.hq.*.plist')):
        out.append(f'{p.name}: the old launchd job')
    if (HOME / 'hq').exists() and not (HOME / 'hq').is_symlink() and not (HOME / 'kith').exists():
        out.append(f'{HOME / "hq"} is the folder itself and ~/kith is missing (~/kith should be the address, ~/hq the alias)')
    k, h = BIN / 'kith', BIN / 'hq'
    if h.exists() and not (k.exists() and os.path.realpath(h) == os.path.realpath(k)):
        out.append('~/.local/bin/hq leads somewhere other than ~/.local/bin/kith (it should be only an alias)')
    try:
        if 'hq@hq' in json.loads((CLAUDE / 'plugins/installed_plugins.json').read_text()).get('plugins', {}):
            out.append('Claude Code plugin hq@hq is still installed')
    except (OSError, ValueError):
        pass
    for f in (CLAUDE / 'settings.json', NEW / 'settings.json'):
        try:
            if PATH_RE.search(f.read_text()):
                out.append(f'{f} still names ~/.hq paths')
        except OSError:
            pass
    for p in sorted(LA.glob('dev.kith.*.plist')):
        try:
            if str(OLD) in json.dumps(plistlib.loads(p.read_bytes())):
                out.append(f'{p.name} still runs from {OLD}')
        except Exception:
            pass
    return out


def report():
    """(mark, text) lines for kith doctor: where the migration stands, and what is left on hq names that is not an alias."""
    s = state()
    left = leftovers()
    if s == 'migrated':
        lines = [('✓', 'migrated: this Mac\'s kith is ~/.kith (~/.hq is a link to it)')]
    elif s == 'unmigrated':
        lines = [('!', 'not migrated: this Mac still runs on the hq names; kith migrate --dry-run shows the steps')]
    elif s == 'partial':
        lines = [('✗', 'partly migrated (a migrate stopped half way): kith migrate continues it, kith migrate --undo reverses it')]
    elif s == 'other':
        lines = [('!', '~/.hq is the older link to the Kith folder, which kith migrate does not move')]
    else:
        lines = [('✓', 'nothing to migrate: this Mac never had ~/.hq' if s == 'fresh' else 'migrated')]
    if s != 'unmigrated':
        lines += [('!', f'still on an hq name: {t}') for t in left]
    else:
        lines.append(('·', f'{len(left)} thing(s) on hq names; kith migrate moves them'))
    return lines


# ---- the checks before anything moves ---------------------------------------------------------------------------------

def alive(pid):
    try:
        os.kill(int(pid), 0)
        return True
    except (OSError, ValueError, TypeError):
        return False


def active():
    """Why migrating now would cut something off: an ask holding its lock, a scheduled run, an agent run."""
    import fcntl
    why = []
    state_dir = NEW if MARKER.exists() else OLD
    lock = state_dir / 'ask.lock'
    if lock.exists():
        with open(lock, 'rb') as f:
            try:
                fcntl.flock(f, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except OSError:
                why.append(f'{lock} is held (kith ask is writing)')
    try:
        last = json.loads((state_dir / 'sched/state.json').read_text()).get('last', {})
        why += [f'scheduled run {k} is still running (pid {v["pid"]})' for k, v in last.items() if alive(v.get('pid'))]
    except (OSError, ValueError, AttributeError):
        pass
    quiet = {'migrate', 'doctor', 'ls', 'init', 'project', 'memory', 'who', 'auth', 'cost', 'serve', 'schedule', 'backup',
             'versions', 'history', 'deps', 'market', 'search', 'remote'}
    # `ps e` appends each process's environment: a running session (claude, a harness) has replaced the kith command
    # that started it, but still carries KITH_AGENT / HQ_AGENT
    ps = sh('ps', 'axeww', '-o', 'pid=,command=').stdout
    for line in ps.splitlines():
        pid, _, cmd = line.strip().partition(' ')
        if pid == str(os.getpid()):
            continue
        env = re.search(r'(?:^|\s)(?:KITH|HQ)_AGENT=(\S+)', cmd)
        if env:
            why.append(f'an agent session is running ({env.group(1)}, pid {pid})')
            continue
        mo = re.search(r'(?:^|[\s/])(?:kith|hq)(-ask|-acp)?\s+(?:-\S+\s+)*([A-Za-z][\w-]*)', cmd)
        if mo and mo.group(2) not in quiet and 'sh -c' not in cmd:
            why.append(f'an agent run is going (pid {pid}: {cmd.strip()[:80]})')
    return why


def problems(resumed=False):
    out = []
    if not resumed:   # a second job under the new name would be overwritten, and undo would delete it
        out += [f'dev.hq.{j} and dev.kith.{j} are both installed: remove one, then run again'
                for j in jobs_of() if (LA / f'dev.kith.{j}.plist').exists()]
    if NEW.exists() and not real_dir(NEW):
        out.append(f'{NEW} exists and is not a folder')
    if real_dir(OLD) and real_dir(NEW):
        for e in sorted(OLD.iterdir()):
            dst = NEW / e.name
            if (dst.exists() or dst.is_symlink()) and not (real_dir(dst) and not any(dst.iterdir()) and e.is_dir()):
                out.append(f'{e.name} is in both {OLD} and {NEW}: move or rename one, then run again')
    return out


# ---- steps ------------------------------------------------------------------------------------------------------------

def jobs_of(prefix='dev.hq.'):
    return sorted(p.name[len('dev.'):-len('.plist')].split('.', 1)[1] for p in LA.glob(prefix + '*.plist'))


def s_backup(m, dry):
    b = Path(m['backup'])
    say(f'back up {OLD} (cp -a) to {b}/dot-hq, and a copy of every plist, link, plugin and settings file this changes')
    if dry:
        return
    if (b / '.complete').exists():
        return
    if b.exists():   # a crash before the state moved: set the half-made copy aside rather than delete anything
        b.rename(b.with_name(b.name + f'.partial-{int(time.time())}'))
    b.mkdir(parents=True)
    if real_dir(OLD):
        m['old_mode'] = OLD.stat().st_mode & 0o7777
        copy(OLD, b / 'dot-hq')
    kept, absent = [], []
    wanted = [*sorted(LA.glob('dev.hq.*.plist')), LA / 'dev.kith.serve.plist', CLAUDE / 'settings.json',
              CLAUDE / 'plugins/installed_plugins.json', CLAUDE / 'plugins/known_marketplaces.json',
              BIN / 'kith', BIN / 'hq', HOME / 'kith', HOME / 'hq', CLAUDE / 'plugins/cache/hq',
              CLAUDE / 'plugins/marketplaces/hq']
    for p in wanted:
        if p.is_symlink() or p.is_file() or (p.is_dir() and 'plugins' in p.parts):
            copy(p, b / 'files' / p.relative_to(HOME))
            kept.append(str(p.relative_to(HOME)))
        elif not p.exists():
            absent.append(str(p.relative_to(HOME)))
        # ~/hq or ~/kith as a real folder is the repo itself: not copied, never moved
    m.update(backup_files=kept, absent=absent,
             was_loaded=[j for j in jobs_of() if sh('launchctl', 'print', f'gui/{UID}/dev.hq.{j}').returncode == 0])
    (b / '.complete').write_text('')


def u_backup(m):
    pass   # the backup is what the other undos read from


def s_stop(m, dry):
    js = sorted(set(jobs_of()) | set(m['steps']['stop'].get('jobs', [])))   # a resumed run no longer sees plists it retired
    say('boot out ' + (', '.join('dev.hq.' + j for j in js) if js else 'no dev.hq.* jobs (none installed)'))
    if dry:
        return
    m['steps']['stop']['jobs'] = js
    for j in js:
        bootout(f'dev.hq.{j}')


def u_stop(m):
    """The original jobs come back last, after the state and links are as they were: a RunAtLoad job starts at once."""
    b = Path(m['backup'])
    for j in m['steps'].get('stop', {}).get('jobs', []):
        saved = b / 'files' / 'Library/LaunchAgents' / f'dev.hq.{j}.plist'
        if saved.exists():
            copy(saved, LA / f'dev.hq.{j}.plist')
            if j in m.get('was_loaded', []):
                bootout(f'dev.hq.{j}')
                bootstrap(LA / f'dev.hq.{j}.plist')


def s_move(m, dry):
    say(f'move every entry of {OLD} into {NEW}, write {MARKER.name}, make {OLD} a link to {NEW}')
    if dry:
        return
    st = m['steps']['move']
    if 'planned' not in st:
        st['planned'] = sorted(e.name for e in OLD.iterdir())
        st['replaced_empty'] = []
        save(m)
    NEW.mkdir(exist_ok=True)
    if OLD.is_symlink():   # stopped after the last rename: the layout is done, replaying the moves would see one folder twice
        MARKER.write_text('')
        return
    for name in st['planned']:
        src, dst = OLD / name, NEW / name
        if src.exists() or src.is_symlink():
            if dst.exists() or dst.is_symlink():
                if real_dir(dst) and not any(dst.iterdir()):
                    dst.rmdir()
                    st['replaced_empty'].append(name)
                    save(m)
                else:
                    raise Stop(f'{name} is in both {OLD} and {NEW}')
            os.rename(src, dst)
    MARKER.write_text('')
    if real_dir(OLD):
        if any(OLD.iterdir()):
            raise Stop(f'{OLD} still holds {[e.name for e in OLD.iterdir()]}: something wrote there during the move; run again')
        OLD.rmdir()
    if not OLD.is_symlink():
        OLD.symlink_to(NEW)


def u_move(m):
    st = m['steps'].get('move', {})
    if not st.get('planned'):
        return
    if OLD.is_symlink():
        OLD.unlink()
    OLD.mkdir(exist_ok=True)
    if m.get('old_mode') is not None:
        OLD.chmod(m['old_mode'])
    for name in st['planned']:
        if (NEW / name).exists() or (NEW / name).is_symlink():
            if not (OLD / name).exists():
                os.rename(NEW / name, OLD / name)
    for name in st.get('replaced_empty', []):
        (NEW / name).mkdir(exist_ok=True)
    MARKER.unlink(missing_ok=True)
    # folders the jobs' installers made since (logs/sched/, ...) and that hold nothing: ~/.hq had none of them
    copy_of = Path(m['backup']) / 'dot-hq'
    if copy_of.is_dir():
        for d, _, _ in sorted(os.walk(OLD), reverse=True):
            d = Path(d)
            if d != OLD and not (copy_of / d.relative_to(OLD)).exists() and not any(d.iterdir()):
                d.rmdir()


def s_exclude(m, dry):
    say(f'keep what was in {NEW} before (kith serve\'s data, ...) out of the state repo\'s commits: .git/info/exclude')
    if dry:
        return
    git = NEW / '.git'
    st = m['steps']['exclude']
    if not git.is_dir():
        st['skipped'] = 'no .git in ~/.kith'
        return
    names = sorted({e.name for e in NEW.iterdir()} - set(m['steps']['move'].get('planned', [])) - {'.git'}
                   | {MANIFEST.name, MARKER.name})
    f = git / 'info/exclude'
    if 'file' not in st:   # the original goes into the manifest before the file changes, and stays there on a retry
        st['file'] = str(f)
        st['original'] = f.read_text() if f.exists() else None
        save(m)
    base = st['original'] or ''
    add = [f'/{n}' for n in names if f'/{n}' not in base.splitlines()]
    f.parent.mkdir(exist_ok=True)
    f.write_text(base + ('' if not base or base.endswith('\n') else '\n') + '# added by kith migrate: not part of the state\n'
                 + '\n'.join(add) + '\n')


def u_exclude(m):
    st = m['steps'].get('exclude', {})
    if not st.get('file'):
        return
    f = Path(st['file'])
    if st['original'] is None:
        f.unlink(missing_ok=True)
    else:
        f.write_text(st['original'])


def s_home(m, dry):
    old, new = HOME / 'hq', HOME / 'kith'
    say(f'make {new} a link to {link_target(old) if old.is_symlink() else old}; {old} stays as it is')
    if dry:
        return
    st = m['steps']['home']
    if not (old.exists() or old.is_symlink()):
        st['skipped'] = '~/hq does not exist (install.sh makes ~/kith)'
        return
    target = link_target(old) if old.is_symlink() else str(old)
    if new.is_symlink() or new.exists():
        if os.path.realpath(new) != os.path.realpath(target):
            raise Stop(f'{new} exists and is not the Kith folder ({os.path.realpath(new)})')
        return
    st['created'] = True   # recorded first: a crash between the two lines must still be undone
    save(m)
    new.symlink_to(target)


def u_home(m):
    new = HOME / 'kith'
    if m['steps'].get('home', {}).get('created') and new.is_symlink():
        new.unlink()


def s_bin(m, dry):
    say(f'point {BIN}/kith and {BIN}/hq at {HOME}/kith/kernel/bin/kith')
    if dry:
        return
    st = m['steps']['bin']
    cmd = HOME / 'kith/kernel/bin/kith'
    k = BIN / 'kith'
    keep = k.is_symlink() and not str(link_target(k)).startswith((str(HOME / 'hq'), str(HOME / 'kith')))
    if keep:   # a packaged install links the release binary: it answers to both names
        target = os.readlink(k)
    elif cmd.exists():
        target = str(cmd)
    else:
        st['skipped'] = f'{cmd} is not there'
        return
    if 'before' not in st:   # the first run's record is the one that counts
        st['before'] = {n: (os.readlink(BIN / n) if (BIN / n).is_symlink() else None) for n in ('kith', 'hq')}
        save(m)
    BIN.mkdir(parents=True, exist_ok=True)
    for n in ('kith', 'hq'):
        p = BIN / n
        if p.is_symlink() or not p.exists():
            tmp = BIN / f'.{n}.migrate'
            tmp.unlink(missing_ok=True)
            tmp.symlink_to(target)
            os.replace(tmp, p)
        else:
            say(f'{p} is a real file: left alone')


def u_bin(m):
    for n, t in m['steps'].get('bin', {}).get('before', {}).items():
        p = BIN / n
        if p.is_symlink():
            p.unlink()
        if t is not None:
            p.symlink_to(t)


# -- launcher

def launcher_configs():
    """The places kith serve keeps its launcher."""
    return [('config', NEW / 'serve/config.json')]


def s_launcher(m, dry):
    old, new = NEW / 'bin/hq-uv', NEW / 'bin/kith-uv'
    say(f'rename {NEW}/bin/hq-uv to kith-uv, and the launcher kith serve reads from {launcher_configs()[0][1]}')
    if dry:
        return
    st = m['steps']['launcher']
    renamed = False
    if (old.exists() or old.is_symlink()) and not (new.exists() or new.is_symlink()):
        st['renamed'] = True
        save(m)
        os.rename(old, new)
        renamed = True
    for key, cfg in launcher_configs():
        if not cfg.is_file():
            continue
        text = cfg.read_text()
        rec = st.get(key)
        if rec:   # stopped between the record and the write: finish it, as s_settings does
            if text == rec['before']:
                cfg.write_text(rec['after'])
            continue
        try:
            c = json.loads(text)
        except ValueError:
            c = None
        if isinstance(c, dict) and isinstance(c.get('launcher'), list):
            c['launcher'] = [rewrite_str(x) if isinstance(x, str) else x for x in c['launcher']]
            out = json.dumps(c, indent=2) + '\n'
            if out != text:
                st[key] = {'before': text, 'after': out}
                save(m)   # the record first: a crash after the write must still be undone
                cfg.write_text(out)
    if renamed:
        # jobs a run of an earlier version already moved still name hq-uv, and their step will not run again
        for j in sorted(m['steps'].get('jobs', {}).get('done', {})):
            plist = LA / f'dev.kith.{j}.plist'
            if not plist.exists():
                continue
            before = plist.read_bytes()
            after = plistlib.dumps(rewrite_plist(plistlib.loads(before)))
            if after != before:
                plist.write_bytes(after)
                if loaded(f'dev.kith.{j}'):
                    bootout(f'dev.kith.{j}')
                    bootstrap(plist)


def u_launcher(m):
    st = m['steps'].get('launcher', {})
    old, new = NEW / 'bin/hq-uv', NEW / 'bin/kith-uv'
    for key, cfg in launcher_configs():
        rec = st.get(key)
        if rec and cfg.is_file():
            if cfg.read_text() == rec['after']:
                cfg.write_text(rec['before'])
            else:
                print(f'  {cfg} changed since the migration: left as it is')
    if st.get('renamed') and new.exists() and not old.exists():
        os.rename(new, old)


# -- jobs

def rewrite_str(s):
    s = s.replace(str(OLD), str(NEW))
    s = re.sub(re.escape(str(HOME)) + r'/hq(?=/|$)', str(HOME) + '/kith', s)
    s = re.sub(r'(' + re.escape(str(NEW)) + r'/bin/)hq-uv(?![\w-])', r'\1kith-uv', s)
    return re.sub(r'(/kernel/bin/)hq-', r'\1kith-', s)


def rewrite_plist(o):
    if isinstance(o, str):
        return rewrite_str(o)
    if isinstance(o, list):
        return [rewrite_plist(x) for x in o]
    if isinstance(o, dict):
        return {k: rewrite_plist(v) for k, v in o.items()}
    return o


def loaded(label):
    return sh('launchctl', 'print', f'gui/{UID}/{label}').returncode == 0


def bootout(label):
    """bootout returns before the job is torn down: a bootstrap straight after it can fail with EIO. Wait until it is gone."""
    sh('launchctl', 'bootout', f'gui/{UID}/{label}')
    end = time.time() + float(os.environ.get('KITH_MIGRATE_LAUNCHD_WAIT', '10'))
    while loaded(label):
        if time.time() > end:
            say(f'{label} is still shown by launchd after the wait: going on')
            return
        time.sleep(0.2)


def bootstrap(plist):
    """Load a job; retry the errors launchd gives while it is still tearing the old one down (5 EIO, 37), then check it is loaded."""
    label = plistlib.loads(Path(plist).read_bytes()).get('Label', Path(plist).stem)
    pause = float(os.environ.get('KITH_MIGRATE_BACKOFF', '0.5'))
    for attempt in range(5):
        r = sh('launchctl', 'bootstrap', f'gui/{UID}', plist)
        if r.returncode == 0:
            break
        if r.returncode not in (5, 37) or attempt == 4:
            raise Stop(f'launchctl bootstrap {plist} failed (exit {r.returncode}): {r.stderr.strip()}')
        time.sleep(pause * 2 ** attempt)
    if not loaded(label):
        raise Stop(f'{label} is not loaded after launchctl bootstrap {plist}')


INSTALLERS = {'sched': ['kith-sched', '--install'], 'backup': ['kith-state', 'backup', '--nightly']}


def serve_stale(p):
    pl = plistlib.loads(p.read_bytes())
    return rewrite_plist(pl) != pl


def s_jobs(m, dry):
    old = jobs_of()
    for j in old:
        how = '(through ' + ' '.join(INSTALLERS[j]) + ')' if j in INSTALLERS else '(plist rewritten)'
        say(f'dev.hq.{j} -> dev.kith.{j} {how}')
    serve = LA / 'dev.kith.serve.plist'
    if serve.exists() and serve_stale(serve):
        say('dev.kith.serve: its plist names ~/.hq or hq-uv: rewritten and restarted')
    elif serve.exists():
        say('dev.kith.serve: left running, its plist does not name ~/.hq or hq-uv')
    if not old:
        say('no dev.hq.* jobs to replace')
    if dry:
        return
    st = m['steps']['jobs']
    st.setdefault('done', {})
    LA.mkdir(parents=True, exist_ok=True)
    # the plan is the jobs stopped in step 2, not what is on disk now: an installer that failed after retiring the old
    # plist leaves nothing under dev.hq.* to find on the retry
    for j in sorted(set(old) | set(m['steps']['stop'].get('jobs', []))):
        if st['done'].get(j):
            continue
        oldp, newp = LA / f'dev.hq.{j}.plist', LA / f'dev.kith.{j}.plist'
        if j in INSTALLERS:
            r = subprocess.run([str(KERNEL / 'bin' / INSTALLERS[j][0]), *INSTALLERS[j][1:]], capture_output=True, text=True,
                               env=clean_env())
            if r.returncode or not newp.exists():
                raise Stop(f'dev.kith.{j}: its installer failed ({(r.stderr or r.stdout).strip()[-200:]})')
            if not loaded(f'dev.kith.{j}'):
                raise Stop(f'dev.kith.{j} is installed but launchd does not show it loaded')
            oldp.unlink(missing_ok=True)   # the installer retires it; make sure
            st['done'][j] = 'installer'
        else:
            if oldp.exists():
                pl = rewrite_plist(plistlib.loads(oldp.read_bytes()))
                pl['Label'] = f'dev.kith.{j}'
                newp.write_bytes(plistlib.dumps(pl))
            if j in m.get('was_loaded', []):
                bootout(f'dev.kith.{j}')
                bootstrap(newp)
            oldp.unlink(missing_ok=True)
            st['done'][j] = 'rewritten'
        save(m)
    if serve.exists():
        before = serve.read_bytes()
        pl = plistlib.loads(before)
        if serve_stale(serve):
            st['serve_rewritten'] = True
            save(m)
            serve.write_bytes(plistlib.dumps(rewrite_plist(pl)))
            bootout(f'dev.kith.serve')
            bootstrap(serve)


def u_jobs(m):
    """Stop and remove the replacements. The original jobs come back in u_stop, after the state is moved back."""
    st = m['steps'].get('jobs', {})
    b = Path(m['backup'])
    for j in sorted(set(st.get('done', {})) | set(m['steps'].get('stop', {}).get('jobs', []))):
        bootout(f'dev.kith.{j}')
        (LA / f'dev.kith.{j}.plist').unlink(missing_ok=True)
    saved = b / 'files/Library/LaunchAgents/dev.kith.serve.plist'
    if st.get('serve_rewritten') and saved.exists():
        copy(saved, LA / 'dev.kith.serve.plist')
        bootout(f'dev.kith.serve')
        bootstrap(LA / 'dev.kith.serve.plist')


# -- plugin

def installed_plugins():
    try:
        return json.loads((CLAUDE / 'plugins/installed_plugins.json').read_text()).get('plugins', {})
    except (OSError, ValueError):
        return {}


def s_plugin(m, dry):
    say('Claude Code: marketplace add kith, install kith@kith, uninstall hq@hq, remove the hq marketplace')
    if dry:
        return
    st = m['steps']['plugin']
    if not shutil.which('claude'):
        st['skipped'] = 'claude is not installed'
        say('claude is not on PATH: plugin left as it is (install.sh does it later)')
        return
    if 'had_hq' not in st:
        have = installed_plugins()
        st['had_hq'], st['had_kith'] = 'hq@hq' in have, 'kith@kith' in have
        save(m)
    home = os.path.realpath(HOME / 'kith' if (HOME / 'kith').exists() else HOME / 'hq')
    sh('claude', 'plugin', 'marketplace', 'add', home)
    r = sh('claude', 'plugin', 'install', 'kith@kith')
    if r.returncode and not st['had_kith']:
        raise Stop(f'claude plugin install kith@kith failed: {(r.stderr or r.stdout).strip()[-200:]}')
    if st['had_hq']:
        r = sh('claude', 'plugin', 'uninstall', 'hq@hq')
        if r.returncode:
            raise Stop(f'claude plugin uninstall hq@hq failed: {(r.stderr or r.stdout).strip()[-200:]}')
    sh('claude', 'plugin', 'marketplace', 'remove', 'hq')


def u_plugin(m):
    st = m['steps'].get('plugin', {})
    if not st or st.get('skipped'):
        return
    b = Path(m['backup']) / 'files'
    if not st.get('had_kith'):
        sh('claude', 'plugin', 'uninstall', 'kith@kith')
        sh('claude', 'plugin', 'marketplace', 'remove', 'kith')
    # the registry files and the plugin's cache come back exactly as they were
    for rel in ('.claude/plugins/installed_plugins.json', '.claude/plugins/known_marketplaces.json',
                '.claude/plugins/cache/hq', '.claude/plugins/marketplaces/hq'):
        if (b / rel).exists():
            if (HOME / rel).is_dir():
                shutil.rmtree(HOME / rel)
            copy(b / rel, HOME / rel)


# -- settings

PATH_RE = re.compile(r'(?<![\w.~/-])(?:' + re.escape(str(HOME)) + r'|~|\$HOME)/\.hq(?=[/"\s\\]|$)')


def rewritten(path, text):
    new = PATH_RE.sub(lambda mo: mo.group(0)[:-3] + '.kith', text)
    new = re.sub(r'(\.kith/bin/)hq-uv(?![\w-])', r'\1kith-uv', new)
    if path == NEW / 'settings.json':
        new = new.replace('"hq@hq"', '"kith@kith"')
    return new


def s_settings(m, dry):
    say(f'~/.hq paths in {CLAUDE}/settings.json and {NEW}/settings.json become ~/.kith paths')
    if dry:
        return
    st = m['steps']['settings']
    st.setdefault('files', {})
    for f in (CLAUDE / 'settings.json', NEW / 'settings.json'):
        rec = st['files'].get(str(f))
        if rec and f.is_file() and digest(f) == rec['before']:   # recorded, but the write did not happen
            f.write_text(rewritten(f, f.read_text()))
            continue
        if not f.is_file() or rec:
            continue
        text = f.read_text()
        new = rewritten(f, text)
        if new == text:
            continue
        try:
            json.loads(new)
        except ValueError:
            say(f'{f}: not valid JSON after the rewrite, left alone')
            continue
        src = Path(m['backup']) / 'files' / f.relative_to(HOME) if f.parent == CLAUDE \
            else Path(m['backup']) / 'dot-hq' / f.name
        st['files'][str(f)] = {'before': hashlib.sha256(text.encode()).hexdigest(),
                               'after': hashlib.sha256(new.encode()).hexdigest(), 'backup': str(src)}
        save(m)   # the record first: a crash after the write must still be undone
        f.write_text(new)


def u_settings(m):
    for f, rec in m['steps'].get('settings', {}).get('files', {}).items():
        f = Path(f)
        if f.is_file() and digest(f) == rec['after'] and Path(rec['backup']).exists():
            shutil.copy2(rec['backup'], f)
        elif f.is_file():
            print(f'  {f} changed since the migration: left as it is (the original is {rec["backup"]})')


STEPS = [('backup', s_backup, u_backup), ('stop', s_stop, u_stop), ('move', s_move, u_move),
         ('exclude', s_exclude, u_exclude), ('home', s_home, u_home), ('bin', s_bin, u_bin), ('launcher', s_launcher, u_launcher), ('jobs', s_jobs, u_jobs),
         ('plugin', s_plugin, u_plugin), ('settings', s_settings, u_settings)]


# ---- commands ---------------------------------------------------------------------------------------------------------

def new_manifest():
    ident = time.strftime('%Y%m%d-%H%M%S')
    return {'version': 1, 'id': ident, 'status': 'in-progress', 'backup': str(HOME / f'.kith-migrate-backup-{ident}'),
            'steps': {}, 'started': ident}


def migrate(dry=False, force=False):
    s = state()
    if s == 'migrated' or s == 'linked':
        print('already migrated: nothing to do' if load() else 'nothing to migrate: ~/.kith is already this Mac\'s kith')
        return 0
    if s == 'fresh':
        print('nothing to migrate: there is no ~/.hq (kith init makes ~/.kith)')
        return 0
    if s == 'other':
        print(f'{OLD} is not a real folder (the older link layout); kith migrate leaves it', file=sys.stderr)
        return 1
    m = load()
    if s == 'partial' and not m:
        print(f'both {MARKER} and a real {OLD} exist and there is no manifest: sort it out by hand', file=sys.stderr)
        return 1
    resumed = m is not None
    print(('resuming a stopped migration' if resumed else 'migrating') + (' (dry run: nothing changes)' if dry else '') + ':')
    bad = problems(resumed)
    if resumed:   # entries already moved are not clashes
        planned = set(m['steps'].get('move', {}).get('planned', []))
        bad = [b for b in bad if b.split(' ', 1)[0] not in planned]
    if bad:
        for b in bad:
            print(f'  refused: {b}', file=sys.stderr)
        return 1
    busy = active()
    if busy and not force:
        for b in busy:
            print(f'  refused: {b}', file=sys.stderr)
        print('  wait for it to finish, or --force', file=sys.stderr)
        return 1
    for b in busy:
        print(f'  --force: going on although {b}')
    if dry:
        m = m or new_manifest()
        for i, (name, do, _) in enumerate(STEPS, 1):
            print(f'{i}/{len(STEPS)} {name}')
            m['steps'].setdefault(name, {})
            do(m, True)
        print('\nthen: a manifest in ~/.kith/migrate-manifest.json; kith migrate --undo reverses all of it')
        return 0
    try:
        if not m:
            m = new_manifest()
            m['new_existed'] = NEW.exists()   # undo removes a ~/.kith this migration made, once it is empty again
            NEW.mkdir(exist_ok=True)
            save(m)
        for i, (name, do, _) in enumerate(STEPS, 1):
            st = m['steps'].setdefault(name, {})
            if st.get('finished'):
                print(f'{i}/{len(STEPS)} {name}: already done')
                continue
            print(f'{i}/{len(STEPS)} {name}')
            save(m)
            do(m, False)
            st['finished'] = True
            save(m)
        m['status'] = 'done'
        m['finished'] = time.strftime('%Y%m%d-%H%M%S')
        save(m)
    except Stop as e:
        print(f'\nstopped: {e}\nfix that and run kith migrate again to continue, or kith migrate --undo to go back', file=sys.stderr)
        return 1
    print(f'\ndone. The backup is {m["backup"]} (never deleted by kith). Undo: kith migrate --undo')
    if (NEW / 'bin/kith-uv').exists():
        print(f'Full Disk Access is granted per path: give {NEW}/bin/kith-uv Full Disk Access again '
              f'(System Settings -> Privacy & Security -> Full Disk Access).')
    return 0


def undo(dry=False, force=False):
    m = load()
    if not m:
        backups = sorted(HOME.glob('.kith-migrate-backup-*/manifest.json'))
        m = json.loads(backups[-1].read_text()) if backups and state() != 'unmigrated' else None
    if not m or m.get('status') not in ('done', 'in-progress'):
        print('nothing to undo: no migration manifest', file=sys.stderr)
        return 1
    busy = active()
    if busy and not force and not dry:
        for b in busy:
            print(f'  refused: {b}', file=sys.stderr)
        print('  wait for it to finish, or --force', file=sys.stderr)
        return 1
    b = Path(m['backup'])
    if not (b / '.complete').exists() and m['steps'].get('move', {}).get('planned'):
        print(f'the backup {b} is incomplete: refusing to undo from it', file=sys.stderr)
        return 1
    order = [(n, u) for n, _, u in reversed(STEPS) if n in m['steps']]
    print('undoing' + (' (dry run: nothing changes)' if dry else '') + ': ' + ', '.join(n for n, _ in order))
    if dry:
        return 0
    try:
        for n, u in order:
            print(f'  undo {n}')
            u(m)
    except Stop as e:
        print(f'\nstopped: {e}', file=sys.stderr)
        return 1
    # the manifest goes into the backup, so ~/.kith holds only what it held before; a folder this migrate made goes too
    final = b / 'manifest.undone.json'
    m['status'] = 'undone'
    final.write_text(json.dumps(m, indent=1))
    MANIFEST.unlink(missing_ok=True)
    (b / 'manifest.json').unlink(missing_ok=True)
    if not m.get('new_existed', True) and real_dir(NEW) and not any(NEW.iterdir()):
        NEW.rmdir()
    print(f'\nundone. {OLD} is a real folder again. The backup stays: {b}')
    return 0


def main(argv):
    if '-h' in argv or '--help' in argv:
        print(__doc__.strip())
        return 0
    flags = set(argv)
    unknown = flags - {'--dry-run', '--undo', '--force', '--status'}
    if unknown:
        print(f'kith migrate: unknown option {sorted(unknown)[0]} (--dry-run, --undo, --force, --status)', file=sys.stderr)
        return 2
    if '--status' in flags:
        for mark, text in report():
            print(f'{mark} {text}')
        return 0
    if '--undo' in flags:
        return undo('--dry-run' in flags, '--force' in flags)
    return migrate('--dry-run' in flags, '--force' in flags)
