"""The session lifecycle's parts that need no claim (F12, designs/2026-10-24-lifecycle.md): intent before signal, the idle
clock, outstanding work, and the 54-minute decision. bin/kith-sessions (the wrapper that stops its own child) and
bin/kith-lifecycle (the sweep that compacts or ends sessions nobody runs) use it; the host's Rust side writes the same
intent file (serve/host/src/terminal/intent.rs).

Files under $KITH_STATE/sessions/:
  intent/<sid>.json     {"why", "at", "pid", "procStart"}: written by whoever is about to end the process, before the signal
                        (rules 8, 14). A process taking the session's writer claim clears it; a session gone with no intent
                        is a crash. Sessions from before this file existed have none: that reads as "unknown", never as ended.
  decisions/<sid>.json  {"decision": compact|sleep|end, "at", "idleSince", "compacted"}: the 54-minute decision of one idle
                        period (idleSince = the last turn it was made for). Any resume clears it (rule 12), and a new turn
                        makes it stale because the period it names is over.

Every judgment here is structural (sizes, ids, timestamps); nothing calls a model.
"""
import datetime as dt
import json
import os
import re
import subprocess
import time
from pathlib import Path

FLOOR_TOKENS = 100_000            # a transcript below this is not worth a /compact (rule 5; the wrapper sets Claude's own floor to it)
DEFAULT_CACHE_LIFE = 3600         # seconds: a subscription's 1 h cache. An API key's is 5 min: no timer then.
SHORT_CACHE = 300
DECIDE_AT = 0.9                   # of the cache lifetime: the earliest a decision is made (rule 5)
DEFAULT_SLEEP_AFTER = 3600        # a human session idle this long with nothing outstanding is stopped by its wrapper (rule 3)
TAIL_BYTES = 4_000_000


def state_dir():
    s = os.environ.get('KITH_STATE')
    return Path(s) if s else None


def sessions_dir(sub):
    s = state_dir()
    return s / 'sessions' / sub if s else None


def now():
    return time.time()


def cache_life():
    """Seconds the prompt cache lives (KITH_CACHE_LIFE, e.g. 300 for an API key); default one hour."""
    try:
        return max(0, int(os.environ.get('KITH_CACHE_LIFE', DEFAULT_CACHE_LIFE)))
    except ValueError:
        return DEFAULT_CACHE_LIFE


def sleep_after():
    """Seconds of idle after which a wrapper stops its own child (KITH_SLEEP_MINUTES, default 60)."""
    try:
        return max(1.0, float(os.environ.get('KITH_SLEEP_MINUTES', '')) * 60)
    except ValueError:
        return DEFAULT_SLEEP_AFTER


def _write(path, data):
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    tmp = path.with_name(f'.{path.name}.{os.getpid()}.tmp')
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, 'w') as f:
        json.dump(data, f)
    os.replace(tmp, path)


def _read(path):
    try:
        v = json.loads(path.read_text())
        return v if isinstance(v, dict) else None
    except (OSError, ValueError):
        return None


def safe(sid):
    return bool(sid) and re.fullmatch(r'[A-Za-z0-9-]{1,64}', sid) is not None


def proc_start(pid):
    """`ps` lstart in the C locale, squeezed: the text the claims carry."""
    try:
        out = subprocess.run(['ps', '-o', 'lstart=', '-p', str(pid)], capture_output=True, text=True, timeout=3,
                             env={**os.environ, 'LC_ALL': 'C'}).stdout
    except Exception:  # noqa: BLE001
        return None
    return ' '.join(out.split()) or None


# --- intent (rules 8, 14) ------------------------------------------------------------------------------------------------

