"""An agent's jobs: the units of trust its package declares for the people it is shared with (guest workbench, step 1.3).

The owner ticks a job at share (`kith-auth job`); what a tick grants is the job as it was ticked, cut to what the package
still declares (`effective`), so a package update can narrow a tick but never widen it. A job draws only on pieces the
kernel bounds: plain host names, the agent's own helpers and the doors its package declares. Read from capabilities.yaml,
stdlib only, in this shape:

    jobs:
      - name: make-videos              # the agent-name rule (letters, digits, . _ -), unique
        label: Make videos             # what the share sheet and the cards say
        domains: [registry.npmjs.org, github.com]
        helpers: [sound]               # each one also in the package's `helpers:`
        doors: [render]                # each one also under the package's `doors:` (step 2)

A door (step 2, kernel/lib/kith_door.py) is a fixed command the kernel runs for a guest in a box of its own, on a folder
of their files. The package declares all of it; the guest names only the door and the folder:

    doors:
      - name: render
        label: Render a video
        npm: [hyperframes@0.8.159]          # exact versions, installed by `kith door prepare` from the lockfile
        lockfile: doors/render.lock.json    # next to capabilities.yaml
        chrome: 152.0.7977.30               # a pinned chrome-headless-shell, optional
        tools: [node, ffmpeg, ffprobe]      # the owner's own, found on PATH; node is required
        bin: hyperframes/bin/hyperframes.mjs   # under node_modules, run with node
        args: [render, --quality, draft]
        writes: [renders, .hyperframes]     # under the folder; nothing else is written
        env: [HYPERFRAMES_BROWSER_PATH=$chrome, HYPERFRAMES_NO_UPDATE_CHECK=1]

A door is kept as `name@digest`, the digest of its declaration and its lockfile, so a changed argv or lockfile under the
same name is a new piece: it is not in an old tick and needs the widening card. A job or door that breaks a rule is left
out (with the reason in `problems`), never read in part.
"""
import hashlib
import json
import os
import re
import sys
from pathlib import Path

import kith_guard

AGENT = re.compile(r'^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$')
NAME = AGENT     # a job name follows the agent-name rule: no ':', so a ledger key `job:<agent>:<person>:<job>` splits
PIECES = ('domains', 'helpers', 'doors')


def _value(v):
    v = re.sub(r'\s+#.*$', '', v).strip()
    if v.startswith('['):
        if not v.endswith(']'):
            return None
        return [s.strip().strip('"\'') for s in v[1:-1].split(',') if s.strip()]
    return v.strip('"\'')


def _block(text, key):
    """The `- key: value` items of a top-level block, each a dict of its keys (None for a line it can't read)."""
    m = re.search(rf'^{key}:[ \t]*(?:#.*)?\n((?:(?:[ \t]+.*|[ \t]*#.*|[ \t]*)\n?)*)', text, re.M)
    items = []
    for line in (m.group(1) if m else '').splitlines():
        if not line.strip() or line.strip().startswith('#'):
            continue
        new = re.match(r'\s*-\s+(.*)$', line)
        body = new.group(1) if new else line.strip()
        if new or not items:
            items.append({})
        kv = re.match(r'([a-z_]+):\s*(.*)$', body)
        if items[-1] is not None and (not kv or kv.group(1) in items[-1]):
            items[-1] = None
        if items[-1] is not None:
            items[-1][kv.group(1)] = _value(kv.group(2))
    return items


def _inline(text, key):
    m = re.search(rf'^{key}:\s*(\[[^\]\n]*\])', text, re.M)
    return _value(m.group(1)) if m else []


