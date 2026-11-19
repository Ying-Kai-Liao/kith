#!/usr/bin/env python3
"""Who uses each Python file of the kernel (docs/plans/retire-python.md, section 1.3a). Re-run it in every retirement step PR.

    python3 -I scripts/py-file-map.py [--json]

A Python file is a tracked file under kernel/ (not kernel/tests) that ends in .py or starts with a python shebang. Users are
found by NAME, not by pattern: every tracked text file (symlinks, docs/*.md, kernel/tests and the file itself left out) is
searched for the file's name and, for a .py, its module name. A pattern only labels the hit, so a call the patterns do not
know (Rust code that holds Python source in a string, a path built with join) still shows up as `ref`.

Labels: import (import/from), load (SourceFileLoader, spec_from_file_location, importlib; load* = the file loads scripts
and mentions this one, confirm by hand), run (python3/uv run/exec of the
path), plugin (a hit under kernel/adapters/claude-code), agent (kernel/**/*.md or .claude/skills: what agents are told to run),
ref (the name appears, none of the above). Callers of `kith <verb>` (the entry) show only as `bin/kith`'s row.
"""
import json
import os
import re
import subprocess
import sys

ROOT = subprocess.check_output(['git', 'rev-parse', '--show-toplevel'], text=True).strip()
# Generated or prose: a name there is not a user.
SKIP_USER = re.compile(r'^(scripts/py-file-map\.py$|docs/|serve/docs/|\.github/kith-protected-paths|.*\.lock$|core/api/(openrpc|released)\.json|serve/contract/)')
# Prose, except what agents are told to run: PROTOCOL.md, the skills (label `agent`).
AGENT_MD = re.compile(r'^(kernel/.*\.md|\.claude/skills/.*\.md)$')
TESTS = 'kernel/tests/'

# Names that are ordinary words elsewhere: search by a longer spelling.
# (regex text; the shim is run by its bare name, `PATH=$here/shim:$PATH` in claude-via-kith)
GENERIC = {'kernel/adapters/acp/shim/claude': r'shim/claude|[/}]shim(?![\w-])', 'kernel/adapters/harness.py': r'harness\.py'}

LOADER = re.compile(r'SourceFileLoader|spec_from_file_location')


def tracked():
    out = subprocess.check_output(['git', '-C', ROOT, 'ls-files', '-z'], text=True)
    return [p for p in out.split('\0') if p and not os.path.islink(os.path.join(ROOT, p))]


def read(path):
    try:
        with open(os.path.join(ROOT, path), 'rb') as f:
            data = f.read()
    except OSError:
        return None
    return None if b'\0' in data[:4096] else data.decode('utf-8', 'replace')


def is_python(path, text):
    if not path.startswith('kernel/') or path.startswith('kernel/tests/'):
        return False
    first = text.split('\n', 1)[0]
    return path.endswith('.py') or (first.startswith('#!') and 'python' in first) or 'uv run' in first


def label(line, name, mod, path):
    if re.search(r'SourceFileLoader|spec_from_file_location|importlib|load_source', line):
        return 'load'
    if mod and re.search(r'^\s*(import|from)\s+.*\b%s\b' % re.escape(mod), line):
        return 'import'
    if mod and re.search(r'\bimport\s+%s\b' % re.escape(mod), line):
        return 'import'  # `python3 -c "import kith_door"` inside a shell or Rust string
    if re.search(r'python3?\b|uv run|exec |\$KITH/bin|\bbin/|\.py\b', line):
        return 'run'
    if path.startswith('kernel/adapters/claude-code'):
        return 'plugin'
    return 'ref'


def main():
    files = {p: read(p) for p in tracked()}
    files = {p: t for p, t in files.items() if t is not None}
    pyfiles = sorted(p for p, t in files.items() if is_python(p, t))
    result = {}
    for py in pyfiles:
        base = os.path.basename(py)
        mod = base[:-3] if base.endswith('.py') else None
        names = [base] + ([mod] if mod else [])
        rx = re.compile(GENERIC[py] if py in GENERIC else '(?<![\\w-])(%s)(?![\\w-])' % '|'.join(re.escape(n) for n in names))
        users = {}
        for p, t in files.items():
            if p == py or SKIP_USER.match(p) or (p.endswith('.md') and not AGENT_MD.match(p)):
                continue
            for i, line in enumerate(t.split('\n'), 1):
                if rx.search(line):
                    kind = 'agent' if p.endswith('.md') else label(line, base, mod, p)
                    if p.startswith('kernel/adapters/claude-code') and kind == 'ref':
                        kind = 'plugin'
                    # A file that loads scripts by a computed name (kith-info) hides the target in a variable: any mention
                    # of the target there is flagged `load*` for a person to confirm.
                    if kind in ('ref', 'run') and LOADER.search(t) and not p.startswith(TESTS):
                        kind = 'load*'
                    users.setdefault(p, {}).setdefault(kind, []).append(i)
        result[py] = users
    if '--json' in sys.argv:
        print(json.dumps(result, indent=1))
        return
    print('| File | Users inside kernel/ (kind) | Checks in kernel/tests | Users elsewhere (files, kinds) |\n|---|---|---|---|')
    for py, users in result.items():
        inside, tests, outside = [], [], {}
        for p, kinds in sorted(users.items()):
            if p.startswith(TESTS):
                rel = p[len(TESTS):]
                tests.append(rel if '/' not in rel else rel.split('/')[0] + '/')  # a folder (difftest/, fixtures/) counts once
            elif p.startswith('kernel/'):
                inside.append('`%s` %s' % (p[len('kernel/'):], '/'.join(sorted(kinds))))
            else:
                area = '/'.join(p.split('/')[:3] if p.startswith(('core/', 'serve/', 'apps/')) else p.split('/')[:2])
                n, ks = outside.get(area, (0, set()))
                outside[area] = (n + 1, ks | set(kinds))
        out = ['`%s` %d (%s)' % (a, n, '/'.join(sorted(ks))) for a, (n, ks) in sorted(outside.items())]
        print('| `%s` | %s | %s | %s |' % (py[len('kernel/'):], '; '.join(inside) or 'none',
                                           ', '.join('`%s`' % t for t in sorted(set(tests))) or 'none', '; '.join(out) or 'none'))

main()
