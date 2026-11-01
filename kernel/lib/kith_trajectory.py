"""kith_trajectory — thread summaries (tier 2) and trajectory chunks (tier 3) for the recall index (docs/memory-context.md).

Sources, read-only and never rewritten here:
  * the host store (kith serve's serve.db): `entries` for every chat and thread (imported terminal sessions are ordinary
    entries), `thread_summaries` (the host writes one per thread), `thread_forgotten` (the owner's `kith forget --thread`);
  * Claude harness logs ~/.claude/projects/*/<sid>.jsonl of runs the host did not import, only when the cost ledger
    attributes the session to an agent (and a person): anything else is skipped.

Every row takes the audience of its thread, never of the asker: a terminal thread is `owner` whoever the chat belongs to,
a person's chat is `person:<id>`, a group chat is `room:<chat>`. Text is redacted before it is indexed; the stored entries
are not touched. A forgotten thread is never indexed again (its entries stay in the host store). The index is derived:
this module only adds rows to it (the caller holds the index lock).
"""
import hashlib, json, os, re, sqlite3, subprocess, time
from pathlib import Path

from kith_paths import DOT, paths

MAX_ROW = 4000        # characters one chunk keeps
BATCH = 2000          # entries per commit, so an interrupted first build resumes from its cursor
MAX_LOG = 20_000_000  # a harness log larger than this is skipped
GROUP_VIAS = ('telegram', 'slack', 'discord', 'group')
KINDS = ('user', 'agent', 'thought', 'tool')
THREAD_ID = re.compile(r'[A-Za-z0-9][A-Za-z0-9:_.-]{0,127}')

# ---- redaction: best effort, so common leaks stay out of search results. The deny rules are the real boundary.

_NAME = (r'[\w.-]*(?:secret|token|passw(?:or)?d|passphrase|api[_-]?key|private[_-]?key|access[_-]?key|credential|authorization'
         r'|_key|_pass|_dsn|database_url|db_url|dsn)[\w.-]*')
REDACTIONS = [
    (re.compile(r'-----BEGIN [A-Z ]*PRIVATE KEY-----.*?(?:-----END [A-Z ]*PRIVATE KEY-----|\Z)', re.S), '[redacted]'),
    (re.compile(r'-----BEGIN [A-Z ]*(?:CERTIFICATE|KEY)[A-Z ]*-----.*?(?:-----END [A-Z ]*-----|\Z)', re.S), '[redacted]'),
    # scheme://user:password@host
    (re.compile(r'(?i)\b([a-z][a-z0-9+.-]*://[^\s:/@]+:)[^\s@/]+@'), r'\1[redacted]@'),
    (re.compile(r'(?i)\b(bearer|basic)\s+[A-Za-z0-9._~+/=-]{8,}'), r'\1 [redacted]'),
    (re.compile(r'\b(?:[sr]k_(?:live|test)_[A-Za-z0-9]{8,}|glpat-[A-Za-z0-9_-]{12,}|npm_[A-Za-z0-9]{12,}|hf_[A-Za-z0-9]{12,}'
                r'|ya29\.[A-Za-z0-9_-]{12,}|sk-[A-Za-z0-9_-]{12,}|gh[pousr]_[A-Za-z0-9]{12,}|github_pat_[A-Za-z0-9_]{12,}|xox[abprs]-[A-Za-z0-9-]{8,}'
                r'|(?:AKIA|ASIA)[0-9A-Z]{12,}|AIza[0-9A-Za-z_-]{20,}|eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{4,})'),
     '[redacted]'),
    # NAME=value, NAME: value, "name": "value" for names that look secret
    (re.compile(r'(?i)(["\']?' + _NAME + r'["\']?\s*[:=]\s*)(?:"[^"\n]*"|\'[^\'\n]*\'|[^\s,;}\]]+)'), r'\1[redacted]'),
    # Taiwan national id and US SSN shapes, as check.rs refuses them in memory
    (re.compile(r'(?<![A-Za-z0-9])[A-Z][1289]\d{8}(?![A-Za-z0-9])'), '[redacted]'),
    (re.compile(r'(?<!\d)\d{3}-\d{2}-\d{4}(?!\d)'), '[redacted]'),
]


