#!/usr/bin/env python3
"""Backup chain entries: what an id means, whether it is ready, and how it launches.

An entry id is `<harness>` or `<harness>:<name>`:

  claude              the ambient Claude login (what runs today)
  claude:<name>       an extra Claude subscription account: CLAUDE_CONFIG_DIR=<machine>/accounts/claude/<name>
  claude-api:<profile>  an Anthropic API key held by kith-auth (anthropic/<profile>), handed to Claude Code through
                      apiKeyHelper, so the key is never in env, JSON or logs
  codex               the ambient Codex login (~/.codex)
  codex:<name>        CODEX_HOME=<machine>/accounts/codex/<name>
  anything else       a harness the host may learn later; here it is "not runnable yet"

Readiness is asked of the harness's own status command with the right config dir; no token, credential file or
Keychain item is ever read. The settings are lib/kith_settings.py's `backups` and `account`.

The main account (kith account use) is the login the agent's runs use when no backup answers: an id of the agent's own
harness family (claude, claude:<name>, claude-api:<profile> for a Claude agent; codex, codex:<name> for a Codex one).
Unset, it is the harness's default login. It is entry 0 of the chain (kind primary); the backups follow it, and an id
equal to it (or repeated) is left out, so no account is in a chain twice.

A session belongs to the account it was made under (its id: the cost ledger's runs.account_id, KITH_ACCOUNT_ID at
SessionStart): it is resumed only under that account.

  kith_accounts.py backups-json AGENT AGENT.MD   the chain for `kith AGENT --backups-json`
  kith_accounts.py launch-env AGENT ID           shell assignments for bin/kith; exit 4 + one line on stderr when not runnable
  kith_accounts.py main-env AGENT AGENT.MD       the main account for bin/kith: main_id=ID plus launch-env's assignments
                                                 (empty for the default login); exit 4 + one line when it can't run
  kith_accounts.py session-account SID           the account id a session was made under ('' when the ledger doesn't know it)
"""
import json, os, re, shlex, subprocess, sys
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.realpath(__file__)))
import kith_settings  # noqa: E402
from kith_paths import paths  # noqa: E402

KERNEL = Path(__file__).resolve().parent.parent
ID_RE = re.compile(r'^([a-z][a-z0-9-]*)(?::([A-Za-z0-9][A-Za-z0-9._-]*))?$')
STATUS_TIMEOUT = 20


def parse(entry):
    """(harness, name or None); ValueError says what is wrong with the id."""
    m = ID_RE.match(entry or '')
    if not m:
        raise ValueError(f"'{entry}' is not a backup id: use <harness> or <harness>:<name> (claude, claude:work, claude-api:main, codex)")
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
    """The entry harnesses that can be an agent's main account: its own, and for Claude an API key too."""
    return FAMILY.get(harness, (harness,))


def check_main(entry, harness):
    """Raise ValueError unless entry can be the main account of an agent on harness."""
    h, _ = parse(entry)
    if h not in family(harness):
        raise ValueError(f"{entry} can't be the main account of a {harness} agent: use "
                         + ', '.join(f'{x}' if x != 'claude-api' else 'claude-api:<profile>' for x in family(harness))
                         + ' or <harness>:<name> of those (a backup can be of another harness)')


def main_id(agent, agent_md=None, harness=None):
    """The agent's main account id: its own setting, else the global one, when of the agent's harness family; else the
    harness itself (its default login)."""
    own = harness or agent_harness(agent_md or agent_md_of(agent))
    s = kith_settings.read()[0]
    for entry in ((s.get('agents', {}).get(agent, {}) if agent else {}).get('account'), s.get('account')):
        if isinstance(entry, str) and entry:
            try:
                check_main(entry, own)
                return entry
            except ValueError:
                continue
    return own


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
           'kind': 'primary' if primary else kind(harness, name), 'ready': ready}
    if not ready:
        out['why'] = why
    return out


def chain(agent, agent_md):
    ids, _ = kith_settings.backups(agent)
    main = main_id(agent, agent_md)
    items, seen = [entry_json(main, agent, primary=True)], {main}
    for i in ids:
        if i in seen:   # the main account, or an id listed twice: an account is in the chain once
            continue
        seen.add(i)
        try:
            items.append(entry_json(i, agent))
        except ValueError as e:   # a hand-edited bad id: listed as not ready rather than failing the whole chain
            items.append({'id': i, 'harness': None, 'account': None, 'kind': 'unknown', 'ready': False, 'why': str(e)})
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


def launch_env(agent, entry, light=False):
    """Shell assignments for bin/kith. Raises ValueError (exit 4) when the entry can't run. light: the main account's
    check (present), not the harness's status command."""
    harness, name = parse(entry)
    if harness not in ('claude', 'claude-api', 'codex'):
        raise ValueError(f'{entry}: {harness} is not supported in launch-json yet')
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
    helper = api_helper(agent, name) if harness == 'claude-api' else ''
    return (f'backup_harness={q(run_harness_name)} backup_account={q(name or "")} backup_cfg_var={q(cfg_var)} '
            f'backup_cfg_dir={q(cfg_dir)} backup_api_helper={q(helper)}')


def main_json(agent=None):
    """The main account for the apps and `kith account use --json`: with an agent, the one its runs use and where that
    comes from ('agent', 'global', or 'default' for the harness's login); without, the global setting (id null when
    none). Readiness as in the chain, with the command that fixes it."""
    if agent:
        md = agent_md_of(agent)
        harness = agent_harness(md)
        entry = main_id(agent, md, harness)
        own = kith_settings.read()[0].get('agents', {}).get(agent, {}).get('account')
        source = 'agent' if entry == own else 'default' if entry == harness else 'global'
    else:
        entry, harness, source = kith_settings.account(None)[0] or None, None, 'global'
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


def main_env(agent, agent_md):
    """bin/kith's assignments for a run on the main account: main_id plus launch_env's (empty for the default login)."""
    entry = main_id(agent, agent_md)
    if parse(entry)[1] is None:
        env = "backup_harness='' backup_account='' backup_cfg_var='' backup_cfg_dir='' backup_api_helper=''"
    else:
        try:
            env = launch_env(agent, entry, light=True)
        except ValueError as e:
            raise ValueError(f"{agent}'s main account {e}; or run on the default login: kith account use --reset "
                             f"--agent {agent}") from None
    return f'main_id={shlex.quote(entry)} {env}'


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


if __name__ == '__main__':
    a = sys.argv[1:]
    try:
        if len(a) == 3 and a[0] == 'backups-json':
            print(json.dumps(chain(a[1], a[2])))
        elif len(a) == 3 and a[0] == 'launch-env':
            print(launch_env(a[1], a[2]))
        elif len(a) == 3 and a[0] == 'main-env':
            print(main_env(a[1], a[2]))
        elif len(a) == 2 and a[0] == 'session-account':
            print(session_account(a[1]) or '')
        else:
            sys.exit(__doc__.strip())
    except ValueError as e:
        print(f'kith: {e}', file=sys.stderr)
        sys.exit(4)
