#!/usr/bin/env python3
"""Accounts: the managed set, what an id means, whether it is ready, which one a run starts on, and how it launches.

An account id is `<harness>` or `<harness>:<name>`:

  claude              the ambient Claude login (what runs today)
  claude:<name>       an extra Claude subscription account: CLAUDE_CONFIG_DIR=<machine>/accounts/claude/<name>
  claude-api:<profile>  an Anthropic API key held by kith-auth (anthropic/<profile>), handed to Claude Code through
                      apiKeyHelper, so the key is never in env, JSON or logs
  codex               the ambient Codex login (~/.codex)
  codex:<name>        CODEX_HOME=<machine>/accounts/codex/<name>
  anything else       a harness the host may learn later; here it is "not runnable yet"

The accounts are a set kept in lib/kith_settings.py (`accounts`, `account_policy`, agents.<a>.accounts and .pin): all
equal, any number per provider. Readiness is asked of the harness's own status command with the right config dir; no
token, credential file or Keychain item is ever read. What each account is doing (out of usage until when, the last usage
reading, when it last started) is the kernel's state file, <machine>/accounts/state.json (see below, docs/accounts.md).

pick() is the one selection: the agent's pinned account first (a preferred pin falls back to the set, an "only" pin never
does), then the accounts of the agent's own harness family (claude, claude:<name>, claude-api:<profile> for a Claude agent;
codex, codex:<name> for a Codex one) in policy order (fill: the set's order; spread: the most room left), then those of
another harness in set order: a Claude agent falls over to Codex only when Codex is in its set (the global one, or its
own agents.<a>.accounts). Accounts that are out or signed out are skipped; every one out: the first runs, as ever.
Without any account of its family: the harness's default login.

A session belongs to the account it was made under (its id: the cost ledger's runs.account_id, KITH_ACCOUNT_ID at
SessionStart): it is resumed only under that account.

  kith_accounts.py backups-json AGENT AGENT.MD   the older chain view for `kith AGENT --backups-json` (static order, no state)
  kith_accounts.py accounts-json AGENT AGENT.MD  `kith AGENT --accounts-json`: set, status, usage, candidates (docs/accounts.md)
  kith_accounts.py launch-env AGENT ID [--record]  shell assignments for bin/kith; exit 4 + one line on stderr when not runnable
  kith_accounts.py main-id AGENT AGENT.MD        the static main account's id
  kith_accounts.py main-env AGENT AGENT.MD [--state] [--prefer ID] [--record]
                                                 the account a plain run starts on for bin/kith: main_id=ID plus launch-env's
                                                 assignments (empty for the default login); --state: pick()'s first candidate
                                                 (--prefer ID first when it is one), else the static main. Exit 4 + one line when it can't run
  kith_accounts.py session-run SID [--sh]         the harness, model and effort a session started with ({} when the ledger doesn't know it)
  kith_accounts.py run-chain AGENT AGENT.MD KITH -- ARGS...   a non-interactive one-shot over pick()'s candidates (see run_chain)
  kith_accounts.py session-account SID           the account id a session was made under ('' when the ledger doesn't know it)
  --record notes last_started in the state file (the launch is real)
"""
import concurrent.futures, contextlib, fcntl, json, os, re, shlex, shutil, subprocess, sys, tempfile, time
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.realpath(__file__)))
import kith_settings  # noqa: E402
from kith_paths import paths  # noqa: E402

KERNEL = Path(__file__).resolve().parent.parent
ID_RE = re.compile(r'^([a-z][a-z0-9-]*)(?::([A-Za-z0-9][A-Za-z0-9._-]*))?$')
STATUS_TIMEOUT = 20


MODEL_RE = re.compile(r'^[\w.:\[\]/-]+$')
CLAUDE_MODEL_RE = re.compile(r'^(opus|sonnet|haiku|fable|claude-)|^(best|opusplan)$')


def is_claude_model(model):
    """A Claude model name or alias; it means nothing to another harness (codex -m sonnet fails)."""
    return bool(CLAUDE_MODEL_RE.match(model or ''))


def split(entry):
    """(id without the model, model or None): a backup entry is <harness>[:<name>][@<model>]."""
    base, at, model = (entry or '').partition('@')
    return base, (model if at else None)


def parse(entry):
    """(harness, name or None); ValueError says what is wrong with the id. A trailing @model is checked and left out."""
    entry, model = split(entry)
    if model is not None and not MODEL_RE.match(model):
        raise ValueError(f"'{entry}@{model}': @ needs a model name (claude@sonnet, claude:work@opus)")
    m = ID_RE.match(entry or '')
    if not m:
        raise ValueError(f"'{entry}' is not an account id: use <harness> or <harness>:<name>, with @<model> for a model of its own "
                         "(claude, claude:work, claude@sonnet, claude-api:main, codex)")
    harness, name = m.groups()
    if harness == 'claude-api' and not name:
        raise ValueError("claude-api needs a profile: claude-api:<profile>")
    return harness, name


def machine():
    return paths()['KITH_MACHINE']


def account_dir(harness, name):
    return machine() / 'accounts' / harness / name


FAMILY = {'claude': ('claude', 'claude-api'), 'codex': ('codex',)}


def family(harness):
    """The account harnesses of an agent's own family: its own, and for Claude an API key too."""
    return FAMILY.get(harness, (harness,))


def check_entry_model(entry):
    """Raise ValueError when the entry's model can't belong to its harness: a Claude model on another one."""
    h, _ = parse(entry)
    model = split(entry)[1]
    if model and is_claude_model(model) and h not in family('claude'):
        raise ValueError(f"{model} is a Claude model and {h} is not Claude: use {h}@<a {h} model>, or claude@{model}")


def check_main(entry, harness):
    """Raise ValueError unless entry can be the pinned account of an agent on harness."""
    if split(entry)[1] is not None:
        raise ValueError(f"{entry}: a pinned account takes no @model (the agent's own model applies); @model is for backups")
    h, _ = parse(entry)
    if h not in family(harness):
        raise ValueError(f"{entry} can't be the account of a {harness} agent: use "
                         + ', '.join(f'{x}' if x != 'claude-api' else 'claude-api:<profile>' for x in family(harness))
                         + ' or <harness>:<name> of those')


