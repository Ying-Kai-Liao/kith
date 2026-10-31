"""What the owner said is OK for an agent in auto mode, kept per agent (<agent home>/allowed.json).

In auto mode a classifier blocks some owner-run calls. The host shows the owner a card (kith-permit `denied`); "Always
allow" saves one narrow entry here, through `kith allow` (bin/kith-allow, the one writer). The next owner run of that agent
loads the entries into its settings (bin/kith, the settings fill): a permission rule goes to permissions.allow, a prose
exception to autoMode.allow. Guests never load them.

The file:  {"version": 1, "allowed": [{"id", "rule", "kind": "rule"|"prose", "reason", "added"}]}
  rule   a Claude Code permission rule written exactly as it is added: Bash(<the exact command>), Edit(//<abs file>),
         WebFetch(domain:<host>), mcp__<server>__<tool>. Never a wildcard.
  prose  for what a permission rule cannot hold (see `derive`): one sentence for the classifier naming the agent and the
         exact action, an exception to its soft rules.
The id is a hash of the rule text, so adding the same rule twice is one entry.

`derive` is the only place that decides what may be remembered and as what; the host asks it (`kith allow --derive`) before
it offers "Always allow", and again, from the same card input, when it saves.
"""
import fcntl
import hashlib
import json
import os
import re
import shlex
import sys
import time
from contextlib import contextmanager
from pathlib import Path

import kith_guard
from kith_paths import paths

AGENT = re.compile(r'^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$')
MAX_RULE = 2000
EDIT_TOOLS = ('Edit', 'Write', 'MultiEdit', 'NotebookEdit')
# Names of the run's own approval tools and of kith's secret handling are never remembered.
NEVER_TOOLS = ('mcp__kithpermit__',)

# A shell that can run anything: a bare one, or one given code to run, is not a rule worth keeping.
RUNNERS = {'sudo', 'su', 'doas', 'eval', 'exec', 'xargs', 'env', 'bash', 'sh', 'zsh', 'dash', 'fish', 'python', 'python3',
           'node', 'deno', 'bun', 'ruby', 'perl', 'php', 'osascript', 'npx', 'pnpx', 'bunx', 'uvx', 'ssh', 'nohup', 'time',
           'command', 'builtin', 'watch', 'timeout', 'nice', 'open', 'xcrun'}
NEVER_FIRST = {'sudo', 'su', 'doas', 'eval', 'exec'}
NETWORK = {'curl', 'wget', 'nc', 'ncat', 'scp', 'rsync', 'sftp', 'ftp', 'telnet', 'ssh'}
# Claude Code sends a write under these to the classifier even when an allow rule matches (permission-modes "protected
# paths"), so a rule would not remember it; the call is still rememberable as a sentence.
CC_PROTECTED = re.compile(r'(?:^|[\s/"\'=])\.(?:git|claude|vscode|idea)(?:/|\b)|\.(?:gitmodules|mcp\.json|envrc)\b'
                          r'|CLAUDE(?:\.local)?\.md')
# Why a must-ask class is never remembered (kith_guard.must_ask).
WHY = {'secret': 'it names a secret or a login',
       'trust': 'it changes what later runs may do (an allow list, settings, a package, a protected path)',
       'destroy': 'a delete or overwrite outside the run\'s own folders is asked each time',
       'outward': 'something public or past the owner (a push to main, a merge, a publish, a post) is asked each time',
       'code': 'code with no name (inline code, a package runner, a pipe into a shell) is asked each time'}
COMPOUND = re.compile(r'[;&|`<>\n\\*?{}\[\]]|\$\(|\$\{|\$[A-Za-z_]')
URL = re.compile(r'https?://', re.I)
# Package managers whose run/exec forms Claude Code drops from allow rules in auto mode (permission-modes: "Package-manager run
# commands"), so a saved rule would never take effect; `npm test` and the like stay rules.
PKG_RUN = {'npm', 'pnpm', 'yarn', 'bun', 'uv', 'poetry', 'pipx', 'pdm', 'hatch'}
PKG_RUN_VERBS = {'run', 'run-script', 'exec', 'x', 'dlx', 'create', 'init'}
PKG_ALWAYS = {'npx', 'pnpx', 'bunx', 'uvx'}
# `claude auto-mode defaults` hard_deny labels. A sentence in autoMode.allow only overrides soft_deny, so a block under one of
# these is never lifted by a remembered sentence (an exact permission rule still resolves before the classifier).
HARD_DENY = ('Data Exfiltration',)


def home_of(agent):
    """The agent's own folder (its life), as kith-sched finds jobs.json."""
    p = paths()
    pkgs = p['KITH_AGENTS']
    return pkgs / agent if (pkgs / agent / 'role').is_dir() else Path(p['KITH_STATE']) / 'agents' / agent


def file_of(agent):
    return home_of(agent) / 'allowed.json'


