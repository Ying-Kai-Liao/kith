"""Process start stamps, the Python side of core/procstart (the Rust crate `kith-procstart`): the same string for the same
process, so a claim written by either side reads right on the other. A start is a stamp compared with plain equality:

  macOS (and Windows, where Python does not run these tools): `e<epoch ms>`, the process' own creation time
  Linux: `b<boot id>:<ticks>`, /proc/sys/kernel/random/boot_id and /proc/<pid>/stat field 22

Never `ps -o lstart=`: local-time text moves with the time zone. Records from before this carry that text (or nothing);
`same_process` reads those as an unknown start: the same process, because a wrong "different" would free a live writer.
A form mismatch or an unreadable start is never "dead"; a process is gone only when the system says so (ESRCH, a zombie).

`python3 kith_procstart.py <pid>` prints the stamp (nothing when it can't be read); the parity test in
core/procstart/tests runs it against the Rust reading.
"""
import errno
import os
import re
import sys

GONE, ALIVE, UNKNOWN = 'gone', 'alive', 'unknown'


def epoch_stamp(ms):
    return f'e{ms}'


def boot_stamp(boot_id, ticks):
    return f'b{boot_id.strip()}:{ticks}'


def form(s):
    """'e' or 'b' for a stamp; None for old lstart text, a test marker, or nothing."""
    if not isinstance(s, str):
        return None
    if re.fullmatch(r'e-?[0-9]+', s):
        return 'e'
    if re.fullmatch(r'b[^:]+:[0-9]+', s):
        return 'b'
    return None


def is_stamp(s):
    return form(s) is not None


def comparable(a, b):
    """Both are stamps of one form, so same_process answers from a real comparison. A caller that signals a process on a
    "yes" asks this first: same_process is the safe answer for "may I call it dead", not for "may I kill it"."""
    fa = form(a)
    return fa is not None and fa == form(b)


def same_process(recorded, current):
    """Whether a recorded start and a current reading are one process: equality when both are in one form, and yes when the
    forms differ or the record is unknown (empty, old lstart text, e against b)."""
    return recorded == current if comparable(recorded, current) else True


def parse_linux_stat(stat):
    """(state letter, starttime ticks) from /proc/<pid>/stat text. The command (field 2) may hold anything, even ')', so
    count from the last ')'. None when it doesn't parse."""
    i = stat.rfind(')')
    if i < 0:
        return None
    fields = stat[i + 1:].split()
    if len(fields) < 20 or not fields[0]:
        return None
    # Field 3 (state) is fields[0]; field 22 (starttime) is 19 on.
    try:
        return fields[0][0], int(fields[19])
    except ValueError:
        return None


def _macos_table_start(lib, pid):
    """(status, start ms) from sysctl(kern.proc.pid): proc_pidinfo refuses another user's process (init's say), the process
    table does not. A kinfo_proc starts with the start (tv_sec i64 at 0, tv_usec i32 at 8) and has p_stat at 36."""
    import ctypes
    import struct

    mib = (ctypes.c_int * 4)(1, 14, 1, pid)   # CTL_KERN, KERN_PROC, KERN_PROC_PID
    buf = ctypes.create_string_buffer(1024)
    size = ctypes.c_size_t(ctypes.sizeof(buf))
    if lib.sysctl(mib, 4, buf, ctypes.byref(size), None, 0) != 0:
        return None
    if size.value == 0:
        return (5, 0)   # no row: not in the table any more
    if size.value < 40:
        return None
    sec, = struct.unpack_from('=q', buf.raw, 0)
    micros, = struct.unpack_from('=i', buf.raw, 8)
    return buf.raw[36], sec * 1000 + micros // 1000


