"""A door: a fixed command the kernel runs for a guest in a box of its own (guest workbench step 2).

A guest's shell cannot render a video (Chrome needs the Mach services, the GPU and loopback sockets their sandbox closes),
so the package declares a door (kith_jobs: npm packages at exact versions, a lockfile, a pinned chrome-headless-shell, a
fixed argv) and a ticked job grants it. kith-permit's run_door(door, path) runs it, outside the guest's sandbox but in a
deny-default sandbox-exec profile of the door's own, on one folder of the person's files:

  - reads: the system, the folder, the door's prepared cache and the owner's node/ffmpeg; writes: only the declared
    folders under it and the door's temp folder; exec: only the door's tools.
  - network: no socket is bound in the box. The supervisor (this process) binds 127.0.0.1 listening sockets before the
    start and passes them in (KITH_DOOR_LISTEN_FDS); door-preload.mjs turns every Node listen into a listen on the next
    one, and drives Chrome over a pipe (no debugging port). The box may connect only to those ports on this Mac, and
    re-listen/accept only on them. The supervisor keeps its own copy of each socket open until every door process is
    gone, so a port is never freed while the box could take it by an implicit bind.
  - start (D2, model GuestDoor.tla): under the kith-auth lock, the step that reads allowed and not held (`kith-auth door`)
    writes the claim and starts the box. A revoke takes the same lock, so a door either started before it (and its claim
    is there for the sweep) or sees it and doesn't start.
  - stop (D1): `sweep` kills every door of the listed people, every door whose supervisor is gone (a host or kith-permit
    killed with -9), and every door its person may no longer run. A door process is one in this door's sandbox (it may
    read <claim>.in and not <claim>.out: sandbox_check), or one whose environment holds KITH_DOOR=<claim>, or a
    descendant of one; hyperframes starts ffmpeg and workers detached, so a process group is not enough. All are stopped,
    rescanned until nothing new appears, then killed.

The cache (`kith door prepare`, owner only) is <cache>/doors/<agent>/<door>@<digest>: npm ci --ignore-scripts of the
package's lockfile and the pinned chrome-headless-shell, made in a deny-default box whose only network is outbound 443,
plus this kernel's preload and Chrome wrapper. Its digest covers the door's declaration, the lockfile and those two files.
"""
import ctypes
import ctypes.util
import fcntl
import hashlib
import json
import os
import pwd
import re
import secrets
import selectors
import shutil
import signal
import socket
import struct
import subprocess
import sys
import time
from pathlib import Path

import kith_jobs
from kith_paths import HOME, guest_sandbox, guest_scrub, paths

LIB = Path(__file__).resolve().parent
PRELOAD, WRAPPER = 'door-preload.mjs', 'door-chrome'
LISTENERS = 4          # hyperframes render starts one file server; spares cost nothing
SERVE_PORTS = (19224,)
RUN_LIMIT_S = 3600
SANDBOX_EXEC = '/usr/bin/sandbox-exec'

SYSTEM_READ = ('/System', '/usr/lib', '/usr/share', '/Library/Fonts', '/Library/Preferences/Logging', '/private/etc',
               '/bin', '/private/var/select', '/private/var/db/timezone', '/opt/homebrew', '/dev')
SYSTEM_EXEC = ('/bin/sh', '/bin/bash')
# What node and chrome-headless-shell use to render (from a `(with report)` run). Left out on purpose: the pasteboard,
# Apple events, location, TCC, the Dock and window manager, audio, analytics and distributed notifications.
MACH_LOOKUP = ('com.apple.logd', 'com.apple.diagnosticd', 'com.apple.system.notification_center',
               'com.apple.system.opendirectoryd.libinfo', 'com.apple.system.opendirectoryd.membership',
               'com.apple.cfprefsd.daemon', 'com.apple.cfprefsd.agent', 'com.apple.fonts', 'com.apple.CARenderServer',
               'com.apple.CoreServices.coreservicesd', 'com.apple.SystemConfiguration.configd',
               'com.apple.SystemConfiguration.DNSConfiguration')
