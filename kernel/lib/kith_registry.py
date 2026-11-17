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
a string, arrival) — later wins; a record without recordedAt is older than any with one.
"""
import json
import os
import re
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


def order_key(rec):
    """(recordedAt, procStart) of a record; one without recordedAt sorts before any with one."""
    at = rec.get('recordedAt') if isinstance(rec, dict) else None
    at = at if isinstance(at, int) and not isinstance(at, bool) else -1
    return (at, str((rec or {}).get('procStart') or ''))


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
    """The inbox's entries as (path, sid), oldest name first. Temp files (a leading dot) and other names are skipped."""
    try:
        names = sorted(os.listdir(inbox(state)))
    except OSError:
        return []
    out = []
    for n in names:
        parts = n.split('.')
        if n.startswith('.') or len(parts) != 4 or parts[3] != 'json' or not safe_id(parts[0]):
            continue
        out.append((inbox(state) / n, parts[0]))
    return out


def _read(path):
    try:
        v = json.loads(path.read_text())
    except (OSError, ValueError):
        return None
    return v if isinstance(v, dict) else None


def drain(state):
    """Files mode only: put each inbox entry newer (order key) than its <sid>.json in place of it, delete the rest. An entry
    that is not a record of its own sid is deleted. Returns how many moved. In Writer mode it does nothing: the core drains."""
    if switched(state):
        return 0
    moved = 0
    for path, sid in entries(state):
        rec = _read(path)
        try:
            if rec is None or rec.get('sessionId') != sid:
                path.unlink()
                continue
            target = reg_dir(state) / f'{sid}.json'
            have = _read(target) if target.exists() else None
            if have is None or order_key(rec) > order_key(have):
                os.chmod(path, 0o600)
                os.replace(path, target)
                moved += 1
            else:
                path.unlink()
        except OSError:   # another drainer took it first
            pass
    return moved
