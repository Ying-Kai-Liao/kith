"""kith_claims: how the kernel's writer-claim takers (kith-sessions, kith-wake) reach the Kith core once the claims are its.

Two modes, picked by one marker, never by an env var:

  Files   (no marker, today)      $KITH_STATE/live/terminal/<sid>.claim files; nothing in this module runs.
  Writer  (the marker exists)     the claims are rows in the core (claim.external / claim.release / claim.sweepPerson /
                                  claim.list, core/runtime/src/conversations/external.rs). No claim file is read or written.

The marker is `$KITH_STATE/live/terminal/claims.tombstone`. `kith migrate step claims` (P9) writes it and the rollback
clears it; this module only reads it. Its mere existence is the switch: a damaged or unreadable marker still means Writer
(fail closed: a taker never goes back to the files by accident).

A call to the core is `kith kernel core-call METHOD --params-stdin` (the daemon over the owner socket; the claim rows are
never run in an embedded core). Its exit code says what happened, and the callers tell these apart on purpose:

  answered   0   the core said something (an answer can be a refusal as data: {"refused": "held", ...})
  refused    4   the core rejected the call (a bad param, a row that is not open to the caller); nothing ran
  no answer  3,5 no daemon, no kith program, a timeout, a lost answer; a claim may or may not have been made

A person's run with no answer refuses; the owner's `--direct` run goes on under the conversation's stub lock alone
(designs/2026-11-14-m3-cutover section 1.6): kith-sessions decides that, per caller.

The marker is also the claims module's tombstone for the core (`cutover/modules/claims.rs`, P9): the `claim.*` rows serve only
while that module is switched, so a marker with no switch behind it (a step part-way, a forged file) makes every call here
a refusal, never a claim a Files-mode host cannot see.
"""
import json
import os
import re
import subprocess
from pathlib import Path

MARKER = 'claims.tombstone'
CALL_SECS = 8   # one claim call: a daemon answers in milliseconds; this only bounds a hung socket
_LOCKABLE = re.compile(r'^[a-z0-9._-]{1,128}$')


class NoAnswer(Exception):
    """The core did not answer (or the answer was lost). `code` is core-call's exit code, 3 when nothing ran."""

    def __init__(self, why, code=3):
        super().__init__(why)
        self.why, self.code = why, code


class Refused(Exception):
    """The core answered with an error: the call was rejected and nothing ran."""


def marker_path(state):
    return Path(state) / 'live' / 'terminal' / MARKER if state else None


def switched(state):
    """Whether the claims are the core's on this Mac. Existence only: the content is P9's business."""
    m = marker_path(state)
    try:
        return bool(m) and os.path.lexists(m)
    except OSError:
        return True   # a marker we cannot even look for is no reason to fall back to the files


def lockable(sid):
    """The stub's key rule: a conversation whose id breaks it runs without the writer lock, so Writer mode refuses it."""
    return bool(_LOCKABLE.match(sid or '')) and sid not in ('.', '..')


def call(method, params):
    """One owner call of the core. Returns the answer object; raises Refused (the core rejected it) or NoAnswer."""
    try:
        from kith_paths import kith_bin
        bin_ = kith_bin(budget=CALL_SECS)
    except Exception:  # noqa: BLE001 — no way to look is no answer
        bin_ = ''
    if not bin_:
        raise NoAnswer('no kith program to ask')
    try:
        r = subprocess.run([bin_, 'kernel', 'core-call', method, '--params-stdin'], input=json.dumps(params),
                           capture_output=True, text=True, timeout=CALL_SECS)
    except subprocess.TimeoutExpired:
        raise NoAnswer('the core did not answer in time', 5)
    except OSError as e:
        raise NoAnswer(str(e))
    why = (r.stderr or r.stdout).strip()
    if r.returncode == 4:
        raise Refused(why or 'refused')
    if r.returncode != 0:
        raise NoAnswer(why or f'core-call exited {r.returncode}', r.returncode)
    try:
        ans = json.loads(r.stdout)
    except ValueError:
        ans = None
    if not isinstance(ans, dict):
        raise NoAnswer('the answer is not a JSON object', 5)
    return ans