def agent_md_of(agent):
    """The agent's agent.md, found as bin/kith's agent_dir finds it; None when there is no such agent."""
    for d in (paths()['KITH_AGENTS'] / agent / 'role', paths()['KITH_AGENTS'] / agent, KERNEL / 'agents' / agent):
        if (d / 'agent.md').is_file():
            return d / 'agent.md'
    return None


def kind(harness, name):
    return 'api-key' if harness == 'claude-api' else 'subscription'


def run_harness(argv, env_extra):
    """Run a status command; (exit code or None when not installed, stdout)."""
    try:
        r = subprocess.run(argv, env={**os.environ, **env_extra}, capture_output=True, text=True,
                           timeout=STATUS_TIMEOUT, stdin=subprocess.DEVNULL)
    except FileNotFoundError:
        return None, ''
    except subprocess.TimeoutExpired:
        return 124, ''
    return r.returncode, r.stdout


def logged_in(harness, directory=None):
    """(True/False, why). claude auth status / codex login status, in the account's dir when it has one."""
    env = {}
    if harness == 'claude':
        argv = ['claude', 'auth', 'status']
        if directory:
            env['CLAUDE_CONFIG_DIR'] = str(directory)
    else:
        argv = ['codex', 'login', 'status']
        if directory:
            env['CODEX_HOME'] = str(directory)
    code, out = run_harness(argv, env)
    if code is None:
        return False, f'{harness} is not installed'
    if code == 124:
        return False, f'{harness} did not answer'
    if code != 0:
        return False, 'not logged in'
    try:
        d = json.loads(out)
        if isinstance(d, dict) and d.get('loggedIn') is False:
            return False, 'not logged in'
    except ValueError:
        pass
    return True, None


def granted_profiles(agent, service):
    """The profiles kith-auth grants the agent for a service (names only; no secret is read)."""
    if not agent:
        return []
    r = subprocess.run([str(KERNEL / 'bin/kith-auth'), 'granted', '--for', agent, service], capture_output=True,
                       text=True, stdin=subprocess.DEVNULL, timeout=STATUS_TIMEOUT)
    return r.stdout.split() if r.returncode == 0 else []


def fix(entry, agent=None):
    """The one command that makes an entry ready."""
    harness, name = parse(entry)
    if harness in ('claude', 'codex') and name:
        return f'kith account add {harness} {name}'
    if harness == 'claude':
        return 'claude auth login'
    if harness == 'codex':
        return 'codex login'
    if harness == 'claude-api':
        return (f'kith auth set anthropic/{name} ANTHROPIC_API_KEY && '
                f'kith auth grant {agent or "<agent>"} anthropic anthropic/{name}')
    return None


def check(entry, agent=None):
    """(ready, why). Unknown harnesses are never ready here."""
    harness, name = parse(entry)
    if harness == 'claude-api':
        if subprocess.run(['which', 'claude'], capture_output=True).returncode != 0:
            return False, 'claude is not installed'
        if f'anthropic/{name}' in granted_profiles(agent, 'anthropic'):
            return True, None
        return False, f'no anthropic/{name} key granted to {agent or "the agent"}'
    if harness in ('claude', 'codex'):
        if not name:
            return logged_in(harness)
        d = account_dir(harness, name)
        if not d.is_dir():
            return False, f'no account {name}'
        return logged_in(harness, d)
    return False, f'{harness} is not supported yet'


def agent_harness(agent_md):
    """The harness agent.md names (claude-code reads as claude); claude when it names none or there is no agent.md."""
    try:
        m = re.match(r'---\n(.*?)\n---', Path(agent_md).read_text(), re.S)
        f = re.search(r'^harness:[ \t]*(\S+)', m.group(1), re.M) if m else None
    except (OSError, TypeError):
        f = None
    h = f.group(1) if f else 'claude'
    return 'claude' if h == 'claude-code' else h


def entry_json(entry, agent, primary=False):
    harness, name = parse(entry)
    # The main account on the default login stays ready as it always was: the host may hold that login's keys where
    # `auth status` can't see them. A named or API-key main account is checked like any entry.
    ready, why = (True, None) if primary and not name else check(entry, agent)
    out = {'id': entry, 'harness': 'claude' if harness == 'claude-api' else harness, 'account': name,
           'model': split(entry)[1], 'kind': 'primary' if primary else kind(harness, name), 'ready': ready}
    if not ready:
        out['why'] = why
    return out


def chain(agent, agent_md):
    """`kith AGENT --backups-json`, the older view the host still reads: the agent's static order (pin first, then its set)
    as a chain, entry 0 the primary. No state in it; --accounts-json carries that."""
    harness = agent_harness(agent_md)
    ids, _ = static_order(agent, harness)
    items = []
    for n, i in enumerate(ids):
        try:
            items.append(entry_json(i, agent, primary=n == 0))
        except ValueError as e:   # a hand-edited bad id: listed as not ready rather than failing the whole chain
            items.append({'id': i, 'harness': None, 'account': None, 'model': None, 'kind': 'unknown', 'ready': False, 'why': str(e)})
    return {'version': 1, 'agent': agent, 'chain': items}


def api_helper(agent, name):
    """The apiKeyHelper command: prints the key on demand, run by Claude Code, so the key never passes through us."""
    return ' '.join(shlex.quote(a) for a in [str(KERNEL / 'bin/kith-auth'), 'get', '--for', agent, '--profile',
                                              f'anthropic/{name}', 'anthropic', 'ANTHROPIC_API_KEY'])


def present(entry, agent):
    """(ready, why) without asking the harness: the account's folder is there, or the API key is granted. For the main
    account, which bin/kith applies to every run: a logged-out one is the harness's to say, at once and in its words."""
    harness, name = parse(entry)
    if harness in ('claude', 'codex') and name and not account_dir(harness, name).is_dir():
        return False, f'no account {name}'
    if harness == 'claude-api' and f'anthropic/{name}' not in granted_profiles(agent, 'anthropic'):
        return False, f'no anthropic/{name} key granted to {agent or "the agent"}'
    return True, None


