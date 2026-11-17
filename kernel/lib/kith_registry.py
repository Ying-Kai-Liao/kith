"""kith_registry: how the kernel's writers of the session registry ($KITH_STATE/live/terminal/<sid>.json) reach the Kith core
once the registry is its (the `transcripts` cutover module).

Two modes, picked as the claims are (kith_claims.switched), never by an env var:

  Files   (no marker, today)    `kith-sessions record` writes <sid>.json itself; kith-sched prunes old files.
  Writer  (the marker exists)   the core is the only writer: the record is `session.record`, and the core writes <sid>.json as
                                a projection of its row. Nothing here writes or prunes a registry file.

The marker is `$KITH_STATE/live/terminal/registry.tombstone` (existence only; the step writes it, the rollback clears it), and
the module's own record `<machine>/core/cutover/transcripts.switch` counts too once it says more than `stepping`.

The write-ahead spool (architect MUST 3): a Writer-mode hook first writes `inbox/<sid>.<recordedAt>.<pid>.json`, then calls the
core, and deletes its own entry only after an answer that settles it (`outcome`, or `refused: invalid`). Anything else (not
serving, no daemon, a timeout, a hook killed at its 5 s limit mid-call) leaves the entry for the core's drain, or for a Files-mode
drain (`drain`) after a rollback. Readers glob top-level *.json and never see the inbox.

The record is the object `kith-sessions record` writes, plus `recordedAt` (time.time_ns()). Order key: (recordedAt, procStart as
a string, arrival) — later wins; a record without recordedAt (an old kernel's hook wrote it) counts its file's st_mtime_ns as its
recordedAt (contract A1, the core's rule too). `order_key` is the only comparison (A6). Every write of <sid>.json and every drain
step runs under the per-sid file lock live/terminal/locks/<sid>.lock (A8), the core's lock too.
"""
import contextlib
import fcntl
import json
import os
import re
import time
from pathlib import Path

MARKER = 'registry.tombstone'
MODULE = 'transcripts'
CALL_SECS = 3   # a SessionStart hook has 5 s in all, and the ps walk comes first
_SAFE = re.compile(r'^[A-Za-z0-9-]{1,64}$')   # the host's registry::safe_id: the file name rule


def claims_lib():
    import kith_claims
    return kith_claims


def reg_dir(state):
    return Path(state) / 'live' / 'terminal'


def inbox(state):
    return reg_dir(state) / 'inbox'


def safe_id(sid):
    return isinstance(sid, str) and bool(_SAFE.match(sid))


def switched(state):
    """Whether the registry is the core's on this Mac: the marker exists, or the transcripts switch file says more than
    `stepping` (switched, rolling-back, unreadable). Same two-source, fail-closed rule as kith_claims.switched."""
    if not state:
        return False
    m = reg_dir(state) / MARKER
    try:
        if os.path.lexists(m):
            return True
    except OSError:
        return True
    f = claims_lib().switch_file(MODULE)
    try:
        if f and os.path.lexists(f):
            return 'state=stepping' not in f.read_text().splitlines()
    except OSError:
        return True
    return False


def order_key(rec, path=None, arrival=0):
    """The one order key (contract A6): (recordedAt, procStart as a string, arrival); later wins. A record without
    recordedAt (an old kernel's hook wrote it) counts its file's st_mtime_ns (A1), or -1 with no file to look at. `arrival`
    is the caller's order of seeing the records (the file first, then the inbox entries by name). Nothing else compares."""
    at = rec.get('recordedAt') if isinstance(rec, dict) else None
    if not isinstance(at, int) or isinstance(at, bool):
        at = -1
        if path is not None:
            try:
                at = os.stat(path).st_mtime_ns
            except OSError:
                pass
    return (at, str((rec or {}).get('procStart') or ''), arrival)


LOCK_WAIT = 1.0   # seconds: A8's bounded wait for the per-sid lock