XPC_LOOKUP = ('com.apple.MTLCompilerService',)
CHROME_RENDEZVOUS = r'^org\.chromium\.Chromium\.MachPortRendezvousServer\.[0-9]+$'
IOKIT_CLASSES = ('IOSurfaceRootUserClient', 'AGXDeviceUserClient', 'RootDomainUserClient')  # Chrome segfaults without the last
CLAIM = re.compile(r'^[0-9a-f]{16}$')
_kept = []    # the sockets of a door a process of which would not die: held while this process lives


class DoorError(Exception):
    """What the agent is told when a door can't run."""


# ---- the profile

def _q(p):
    return '"' + p.replace('\\', '\\\\').replace('"', '\\"') + '"'


def _ancestors(p):
    out, cur = [], os.path.dirname(p)
    while cur and cur != '/':
        out.append(cur)
        cur = os.path.dirname(cur)
    return out + ['/']


def profile(*, dir, reads, execs, writes, ports=(), outbound=(), mark=None, serve_ports=SERVE_PORTS):
    """The door's seatbelt profile. Later rules win, so the network denies come last."""
    lines = ['(version 1)', '(deny default)',
             '(allow process-fork)',
             '(allow signal (target same-sandbox))',       # render's orphan cleanup signals every chrome of the user
             '(allow process-info* (target same-sandbox))',
             '(allow sysctl-read)',
             '(allow system-sched (target self))',
             '(allow ipc-posix-shm)',
             '(allow ipc-posix-sem)',
             '(allow mach-lookup ' + ' '.join(f'(global-name {_q(n)})' for n in MACH_LOOKUP)
             + ' ' + ' '.join(f'(xpc-service-name {_q(n)})' for n in XPC_LOOKUP)
             + f' (global-name-regex #"{CHROME_RENDEZVOUS}"))',
             f'(allow mach-register (global-name-regex #"{CHROME_RENDEZVOUS}"))',
             '(allow iokit-open ' + ' '.join(f'(iokit-user-client-class {_q(c)})' for c in IOKIT_CLASSES) + ')']
    trees = (*SYSTEM_READ, dir, *reads, *writes)
    meta = sorted({a for p in trees for a in _ancestors(p)} | {'/var', '/tmp', '/etc', '/private/var', '/private/tmp'})
    lines.append('(allow file-read-metadata ' + ' '.join(f'(literal {_q(a)})' for a in meta) + ')')
    lines.append('(allow file-read-data (literal "/"))')
    lines.append('(allow file-read* ' + ' '.join(f'(subpath {_q(p)})' for p in trees) + ')')
    lines.append('(allow file-map-executable ' + ' '.join(f'(subpath {_q(p)})' for p in ('/System', '/usr/lib', '/opt/homebrew', *reads)) + ')')
    lines.append('(allow process-exec ' + ' '.join(f'(literal {_q(p)})' for p in (*SYSTEM_EXEC, *execs)) + ')')
    lines.append('(allow file-write* ' + ' '.join(f'(subpath {_q(p)})' for p in writes) + ' (literal "/dev/null") (literal "/dev/dtracehelper"))')
    if mark:   # the door's identity: only its processes may read this file
        lines.append(f'(allow file-read-data (literal {_q(mark)}))')
    lines.append('(deny network*)')
    if ports:
        # Seatbelt ignores the address of a local rule, and "localhost" in a remote rule is any of this Mac's addresses:
        # what holds the box to 127.0.0.1 is that only the supervisor binds (below). Node's listen({fd}) calls listen(2)
        # again on the passed socket, which seatbelt checks as inbound.
        lines.append('(allow network-outbound ' + ' '.join(f'(remote ip "localhost:{p}")' for p in ports) + ')')
        lines.append('(allow network-inbound ' + ' '.join(f'(local ip "localhost:{p}")' for p in ports) + ')')
    if outbound:   # prepare only: the registry and the Chrome download, and DNS
        lines.append('(allow network-outbound ' + ' '.join(f'(remote ip {_q(r)})' for r in outbound)
                     + ' (literal "/private/var/run/mDNSResponder"))')
    lines.append('(deny network* ' + ' '.join(f'(local ip "*:{p}") (remote ip "*:{p}")' for p in serve_ports) + ')')
    lines.append('(deny network-bind)')   # last: else a bind falls back to the inbound rule and 0.0.0.0:P passes
    return '\n'.join(lines) + '\n'


