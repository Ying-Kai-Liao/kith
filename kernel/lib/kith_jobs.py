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

A door is kept as `name@digest`, the digest of its whole declaration (argv and options), so a changed argv under the
same name is a new piece: it is not in an old tick and needs the widening card. A job that breaks a rule is left out
(with the reason in `problems`), never read in part.
"""
import hashlib
import json
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
    doors = {d['name']: door_digest(d) for d in _block(text, 'doors') if d and isinstance(d.get('name'), str)}
    jobs, problems, seen = [], [], set()
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
        elif bad := [d for d in j.get('doors', []) if d not in doors]:
            why = 'not among the package doors: %s' % ', '.join(bad)
        if why:
            problems.append('job %s: %s' % (name if isinstance(name, str) else '#%d' % (i + 1), why))
            continue
        seen.add(name)
        pieces = {k: list(dict.fromkeys(j.get(k, []))) for k in PIECES}
        pieces['doors'] = ['%s@%s' % (d, doors[d]) for d in pieces['doors']]
        jobs.append({'name': name, 'label': j.get('label') or name, **pieces})
    return jobs, problems


def door_digest(decl):
    return hashlib.sha256(json.dumps(decl, sort_keys=True).encode()).hexdigest()[:12]


def door_name(piece):
    """The door a snapshot piece names (`render@<digest>` -> render)."""
    return piece.partition('@')[0]


def holding(jobs, kind, value):
    """The first job that lists value among its domains or helpers, or None. A path never names a job."""
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
    print(json.dumps({'jobs': jobs, 'problems': problems}))