def entry_id(rule):
    return hashlib.sha256(rule.encode()).hexdigest()[:8]


def read_file(f):
    """(entries, problem): the well-formed entries of a file; a missing file is none, a malformed one is none with a problem."""
    try:
        text = Path(f).read_text()
    except FileNotFoundError:
        return [], None
    except OSError as e:
        return [], 'cannot read %s (%s)' % (f, e.strerror or type(e).__name__)
    try:
        doc = json.loads(text)
        items = doc['allowed']
        if not isinstance(items, list):
            raise ValueError('allowed is not a list')
    except (ValueError, KeyError, TypeError) as e:
        return [], '%s is not usable (%s); skipped' % (f, e)
    out = []
    for e in items:
        if (isinstance(e, dict) and isinstance(e.get('rule'), str) and e['rule'].strip() and len(e['rule']) <= MAX_RULE
                and e.get('kind') in ('rule', 'prose')):
            out.append({'id': entry_id(e['rule']), 'rule': e['rule'], 'kind': e['kind'],
                        'reason': e['reason'] if isinstance(e.get('reason'), str) else '',
                        'added': e['added'] if isinstance(e.get('added'), str) else ''})
    return out, None


@contextmanager
def locked(agent):
    """The one way to change allowed.json: an exclusive lock, then the file replaced whole."""
    f = file_of(agent)
    f.parent.mkdir(parents=True, exist_ok=True)
    with open(str(f) + '.lock', 'a') as lk:
        fcntl.flock(lk, fcntl.LOCK_EX)
        yield f


def write(f, entries):
    tmp = '%s.%d.tmp' % (f, os.getpid())
    with open(tmp, 'w') as out:
        json.dump({'version': 1, 'allowed': entries}, out, indent=1)
        out.write('\n')
    os.replace(tmp, f)


def add(agent, kind, rule, reason=''):
    """Saves one entry; (entry, True) when it is new, (entry, False) when the rule is already there."""
    if kind not in ('rule', 'prose') or not isinstance(rule, str) or not rule.strip() or len(rule) > MAX_RULE or '\0' in rule:
        raise ValueError('not a usable rule')
    with locked(agent) as f:
        entries, bad = read_file(f)
        if bad and f.exists():
            raise ValueError(bad)
        for e in entries:
            if e['rule'] == rule:
                return e, False
        e = {'id': entry_id(rule), 'rule': rule, 'kind': kind, 'reason': str(reason)[:200],
             'added': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())}
        write(f, entries + [e])
        return e, True


def remove(agent, eid):
    """True when an entry with that id (or the start of it) was removed."""
    with locked(agent) as f:
        entries, bad = read_file(f)
        if bad and f.exists():
            raise ValueError(bad)
        hit = [e for e in entries if e['id'] == eid or (len(eid) >= 4 and e['id'].startswith(eid))]
        if len(hit) != 1:
            return False
        write(f, [e for e in entries if e is not hit[0]])
        return True


def for_settings(f):
    """(permission rules, prose entries, problem) a run's settings take from an allowed.json."""
    entries, bad = read_file(f)
    return ([e['rule'] for e in entries if e['kind'] == 'rule'], [e['rule'] for e in entries if e['kind'] == 'prose'], bad)


# ---- what may be remembered, and as what

def _first_words(command):
    try:
        return shlex.split(command)
    except ValueError:
        return None


def prose(agent, tool, tool_input):
    action = json.dumps(tool_input, sort_keys=True, ensure_ascii=False)
    return ('The owner approved the agent "%s" doing exactly this one thing with %s: %s. It is allowed, with that input only; '
            'it covers nothing else, not a similar command, another path or another agent.' % (agent, tool, action))


def derive(agent, tool, tool_input, reason=''):
    """What "Always allow" would save for this call: {"ok": True, "kind", "rule"} or {"ok": False, "why"}.

    Narrow by construction: an exact command, one file, one host, one named MCP tool; a prose entry (naming the agent and the
    exact input) for what a permission rule cannot hold. Refused: anything in a must-ask class (kith_guard.must_ask: a
    secret, a trust file, a delete, something outward, code with no name), any delete (a remembered rule has no roots to
    keep it inside), a bare or code-running shell, a folder, a rule that would be a wildcard. A sentence
    is not offered when `reason` (the classifier's) names a hard_deny rule: it would never take effect."""
    d = _derive(agent, tool, tool_input)
    if d['ok'] and d['kind'] == 'prose' and any(h in (reason or '') for h in HARD_DENY):
        return {'ok': False, 'why': 'the classifier blocks this under a rule a remembered sentence cannot lift'}
    return d