# ---- the declaration and the cache

def declared(caps, name):
    """The door `name` of a capabilities.yaml file, or DoorError."""
    decls, problems = kith_jobs.doors(caps)
    if name not in decls:
        why = next((p for p in problems if p.startswith(f'door {name}:')), None)
        raise DoorError(f'this agent declares no door {name}' + (f' ({why})' if why else ''))
    return decls[name]


def piece(decl):
    return f"{decl['name']}@{decl['digest']}"


def cache_dir(agent, decl):
    """<cache>/doors/<agent>/<door>@<digest>: the digest covers the declaration, the lockfile, and this kernel's preload
    and Chrome wrapper (a kernel that changes them prepares again)."""
    h = hashlib.sha256(decl['digest'].encode())
    for n in (PRELOAD, WRAPPER):
        h.update(b'\0' + (LIB / n).read_bytes())
    return paths()['KITH_CACHE'] / 'doors' / agent / f"{decl['name']}@{h.hexdigest()[:16]}"


def ready(agent, decl):
    return (cache_dir(agent, decl) / '.ready').is_file()


def tools(decl, env=os.environ):
    """{tool: real path} of the door's tools on the owner's PATH (never one in a guest's folders), or DoorError."""
    path = guest_scrub({'PATH': env.get('PATH') or '/usr/bin:/bin'}, env.get('KITH_PERSON') or '-').get('PATH', '')
    path = ':'.join(p for p in path.split(':') + ['/opt/homebrew/bin', '/usr/local/bin'] if p.startswith('/'))
    out = {}
    for t in decl.get('tools', []) + (['npm'] if 'npm' not in decl.get('tools', []) else []):
        found = shutil.which(t, path=path)
        if not found:
            if t == 'npm':
                continue
            raise DoorError(f'{t} is not installed on this Mac (the door needs it)')
        out[t] = os.path.realpath(found)
        if t != 'npm':
            _tree(t, out[t], env)
    return out


def _prefix(tool):
    """The install folder of a tool (…/bin/node -> …), which it reads and maps libraries from."""
    return os.path.dirname(os.path.dirname(tool))


def _fold(p):
    return p.lower() if os.uname().sysname == 'Darwin' else p


def _tree(name, tool, env=os.environ):
    """The install folder the box may read for `tool`, or DoorError when that folder is the owner's home folder, holds
    it, holds one of Kith's folders, the keys or ~/.ssh, or is inside the keys, ~/.ssh or the agents' files (a tool in
    ~/bin would open the whole home folder to whatever the guest feeds it)."""
    p = paths()
    homes = {env.get('HOME') or '', str(HOME), pwd.getpwuid(os.getuid()).pw_dir} - {''}
    ssh = {os.path.join(h, '.ssh') for h in homes}
    held = {*homes, *ssh, env.get('KITH_AUTH') or str(p['KITH_SECRETS'] / 'auth.json'),
            *(str(p[k]) for k in ('KITH_HOME', 'KITH_STATE', 'KITH_AGENTS', 'KITH_MACHINE', 'KITH_SECRETS'))}
    inside = {*ssh, str(p['KITH_SECRETS']), str(p['KITH_AGENTS'])}
    real = lambda s: _fold(os.path.realpath(s)).rstrip('/') or '/'
    under = lambda a, b: a == b or b == '/' or a.startswith(b + '/')
    tree = real(_prefix(tool))
    if any(under(real(h), tree) for h in held) or any(under(tree, real(i)) for i in inside):
        raise DoorError(f"{name} is installed in {_prefix(tool)}, where the door's box would read the owner's files; "
                        f"install {name} outside the home folder (for example with Homebrew)")
    return _prefix(tool)


