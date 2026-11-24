"""kith_wake_core: how the kernel's wake callers reach the Kith core once the wake queue is its (retire-python S4, R9).

Two modes, picked by files, never by an env var:

  Files   (no marker, today)  $KITH_STATE/sessions/queue/*.json, written by kernel/bin/kith-wake. Nothing here runs.
  Core    (the marker exists)  the queue is rows in the core (wake.* in core/api/src/table/wake.rs). No file under
                               sessions/queue/ is read or written by any caller.

The marker is `$KITH_STATE/sessions/queue/.tombstone`, which `kith migrate step wake` writes after the import and
the rollback removes; this module only reads it. The module's own record, `<machine>/core/cutover/wake.switch`, counts too
once it says more than `stepping` (as kith_claims reads the claims'): Core mode is either source, and a marker that cannot
even be looked at still means Core (fail closed: a caller never goes back to the files by accident).

A call to the core is `kith kernel core-call METHOD --params-stdin` (kith_claims.call). With the queue the core's, a core
that does not answer is a refusal: the caller says so and writes nothing. There is no fallback to the files, which a
tombstoned queue must not be written beside.

kith-wake's helpers (enqueue, withdraw, withdraw_person, status) forward here, so kith-ask, kith-sessions and `kith wake`
reach the core through the one door, and `via_core` runs kith-wake's other verbs. The host ticks the queue (wake.tick), so
`drain` and `deliver` do nothing here.
"""
import json
import os
import re
from pathlib import Path

import kith_claims

TOMBSTONE = ('sessions', 'queue', '.tombstone')


def switched(state):
    """Whether the wake queue is the core's on this Mac: the tombstone, or the module's switch record past `stepping`."""
    t = Path(state).joinpath(*TOMBSTONE) if state else None
    try:
        if t and os.path.lexists(t):
            return True
    except OSError:
        return True
    f = kith_claims.switch_file('wake')
    try:
        if f and os.path.lexists(f):
            return 'state=stepping' not in f.read_text().splitlines()
    except OSError:
        return True
    return False


def call(method, params):
    """One owner call of the core, as the answer object. Exits (SystemExit, as kith-wake refuses everything) when the core
    rejects it or does not answer: nothing was written."""
    try:
        return kith_claims.call(method, params)
    except kith_claims.Refused as e:
        raise SystemExit('kith-wake: %s' % re.sub(r'^kith-core: ', '', str(e)))
    except kith_claims.NoAnswer as e:
        raise SystemExit('kith-wake: the wake queue is the Kith core\'s on this Mac and it did not answer (%s); nothing was done'
                         % e)


def enqueue(session, rid, agent, text, person=None, room=None, door='run', why=''):
    params = {'session': session, 'id': rid, 'agent': agent, 'text': [text], 'door': door, 'why': why}
    if person:
        params['person'] = person
    if room:
        params['room'] = room
    return call('wake.enqueue', params)['result']


def withdraw(session, rid=None):
    return call('wake.withdraw', {'session': session, **({'id': rid} if rid else {})})['count']


def withdraw_person(persons):
    return call('wake.withdrawPerson', {'persons': list(persons)})['count']


def status(rid):
    a = call('wake.queueStatus', {'id': rid})
    return a['state'], a.get('run')


def _say(answer):
    for line in answer.get('lines') or []:
        print(line)


def via_core(a):
    """kith-wake's verbs the helpers do not cover, for the parsed arguments `a`; returns the exit code."""
    if a.cmd in ('drain', 'deliver'):
        return 0   # the host drives wake.tick; a tick or a delivery from here would be a second runner
    if a.cmd == 'take':
        turn = call('wake.take', {'holder': a.holder, **({'session': a.session} if a.session else {}),
                                  **({'sid': a.sid} if a.sid else {})}).get('turn')
        if not turn:
            return 1
        print(json.dumps(turn, ensure_ascii=False))
    elif a.cmd == 'done':
        return 0 if call('wake.done', {'run': a.run, 'how': a.how})['done'] else 1
    elif a.cmd == 'back':
        return 0 if call('wake.back', {'run': a.run, 'held': a.held})['done'] else 1
    elif a.cmd == 'wait':
        params = {'on': a.on, 'timeout': a.timeout, 'why': a.why, 'note': a.note,
                  'via': os.environ.get('KITH_VIA') or 'terminal'}
        for key, val in (('session', a.session), ('agent', a.agent), ('person', os.environ.get('KITH_PERSON'))):
            if val:
                params[key] = val
        _say(call('wake.wait', params))
    elif a.cmd == 'ls':
        got = call('wake.queueList', {})
        if a.json:
            print(json.dumps(got.get('items') or [], ensure_ascii=False, indent=1))
        else:
            _say(got)
    return 0