# The acknowledgements a login makes once (onboarding, the auto-mode warnings): without them auto mode is not entered
# silently, and Claude Code asks instead. Copied from the main login into an account's .claude.json, nothing else of it.
MIRRORED_FLAGS = ('hasCompletedOnboarding', 'lastOnboardingVersion', 'hasSeenAutoModeEntryWarning',
                  'hasSeenAutoModeOutsideReadPrompt', 'hasSeenAutoDefaultNotice', 'autoModeEnvSetup')
# Plugin state: the JSON files are copied (Claude Code rewrites them in place, so a link would not survive), the folders
# are linked (their paths in installed_plugins.json are absolute, into the main login's).
PLUGIN_FILES = ('installed_plugins.json', 'known_marketplaces.json', 'blocklist.json')
PLUGIN_DIRS = ('marketplaces', 'cache', 'data')


def _write_atomic(path, data, mode=0o600):
    fd, tmp = tempfile.mkstemp(dir=str(path.parent), prefix=f'.{path.name}.')
    try:
        with os.fdopen(fd, 'wb') as f:
            f.write(data)
        os.chmod(tmp, mode)
        os.replace(tmp, path)
    except BaseException:
        try:
            os.unlink(tmp)
        except OSError:
            pass
        raise


def _read_json(path):
    try:
        d = json.loads(path.read_bytes())
    except (OSError, ValueError):
        return None
    return d if isinstance(d, dict) else None


def _copy_if_changed(src, dst):
    """Copy src to dst (atomically) when the bytes differ; whether it wrote."""
    data = src.read_bytes()
    try:
        if dst.read_bytes() == data:
            return False
    except OSError:
        pass
    _write_atomic(dst, data)
    return True


def main_claude_paths():
    """(config dir, .claude.json) of the main Claude login. CLAUDE_CONFIG_DIR is the main login's only when it is not
    one of the accounts' own folders (a run on an account has it set, and must not take itself for the main login)."""
    home = Path(os.path.expanduser('~'))
    env = os.environ.get('CLAUDE_CONFIG_DIR')
    if env and Path(env).parent.resolve() != (machine() / 'accounts' / 'claude').resolve():
        return Path(env), Path(env) / '.claude.json'
    return home / '.claude', home / '.claude.json'


def prepare_claude_dir(directory, main_dir=None, main_json=None):
    """Makes an extra Claude account's folder behave like the main login: its settings (hooks, plugins on, model), the
    plugins themselves, and the auto-mode/onboarding acknowledgements. With CLAUDE_CONFIG_DIR set Claude Code reads
    nothing of ~/.claude, so a backup otherwise ran with no settings, no kith plugin and auto mode never entered.
    Never touches the account's login, machine id, projects or history. Runs on every launch: it writes only what
    differs, atomically, and any failure is left for the next launch. The names of what it changed."""
    directory = Path(directory)
    if main_dir is None:
        main_dir, main_json = main_claude_paths()
    main_dir, main_json = Path(main_dir), Path(main_json or Path(main_dir) / '.claude.json')
    changed = []
    try:
        if not directory.is_dir() or directory.resolve() == main_dir.resolve():
            return changed
        os.chmod(directory, 0o700)
        # settings.json: a copy, taken again whenever the main one changed. A missing or broken main file changes nothing.
        src = main_dir / 'settings.json'
        if _read_json(src) is not None and _copy_if_changed(src, directory / 'settings.json'):
            changed.append('settings.json')
        # plugins
        plugins_src, plugins_dst = main_dir / 'plugins', directory / 'plugins'
        if plugins_src.is_dir():
            plugins_dst.mkdir(mode=0o700, exist_ok=True)
            for name in PLUGIN_FILES:
                if (plugins_src / name).is_file() and _read_json(plugins_src / name) is not None \
                        and _copy_if_changed(plugins_src / name, plugins_dst / name):
                    changed.append(f'plugins/{name}')
            for name in PLUGIN_DIRS:
                link = plugins_dst / name
                if (plugins_src / name).is_dir() and not os.path.lexists(link):
                    os.symlink(plugins_src / name, link)
                    changed.append(f'plugins/{name}')
        # .claude.json: only the acknowledgement flags, into the file Claude Code keeps rewriting
        main_cfg, cfg_path = _read_json(main_json), directory / '.claude.json'
        if main_cfg is not None:
            want = {k: main_cfg[k] for k in MIRRORED_FLAGS if k in main_cfg}
            cfg = _read_json(cfg_path) if cfg_path.exists() else {}
            if cfg is not None and any(cfg.get(k) != v for k, v in want.items()):
                # Read again just before the swap: Claude Code may have written in between.
                cfg = _read_json(cfg_path) if cfg_path.exists() else {}
                if cfg is not None:
                    cfg.update(want)
                    _write_atomic(cfg_path, json.dumps(cfg, indent=2).encode())
                    changed.append('.claude.json')
    except OSError as e:
        print(f'kith: could not sync {directory} with the main Claude login: {e}', file=sys.stderr)
    return changed


def launch_env(agent, entry, light=False, record=False):
    """Shell assignments for bin/kith. Raises ValueError (exit 4) when the entry can't run. light: the main account's
    check (present), not the harness's status command."""
    harness, name = parse(entry)
    if harness not in ('claude', 'claude-api', 'codex'):
        raise ValueError(f'{entry}: {harness} is not supported in launch-json yet')
    model = split(entry)[1]
    if model is None and not light:
        # --backup claude for a chain entry claude@sonnet: the id without its model finds it, unless the chain lists the
        # plain id too (the exact entry wins).
        # The set's first entry is the main account (it takes no model), so only the rest are backups.
        ids = (kith_settings.accounts(agent)[0] or [])[1:]
        if entry not in ids:
            found = [split(i)[1] for i in ids if split(i)[0] == entry and split(i)[1]]
            model = found[0] if found else None
    ready, why = present(entry, agent) if light else check(entry, agent)
    if not ready:
        hint = fix(entry, agent)
        raise ValueError(f'{entry}: {why}' + (f' (make it ready: {hint})' if hint else ''))
    q = shlex.quote
    run_harness_name = 'claude' if harness == 'claude-api' else harness
    cfg_var = cfg_dir = ''
    if name and harness in ('claude', 'codex'):
        cfg_var = 'CLAUDE_CONFIG_DIR' if harness == 'claude' else 'CODEX_HOME'
        cfg_dir = str(account_dir(harness, name))
        if harness == 'claude':
            prepare_claude_dir(cfg_dir)
    helper = api_helper(agent, name) if harness == 'claude-api' else ''
    if record:
        record_started(entry)
    return (f'backup_harness={q(run_harness_name)} backup_account={q(name or "")} backup_cfg_var={q(cfg_var)} '
            f'backup_cfg_dir={q(cfg_dir)} backup_api_helper={q(helper)} backup_model={q(model or "")}')