def _npm_cli(t, env=os.environ):
    """npm-cli.js of the owner's npm: run with node, never through the npm shell script."""
    if 'npm' in t and t['npm'].endswith('npm-cli.js'):
        _tree('npm', t['npm'], env)
        return t['npm']
    guess = os.path.join(_prefix(t['node']), 'lib', 'node_modules', 'npm', 'bin', 'npm-cli.js')
    if os.path.isfile(guess):
        return guess
    raise DoorError('npm is not installed next to node on this Mac')


def describe(decl):
    """What a prepare card says it downloads."""
    got = ', '.join(decl['npm']) + (f" and chrome-headless-shell {decl['chrome']}" if decl.get('chrome') else '')
    return f"Downloads {got} into this Mac's door cache, from the package's lockfile, with no install scripts."


def prepare(agent, decl, log=lambda s: None, env=os.environ):
    """Builds the door's cache in a deny-default box (outbound 443 only). Owner only: never from a guest's sandbox."""
    if guest_sandbox():
        raise DoorError('a door is prepared by the owner, not from a guest sandbox')
    if not os.path.exists(SANDBOX_EXEC):
        raise DoorError('doors need macOS sandbox-exec')
    cache = cache_dir(agent, decl)
    if (cache / '.ready').is_file():
        return cache
    t = tools(decl, env)
    npm_cli = _npm_cli(t, env)
    work = Path(str(cache) + '.prep')
    shutil.rmtree(work, ignore_errors=True)
    (work / 'tmp').mkdir(parents=True, mode=0o700)
    deps = {}
    for spec in decl['npm']:
        m = kith_jobs.NPM_SPEC.match(spec)
        deps[m.group(1)] = m.group(3)
    (work / 'package.json').write_text(json.dumps({'name': 'kith-door-' + decl['name'], 'private': True, 'dependencies': deps}))
    shutil.copyfile(decl['lock'], work / 'package-lock.json')
    for n in ('npmrc', 'npmrc-global'):
        (work / 'tmp' / n).write_text('')
    sb = work / 'tmp' / 'prepare.sb'
    sb.write_text(profile(dir=str(work), reads=sorted({_prefix(t['node']), os.path.dirname(npm_cli)}), writes=[str(work)],
                          execs=[t['node'], '/usr/bin/unzip'], outbound=['*:443']))
    box_env = {'PATH': os.path.dirname(t['node']) + ':/usr/bin:/bin', 'HOME': f'{work}/tmp', 'TMPDIR': f'{work}/tmp/',
               'LANG': 'en_US.UTF-8', 'npm_config_cache': f'{work}/tmp/npm-cache',
               'npm_config_userconfig': f'{work}/tmp/npmrc', 'npm_config_globalconfig': f'{work}/tmp/npmrc-global',
               'npm_config_update_notifier': 'false', 'npm_config_fund': 'false', 'npm_config_audit': 'false'}

    def boxed(*argv):
        r = subprocess.run([SANDBOX_EXEC, '-f', str(sb), *argv], cwd=work, env=box_env, stdin=subprocess.DEVNULL,
                           stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, timeout=1800)
        log(r.stdout[-2000:])
        if r.returncode:
            raise DoorError(f'preparing the door failed ({os.path.basename(argv[1])} exited {r.returncode}): '
                            + r.stdout.strip()[-600:])

    boxed(t['node'], npm_cli, 'ci', '--ignore-scripts', '--no-audit', '--no-fund')
    if decl.get('chrome'):
        cli = work / 'node_modules' / '@puppeteer' / 'browsers' / 'lib' / 'main-cli.js'
        if not cli.is_file():
            raise DoorError('the lockfile has no @puppeteer/browsers to install chrome-headless-shell with')
        boxed(t['node'], str(cli), 'install', f"chrome-headless-shell@{decl['chrome']}", '--path', f'{work}/chrome')
    for n in (PRELOAD, WRAPPER):
        shutil.copyfile(LIB / n, work / n)
    os.chmod(work / WRAPPER, 0o755)
    shutil.rmtree(work / 'tmp')
    (work / '.ready').write_text(piece(decl) + '\n')
    shutil.rmtree(cache, ignore_errors=True)
    os.rename(work, cache)
    return cache