def _luhn(digits):
    total, flip = 0, False
    for ch in reversed(digits):
        d = int(ch)
        if flip:
            d = d * 2 - 9 if d > 4 else d * 2
        total += d
        flip = not flip
    return total % 10 == 0


def redact(text):
    for pattern, repl in REDACTIONS:
        text = pattern.sub(repl, text)
    return re.sub(r'(?<!\d)\d(?:[ -]?\d){12,18}(?!\d)',
                  lambda m: '[redacted]' if _luhn(re.sub(r'\D', '', m.group(0))) else m.group(0), text)


# ---- the host store

def host_db_path():
    """serve.db of this install's host: KITH_SERVE_DB, else the newest of the packaged and the dev data folders."""
    if os.environ.get('KITH_SERVE_DB'):
        return Path(os.environ['KITH_SERVE_DB'])
    if os.environ.get('KITH_SERVE_HOME'):
        folders = [Path(os.environ['KITH_SERVE_HOME'])]
    else:   # the data folder kith-serve is configured for ; the newest store only when it can't say
        try:
            out = subprocess.run([str(Path(__file__).resolve().parent.parent / 'bin' / 'kith-serve'), 'endpoint'],
                                 capture_output=True, text=True, timeout=4, check=True).stdout.split()
            folders = [Path(out[0])]
        except Exception:  # noqa: BLE001
            folders = [DOT / 'serve']
    found = [f / 'serve.db' for f in folders if (f / 'serve.db').is_file()]
    return max(found, key=lambda p: p.stat().st_mtime, default=None)


def host_open(path=None):
    """The host store, read-only; None when there is none or it can't be read."""
    path = path or host_db_path()
    if not path or not Path(path).is_file():
        return None
    try:
        db = sqlite3.connect('file:%s?mode=ro' % Path(path).resolve(), uri=True, timeout=5)
        db.execute('PRAGMA busy_timeout=5000')
        db.execute('SELECT 1 FROM entries LIMIT 1')
        return db
    except sqlite3.Error:
        return None


def _table(host, name):
    return host.execute("SELECT 1 FROM sqlite_master WHERE type='table' AND name=?", (name,)).fetchone() is not None


def forgotten_ids(host):
    if not _table(host, 'thread_forgotten'):
        return set()
    return {r[0] for r in host.execute('SELECT thread_id FROM thread_forgotten')}


def thread_facets(cfg, root_data, chat):
    """{agent, room, audience, projects} of a thread from its chat's config and its root entry's data. The host's
    kith_mem::summary::audience is the same rule."""
    group = cfg.get('kind') == 'group'
    thread = (root_data or {}).get('thread') if isinstance(root_data, dict) else None
    terminal = isinstance(thread, dict) and thread.get('origin') == 'terminal'
    agent = '' if group else (cfg.get('hqAgent') or '')
    person = cfg.get('person') or ''
    if terminal:
        audience = 'owner'
    elif group:
        audience = 'room:%s' % chat
    else:
        audience = 'person:%s' % person if person else 'owner'
    room = chat if group else '1:1:%s:%s' % (agent, person or 'owner')
    return {'agent': agent, 'room': room, 'audience': audience, 'projects': [cfg['project']] if cfg.get('project') else []}


def chunk_text(kind, data):
    """What a trajectory entry says: a message's text, a tool's title (the tool and the file or command) and result."""
    if not isinstance(data, dict):
        return ''
    if kind == 'tool':
        diffs = data.get('diffs')
        parts = [data.get('title'), data.get('output'), '\n'.join(map(str, diffs)) if isinstance(diffs, list) else None]
        return '\n'.join(str(p) for p in parts if p)
    text = data.get('text')
    return text if isinstance(text, str) else ''


# ---- rows

def _cursor(db, name):
    r = db.execute('SELECT value FROM cursors WHERE name=?', (name,)).fetchone()
    return r[0] if r else None


def _set_cursor(db, name, value):
    db.execute('INSERT OR REPLACE INTO cursors VALUES(?,?)', (name, str(value)))


def _delete(db, where, args):
    db.execute('DELETE FROM fts WHERE rowid IN (SELECT rowid FROM index_rows WHERE %s)' % where, args)
    db.execute('DELETE FROM index_rows WHERE %s' % where, args)