def ledger_id(provider, account, account_id):
    """A ledger row's account id; None when it can't tell. Rows from before account_id: provider[:account]; rows from
    before provider don't say."""
    if account_id:
        return account_id
    return provider + (f':{account}' if account else '') if provider else None


def session_account(sid):
    """The account id the session was made under, from the cost ledger; None when the ledger doesn't know it."""
    import sqlite3
    db = machine() / 'cost.db'
    if not sid or not db.is_file():
        return None
    try:
        c = sqlite3.connect(db.as_uri() + '?mode=ro', uri=True)
        try:
            cols = [r[1] for r in c.execute('pragma table_info(runs)')]
            if 'provider' not in cols:
                return None
            pick = 'account_id' if 'account_id' in cols else 'null'
            row = c.execute(f'select provider, account, {pick} from runs where session = ?', (sid,)).fetchone()
        finally:
            c.close()
    except sqlite3.Error:
        return None
    return ledger_id(*row) if row else None


def session_run(sid):
    """{agent, harness, model, effort} a session was started with, from the cost ledger (the run's tag: KITH_PROVIDER,
    KITH_MODEL, KITH_EFFORT); None when the ledger doesn't know it. Harness is claude for a row from before provider."""
    import sqlite3
    db = machine() / 'cost.db'
    if not sid or not db.is_file():
        return None
    try:
        c = sqlite3.connect(db.as_uri() + '?mode=ro', uri=True)
        try:
            cols = [r[1] for r in c.execute('pragma table_info(runs)')]
            pick = 'provider' if 'provider' in cols else 'null'
            row = c.execute(f'select agent, {pick}, model, effort from runs where session = ?', (sid,)).fetchone()
        finally:
            c.close()
    except sqlite3.Error:
        return None
    return {'agent': row[0], 'harness': row[1] or 'claude', 'model': row[2] or None, 'effort': row[3] or None} if row else None


# --- the state file: what each account is doing right now ---------------------------------------------------------------
# One file for the whole Mac, <machine>/accounts/state.json, owned by the kernel and written by every run that learns
# something (a limit hit, a launch) and, later, by the host (kith account out|clear|usage). Local to this Mac like the
# logins themselves; never an account folder (list reads accounts/<harness>/<name> only). No token, credential or key is
# ever put in it: ids, times, reasons and usage percentages only.
#
#   {"version": 1, "accounts": {"claude:work": {"out_until": 1900000000, "reason": "rate_limit", "since": 1899990000,
#                                               "cleared_at": 1899999000,
#                                               "usage": {"read_at": 1899990000, "windows": [
#                                                  {"name": "five_hour", "percent": 71.5, "resets_at": 1899999999}]},
#                                               "last_started": 1899990000}}}
#
# out_until in the past means ready again; a missing, unreadable or corrupt file means nothing is out. Writers take an
# flock on state.json.lock, read the file again under it, and swap a new one in (temp + rename), so two runs marking
# different accounts at once lose nothing. A corrupt file is kept as state.json.corrupt, then replaced on the next write.
OUT_FOR = 3600   # as the host: how long an account stays out when the provider didn't say when its limit lifts
USAGE_STALE = 30 * 60   # a usage reading older than this says nothing about now (as the host's)
REASON_RE = re.compile(r'^[a-z0-9][a-z0-9 _.-]{0,39}$')
WINDOW_RE = re.compile(r'^[A-Za-z0-9][A-Za-z0-9 _.-]{0,31}$')


def state_file():
    return machine() / 'accounts' / 'state.json'