@contextlib.contextmanager
def sid_lock(state, sid, wait=LOCK_WAIT):
    """The per-sid lock (contract A8): flock(LOCK_EX) on live/terminal/locks/<sid>.lock (folder 0700, file 0600, never
    deleted), the same lock the core's settle, reconcile, prune and export_back take. Yields True when held, False when
    another holder kept it past `wait` seconds (or it could not be opened): the caller skips or fails open."""
    fd = None
    try:
        if not safe_id(sid):
            raise OSError('not a lockable session id')
        d = reg_dir(state) / 'locks'
        d.mkdir(mode=0o700, parents=True, exist_ok=True)
        fd = os.open(d / f'{sid}.lock', os.O_RDWR | os.O_CREAT, 0o600)
        end = time.monotonic() + wait
        while True:
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except BlockingIOError:
                if time.monotonic() >= end:
                    os.close(fd)
                    fd = None
                    break
                time.sleep(0.02)
    except OSError:
        if fd is not None:
            os.close(fd)
        fd = None
    try:
        yield fd is not None
    finally:
        if fd is not None:
            os.close(fd)   # closing drops the flock


def valid(rec, sid):
    """Whether the core would take rec as the record of sid (the contract's Validation, A5): a JSON object, a safe_id sid
    naming this sid, v == 1, no person key."""
    return (isinstance(rec, dict) and safe_id(sid) and rec.get('sessionId') == sid
            and type(rec.get('v')) is int and rec['v'] == 1 and 'person' not in rec)


def _write(path, data):
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    os.chmod(path.parent, 0o700)
    tmp = path.with_name(f'.{path.name}.tmp')
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, 'w') as f:
        json.dump(data, f)
    os.replace(tmp, path)


def spool(state, rec, pid=None):
    """Write the record's spool entry `inbox/<sid>.<recordedAt>.<pid>.json` atomically (0700 folder, 0600 file); its path."""
    reg_dir(state).mkdir(mode=0o700, parents=True, exist_ok=True)
    path = inbox(state) / f"{rec['sessionId']}.{rec['recordedAt']}.{pid or os.getpid()}.json"
    _write(path, rec)
    return path


def call(rec, timeout=CALL_SECS):
    """`session.record` for rec. 'settled' (the core has it, or newer: delete the spool), 'invalid' (the core will never take it:
    delete, drop), else 'keep' with the reason (not serving, no answer, a timeout, a rejected call: the spool stays)."""
    k = claims_lib()
    try:
        ans = k.call('session.record', {'record': rec}, timeout=timeout)
    except k.Refused as e:
        return 'keep', f'the core rejected the call: {e}'
    except k.NoAnswer as e:
        return 'keep', e.why
    if ans.get('outcome') in ('recorded', 'older'):
        return 'settled', ans['outcome']
    if ans.get('refused') == 'invalid':
        return 'invalid', ans.get('why') or 'invalid record'
    return 'keep', ans.get('why') or ans.get('refused') or 'no outcome in the answer'


def entries(state):
    """The inbox's entries as (path, sid), by name. Names that start with `.` (temp files) or do not end in `.json` are
    skipped (A9); any other name is an entry, its sid the part before the first dot (None when the name is not
    <sid>.<recordedAt>.<pid>.json with a safe sid: the drain deletes it as invalid)."""
    try:
        names = sorted(os.listdir(inbox(state)))
    except OSError:
        return []
    out = []
    for n in names:
        if n.startswith('.') or not n.endswith('.json'):
            continue
        parts = n.split('.')
        out.append((inbox(state) / n, parts[0] if len(parts) == 4 and safe_id(parts[0]) else None))
    return out


def _read(path, any_json=False):
    try:
        v = json.loads(path.read_text())
    except (OSError, ValueError):
        return None
    return v if any_json or isinstance(v, dict) else None


def drain(state):
    """Files mode only: put each inbox entry newer (order_key) than its <sid>.json in place of it, delete the rest, each
    under the sid's lock (A8; a lock not had within LOCK_WAIT skips that entry for the next drain). An entry the core would
    refuse as invalid (`valid`: not an object, a bad sid or name, another sid, v != 1, a person key) is deleted, never
    placed (A5). Returns how many moved. In Writer mode it does nothing: the core drains."""
    if switched(state):
        return 0
    moved = 0
    for n, (path, sid) in enumerate(entries(state), start=1):
        try:
            if sid is None:
                path.unlink()
                continue
            with sid_lock(state, sid) as held:
                if not held:
                    continue
                rec = _read(path, any_json=True)
                if not valid(rec, sid):
                    path.unlink()
                    continue
                target = reg_dir(state) / f'{sid}.json'
                have = _read(target) if target.exists() else None
                if have is None or order_key(rec, path, n) > order_key(have, target, 0):
                    os.chmod(path, 0o600)
                    os.replace(path, target)
                    moved += 1
                else:
                    path.unlink()
        except OSError:   # another drainer took it first
            pass
    return moved
