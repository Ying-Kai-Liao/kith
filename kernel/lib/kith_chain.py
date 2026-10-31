"""kith_chain — the ledger of the health-check chain (designs/2026-10-25-all-together.md, rules C1-C6).

A scheduled check that finds a bug opens a chain on the bug's signature: the service plus a fingerprint of the error with
the parts that change from run to run (times, ids, numbers, paths) taken out. One ledger entry per signature holds the
chain's state, so the schedule firing again, or two runs firing at once, never starts a second chain for the same bug
(C3). A signature holds until its PR is merged or HOLD_DAYS pass, whatever became of the chain (a PR, "not a bug", a
failure): the same bug is not asked about, fixed or called about twice in a week.

The ledger is <machine>/chain/ledger.json under an flock, replaced whole each time, so it survives a restart and a
crash mid-write. Stdlib only.
"""
import fcntl, hashlib, json, os, re, subprocess, time
from contextlib import contextmanager

HOLD_DAYS = 7
HOLD_S = HOLD_DAYS * 86400
PR_URL = re.compile(r'https://github\.com/[\w.-]+/[\w.-]+/pull/\d+')
MAX_FINDINGS = 4000   # characters: findings are a summary, not a log

_NOISE = [
    (re.compile(r'\b\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(?:[.,]\d+)?(?:Z|[+-]\d{2}:?\d{2})?', re.I), ''),   # the error is lowercased first
    (re.compile(r'\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b'), '<id>'),
    (re.compile(r'\b0x[0-9a-f]+\b|\b[0-9a-f]{12,}\b'), '<hex>'),
    (re.compile(r'(?:/[\w.@-]+){2,}'), '<path>'),
    (re.compile(r'\b\d+(?:\.\d+)*\b'), '<n>'),
]
_ERRORISH = re.compile(r'error|fail|exception|panic|refused|timeout|timed out|unreachable|denied|crash|fatal', re.I)


def now():
    return float(os.environ.get('KITH_CHAIN_NOW') or time.time())


def fingerprint(error):
    """The error with what changes per run taken out, hashed: the same bug on another day gives the same string."""
    lines = [l.strip() for l in error.splitlines() if l.strip()]
    line = next((l for l in lines if _ERRORISH.search(l)), lines[0] if lines else '')
    line = line.lower()
    for rx, sub in _NOISE:
        line = rx.sub(sub, line)
    line = re.sub(r'\s+', ' ', line).strip()
    return hashlib.sha1(line.encode()).hexdigest()[:12]


def signature(service, error):
    svc = re.sub(r'[^a-z0-9._-]+', '-', service.lower()).strip('-') or 'service'
    return '%s:%s' % (svc, fingerprint(error))


def findings(text, redact):
    """What may leave this run for another agent (C4): the text redacted by this Mac's own rules, with environment-style
    NAME=value lines dropped whole and a length cap. Findings only; the callee reads its own logs."""
    kept = [l for l in text.splitlines() if not re.match(r'\s*(?:export\s+)?[A-Z][A-Z0-9_]{2,}=\S', l)]
    out = redact('\n'.join(kept)).strip()
    return out if len(out) <= MAX_FINDINGS else out[:MAX_FINDINGS] + '\n[cut]'


class Ledger:
    def __init__(self, folder):
        self.dir = folder
        self.file = folder / 'ledger.json'

    @contextmanager
    def locked(self):
        """Yields the entries (a dict signature -> entry) under an exclusive lock; what the block leaves is saved."""
        self.dir.mkdir(parents=True, exist_ok=True)
        with open(self.dir / 'ledger.lock', 'a') as lock:   # flock goes with the process: a crash frees it
            fcntl.flock(lock, fcntl.LOCK_EX)
            try:
                entries = json.loads(self.file.read_text())
            except FileNotFoundError:
                entries = {}
            yield entries
            tmp = self.file.with_name('.ledger.%d.tmp' % os.getpid())
            with open(tmp, 'w') as f:
                f.write(json.dumps(entries, indent=1, sort_keys=True) + '\n')
                f.flush()
                os.fsync(f.fileno())
            os.replace(tmp, self.file)

    def get(self, sig):
        with self.locked() as e:
            return e.get(sig)


def pr_merged(url):
    """Whether the PR at url is merged (gh; KITH_CHAIN_GH names another binary). False when it can't tell: a signature
    held a little long is cheaper than a second PR."""
    try:
        out = subprocess.run([os.environ.get('KITH_CHAIN_GH', 'gh'), 'pr', 'view', url, '--json', 'state', '-q', '.state'],
                             capture_output=True, text=True, timeout=30)
        return out.returncode == 0 and out.stdout.strip() == 'MERGED'
    except (OSError, subprocess.SubprocessError):
        return False


def holds(entry, t, merged=pr_merged):
    """Whether the entry still stops a new chain for its signature: until its PR is merged, or HOLD_DAYS after it began."""
    if entry['state'] == 'merged' or t - entry['opened'] >= HOLD_S:
        return False
    if entry.get('pr') and merged(entry['pr']):
        entry['state'] = 'merged'
        return False
    return True


def open_chain(ledger, sig, service, summary, agent, merged=pr_merged):
    """(True, entry) when this call began the chain; (False, entry) when the signature holds (nothing changes)."""
    with ledger.locked() as e:
        t = now()
        old = e.get(sig)
        if old and holds(old, t, merged):
            return False, dict(old)
        e[sig] = {'service': service, 'summary': summary[:300], 'agent': agent, 'state': 'open', 'opened': t,
                  'pr': None, 'why': None, 'noted': False}
        return True, dict(e[sig])


def update(ledger, sig, **fields):
    with ledger.locked() as e:
        if sig not in e:
            raise KeyError(sig)
        e[sig].update(fields)
        return dict(e[sig])