@contextlib.contextmanager
def _state_lock():
    f = state_file()
    f.parent.mkdir(parents=True, exist_ok=True)
    with open(str(f) + '.lock', 'a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        yield


def _read_state():
    """(accounts dict, corrupt): the file's accounts; {} and corrupt=True when it exists but isn't ours."""
    try:
        text = state_file().read_text()
    except FileNotFoundError:
        return {}, False
    except OSError:
        return {}, True
    try:
        d = json.loads(text)
        accts = d['accounts']
        if not isinstance(accts, dict):
            raise ValueError
    except (ValueError, KeyError, TypeError):
        return {}, True
    return {k: v for k, v in accts.items() if isinstance(k, str) and isinstance(v, dict)}, False


def _write_state(accts):
    f = state_file()
    _write_atomic(f, (json.dumps({'version': 1, 'accounts': accts}, indent=2) + '\n').encode())


def legacy_files():
    """The host's backups.json (read only: the host holds it in memory and rewrites it until it moves to this file) and
    the file the first kernel chain wrote beside it (imported once, then renamed)."""
    d = machine() / 'serve'
    return d / 'backups.json', d / 'kernel-backups-out.json'


def _legacy_marks(path):
    try:
        d = json.loads(path.read_text())
        return {k: v for k, v in d.items() if isinstance(k, str) and isinstance(v, dict)
                and float(v.get('out_until', 0)) > 0}
    except (OSError, ValueError, TypeError, AttributeError):
        return {}


def _import_kernel_marks():
    """serve/kernel-backups-out.json into the state file, once: the later out_until wins; the old file is renamed."""
    old = legacy_files()[1]
    if not old.exists():
        return
    try:
        with _state_lock():
            marks = _legacy_marks(old)
            accts, _ = _read_state()
            for k, v in marks.items():
                e = accts.setdefault(k, {})
                if float(v['out_until']) > float(e.get('out_until', 0)):
                    e.update(out_until=int(v['out_until']), reason=str(v.get('reason') or 'rate_limit')[:40],
                             since=int(v.get('since') or v['out_until']))
            _write_state(accts)
            os.replace(old, old.with_name(old.name + '.imported'))
    except (OSError, ValueError, TypeError):
        pass   # read-only disk or a race that lost: the next call tries again


def read_state():
    """The state file's accounts ({} when there is none or it is corrupt), the old kernel marks imported first."""
    _import_kernel_marks()
    return _read_state()[0]


def update_state(change):
    """change(accounts) under the lock, then written back atomically. Returns nothing; a disk that can't be written to
    loses the mark (the run goes on), it never fails the run."""
    try:
        with _state_lock():
            accts, corrupt = _read_state()
            if corrupt:
                try:
                    os.replace(state_file(), state_file().with_name('state.json.corrupt'))
                except OSError:
                    pass
            change(accts)
            _write_state(accts)
    except OSError:
        pass


def out_marks(state=None, now=None):
    """{id: {out_until, reason}} for every id out until a time in the future: this file, and the host's own file while the
    host still keeps one (a mark of the host's older than the account's last `clear` is ignored)."""
    now = now or time.time()
    state = read_state() if state is None else state
    marks = {}
    for k, v in _legacy_marks(legacy_files()[0]).items():
        cleared = state.get(k) or {}
        # A mark the host made before our `clear` is ignored; one it made or extended after it (a later since, or an
        # out_until past the one that was cleared: the host keeps `since` when it extends) counts.
        if float(v['out_until']) > now and (float(v.get('since', 0)) > float(cleared.get('cleared_at', 0))
                                            or float(v['out_until']) > float(cleared.get('cleared_host_until', 0))):
            marks[k] = {'out_until': int(float(v['out_until'])), 'reason': str(v.get('reason') or 'rate_limit')}
    for k, v in state.items():
        try:
            if float(v.get('out_until', 0)) > now and float(v['out_until']) > float(marks.get(k, {}).get('out_until', 0)):
                marks[k] = {'out_until': int(v['out_until']), 'reason': str(v.get('reason') or 'rate_limit')}
        except (TypeError, ValueError):
            continue
    return marks


def mark_out(entry, until=None, reason='rate_limit', now=None, extend_only=True):
    """Mark an account out until `until` (epoch; default an hour from now). By default an account already out for longer
    stays so; `kith account out` sets it exactly."""
    now = int(now or time.time())
    until = int(until) if until else now + OUT_FOR

    def change(accts):
        e = accts.setdefault(entry, {})
        cur = e.get('out_until', 0)
        if extend_only and isinstance(cur, (int, float)) and cur > max(until, now):
            return
        e['since'] = e.get('since', now) if isinstance(cur, (int, float)) and cur > now else now
        e['out_until'], e['reason'] = until, reason
    update_state(change)


def clear_out(entry, now=None):
    now = int(now or time.time())
    host = _legacy_marks(legacy_files()[0]).get(entry)

    def change(accts):
        e = accts.setdefault(entry, {})
        for k in ('out_until', 'reason', 'since'):
            e.pop(k, None)
        e['cleared_at'] = now
        e['cleared_host_until'] = int(float(host['out_until'])) if host else 0
    update_state(change)


def record_started(entry, now=None):
    now = int(now or time.time())
    update_state(lambda accts: accts.setdefault(entry, {}).__setitem__('last_started', now))


def validate_usage(reading, now=None):
    """A usage reading as the state keeps it, or ValueError saying what's wrong. Input:
    {"windows": [{"name": "five_hour", "percent": 71.5, "resets_at": 1900000000}], "read_at": 1899990000};
    read_at defaults to now, resets_at is optional. Nothing else is accepted, so nothing secret can be kept."""
    if not isinstance(reading, dict) or set(reading) - {'windows', 'read_at'}:
        raise ValueError('a usage reading is {"windows": [{"name", "percent", "resets_at"?}], "read_at"?}')
    wins = reading.get('windows')
    if not isinstance(wins, list) or not wins or len(wins) > 8:
        raise ValueError('"windows" is a list of 1 to 8 windows')
    out = []
    for w in wins:
        if not isinstance(w, dict) or set(w) - {'name', 'percent', 'resets_at'}:
            raise ValueError('a window is {"name", "percent", "resets_at"?}')
        name, pct, reset = w.get('name'), w.get('percent'), w.get('resets_at')
        if not isinstance(name, str) or not WINDOW_RE.match(name):
            raise ValueError('a window name is up to 32 letters, digits, space . _ -')
        if isinstance(pct, bool) or not isinstance(pct, (int, float)) or not 0 <= pct <= 100:
            raise ValueError('percent is a number from 0 to 100')
        item = {'name': name, 'percent': float(pct)}
        if reset is not None:
            if isinstance(reset, bool) or not isinstance(reset, (int, float)) or reset <= 0:
                raise ValueError('resets_at is a Unix time in seconds')
            item['resets_at'] = int(reset)
        out.append(item)
    at = reading.get('read_at', int(now or time.time()))
    if isinstance(at, bool) or not isinstance(at, (int, float)) or at <= 0:
        raise ValueError('read_at is a Unix time in seconds')
    return {'read_at': int(at), 'windows': out}


def record_usage(entry, reading, now=None):
    usage = validate_usage(reading, now)
    update_state(lambda accts: accts.setdefault(entry, {}).__setitem__('usage', usage))
    return usage


def usage_level(entry_state, now):
    """The fullest window's percent of a fresh reading; None when there is none (or it is stale)."""
    u = (entry_state or {}).get('usage')
    try:
        if not u or now - float(u['read_at']) > USAGE_STALE:
            return None
        return max(float(w['percent']) for w in u['windows'])
    except (KeyError, TypeError, ValueError):
        return None


def parse_until(text, now=None):
    """`--until T`: a Unix time, a span (90m, 2h, 1d, +30s), or a local date and time (2026-11-04 09:30, with a T too)."""
    now = int(now or time.time())
    t = text.strip()
    if re.fullmatch(r'\d{9,11}', t):
        return int(t)
    m = re.fullmatch(r'\+?(\d+)\s*([smhd])', t)
    if m:
        return now + int(m.group(1)) * {'s': 1, 'm': 60, 'h': 3600, 'd': 86400}[m.group(2)]
    for fmt in ('%Y-%m-%d %H:%M:%S', '%Y-%m-%dT%H:%M:%S', '%Y-%m-%d %H:%M', '%Y-%m-%dT%H:%M', '%Y-%m-%d'):
        try:
            return int(time.mktime(time.strptime(t, fmt)))
        except ValueError:
            pass
    raise ValueError(f"'{text}' is not a time: use a Unix time, a span like 90m or 2h, or a local date and time (2026-11-04 09:30)")


# --- selection: which account a run starts on ---------------------------------------------------------------------------
# static_order is what the settings say (pin, then the set), the same on every call. pick() lays the state over it: the
# policy orders the set, accounts that are out or signed out are skipped, and what is left is the run's candidate list.

def fill_policy(ids, state, now):
    """The set's own order: each account until it is out."""
    return list(ids)


def spread_policy(ids, state, now):
    """The account with the most room first: the lowest fullest-window percent of a fresh reading (no reading counts as
    empty), ties by the one started least recently, then the set's order."""
    def key(n):
        e = state.get(ids[n]) or {}
        level, started = usage_level(e, now), e.get('last_started', 0)
        return (level if level is not None else 0.0, started if isinstance(started, (int, float)) else 0, n)
    return [ids[n] for n in sorted(range(len(ids)), key=key)]


# A later resource manager adds its policy here (a function (ids, state, now) -> ids) and names it in account_policy.
POLICIES = {'fill': fill_policy, 'spread': spread_policy}


def in_family(entry, harness):
    return split(entry)[0].split(':')[0] in family(harness)   # claude@sonnet is a Claude account (the model is not part of it)


def layout(agent, harness):
    """(pin, own, foreign): the agent's pin (None when it has none or the harness can't use it), then the accounts of its
    own harness family in set order, then those of other harnesses in set order. Accounts of another harness count only
    when the agent's own list names them, or cross-harness fallback is on for it (kith account cross-harness). The
    agent's own list when it has one, else the global set. Without any account of the family: the harness's default login,
    unless the agent's own list is explicitly empty (then the pin, if any, is all there is)."""
    ids, where = kith_settings.accounts(agent)
    ids = ids or []
    pin = kith_settings.pin(agent) if agent else None
    if pin:
        try:
            check_main(pin['id'], harness)
        except ValueError:
            pin = None   # a hand-edited pin the agent's harness can't use
    own = [i for i in ids if in_family(i, harness)]
    foreign = [i for i in ids if not in_family(i, harness)] if where == 'agent' or kith_settings.cross_harness(agent) else []
    if not own and not (where == 'agent' and not ids and pin):
        own = [harness]
    if pin:
        own, foreign = [i for i in own if i != pin['id']], [i for i in foreign if i != pin['id']]
    return pin, own, foreign


def static_order(agent, harness):
    """(order, pin): the agent's accounts as the settings say, the same on every call: the pin first (alone when it is
    "only"), then its own harness family, then other harnesses."""
    pin, own, foreign = layout(agent, harness)
    if pin:
        return ([pin['id']] if pin['only'] else [pin['id']] + own + foreign), pin
    return own + foreign, None


def main_id(agent, agent_md=None, harness=None):
    """The account the agent's runs start on when nothing is out: the first of its static order."""
    h = harness or agent_harness(agent_md or agent_md_of(agent))
    return static_order(agent, h)[0][0]


def account_status(entry, agent, out, deep=True, assume_default=False):
    """{'status': ready|out|signed_out|unsupported|invalid, 'why', 'fix'?, 'out_until'?, 'reason'?}.
    deep asks the harness whether the login is good (claude auth status / codex login status); not deep only looks for the
    folder or the granted key. assume_default: the default login counts as signed in (the host may hold its keys where
    `auth status` can't see them), as the main account always did."""
    try:
        harness, name = parse(entry)
    except ValueError as e:
        return {'status': 'invalid', 'why': str(e)}
    if harness not in ('claude', 'claude-api', 'codex'):
        return {'status': 'unsupported', 'why': f'{harness} is not supported yet'}
    if entry in out:
        o = out[entry]
        return {'status': 'out', 'why': 'out of usage', 'out_until': o['out_until'], 'reason': o['reason']}
    if assume_default and not name and harness != 'claude-api':
        ready, why = True, None
    else:
        ready, why = check(entry, agent) if deep else present(entry, agent)
    if ready:
        return {'status': 'ready', 'why': None}
    out_ = {'status': 'signed_out', 'why': why}
    hint = fix(entry, agent)
    if hint:
        out_['fix'] = hint
    return out_


def _statuses(ids, agent, out, deep, assume_default):
    """account_status for each id; the harness status commands run side by side."""
    ids = list(dict.fromkeys(ids))
    if deep and len(ids) > 1:
        with concurrent.futures.ThreadPoolExecutor(max_workers=min(8, len(ids))) as ex:
            res = list(ex.map(lambda i: account_status(i, agent, out, deep, assume_default), ids))
    else:
        res = [account_status(i, agent, out, deep, assume_default) for i in ids]
    return dict(zip(ids, res))


def pick(agent, agent_md=None, deep=True, now=None, state=None):
    """The ordered candidate list for a run of the agent (`kith account pick AGENT --json`):

      {"version": 1, "agent", "harness", "policy", "pin": {"id","only"} | null, "cross_harness": bool,
       "candidates": [ids to try in order], "skipped": [{"id","status","why", "out_until"?, "reason"?}], "all_out": bool}

    The pin (preferred, or only) first, then the agent's set in policy order; accounts that are out or signed out are
    skipped. When every candidate is skipped the first of the static order is the only candidate (all_out true): it runs
    and the harness says what is wrong, as the main account always did."""
    now = now or time.time()
    harness = agent_harness(agent_md or agent_md_of(agent))
    state = read_state() if state is None else state
    out = out_marks(state, now)
    order, _ = static_order(agent, harness)
    policy = kith_settings.account_policy()
    pin, own, foreign = layout(agent, harness)
    ordered = order[:1] if pin else []
    if not (pin and pin['only']):
        ordered += POLICIES.get(policy, fill_policy)(own, state, now) + foreign
    status = _statuses(ordered, agent, out, deep, assume_default=not deep)
    cands = [i for i in ordered if status[i]['status'] == 'ready']
    skipped = [{'id': i, **{k: v for k, v in status[i].items() if k != 'fix' and v is not None}}
               for i in ordered if status[i]['status'] != 'ready']
    all_out = not cands
    return {'version': 1, 'agent': agent, 'harness': harness, 'policy': policy, 'pin': pin,
            'cross_harness': kith_settings.cross_harness(agent),
            'candidates': cands or order[:1], 'skipped': skipped, 'all_out': all_out}


# --- every account, as `kith account list` and `--accounts-json` show them ---------------------------------------------------

def known_ids(agent=None):
    """Every id worth listing: the sets (global, each agent's, or just this agent's), pins, the default logins of the
    installed harnesses, and every account folder on this Mac. Hand-edited bad ids stay in, as invalid."""
    s = kith_settings.read()[0]
    ids = list(s.get('accounts') or [])
    agents = s.get('agents', {})
    for name, a in agents.items():
        if agent and name != agent:
            continue
        ids += a.get('accounts', []) + ([a['pin']['id']] if a.get('pin') else [])
    for h in ('claude', 'codex'):
        if shutil.which(h):
            ids.append(h)
        root = machine() / 'accounts' / h
        ids += [f'{h}:{d.name}' for d in sorted(root.iterdir()) if d.is_dir() and not d.name.startswith('.')] if root.is_dir() else []
    return list(dict.fromkeys(ids))


def usage_json(e, now):
    u = (e or {}).get('usage')
    if not u:
        return None
    try:
        return {'read_at': int(u['read_at']), 'age_s': max(0, int(now - float(u['read_at']))),
                'windows': [{'name': w['name'], 'percent': w['percent'], **({'resets_at': w['resets_at']} if 'resets_at' in w else {})}
                            for w in u['windows']]}
    except (KeyError, TypeError, ValueError):
        return None


def rows(agent=None, deep=True, now=None, ids=None):
    """One row per account (the contract of `kith account list --json`, an array of):

      id, harness ('claude'|'codex'|other|null), account (name or null), kind ('subscription'|'api-key'|'unknown'),
      in_set (global set, or with an agent that agent's set), order (1-based place in it, or null),
      status ('ready'|'out'|'signed_out'|'unsupported'|'invalid'), ready (bool), why, fix (the command that makes it ready),
      out_until (epoch) and reason when out, usage ({read_at, age_s, windows: [{name, percent, resets_at?}]} or null),
      last_started (epoch or null), pins ([{agent, only}]), logged_in (not signed_out), name and dir (folders only; the older shape)

    With an agent, in_set/order are that agent's static order (pin first) and `candidate` is its place in pick()."""
    now = now or time.time()
    state = read_state()
    out = out_marks(state, now)
    s = kith_settings.read()[0]
    if agent:
        harness = agent_harness(agent_md_of(agent))
        in_order, _ = static_order(agent, harness)
        cands = pick(agent, deep=deep, now=now, state=state)['candidates']
    else:
        in_order, cands = list((s.get('accounts') or [])), []
    ids = list(dict.fromkeys(list(in_order) + (ids if ids is not None else known_ids(agent))))
    status = _statuses(ids, agent, out, deep, assume_default=False)
    pins = {}
    for name, a in s.get('agents', {}).items():
        if a.get('pin'):
            pins.setdefault(a['pin']['id'], []).append({'agent': name, 'only': bool(a['pin'].get('only'))})
    result = []
    for i in ids:
        try:
            harness_, name = parse(i)
        except ValueError:
            harness_, name = None, None
        st, e = status[i], state.get(i) or {}
        row = {'id': i, 'harness': 'claude' if harness_ == 'claude-api' else harness_, 'account': name,
               'kind': kind(harness_, name) if harness_ else 'unknown',
               'in_set': i in in_order, 'order': in_order.index(i) + 1 if i in in_order else None,
               'status': st['status'], 'ready': st['status'] == 'ready', 'why': st.get('why'),
               'fix': st.get('fix'), 'usage': usage_json(e, now),
               'last_started': e.get('last_started') if isinstance(e.get('last_started'), (int, float)) else None,
               'pins': pins.get(i, [])}
        if st['status'] == 'out':
            row['out_until'], row['reason'] = st['out_until'], st['reason']
        if agent:
            row['candidate'] = cands.index(i) + 1 if i in cands else None
        row['logged_in'] = st['status'] != 'signed_out'   # the older fields, which the host reads
        if harness_ in ('claude', 'codex') and name:
            row.update(name=name, dir=str(account_dir(harness_, name)))
        result.append(row)
    return result


def accounts_json(agent, agent_md):
    """`kith AGENT --accounts-json`, the host's view of the agent's accounts (docs/accounts.md):

      {"version": 1, "agent", "harness", "policy", "pin": {"id","only"} | null, "cross_harness": bool, "main": id,
       "order": [ids, pin first],
       "candidates": [ids pick() would try], "all_out": bool, "accounts": [rows() for each id in order]}"""
    harness = agent_harness(agent_md)
    order, pin = static_order(agent, harness)
    state = read_state()
    p = pick(agent, agent_md, deep=True, state=state)
    return {'version': 1, 'agent': agent, 'harness': harness, 'policy': p['policy'], 'pin': pin, 'main': order[0],
            'cross_harness': p['cross_harness'],
            'order': order, 'candidates': p['candidates'], 'all_out': p['all_out'], 'skipped': p['skipped'],
            'accounts': rows(agent, deep=True, ids=order)}


def main_json(agent=None):
    """The main account for the apps and `kith account use --json`: with an agent, the first of its static order and where
    that comes from ('agent': its pin, 'global': the set, 'default': the harness's login); without, the first of the
    global set (id null when there is none). Readiness as in the chain, with the command that fixes it."""
    if agent:
        md = agent_md_of(agent)
        harness = agent_harness(md)
        order, pin = static_order(agent, harness)
        entry = order[0]
        source = 'agent' if pin and pin['id'] == entry else 'default' if entry == harness else 'global'
    else:
        gl = kith_settings.accounts(None)[0] or []
        entry, harness, source = (gl[0] if gl else None), None, 'global'
    out = {'id': entry, 'source': source, 'harness': harness}
    if entry:
        try:
            e = entry_json(entry, agent, primary=True)
        except ValueError as err:   # a hand-edited bad id
            e = {'ready': False, 'why': str(err)}
        out['ready'] = e['ready']
        if not e['ready']:
            out['why'] = e.get('why')
            try:
                out['fix'] = fix(entry, agent)
            except ValueError:
                pass
    return out


def env_for(agent, entry, record=False):
    """bin/kith's assignments for a run on `entry` (main_id plus launch_env's; empty for a default login). A named
    account the run can't use raises ValueError (exit 4) with what fixes it."""
    if parse(entry)[1] is None:
        env = "backup_harness='' backup_account='' backup_cfg_var='' backup_cfg_dir='' backup_api_helper=''"
    else:
        try:
            env = launch_env(agent, entry, light=True)
        except ValueError as e:
            raise ValueError(f"{agent}'s main account {e}; or run on the default login: kith account unpin {agent}") from None
    if record:
        record_started(entry)
    return f'main_id={shlex.quote(entry)} {env}'


def main_env(agent, agent_md, use_state=False, record=False, prefer=None):
    """The account a plain run starts on, for bin/kith. By default the static main (pin, else the set's first): what a
    host launch and a guest's run use. use_state: pick()'s first candidate, so a run skips accounts that are out; prefer
    (the account a handed-on session was made under) goes first when it is one of the candidates."""
    entry = main_id(agent, agent_md)
    if use_state:
        picked = pick(agent, agent_md, deep=False)
        cands = picked['candidates']
        if prefer and prefer in cands and not picked['all_out']:
            cands = [prefer] + [i for i in cands if i != prefer]
        elif cands[0] != entry:
            why = next((x for x in picked['skipped'] if x['id'] == entry), None)
            if why:
                print(f"kith: {entry} is {why['status'].replace('_', ' ')}"
                      + (f" until {time.strftime('%H:%M', time.localtime(why['out_until']))}" if why.get('out_until') else '')
                      + f"; starting on {cands[0]}", file=sys.stderr)
        entry = cands[0]
    return env_for(agent, entry, record)


# --- a one-shot over the candidates (bin/kith, when another run or a schedule started it) -----------------------------------
# The host picks an account per turn itself and passes --account. A run kith starts on its own (an agent asking another,
# a schedule) has no host in between, so this is the same walk in the kernel: pick()'s candidates, the first one that
# can run, and on a usage-limit message mark the account out and move to the next. Each attempt is a whole
# `kith AGENT --account ID ...` child, so it claims its session and execs claude as any run does.
LIMIT_RE = re.compile(r"you've hit your .{0,30}limit|usage limit|weekly limit|rate.?limit", re.I)


def limited(code, text):
    """A failed run whose short output says the account is out of usage (a long output that mentions limits is a task)."""
    return code != 0 and len(text) < 4000 and bool(LIMIT_RE.search(text))


def run_chain(agent, agent_md, kith, args):
    harness = agent_harness(agent_md)
    todo = pick(agent, agent_md, deep=False)['candidates']
    code, final = 4, ''
    for n, entry in enumerate(todo):
        # The first attempt of its own harness's family is the plain run (it picks the same account); any other is named.
        plain = n == 0 and in_family(entry, harness)
        argv = [str(kith), agent] + ([] if plain else ['--account', entry]) + args
        with tempfile.TemporaryFile('w+') as so, tempfile.TemporaryFile('w+') as se:
            code = subprocess.run(argv, stdout=so, stderr=se, env={**os.environ, 'KITH_CHAIN_CHILD': '1'}).returncode
            so.seek(0); se.seek(0)
            final, err = so.read(), se.read()
        sys.stderr.write(err)
        if code == 4 and n + 1 < len(todo):   # this account can't run (not logged in): the next one
            print(f'kith: {entry} can\'t run; trying {todo[n + 1]}', file=sys.stderr)
            continue
        if limited(code, final + err):
            mark_out(entry)
            if n + 1 < len(todo):
                print(f'kith: {entry} is out of usage; moving to {todo[n + 1]}', file=sys.stderr)
                continue
        break
    sys.stdout.write(final)
    sys.stdout.flush()
    return code


if __name__ == '__main__':
    a = sys.argv[1:]
    record = '--record' in a
    use_state = '--state' in a
    prefer = a[a.index('--prefer') + 1] if '--prefer' in a and a.index('--prefer') + 1 < len(a) else None
    skip = {'--record', '--state', '--prefer', prefer} if prefer else {'--record', '--state'}
    a = [x for x in a if x not in skip]
    try:
        if len(a) >= 5 and a[0] == 'run-chain' and a[4] == '--':
            sys.exit(run_chain(a[1], a[2], a[3], a[5:]))
        if len(a) == 3 and a[0] == 'backups-json':
            print(json.dumps(chain(a[1], a[2])))
        elif len(a) == 3 and a[0] == 'accounts-json':
            print(json.dumps(accounts_json(a[1], a[2])))
        elif len(a) == 3 and a[0] == 'launch-env':
            print(launch_env(a[1], a[2], record=record))
        elif len(a) == 3 and a[0] == 'main-env':
            print(main_env(a[1], a[2], use_state=use_state, record=record, prefer=prefer))
        elif len(a) in (2, 3) and a[0] == 'session-run':
            r = session_run(a[1]) or {}
            if len(a) == 3 and a[2] == '--sh':   # for bin/kith: the harness and model a resumed thread keeps
                print(f"s_harness={shlex.quote(r.get('harness') or '')} s_model={shlex.quote(r.get('model') or '')} "
                      f"s_effort={shlex.quote(r.get('effort') or '')}")
            else:
                print(json.dumps(r))
        elif len(a) == 3 and a[0] == 'main-id':
            print(main_id(a[1], a[2]))
        elif len(a) == 2 and a[0] == 'session-account':
            print(session_account(a[1]) or '')
        else:
            sys.exit(__doc__.strip())
    except ValueError as e:
        print(f'kith: {e}', file=sys.stderr)
        sys.exit(4)