def _add(db, rid, kind, f, thread, source, entry, text, when):
    cur = db.execute('INSERT OR REPLACE INTO index_rows VALUES(?,?,?,?,?,?,?,?,?,?,?,?)', (
        rid, kind, f['agent'], f['audience'], f['room'], json.dumps(f['projects']), thread, source, entry, text, when, ''))
    db.execute('INSERT INTO fts(rowid, text, audience, agent) VALUES(?,?,?,?)', (cur.lastrowid, text, f['audience'], f['agent']))


def _rid(*parts):
    return hashlib.sha1(':'.join(parts).encode()).hexdigest()[:20]


def sync(db):
    """Bring the host's summaries and trajectory, and attributable harness logs, up to date in the index."""
    db.execute('CREATE TABLE IF NOT EXISTS cursors(name TEXT PRIMARY KEY, value TEXT)')
    db.execute('CREATE TABLE IF NOT EXISTS traj_files(path TEXT PRIMARY KEY, mtime_ns INTEGER, size INTEGER)')
    host = host_open()
    try:
        if host is not None:
            sync_host(db, host)
        sync_harness(db, host)
    finally:
        if host is not None:
            host.close()
    db.commit()


def _bots(host):
    out = {}
    for bid, deleted, config in host.execute('SELECT id, deleted, config FROM bots'):
        try:
            out[bid] = (bool(deleted), json.loads(config))
        except ValueError:
            continue
    return out


class _Facets:
    """Facets per thread, looked up once: the chat's config and the root entry's data."""

    def __init__(self, host, bots):
        self.host, self.bots, self.cache = host, bots, {}

    def of(self, chat, thread):
        key = (chat, thread)
        if key not in self.cache:
            deleted, cfg = self.bots.get(chat, (True, {}))
            root = None
            if thread:
                r = self.host.execute('SELECT data FROM entries WHERE id=?', (thread,)).fetchone()
                try:
                    root = json.loads(r[0]) if r else None
                except ValueError:
                    root = None
            self.cache[key] = None if deleted else thread_facets(cfg, root, chat)
        return self.cache[key]


def sync_host(db, host):
    bots = _bots(host)
    forgotten = forgotten_ids(host)
    gone = {b for b, (deleted, _) in bots.items() if deleted}
    # Removals first: a forgotten thread, or the chat of a deleted bot, leaves the index and stays out.
    mark = json.dumps([sorted(forgotten), sorted(gone)])
    if _cursor(db, 'removed') != mark:
        for t in forgotten:
            _delete(db, 'thread_id = ?', (t,))
        for b in gone:
            _delete(db, 'source_path = ?', ('host:' + b,))
        _set_cursor(db, 'removed', mark)
        _set_cursor(db, 'summaries', '')
    facets = _Facets(host, bots)
    _sync_summaries(db, host, bots, forgotten, facets)
    cur = int(_cursor(db, 'host_rev') or 0)
    # By `rev`, not `seq`: a streamed reply is one entry updated in place, and each update is a new rev.
    while True:
        rows = host.execute('SELECT seq, id, bot_id, thread_id, rev, kind, data, created_at FROM entries WHERE rev > ? AND kind IN (%s) '
                            'ORDER BY rev LIMIT ?' % ','.join('?' * len(KINDS)), (cur, *KINDS, BATCH)).fetchall()
        if not rows:
            break
        for row in rows:
            cur = max(cur, row[4])
            _index_entry(db, host, facets, gone, forgotten, row)
            if row[3]:   # a reply in a thread: the thread's root entry (in the chat itself) belongs to the thread
                root = host.execute('SELECT seq, id, bot_id, thread_id, rev, kind, data, created_at FROM entries WHERE id = ? AND kind IN (%s)'
                                    % ','.join('?' * len(KINDS)), (row[3], *KINDS)).fetchone()
                have = db.execute('SELECT thread_id FROM index_rows WHERE id = ?', (_rid('traj', row[3]),)).fetchone()
                if root and (have is None or have[0] != row[3]):
                    _index_entry(db, host, facets, gone, forgotten, root, as_root=True)
        _set_cursor(db, 'host_rev', cur)
        db.commit()