def read(caps):
    """(jobs, problems) of a capabilities.yaml file: the valid jobs in package order, and why any other was left out."""
    text = Path(caps).read_text(errors='replace') if Path(caps).is_file() else ''
    helpers = set(_inline(text, 'helpers'))
    decls, problems = doors(caps, text)
    doors_ = {n: d['digest'] for n, d in decls.items()}
    jobs, seen = [], set()
    for i, j in enumerate(_block(text, 'jobs')):
        name = (j or {}).get('name')
        why = None
        if not j or set(j) - {'name', 'label', *PIECES}:
            why = 'a line it does not read (keys: name, label, %s)' % ', '.join(PIECES)
        elif not isinstance(name, str) or not NAME.match(name):
            why = 'no name, or not one of letters, digits, . _ and -'
        elif name in seen:
            why = 'a second job with this name'
        elif not isinstance(j.get('label', name), str):
            why = 'a label that is not plain text'
        elif any(not isinstance(j.get(k, []), list) for k in PIECES):
            why = 'domains, helpers and doors are lists ([a, b])'
        elif bad := [d for d in j.get('domains', []) if not kith_guard.valid_domain(d)]:
            why = 'not a plain host name: %s' % ', '.join(bad)
        elif bad := [h for h in j.get('helpers', []) if not AGENT.match(h) or h not in helpers]:
            why = 'not among the package helpers: %s' % ', '.join(bad)
        elif bad := [d for d in j.get('doors', []) if d not in doors_]:
            why = 'not among the package doors: %s' % ', '.join(bad)
        if why:
            problems.append('job %s: %s' % (name if isinstance(name, str) else '#%d' % (i + 1), why))
            continue
        seen.add(name)
        pieces = {k: list(dict.fromkeys(j.get(k, []))) for k in PIECES}
        pieces['doors'] = ['%s@%s' % (d, doors_[d]) for d in pieces['doors']]
        jobs.append({'name': name, 'label': j.get('label') or name, **pieces})
    return jobs, problems


DOOR_KEYS = {'name', 'label', 'npm', 'lockfile', 'chrome', 'tools', 'bin', 'args', 'writes', 'env'}
NPM_SPEC = re.compile(r'^((@[a-z0-9][a-z0-9._-]*/)?[a-z0-9][a-z0-9._-]*)@(\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?)$')
CHROME = re.compile(r'^\d+\.\d+\.\d+\.\d+$')
TOOLS = ('node', 'ffmpeg', 'ffprobe')
ENV_NAME = re.compile(r'^[A-Z][A-Z0-9_]{0,63}$')
ENV_VALUE = re.compile(r'^(\$(chrome|node|ffmpeg|ffprobe)|[A-Za-z0-9._-]{0,64})$')
# the kernel sets these itself (or never lets a door have them)
ENV_KEPT = re.compile(r'^(PATH|HOME|TMPDIR|LANG|SHELL|USER|NODE_OPTIONS|NODE_PATH|NODE_EXTRA_CA_CERTS|KITH_.*|DYLD_.*|LD_.*|npm_.*)$')


def _relative(p):
    """A plain relative path: no leading /, no '..' or '.' part, no empty part."""
    return isinstance(p, str) and bool(p) and not p.startswith('/') and all(x not in ('', '.', '..') for x in p.split('/'))


def _door_problem(d, base):
    if set(d) - DOOR_KEYS:
        return 'a line it does not read (keys: %s)' % ', '.join(sorted(DOOR_KEYS))
    if not isinstance(d.get('name'), str) or not NAME.match(d['name']):
        return 'no name, or not one of letters, digits, . _ and -'
    if any(not isinstance(d.get(k, []), list) for k in ('npm', 'tools', 'args', 'writes', 'env')):
        return 'npm, tools, args, writes and env are lists ([a, b])'
    if not d.get('npm') or (bad := [s for s in d['npm'] if not NPM_SPEC.match(s)]):
        return 'npm lists packages at exact versions (name@1.2.3): %s' % ', '.join(bad if d.get('npm') else ['none'])
    if not _relative(d.get('lockfile')) or not os.path.isfile(os.path.join(base, d['lockfile'])) \
            or os.path.islink(os.path.join(base, d['lockfile'])) \
            or not os.path.realpath(os.path.join(base, d['lockfile'])).startswith(os.path.realpath(base) + '/'):
        return 'lockfile is a file next to capabilities.yaml (a relative path inside the package)'
    if 'chrome' in d and not (isinstance(d['chrome'], str) and CHROME.match(d['chrome'])):
        return 'chrome is an exact chrome-headless-shell version (1.2.3.4)'
    if 'node' not in d.get('tools', []) or (bad := [x for x in d['tools'] if x not in TOOLS]):
        return 'tools are among %s, node included' % ', '.join(TOOLS)
    if not _relative(d.get('bin')):
        return 'bin is a relative path under node_modules'
    if not d.get('writes') or (bad := [w for w in d['writes'] if not _relative(w)]):
        return 'writes lists folders under the given folder (relative, no ..)'
    for e in d.get('env', []):
        k, eq, v = e.partition('=')
        if not eq or not ENV_NAME.match(k) or ENV_KEPT.match(k) or not ENV_VALUE.match(v):
            return 'env is NAME=value, a plain value or one of $chrome $node $ffmpeg $ffprobe (not %s)' % k
        if v == '$chrome' and 'chrome' not in d or v[1:] in TOOLS and v[1:] not in d['tools']:
            return 'env names %s, which the door does not have' % v
    return None