def write_intent(sid, why, pid):
    """Records that the process `pid` of session sid is about to be ended on purpose, and why. Always before the signal."""
    d = sessions_dir('intent')
    if d and safe(sid):
        _write(d / f'{sid}.json', {'why': why, 'at': dt.datetime.now(dt.timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ'),
                                   'pid': pid, 'procStart': proc_start(pid)})


def read_intent(sid):
    d = sessions_dir('intent')
    return _read(d / f'{sid}.json') if d and safe(sid) else None


def clear_intent(sid):
    d = sessions_dir('intent')
    if d and safe(sid):
        (d / f'{sid}.json').unlink(missing_ok=True)


def how_it_ended(sid, had_process=True, pid=None, start=None):
    """Why a session with no process stopped: the intent's reason ('idle', 'quit', 'revoked', ...), or 'crash' when a process
    ran and ended with none (SessionEnd always says "other", so only the intent tells). The intent must be for the process
    that ended (its pid, and its start time when both sides know it): an older one is not this process' reason. A session we
    never saw run (old data, from before intents) is None: nothing is claimed about it."""
    i = read_intent(sid)
    if i and (pid is None or i.get('pid') is None or i['pid'] == pid) \
            and not (start and i.get('procStart') and ' '.join(start.split()) != i['procStart']):
        return i.get('why')
    return 'crash' if had_process else None


# --- decisions (rules 5, 11, 12) -----------------------------------------------------------------------------------------

def read_decision(sid):
    d = sessions_dir('decisions')
    return _read(d / f'{sid}.json') if d and safe(sid) else None


def write_decision(sid, rec):
    d = sessions_dir('decisions')
    if d and safe(sid):
        _write(d / f'{sid}.json', rec)


def clear_decision(sid):
    d = sessions_dir('decisions')
    if d and safe(sid):
        (d / f'{sid}.json').unlink(missing_ok=True)


def taken(sid):
    """A process took the session's writer claim: the intent it ends (rule 14) and the decision it answers (rule 12) are over.
    Best effort: the claim must never fail on bookkeeping."""
    try:
        clear_intent(sid)
        clear_decision(sid)
    except OSError:
        pass


# --- the transcript ------------------------------------------------------------------------------------------------------

def _tail_lines(transcript, limit=TAIL_BYTES):
    try:
        with open(transcript, 'rb') as f:
            f.seek(0, os.SEEK_END)
            size = f.tell()
            f.seek(max(0, size - limit))
            raw = f.read()
    except OSError:
        return []
    lines = raw.decode('utf-8', 'replace').splitlines()
    if size > limit and lines:
        lines = lines[1:]   # the first may be cut
    out = []
    for line in lines:
        try:
            e = json.loads(line)
        except ValueError:
            continue
        if isinstance(e, dict):
            out.append(e)
    return out


def _text(msg):
    c = (msg or {}).get('content')
    if isinstance(c, str):
        return c
    return '\n'.join(b.get('text') or '' for b in c or [] if isinstance(b, dict) and b.get('type') == 'text')


def is_turn(e):
    """A user or model turn, not what a compaction or a command writes: the summary, its boundary, command echoes, hook
    output. A compaction's own write must not restart the idle clock (rule 4)."""
    if e.get('type') not in ('user', 'assistant') or e.get('isSidechain') or e.get('isMeta') or e.get('isCompactSummary'):
        return False
    if e.get('type') == 'user':
        t = _text(e.get('message')).lstrip()
        if t.startswith(('<command-name>', '<local-command', '<system-reminder>')):
            return False
    return True


def when(e):
    try:
        return dt.datetime.fromisoformat(str(e.get('timestamp')).replace('Z', '+00:00')).timestamp()
    except ValueError:
        return None


def last_turn_at(transcript):
    """Epoch seconds of the last user or model turn (from the entries' own timestamps), else None."""
    for e in reversed(_tail_lines(transcript)):
        if is_turn(e) and when(e):
            return when(e)
    return None


def context_tokens(transcript):
    """What the last model request read: input + cached + new cache tokens of the last assistant turn that reports usage;
    else a quarter of the file's bytes. After a compaction the next turn reports the smaller number."""
    for e in reversed(_tail_lines(transcript)):
        u = ((e.get('message') or {}).get('usage')) if e.get('type') == 'assistant' else None
        if u:
            return sum(int(u.get(k) or 0) for k in ('input_tokens', 'cache_read_input_tokens', 'cache_creation_input_tokens'))
    try:
        return os.path.getsize(transcript) // 4
    except OSError:
        return 0


def compacted_since(transcript, t):
    """Whether a compaction (Claude's own, at 54 min, or ours) wrote its boundary at or after epoch t."""
    for e in reversed(_tail_lines(transcript)):
        if e.get('type') == 'system' and e.get('subtype') == 'compact_boundary':
            w = when(e)
            return w is not None and w >= t - 1
        if is_turn(e) and (when(e) or 0) < t - 1:
            return False
    return False


def asked_a_question(transcript):
    """The last model turn ends by asking the person something (its text ends with a question mark)."""
    for e in reversed(_tail_lines(transcript)):
        if e.get('type') == 'assistant' and is_turn(e):
            t = _text(e.get('message')).rstrip()
            return t.endswith(('?', '？'))
    return False


def _blocks(e):
    c = (e.get('message') or {}).get('content')
    return [b for b in c if isinstance(b, dict)] if isinstance(c, list) else []


def pending_work(transcript):
    """What the transcript shows still running: tool calls with no result yet (a running tool, a running subagent), and
    background shells started with no <task-notification> since (rule 7). Returns a list of short reasons."""
    started, results, shells, notified = {}, set(), set(), set()
    for e in _tail_lines(transcript):
        for b in _blocks(e):
            if b.get('type') == 'tool_use' and b.get('id'):
                started[b['id']] = b.get('name') or 'tool'
            elif b.get('type') == 'tool_result':
                results.add(b.get('tool_use_id'))
                body = b.get('content')
                text = body if isinstance(body, str) else ' '.join(x.get('text') or '' for x in body or [] if isinstance(x, dict))
                shells.update(re.findall(r'running in background with ID: (\w+)', text))
        t = _text(e.get('message')) if e.get('type') == 'user' else ''
        if '<task-notification>' in t:
            notified.update(re.findall(r'<task-id>(\w+)</task-id>', t))
    out = []
    for tid, name in started.items():
        if tid not in results:
            out.append('a subagent is running' if name in ('Task', 'Agent') else f'{name} has not returned')
    out += [f'background shell {s} has not reported' for s in sorted(shells - notified)]
    return out


def sidecars(transcript):
    """Claude's MCP task sidecars of the session: <projects>/<slug>/<sid>/mcp-tasks/*.meta.json (deleted once delivered)."""
    t = Path(transcript)
    return sorted((t.with_suffix('') / 'mcp-tasks').glob('*.meta.json'))


def detached_children(pid):
    """Descendants of pid that live in another session than pid's own: a dev server or build that outlives the shell
    that started it. Unsure counts as busy (the same rule as the host's idle check)."""
    try:
        rows = subprocess.run(['ps', '-A', '-o', 'pid=,ppid='], capture_output=True, text=True, timeout=5).stdout.split('\n')
        kids = {}
        for r in rows:
            f = r.split()
            if len(f) == 2:
                kids.setdefault(int(f[1]), []).append(int(f[0]))
        own, found, stack = os.getsid(pid), [], [pid]
        while stack:
            for c in kids.get(stack.pop(), ()):
                try:
                    if os.getsid(c) != own:
                        found.append(c)
                except OSError:
                    continue   # gone since the listing
                stack.append(c)
        return found
    except Exception:  # noqa: BLE001
        return [-1]


def outstanding(transcript, pid=None):
    """Why the session must not be signalled now (rule 7); empty when nothing is outstanding."""
    why = pending_work(transcript) if transcript else []
    if transcript and sidecars(transcript):
        why.append('an MCP task is open')
    if pid and detached_children(pid):
        why.append('a child process runs detached')
    return why


# --- the idle clock (rule 4 and the seen-in-the-model note) ----------------------------------------------------------------

def idle_since(transcript, started, keystroke=None):
    """The latest of the last user or model turn, the process start and the last keystroke (else a terminal resumed after
    an idle hour would be stopped before Kai types). A compaction's own write is not a turn."""
    return max(t for t in (last_turn_at(transcript) if transcript else None, started, keystroke) if t is not None)


# --- the decision (rule 5) --------------------------------------------------------------------------------------------------

def decide(transcript, kind, waiting, thread=None, at=None, life=None):
    """compact | sleep | end | None (too early or no timer), from structural signals only.

    kind: the registry's run kind (terminal, ask, schedule, helper). waiting: a parked card, a waiter or a wake request is
    still open for the session. thread: what the host says of its thread, {"open", "pinned", "closed"} (or None).
    The earliest is 0.9 of the cache lifetime after the last turn, and never on a 5-minute cache: there the caller asks
    at the moment the session goes to sleep (at=idle_since + life)."""
    life = cache_life() if life is None else life
    since = last_turn_at(transcript)
    if since is None:
        return None
    t = now() if at is None else at
    if life <= SHORT_CACHE:
        if at is None:
            return None
    elif t - since < DECIDE_AT * life:
        return None
    thread = thread or {}
    question = asked_a_question(transcript)
    if thread.get('closed') or (kind in ('ask', 'schedule', 'helper') and not question and not waiting):
        return 'end'
    big = context_tokens(transcript) >= FLOOR_TOKENS
    returns = waiting or question or thread.get('open') or thread.get('pinned')
    return 'compact' if big and returns else 'sleep'