def _index_entry(db, host, facets, gone, forgotten, row, as_root=False):
    """One entry as a row of its thread. A thread's root entry lives in the chat itself (no thread id) and is the
    thread's first message: it is the thread's when it carries thread data or has replies, and a terminal thread's root
    takes the owner-only audience whatever chat it sits in."""
    seq, eid, chat, thread, rev, kind, data, created = row
    try:
        body = json.loads(data)
    except ValueError:
        return
    if thread is None and (as_root or (isinstance(body, dict) and isinstance(body.get('thread'), dict))
                           or host.execute('SELECT 1 FROM entries WHERE bot_id = ? AND thread_id = ? LIMIT 1', (chat, eid)).fetchone()):
        thread = eid
    key = thread or 'main:' + chat
    f = facets.of(chat, thread) if chat not in gone and key not in forgotten else None
    rid = _rid('traj', eid)
    _delete(db, 'id = ?', (rid,))
    if f is None:
        return
    text = redact(chunk_text(kind, body)).strip()[:MAX_ROW]
    if text:
        _add(db, rid, 'trajectory', f, key, 'host:' + chat, str(seq), text, (created or 0) / 1000)


def _sync_summaries(db, host, bots, forgotten, facets):
    if not _table(host, 'thread_summaries'):
        return
    rows = host.execute('SELECT thread_id, text, links, covered_seq, written_at FROM thread_summaries').fetchall()
    finger = json.dumps([(r[0], r[3], r[4]) for r in rows]) + _cursor(db, 'removed')
    if _cursor(db, 'summaries') == finger:
        return
    _delete(db, 'source_path = ?', ('host-summary',))
    for key, text, links, covered, written in rows:
        if key in forgotten:
            continue
        if key.startswith('main:'):
            chat, thread = key[5:], None
        else:
            r = host.execute('SELECT bot_id FROM entries WHERE id=?', (key,)).fetchone()
            chat, thread = (r[0], key) if r else (None, None)
        f = facets.of(chat, thread) if chat else None
        if f is None:   # a thread whose chat can't be found has no known audience: not indexed
            continue
        try:
            first = (json.loads(links) or [covered])[0]
        except (ValueError, TypeError):
            first = covered
        body = redact(text).strip()
        if body:
            _add(db, _rid('summary', key), 'summary', f, key, 'host-summary', str(first), body, (written or 0) / 1000)
    _set_cursor(db, 'summaries', finger)


# ---- Claude harness logs of runs the host did not import

def claude_dir():
    return Path(os.environ.get('CLAUDE_CONFIG_DIR') or Path.home() / '.claude')


def _attributed():
    """{session: (agent, person)} of the cost ledger's terminal runs that name an agent. Chat and channel runs (serve, app,
    Discord...) are in the host store already, and their ledger person can differ from the thread's."""
    path = paths()['KITH_MACHINE'] / 'cost.db'
    if not path.is_file():
        return {}
    try:
        c = sqlite3.connect('file:%s?mode=ro' % path, uri=True, timeout=5)
        try:
            return {s: (a, p or '') for s, a, v, p in c.execute('SELECT session, agent, via, person FROM runs WHERE agent IS NOT NULL AND agent != ""')
                    if (v or 'terminal') == 'terminal'}
        finally:
            c.close()
    except sqlite3.Error:
        return {}


def _blocks(content):
    if isinstance(content, str):
        yield 'text', content
    elif isinstance(content, list):
        for b in content:
            if not isinstance(b, dict):
                continue
            t = b.get('type')
            if t == 'text':
                yield 'text', b.get('text') or ''
            elif t == 'tool_use':
                inp = b.get('input') if isinstance(b.get('input'), dict) else {}
                arg = next((str(inp[k]) for k in ('file_path', 'path', 'command', 'pattern', 'url', 'query', 'description') if inp.get(k)), '')
                yield 'tool', ('%s %s' % (b.get('name') or 'Tool', arg)).strip()
            elif t == 'tool_result':
                c = b.get('content')
                yield 'result', c if isinstance(c, str) else '\n'.join(x.get('text', '') for x in c if isinstance(x, dict)) if isinstance(c, list) else ''