def _chrome(cache):
    found = sorted((cache / 'chrome' / 'chrome-headless-shell').glob('*/chrome-headless-shell-*/chrome-headless-shell'))
    if not found:
        raise DoorError('the door cache has no chrome-headless-shell; prepare it again (kith door prepare)')
    return str(found[-1])


# ---- processes (macOS)

_libc = None


def _c():
    global _libc
    if _libc is None:
        _libc = ctypes.CDLL(ctypes.util.find_library('c'), use_errno=True)
        _libc.sandbox_check.restype = ctypes.c_int
        _libc.sandbox_check.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int]   # the path goes as a variadic
    return _libc


class _BsdInfo(ctypes.Structure):
    _fields_ = [('flags', ctypes.c_uint32), ('status', ctypes.c_uint32), ('xstatus', ctypes.c_uint32),
                ('pid', ctypes.c_uint32), ('ppid', ctypes.c_uint32), ('uid', ctypes.c_uint32),
                ('gid', ctypes.c_uint32), ('ruid', ctypes.c_uint32), ('rgid', ctypes.c_uint32),
                ('svuid', ctypes.c_uint32), ('svgid', ctypes.c_uint32), ('rfu', ctypes.c_uint32),
                ('comm', ctypes.c_char * 16), ('name', ctypes.c_char * 32), ('nfiles', ctypes.c_uint32),
                ('pgid', ctypes.c_uint32), ('pjobc', ctypes.c_uint32), ('tdev', ctypes.c_uint32),
                ('tpgid', ctypes.c_uint32), ('nice', ctypes.c_int32),
                ('start_sec', ctypes.c_uint64), ('start_usec', ctypes.c_uint64)]


def _pids():
    n = _c().proc_listallpids(None, 0)
    buf = (ctypes.c_int * (n + 64))()
    n = _c().proc_listallpids(buf, ctypes.sizeof(buf))
    return [p for p in buf[:n] if p > 0]


def proc_info(pid):
    """(ppid, start time in s) of a running process, or None (a zombie runs nothing)."""
    bi = _BsdInfo()
    if _c().proc_pidinfo(pid, 3, 0, ctypes.byref(bi), ctypes.sizeof(bi)) != ctypes.sizeof(bi):   # PROC_PIDTBSDINFO
        return None
    if bi.status == 5:   # SZOMB
        return None
    return bi.ppid, bi.start_sec + bi.start_usec / 1e6


def _environ(pid):
    """A process's environment from the kernel (KERN_PROCARGS2; macOS hides it for platform binaries such as /bin/sh)."""
    mib = (ctypes.c_int * 3)(1, 8, 0)    # CTL_KERN, KERN_ARGMAX
    argmax, size = ctypes.c_int(), ctypes.c_size_t(4)
    _c().sysctl(mib, 2, ctypes.byref(argmax), ctypes.byref(size), None, 0)
    mib = (ctypes.c_int * 3)(1, 49, pid)  # KERN_PROCARGS2
    buf, size = ctypes.create_string_buffer(argmax.value), ctypes.c_size_t(argmax.value)
    if _c().sysctl(mib, 3, buf, ctypes.byref(size), None, 0) != 0:
        return []
    raw = buf.raw[:size.value]
    argc = struct.unpack_from('i', raw)[0]
    rest = raw[4:]
    rest = rest[rest.index(b'\0'):].lstrip(b'\0')    # past the exec path
    out = []
    for e in rest.split(b'\0')[argc:]:
        if not e:
            break
        out.append(e)
    return out


def _may_read(pid, path):
    return _c().sandbox_check(pid, b'file-read-data', 1 | 0x40000000, path.encode())   # FILTER_PATH | NO_REPORT


def run_dir():
    d = paths()['KITH_MACHINE'] / 'run' / 'doors'
    d.mkdir(parents=True, mode=0o700, exist_ok=True)
    return d