def _macos_start(pid):
    """(status, start ms) from proc_pidinfo(PROC_PIDTBSDINFO), the record `ps` prints from, else the process table; None when
    unreadable."""
    import ctypes
    import ctypes.util

    class BsdInfo(ctypes.Structure):
        _fields_ = [('pbi_flags', ctypes.c_uint32), ('pbi_status', ctypes.c_uint32), ('pbi_xstatus', ctypes.c_uint32),
                    ('pbi_pid', ctypes.c_uint32), ('pbi_ppid', ctypes.c_uint32), ('pbi_uid', ctypes.c_uint32),
                    ('pbi_gid', ctypes.c_uint32), ('pbi_ruid', ctypes.c_uint32), ('pbi_rgid', ctypes.c_uint32),
                    ('pbi_svuid', ctypes.c_uint32), ('pbi_svgid', ctypes.c_uint32), ('rfu_1', ctypes.c_uint32),
                    ('pbi_comm', ctypes.c_char * 16), ('pbi_name', ctypes.c_char * 32), ('pbi_nfiles', ctypes.c_uint32),
                    ('pbi_pgid', ctypes.c_uint32), ('pbi_pjobc', ctypes.c_uint32), ('e_tdev', ctypes.c_uint32),
                    ('e_tpgid', ctypes.c_uint32), ('pbi_nice', ctypes.c_int32), ('pbi_start_tvsec', ctypes.c_uint64),
                    ('pbi_start_tvusec', ctypes.c_uint64)]

    lib = ctypes.CDLL(ctypes.util.find_library('proc') or '/usr/lib/libproc.dylib', use_errno=True)
    lib.proc_pidinfo.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_uint64, ctypes.c_void_p, ctypes.c_int]
    lib.proc_pidinfo.restype = ctypes.c_int
    info = BsdInfo()
    size = ctypes.sizeof(info)
    if lib.proc_pidinfo(pid, 3, 0, ctypes.byref(info), size) != size:   # 3 = PROC_PIDTBSDINFO
        # It was there for kill(2) a moment ago: gone in between (ESRCH), or not readable this way.
        return (5, 0) if ctypes.get_errno() == errno.ESRCH else _macos_table_start(lib, pid)
    # Same arithmetic as the Rust side: seconds * 1000 + micros / 1000.
    return info.pbi_status, info.pbi_start_tvsec * 1000 + info.pbi_start_tvusec // 1000


def _start(pid):
    """(ALIVE | GONE, stamp or None) for a pid that kill(2) says exists."""
    try:
        if sys.platform == 'darwin':
            got = _macos_start(pid)
            if got is None:
                return ALIVE, None
            status, ms = got
            return (GONE, None) if status == 5 else (ALIVE, epoch_stamp(ms))   # 5 = SZOMB: ended, waiting for its parent
        if sys.platform.startswith('linux'):
            with open(f'/proc/{pid}/stat') as f:
                parsed = parse_linux_stat(f.read())
            if parsed is None:
                return ALIVE, None
            state, ticks = parsed
            if state == 'Z':
                return GONE, None
            with open('/proc/sys/kernel/random/boot_id') as f:
                return ALIVE, boot_stamp(f.read(), ticks)
    except Exception:  # noqa: BLE001 — an unreadable record is unknown, not dead
        return ALIVE, None
    return ALIVE, None


def probe(pid):
    """(ALIVE | GONE | UNKNOWN, stamp or None). GONE only when the system says there is no such process (or it is a zombie)."""
    try:
        pid = int(pid)
    except (TypeError, ValueError):
        return GONE, None
    if pid <= 0:   # kill(2) would name a process group; no process has this pid
        return GONE, None
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return GONE, None
    except PermissionError:
        pass   # it exists, we may not signal it
    except OSError:
        return UNKNOWN, None
    return _start(pid)


def start_stamp(pid):
    """The start stamp of pid; None when it is gone or can't be read."""
    state, stamp = probe(pid)
    return stamp if state == ALIVE else None


def running(pid, recorded=None):
    """Whether pid is a live process that may be the one that recorded `recorded` (a stamp, old lstart text or nothing).
    False only for a pid the system says is gone, or a readable start of the same form that differs (a reused pid)."""
    state, current = probe(pid)
    if state == GONE:
        return False
    if state == UNKNOWN:
        return True
    return not (recorded and current) or same_process(recorded, current)


if __name__ == '__main__':
    stamp = start_stamp(sys.argv[1]) if len(sys.argv) > 1 else None
    if stamp:
        print(stamp)
    sys.exit(0 if stamp else 1)