def log_chunks(path):
    """(line number, text) for what a Claude log says: messages, tool calls and their results (thinking is left out)."""
    with open(path, errors='replace') as f:
        for n, line in enumerate(f, 1):
            try:
                obj = json.loads(line)
            except ValueError:
                continue
            msg = obj.get('message') if isinstance(obj, dict) else None
            if not isinstance(msg, dict) or obj.get('type') not in ('user', 'assistant'):
                continue
            for kind, text in _blocks(msg.get('content')):
                if text and text.strip():
                    yield n, text


def sync_harness(db, host):
    attributed = _attributed()
    root = claude_dir() / 'projects'
    files = sorted(root.glob('*/*.jsonl')) if attributed and root.is_dir() else []
    known = {p: (m, s) for p, m, s in db.execute('SELECT path, mtime_ns, size FROM traj_files')}
    forgotten = forgotten_ids(host) if host is not None else set()
    registry = paths()['KITH_STATE'] / 'live' / 'terminal'
    imported = None
    seen = set()
    for path in files:
        sid, run = path.stem, attributed.get(path.stem)
        if run is None or sid in forgotten or (registry / (sid + '.json')).exists():
            continue   # unattributed, forgotten, or a terminal session the host imports as entries
        try:
            st = path.stat()
        except OSError:
            continue
        if st.st_size > MAX_LOG:
            continue
        seen.add(str(path))
        if known.get(str(path)) == (st.st_mtime_ns, st.st_size):
            continue
        if imported is None:
            imported = set()
            if host is not None:
                imported = {r[0] for r in host.execute("SELECT json_extract(data, '$.thread.terminal.sessionId') FROM entries "
                                                       "WHERE json_extract(data, '$.thread.origin') = 'terminal'")}
        _delete(db, 'source_path = ?', ('harness:' + str(path),))
        if sid in imported:   # the host has it as entries: remembered, so it isn't checked again until the log changes
            db.execute('INSERT OR REPLACE INTO traj_files VALUES(?,?,?)', (str(path), st.st_mtime_ns, st.st_size))
            continue
        agent, person = run
        f = {'agent': agent, 'room': '1:1:%s:%s' % (agent, person or 'owner'),
             'audience': 'person:' + person if person else 'owner', 'projects': []}
        for n, text in log_chunks(path):
            text = redact(text).strip()[:MAX_ROW]
            if text:
                _add(db, _rid('log', str(path), str(n), hashlib.sha1(text.encode()).hexdigest()[:8]), 'trajectory', f, sid,
                     'harness:' + str(path), str(n), text, st.st_mtime)
        db.execute('INSERT OR REPLACE INTO traj_files VALUES(?,?,?)', (str(path), st.st_mtime_ns, st.st_size))
    for path in set(known) - seen:   # a log that went away, or is no longer indexable
        _delete(db, 'source_path = ?', ('harness:' + path,))
        db.execute('DELETE FROM traj_files WHERE path=?', (path,))
    for t in forgotten:
        _delete(db, 'thread_id = ?', (t,))


# ---- forget

def forget(thread_id, host_path=None):
    """Marks a thread forgotten in the host store and removes its summary there (the entries stay). The caller has
    checked that the asker is the owner. Returns False when there is no host store to mark it in."""
    if not THREAD_ID.fullmatch(thread_id):
        raise ValueError('not a thread id')
    path = host_path or host_db_path()
    if not path or not Path(path).is_file():
        return False
    db = sqlite3.connect(str(path), timeout=10)
    try:
        db.execute('PRAGMA busy_timeout=10000')
        # The same tables the host's migration makes (crates/memory/src/schema.rs, 4): a host that never ran it has none yet.
        db.execute('CREATE TABLE IF NOT EXISTS thread_forgotten (thread_id TEXT PRIMARY KEY, at INTEGER NOT NULL)')
        db.execute('INSERT OR IGNORE INTO thread_forgotten (thread_id, at) VALUES (?, ?)', (thread_id, int(time.time() * 1000)))
        if _table(db, 'thread_summaries'):
            db.execute('DELETE FROM thread_summaries WHERE thread_id = ?', (thread_id,))
        db.commit()
    finally:
        db.close()
    return True