def door_procs(claim, since, parent=None):
    """Every process of the door `claim`: in its sandbox, or tagged KITH_DOOR=<claim>, or (given its supervisor's pid
    `parent`) a child of the supervisor, started at or after `since`, and their descendants."""
    d = run_dir()
    tag = b'KITH_DOOR=' + claim.encode()
    procs = {p: i for p in _pids() if (i := proc_info(p)) and i[1] >= since - 1}
    found = {p for p in procs if (_may_read(p, f'{d}/{claim}.in') == 0 and _may_read(p, f'{d}/{claim}.out') == 1)
             or procs[p][0] == parent or tag in _environ(p)}
    grew = True
    while grew:
        grew = False
        for p, (ppid, _) in procs.items():
            if p not in found and ppid in found:
                found.add(p)
                grew = True
    return found - {os.getpid(), 0, 1}


def kill(claim, since, parent=None):
    """Stops every process of the door, rescans until nothing new appears, kills them all, and waits up to 10 s until
    none is left. Returns the ones still left (none, unless one would not die)."""
    stopped = set()
    while new := door_procs(claim, since, parent) - stopped:
        for p in new:
            try:
                os.kill(p, signal.SIGSTOP)
            except ProcessLookupError:
                pass
        stopped |= new
    deadline, left = time.time() + 10, stopped
    while left and time.time() < deadline:
        for p in left:    # only what the last scan found: a pid that ended may already be someone else's
            try:
                os.kill(p, signal.SIGKILL)
            except ProcessLookupError:
                pass
        time.sleep(0.05)
        left = door_procs(claim, since, parent)
    return left


# ---- the locked start, the run and the sweep

def auth_lock():
    """kith-auth's lock (lock_auth): every write of auth.json, a revoke included, holds it."""
    f = Path(os.environ.get('KITH_AUTH') or paths()['KITH_SECRETS'] / 'auth.json')
    f.parent.mkdir(parents=True, exist_ok=True)
    return open(f.with_name(f.name + '.lock'), 'a')


def check(agent, person, door, env=os.environ):
    """kith-auth door: {ok, state, why}. Read-only; the caller holds the lock when it is the start."""
    try:
        r = subprocess.run([str(LIB.parent / 'bin' / 'kith-auth'), 'door', agent, person, door, '--json'], env=env,
                           stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=30)
        v = json.loads(r.stdout)
        return v if isinstance(v, dict) else {'ok': False, 'state': 'error', 'why': 'no answer'}
    except (OSError, ValueError, subprocess.TimeoutExpired) as e:
        return {'ok': False, 'state': 'error', 'why': f'kith-auth could not say ({type(e).__name__})'}


def _claims():
    out = []
    for f in run_dir().glob('*.json'):
        try:
            c = json.loads(f.read_text())
        except (OSError, ValueError):
            continue
        if isinstance(c, dict) and CLAIM.match(str(c.get('claim', ''))) and f.stem == c['claim']:
            out.append(c)
    return out


def _alive(c):
    """Whether the claim's supervisor still runs (the same pid, started when the claim says)."""
    i = proc_info(int(c.get('pid') or 0)) if isinstance(c.get('pid'), int) and c['pid'] > 1 else None
    return bool(i) and abs(i[1] - float(c.get('procStart') or 0)) < 1


def _forget(claim):
    d = run_dir()
    shutil.rmtree(d / f'{claim}.tmp', ignore_errors=True)
    for end in ('json', 'in', 'out', 'sb'):
        (d / f'{claim}.{end}').unlink(missing_ok=True)


def in_slice(path, agent, person):
    """Whether `path` is, by real path, inside the person's files of this agent: <agents>/<agent>/files/people/<person>."""
    fold = (lambda s: s.lower()) if os.uname().sysname == 'Darwin' else (lambda s: s)
    root = fold(os.path.realpath(paths()['KITH_AGENTS'] / agent / 'files' / 'people' / person))
    real = fold(os.path.realpath(path))
    return real.startswith(root + '/')


