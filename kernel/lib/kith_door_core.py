"""kith_door_core: a guest door's start and sweep on the core's `doors.*` table, when this Mac is switched (M2(b)).

kernel/lib/kith_door.py keeps a claim file per door and takes kith-auth's flock around a start and a sweep. On a switched Mac
(auth.json is the authority tombstone) a revoke commits in the core and does not take that flock, so the file cannot make
"check, then claim" one step. The core can: `doors.start` reads the person and writes the row in one transaction, and the
revoke's own transaction marks the person's open rows swept. This module is the thin call path (`kith kernel core-call`, the
one kith-auth's router uses) and the one rule for a call that gets no answer:

    A call of the core has three outcomes, never two. `ok` (an answer), `refused` (the core said no; nothing ran) and
    `unknown` (no daemon, a timeout, a lost or unreadable answer). Unknown is not "revoked" and not "allowed":
      - a start on unknown does NOT start (nothing opens for a person the core did not clear);
      - a sweep's recheck on unknown leaves running doors alone and is asked again on the next pass (a definite "revoked", a
        named person, a gone supervisor still end a door without asking the core).

Only a person's own door goes through here. A room door (`room-<id>`, a basis of people) keeps the file path: the core's row
has no basis. Doors started before the switch have a claim file and no row, and the file path sweeps them.
"""
import json
import os
import subprocess
from collections import namedtuple
from pathlib import Path

import kith_tombstone
from kith_paths import guest_sandbox, kith_bin, paths

KERNEL = Path(__file__).resolve().parent.parent
# What the core reads of the caller's view to find the agent folders (as kith-auth's router sends it), and the serve folder
# (the sync lease). Never KITH_PERSON: a door's person is a parameter, and a caller's person would make the core judge the owner's call as theirs.
ENV_NAMES = ('KITH_STATE', 'HQ_STATE', 'KITH_HOME', 'HQ_HOME', 'KITH_AGENTS', 'HQ_AGENTS', 'KITH_SERVE_DIR')
# The host kills a sweep at 60 s (channels/hq.rs TOOL_TIMEOUT): the core's sweep and the legacy claims' checks fit inside it.
START_SECS, SWEEP_SECS, QUICK_SECS = 20, 10, 10

# kind: ok | refused | unknown. body: the answer (ok). why: one line (refused, unknown).
Answer = namedtuple('Answer', 'kind body why')
_bin = []


def switched(env=os.environ):
    """Whether this Mac's authority is the core's: auth.json is the tombstone (kith-auth's own test)."""
    f = Path(env.get('KITH_AUTH') or paths()['KITH_SECRETS'] / 'auth.json')
    try:
        kith_tombstone.check(f)
    except kith_tombstone.Tombstoned:
        return True
    return False


def _secs(default):
    """KITH_DOOR_CORE_SECS lowers every call's wait (a test's hung core); never raises it."""
    try:
        return min(default, float(os.environ['KITH_DOOR_CORE_SECS']))
    except (KeyError, ValueError):
        return default


def call(method, params, secs, refuses=False, caller=None, env=os.environ):
    """One call of the core. An Answer; never raises. core-call's exit 4 is "the core refused" for a start (`refuses`), but also
    "no such method" and a bad call (a param the row does not declare), so for every other call it is just no answer.
    `caller`: this call reads the agent folders, so it carries the caller's view (`kernel`, `home`, `env`); markLive and end
    declare only their own params and the core refuses any other."""
    if guest_sandbox():
        return Answer('unknown', None, 'a guest run does not reach the core')
    if not _bin:
        try:
            _bin.append(os.environ.get('KITH_BIN') or kith_bin(budget=4))
        except Exception:
            _bin.append('')
    if not _bin[0]:
        _bin.clear()   # look again next time: the program may be installed by then
        return Answer('unknown', None, 'no kith program to ask')
    body = dict(params)
    if caller:
        body.update({'kernel': str(KERNEL), 'home': str(Path.home()), 'env': {k: env[k] for k in ENV_NAMES if env.get(k)}})
    try:
        r = subprocess.run([_bin[0], 'kernel', 'core-call', method, '--params-stdin'], input=json.dumps(body),
                           capture_output=True, text=True, timeout=_secs(secs))
    except subprocess.TimeoutExpired:
        return Answer('unknown', None, 'the core did not answer in time')
    except (OSError, subprocess.SubprocessError) as e:
        return Answer('unknown', None, str(e))
    if r.returncode == 2:   # core-call does not list the method: a kith program from before doors.*
        return Answer('unknown', None, 'this kith program does not know the core\'s door calls (kith update)')
    if r.returncode == 4 and refuses:   # core-call's: the core refused, nothing ran
        why = (r.stderr or r.stdout).strip().splitlines()
        return Answer('refused', None, (why[-1] if why else 'the core refused').removeprefix('kith-core: doors: '))
    if r.returncode != 0:   # 3 no daemon, 5 a lost answer, anything else
        return Answer('unknown', None, ((r.stderr or r.stdout).strip().splitlines() or [f'core-call exited {r.returncode}'])[-1])
    try:
        ans = json.loads(r.stdout)
    except ValueError:
        ans = None
    if not isinstance(ans, dict) or ans.get('noCore'):
        return Answer('unknown', None, 'the core gave no usable answer')
    return Answer('ok', ans, '')


def start(agent, piece, person, folder, claim, pid, env=os.environ):
    """doors.start: the access check and the row, one step in the core. ok: body is the row (`version`, `state`)."""
    ans = call('doors.start', {'agent': agent, 'door': piece, 'person': person, 'folder': folder, 'id': claim, 'pid': pid},
               START_SECS, refuses=True, caller=True, env=env)
    if ans.kind == 'ok' and not (isinstance(ans.body.get('version'), int) and ans.body.get('id') == claim):
        return Answer('unknown', None, 'the core answered a door that is not this one')
    return ans


def mark_live(claim, version):
    """True when the row went live, False when a sweep (or an end) got to it first (the box must stop), None when unknown."""
    ans = call('doors.markLive', {'id': claim, 'version': version}, QUICK_SECS)
    if ans.kind != 'ok':
        return None if ans.kind == 'unknown' else False
    return {'live': True, 'lost': False}.get(ans.body.get('result'))


def end(claim):
    """doors.end: the box is gone (best effort: a row left open is swept as "supervisor gone")."""
    return call('doors.end', {'id': claim}, QUICK_SECS).kind == 'ok'


def sweep(persons, recheck, env=os.environ):
    """doors.sweep: the rows to stop, each `{id, since (s), pid, gone}` (`gone`: the core swept it because its supervisor is gone): swept by this call, and swept before with the box not yet
    confirmed gone. None when the core gave no answer (unknown: nothing was decided)."""
    ans = call('doors.sweep', {'persons': list(persons), 'noRecheck': not recheck}, SWEEP_SECS,
               caller=True, env=env)
    if ans.kind != 'ok':
        return None
    out = []
    for d in [*(ans.body.get('swept') or []), *(ans.body.get('pending') or [])]:
        if isinstance(d, dict) and isinstance(d.get('id'), str) and d['id'] not in [o['id'] for o in out]:
            started = d.get('startedAt')
            out.append({'id': d['id'], 'pid': d.get('pid') if isinstance(d.get('pid'), int) else 0,
                        'since': started / 1000 if isinstance(started, (int, float)) else 0,
                        'gone': 'supervisor is gone' in str(d.get('why') or '')})
    return out