def _derive(agent, tool, tool_input):
    no = lambda why: {'ok': False, 'why': why}  # noqa: E731
    if not AGENT.match(agent or '') or not isinstance(tool, str) or not tool or len(tool) > 200:
        return no('not a call to remember')
    if not isinstance(tool_input, dict):
        tool_input = {}
    if any(tool.startswith(p) for p in NEVER_TOOLS):
        return no("kith's own approval tools are never remembered")
    text = json.dumps(tool_input, ensure_ascii=False)
    if len(text) > MAX_RULE - 400:
        return no('it is too long to remember exactly')
    c = kith_guard.must_ask(tool, tool_input, places=kith_guard.Places(package=home_of(agent) / 'role'))
    if c:
        return no(WHY[c])
    if tool == 'Bash':
        return _derive_bash(agent, tool_input)
    if tool in EDIT_TOOLS:
        return _derive_edit(agent, tool, tool_input)
    if tool == 'WebFetch':
        from urllib.parse import urlsplit
        try:
            u = urlsplit(str(tool_input.get('url', '')))
            host = u.hostname
        except ValueError:
            return no('not a plain web address')
        if u.scheme not in ('http', 'https') or u.username or u.password or not host or not kith_guard.valid_domain(host):
            return no('not a plain web address')
        return {'ok': True, 'kind': 'rule', 'rule': 'WebFetch(domain:%s)' % host}
    if tool.startswith('mcp__'):
        if not re.fullmatch(r'mcp__[A-Za-z0-9_.-]+__[A-Za-z0-9_.-]+', tool):
            return no('not a plain tool name')
        return {'ok': True, 'kind': 'rule', 'rule': tool}
    # Anything else (a web search, a subagent, Monitor...): a rule would be dropped or too wide.
    return {'ok': True, 'kind': 'prose', 'rule': prose(agent, tool, tool_input)}


def _derive_bash(agent, tool_input):
    no = lambda why: {'ok': False, 'why': why}  # noqa: E731
    command = tool_input.get('command')
    if not isinstance(command, str) or not command.strip() or '\0' in command:
        return no('no command')
    command = command.strip()
    words = _first_words(command)
    if words is None or not words:
        return no('not a command that can be remembered exactly')
    first = os.path.basename(words[0])
    if re.search(r'(?:^|[\s;&|(`/])(?:rm|rmdir|unlink|trash|truncate)\s', command):
        return no(WHY['destroy'])
    if first in NEVER_FIRST:
        return no('a command that runs another command is asked each time')
    if first in RUNNERS and first not in PKG_ALWAYS and (len(words) == 1 or any(w in ('-c', '-e', '-i', '--eval', 'exec', 'run', '-m') for w in words[1:])):
        return no('a shell or interpreter given code to run is asked each time')
    if first in PKG_ALWAYS or (first in PKG_RUN and any(w in PKG_RUN_VERBS for w in words[1:2])):
        return {'ok': True, 'kind': 'prose', 'rule': prose(agent, 'Bash', {'command': command})}
    if first in NETWORK or URL.search(command) or CC_PROTECTED.search(command) or first == 'git' and \
            len(words) > 1 and words[1] in ('push', 'fetch', 'pull', 'clone', 'remote', 'config'):
        # A permission rule would not remember it: Claude Code sends these to the classifier whatever allows them.
        return {'ok': True, 'kind': 'prose', 'rule': prose(agent, 'Bash', {'command': command})}
    if COMPOUND.search(command) or '(' in command or ')' in command or ':*' in command:
        # A permission rule is matched piece by piece (and * is a wildcard), so the whole line cannot be one exact rule.
        return {'ok': True, 'kind': 'prose', 'rule': prose(agent, 'Bash', {'command': command})}
    return {'ok': True, 'kind': 'rule', 'rule': 'Bash(%s)' % command}


def _derive_edit(agent, tool, tool_input):
    no = lambda why: {'ok': False, 'why': why}  # noqa: E731
    raw = tool_input.get('file_path') or tool_input.get('notebook_path')
    if not isinstance(raw, str) or not raw.startswith('/') or '\0' in raw or '*' in raw or '?' in raw or '[' in raw:
        return no('not one absolute file')
    real = os.path.realpath(raw)
    if os.path.isdir(real) or raw.endswith('/'):
        return no('not one absolute file')
    home = str(Path.home())
    if any(kith_guard.is_rule_w(home, x) for x in (raw, real)):
        return {'ok': True, 'kind': 'prose', 'rule': prose(agent, tool, {'file_path': real})}
    return {'ok': True, 'kind': 'rule', 'rule': 'Edit(/%s)' % real}


def deny_rules(agent_home_files):
    """Edit deny rules for allowed.json files, under the logical and the resolved path."""
    out = []
    for f in agent_home_files:
        for x in dict.fromkeys((str(f), os.path.realpath(str(f)))):
            r = 'Edit(/%s)' % x
            if r not in out:
                out.append(r)
    return out


if __name__ == '__main__':
    sys.exit('kith_allowed is a library; use bin/kith-allow')