def run(agent, person, decl, folder, env=os.environ, on_line=None):
    """Runs the door on `folder` and waits for it: (exit code, the output's tail). DoorError when it can't start."""
    if not os.path.exists(SANDBOX_EXEC):
        raise DoorError('doors need macOS sandbox-exec')
    if not person or person == '-':
        raise DoorError('a door runs for a person')
    real = os.path.realpath(folder)
    if not in_slice(real, agent, person) or not os.path.isdir(real):
        raise DoorError(f"the folder must be one of {person}'s folders under files/people/{person}")
    cache = cache_dir(agent, decl)
    if not (cache / '.ready').is_file():
        raise DoorError('the door is not prepared on this Mac')
    t = tools(decl, env)
    chrome = _chrome(cache) if decl.get('chrome') else None
    d = run_dir()
    claim = secrets.token_hex(8)
    tmp = d / f'{claim}.tmp'
    tmp.mkdir(mode=0o700)
    for end in ('in', 'out'):
        (d / f'{claim}.{end}').write_text('')
    socks = []
    for _ in range(LISTENERS):
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        s.bind(('127.0.0.1', 0))
        s.listen(128)
        socks.append(s)
    ports, fds = [s.getsockname()[1] for s in socks], [s.fileno() for s in socks]
    execs = [t[x] for x in decl['tools']] + ([chrome, str(cache / WRAPPER)] if chrome else [])
    sb = d / f'{claim}.sb'
    sb.write_text(profile(dir=real, reads=sorted({str(cache), *(_prefix(t[x]) for x in decl['tools'])}), execs=execs,
                          writes=[os.path.join(real, w) for w in decl['writes']] + [str(tmp)], ports=ports,
                          mark=f'{d}/{claim}.in'))
    subst = {'$node': t['node'], '$ffmpeg': t.get('ffmpeg', ''), '$ffprobe': t.get('ffprobe', ''),
             '$chrome': str(cache / WRAPPER)}
    box_env = {'PATH': ':'.join(dict.fromkeys(os.path.dirname(t[x]) for x in decl['tools'])) + ':/usr/bin:/bin',
               'HOME': str(tmp), 'TMPDIR': f'{tmp}/', 'LANG': 'en_US.UTF-8', 'DO_NOT_TRACK': '1',
               'NODE_OPTIONS': f'--import {cache / PRELOAD}', 'KITH_DOOR': claim,
               'KITH_DOOR_LISTEN_FDS': ','.join(map(str, fds))}
    if chrome:
        box_env['KITH_DOOR_CHROME'] = chrome
    for e in decl.get('env', []):
        k, _, v = e.partition('=')
        box_env[k] = subst.get(v, v)
    argv = [SANDBOX_EXEC, '-f', str(sb), '/bin/sh', '-c', 'cd -- "$0" && exec "$@"', real,
            t['node'], str(cache / 'node_modules' / decl['bin']), *decl.get('args', [])]
    me = proc_info(os.getpid())
    parent = os.getppid()
    since = time.time()
    p, tail, record = None, [], None

    def stop(*_):
        raise KeyboardInterrupt

    old = signal.signal(signal.SIGTERM, stop)
    try:
        with auth_lock() as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            v = check(agent, person, piece(decl), env)
            if not v.get('ok'):
                raise DoorError(v.get('why') or 'not allowed')
            record = {'claim': claim, 'pid': os.getpid(), 'procStart': me[1] if me else 0, 'person': person,
                      'agent': agent, 'door': piece(decl), 'since': since,
                      'at': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())}
            (d / f'{claim}.json').write_text(json.dumps(record))
            p = subprocess.Popen(argv, env=box_env, cwd=tmp, start_new_session=True, stdin=subprocess.DEVNULL,
                                 stdout=subprocess.PIPE, stderr=subprocess.STDOUT, pass_fds=fds)
        sel = selectors.DefaultSelector()
        sel.register(p.stdout, selectors.EVENT_READ)
        buf, ended = b'', False
        while True:
            if os.getppid() != parent or time.time() - since > RUN_LIMIT_S:
                tail.append('the door was stopped')   # kith-permit's run is gone, or the door ran too long
                break
            if not sel.select(0.5):
                continue
            chunk = os.read(p.stdout.fileno(), 65536)
            if not chunk:
                ended = True
                break
            buf += chunk
            *lines, buf = buf.split(b'\n')
            for line in lines:
                s = line.decode(errors='replace')
                tail = (tail + [s])[-40:]
                if on_line:
                    on_line(s)
        code = p.wait(timeout=30) if ended else None
        if code is not None and code < 0:    # a sweep killed it
            code = None
            tail.append('the door was stopped')
    except KeyboardInterrupt:
        code = None
        tail.append('the door was stopped')
    except subprocess.TimeoutExpired:
        code = None
        tail.append('the door closed its output but did not end')
    finally:
        signal.signal(signal.SIGTERM, old)
        left = set()
        if record is not None:   # the box may run even when p is None: a SIGTERM inside Popen, after the fork
            left = kill(claim, since, os.getpid())
            if p is not None:
                if p.poll() is None:
                    p.kill()
                try:
                    p.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    pass
                left = door_procs(claim, since, os.getpid())
        if left:   # one would not die: the claim stays for the next sweep, and the ports stay held until it is gone
            _kept.extend(socks)
            (d / f'{claim}.json').write_text(json.dumps({**record, 'left': True}))
            tail.append('a process of the door would not end; the next sweep ends it')
        else:
            for s in socks:     # only now: no door process is left that could take the port by an implicit bind
                s.close()
            _forget(claim)
    return code, '\n'.join(tail)


