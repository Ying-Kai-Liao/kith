"""kith_redact — scrub a text before it leaves this Mac (a bug report that becomes a public GitHub issue).

Builds on kith_trajectory.redact (keys, tokens, bearer/JWT, NAME=value for secret-looking names, card and id numbers) and
adds what only this machine knows: the values of the environment variables whose names look secret, the home folder
(-> ~), the real user name, every word in <state>/private-words (the list kith-pkg refuses to share), email addresses,
and the ids and names in the people registry. Best effort, like the redaction it extends: the yes in front of every
filing is the real boundary.
"""
import json, os, re
from pathlib import Path

from kith_paths import paths
from kith_tombstone import Tombstoned, load_json
from kith_trajectory import redact as base_redact

_SECRET_NAME = re.compile(r'(?i)secret|token|passw|passphrase|api[_-]?key|private[_-]?key|access[_-]?key|credential|authorization'
                          r'|_key$|_pass$|_dsn$|database_url|db_url|^dsn$')
EMAIL = re.compile(r'[\w.+-]+@[\w-]+(\.[\w-]+)*\.[A-Za-z]{2,}\b')
MIN_WORD = 3          # shorter names/ids would eat ordinary words


def _people(auth):
    """The people registry. After the authority cutover auth.json is a tombstone note with no people in it; the people
    are then read from the newest `auth.json.pre-cutover-*` copy the cutover kept beside it (a stale list is fine here:
    hiding too many names is harmless). With no readable copy there is no list to hide, and the rest of the scrub still runs."""
    try:
        try:
            people = load_json(auth).get('people')
        except Tombstoned:
            copies = sorted(Path(auth).parent.glob(Path(auth).name + '.pre-cutover-*'),
                            key=lambda c: (c.stat().st_mtime, c.name), reverse=True)
            people = None
            for c in copies:
                try:
                    people = load_json(c).get('people')
                    break
                except (OSError, ValueError, AttributeError):
                    continue   # unreadable copy: try the next newest
        return people if isinstance(people, dict) else {}
    except (OSError, ValueError, AttributeError):
        return {}


def _words(env):
    """Words to hide, longest first: private-words, the user name, people ids and names from the registry."""
    p = paths()
    out = []
    f = Path(env.get('KITH_STATE') or p['KITH_STATE']) / 'private-words'
    try:
        out += [w.strip() for w in f.read_text().splitlines() if w.strip() and not w.startswith('#')]
    except OSError:
        pass
    home = env.get('HOME') or str(Path.home())
    out += [os.path.basename(home.rstrip('/')), env.get('USER', ''), env.get('LOGNAME', '')]
    auth = env.get('KITH_AUTH') or str(Path(str(p['KITH_SECRETS'])) / 'auth.json')
    for pid, v in _people(auth).items():
        out.append(pid)
        if isinstance(v, dict):
            out += [v.get(k) for k in ('name', 'display_name')]
            if isinstance(v.get('handles'), list):
                out += v['handles']
    return sorted({w for w in out if isinstance(w, str) and len(w) >= MIN_WORD}, key=len, reverse=True)


def _secret_values(env):
    return sorted({v for k, v in env.items() if _SECRET_NAME.search(k) and len(v) >= 6}, key=len, reverse=True)


def redactor(env=None):
    """A function text -> text with this run's secrets and personal details removed."""
    env = dict(os.environ if env is None else env)
    values = _secret_values(env)
    homes = {str(Path.home())}
    if env.get('HOME'):
        homes |= {env['HOME'], str(Path(env['HOME']).resolve())}
    homes = sorted((h for h in homes if len(h) > 1), key=len, reverse=True)
    words = [re.compile(r'(?<![\w-])' + re.escape(w) + r'(?![\w-])', re.I) for w in _words(env)]

    def run(text):
        for v in values:
            text = text.replace(v, '[redacted]')
        for h in homes:
            text = text.replace(h, '~')
        text = re.sub(r'/(?:Users|home)/[A-Za-z0-9._-]+(?=/|\b)', '~', text)
        text = base_redact(text)
        text = EMAIL.sub('[email]', text)
        for w in words:
            text = w.sub('[private]', text)
        return text
    return run