def doors(caps, text=None):
    """({name: declaration}, problems): the valid doors of a capabilities.yaml file. Each declaration also carries its
    `digest` and the lockfile's real path (`lock`)."""
    base = os.path.dirname(os.path.realpath(caps))
    if text is None:
        text = Path(caps).read_text(errors='replace') if Path(caps).is_file() else ''
    out, problems = {}, []
    for i, d in enumerate(_block(text, 'doors')):
        name = (d or {}).get('name')
        why = 'a line it does not read' if not d else 'a second door with this name' if name in out else _door_problem(d, base)
        if why:
            problems.append('door %s: %s' % (name if isinstance(name, str) else '#%d' % (i + 1), why))
            continue
        lock = os.path.realpath(os.path.join(base, d['lockfile']))
        with open(lock, 'rb') as f:
            out[name] = {**d, 'digest': door_digest(d, f.read()), 'lock': lock}
    return out, problems


def door_digest(decl, lock):
    """The digest of a door's declaration and its lockfile's bytes: what a tick names."""
    body = json.dumps({k: v for k, v in decl.items() if k in DOOR_KEYS}, sort_keys=True).encode()
    return hashlib.sha256(body + b'\0' + lock).hexdigest()[:12]


def door_name(piece):
    """The door a snapshot piece names (`render@<digest>` -> render)."""
    return piece.partition('@')[0]


def holding(jobs, kind, value):
    """The first job that lists value among its domains, helpers or doors (a door by name, or name@digest), or None. A
    path never names a job."""
    if kind == 'doors':
        return next((j for j in jobs if any(value in (d, door_name(d)) for d in j['doors'])), None)
    return next((j for j in jobs if kind in ('domains', 'helpers') and value in j[kind]), None)


def snapshot(job):
    """What a tick keeps: the pieces of the job as it is now."""
    return {k: list(job[k]) for k in PIECES}


def effective(tick, job):
    """What a tick grants: its snapshot, cut to what the package still declares (nothing when the job is gone)."""
    have = tick.get('snapshot') if isinstance(tick, dict) and isinstance(tick.get('snapshot'), dict) else {}
    return {k: [v for v in have.get(k, []) if job and v in job[k]] for k in PIECES}


def widening(tick, job):
    """The pieces a package update added to a ticked job, and their hash (the widening card is asked once per hash)."""
    have = tick.get('snapshot') if isinstance(tick, dict) and isinstance(tick.get('snapshot'), dict) else {}
    added = {k: [v for v in job[k] if v not in have.get(k, [])] for k in PIECES} if job else {k: [] for k in PIECES}
    if not any(added.values()):
        return added, ''
    return added, hashlib.sha256(json.dumps(added, sort_keys=True).encode()).hexdigest()[:16]


if __name__ == '__main__':
    if len(sys.argv) != 2:
        sys.exit('usage: kith_jobs.py <capabilities.yaml>')
    jobs, problems = read(sys.argv[1])
    print(json.dumps({'jobs': jobs, 'problems': problems, 'doors': doors(sys.argv[1])[0]}))