def sweep(persons=(), recheck=True):
    """Kills the doors of `persons`, every door whose supervisor is gone, and (recheck) every door its person may no
    longer run. Returns the claims it ended."""
    if not os.path.exists(SANDBOX_EXEC):
        return []
    ended = []
    with auth_lock() as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)    # no start is half done while the claims are read
        doomed = []
        for c in _claims():
            if c.get('person') in persons or not _alive(c) or c.get('left'):
                doomed.append(c)
            elif recheck:
                v = check(str(c.get('agent')), str(c.get('person')), str(c.get('door')))
                if not v.get('ok') and v.get('state') != 'held':
                    doomed.append(c)
    for c in doomed:
        if kill(c['claim'], float(c.get('since') or 0)):
            continue               # one would not die: the claim stays, and the next sweep tries again
        if not _alive(c) or c.get('left'):
            _forget(c['claim'])    # a live supervisor cleans up its own when its box ends, unless it gave up on it
        ended.append(c['claim'])
    return ended


def package(agent):
    """The agent's live package folder, as kith-auth finds it (agent_dirs): what its jobs and ticks are read from."""
    for root in (paths()['KITH_AGENTS'], Path(os.environ.get('KITH_ROOT') or LIB.parent) / 'agents'):
        d = root / agent
        pkg = d / 'role' if (d / 'role' / 'agent.md').is_file() else d
        if (pkg / 'agent.md').is_file():
            return pkg
    raise DoorError(f'no agent {agent}')


def main(argv):
    """kith door prepare AGENT DOOR | sweep [--person ID]... [--no-recheck] | ls"""
    if argv[:1] == ['prepare'] and len(argv) == 3 and kith_jobs.AGENT.match(argv[1]):
        agent, name = argv[1:]
        print(prepare(agent, declared(package(agent) / 'capabilities.yaml', name), log=lambda s: print(s, file=sys.stderr)))
        return 0
    if argv[:1] == ['sweep']:
        persons, rest = [], argv[1:]
        while rest[:1] == ['--person'] and len(rest) >= 2:
            persons.append(rest[1])
            rest = rest[2:]
        if rest not in ([], ['--no-recheck']):
            raise DoorError('sweep [--person ID]... [--no-recheck]')
        print(json.dumps({'ended': sweep(persons, recheck=not rest)}))
        return 0
    if argv == ['ls']:
        print(json.dumps([{**c, 'running': _alive(c)} for c in _claims()]))
        return 0
    print(main.__doc__)
    return 0 if argv[:1] in (['--help'], ['-h']) else 2
