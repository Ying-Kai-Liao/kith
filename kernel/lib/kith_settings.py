#!/usr/bin/env python3
"""kith's own settings: kith-wide, not secret, for this Mac.

  kith_settings.py KEY            print one setting: this Mac's ~/.kith/settings.json, else kith's default
  kith_settings.py KEY AGENT      print this Mac's choice for one agent (agents.AGENT.KEY), empty when there is none
  kith_settings.py set permission_mode AGENT MODE   record this Mac's choice for one agent; an empty MODE forgets it
                                  (the agent's own suggestion, then kith's default, apply again). kith AGENT set permission MODE
  kith_settings.py set call_model [AGENT] MODEL     the model voice-call turns run on: for one agent (kith AGENT set call-model
                                  MODEL), or without AGENT for every agent. 'default' (or '') forgets the choice; 'own' means
                                  the agent's own model, so a call changes nothing about which model answers

Settings that are kith's, not one agent's (its settings.json), one account's (kith-auth) or one entry point's (an env
var such as KITH_BRIDGE_PERMISSION_MODE). The file is this Mac's and is never committed, like secrets/env: what an
unattended Mac may do is decided per Mac. Without the file, or for a key it leaves out, the default below.

  permission_mode   the mode an owner's Claude Code run starts in when neither the caller (KITH_PERMISSION_MODE and
                    the entry points' variables) nor the agent's settings.json (permissions.defaultMode) chose one
  plugins           Claude Code plugins (ids, as `claude plugin list` shows them) every agent's run turns on, e.g.
                    ["jev-effort-router@jev-effort-router"]; an agent whose settings.json sets one in enabledPlugins
                    keeps its own choice. Without it, runs turn on kith@kith only
  agents            per agent, what this Mac chose over the agent's own settings: {"<agent>": {"permission_mode":
                    MODE}}. An agent's defaultMode is its author's suggestion; the mode chosen here (kith install asks)
                    wins over it, and only the caller's wins over this
  call_model        the model a turn spoken in a voice call runs on (same session, only the model differs): a model name
                    Claude Code takes ("sonnet", "haiku", "opus"), or "own" for the agent's own model. Default "sonnet":
                    a call wants a quick reply. agents.<agent>.call_model replaces it for that agent. Schedules, channels
                    and typed messages never use it
  backups           the backup chain kith serve tries when the agent's own harness runs out of usage: entry ids in
                    order ("claude:work", "claude-api:main", "codex"; see lib/kith_accounts.py). Empty by default.
                    agents.<agent>.backups replaces it for that agent; an explicit [] means no backups for it
                    (kith backup list|add|remove edits both)
  account           the main account: the login an agent's runs use when no backup answers, as one entry id of the
                    agent's own harness family (claude, claude:<name>, claude-api:<profile>; codex, codex:<name>).
                    Missing = the harness's default login (what ran before). agents.<agent>.account replaces it for that
                    agent. A global id of another family than an agent's harness does not apply to that agent.
                    The backup chain sits on top of it (kith account use edits both; lib/kith_accounts.py)

  limit             agents.<agent>.limit: a monthly limit in dollars at API prices for the agent's runs (kith AGENT set
                    limit N|none). No key, no limit. A calendar month counts; every run of the agent counts (kith-cost
                    limit AGENT reads it with what the agent has used)
  at_limit          agents.<agent>.at_limit: what happens to the owner's run once the agent is at its limit: "ask"
                    (default: the host parks the run behind a card, Allow this run / Raise / Stop; a terminal or
                    one-shot run says so and goes on), "tell" (a line, the run goes on) or "hold" (runs wait until the
                    limit is raised or the month turns). An owner's run is never refused unless "hold" is chosen here.
                    A guest's runs keep their own budget (kith-auth member AGENT PERSON budget=) and stop at it

  delegation        delegation.chain_cost_cap_usd: what one chain of `kith ask`s (an agent asks another, which asks a
                    third, ...) may cost in dollars at API prices before it starts no new ask and the owner gets a Needs-you
                    card. Default 5; "none" turns the cap off. Asks already running finish (kith ask --cap shows or sets it)

  kith_settings.py backups [AGENT]   the chain that applies, one id per line
  kith_settings.py account [AGENT]   the main account set for the agent (else the global one), '' when none is
  kith_settings.py limit AGENT       the agent's limit as JSON: {"limit": N or null, "at": "ask"|"tell"|"hold"}
  kith_settings.py set limit AGENT N|none          set or forget the agent's monthly limit (kith AGENT set limit)
  kith_settings.py set at_limit AGENT MODE         ask, tell or hold ('' forgets: ask) (kith AGENT set at-limit)
  kith_settings.py delegation.chain_cost_cap_usd   the chain cap that applies: a number, or none
  kith_settings.py set delegation.chain_cost_cap_usd N|none|default   set it (kith ask --cap N)
"""
import contextlib, fcntl, json, os, re, sys, tempfile

sys.path.insert(0, os.path.dirname(os.path.realpath(__file__)))
from kith_paths import paths  # noqa: E402  the one place that knows where kith's folders are

FILE = paths()['KITH_MACHINE'] / 'settings.json'
DEFAULTS = {'permission_mode': 'auto', 'plugins': [], 'backups': [], 'call_model': 'sonnet', 'account': ''}
CHOICES = {'permission_mode': ('default', 'acceptEdits', 'plan', 'auto', 'bypassPermissions'),
           'at_limit': ('ask', 'tell', 'hold')}
# Modes kith no longer offers, and what a stored one reads as: 'manual' was kith's own word for Claude Code's 'default';
# 'dontAsk' (refuse whatever no allow rule names) is gone, and its runs start in auto.
CHAIN_CAP_USD = 5   # the default cost cap of a delegation chain
RETIRED = {'manual': 'default', 'dontAsk': 'auto'}


# A Bash allow rule for a shell or interpreter lets the model run anything: broad. `Bash`, `Bash(*)`, `Bash(:*)` and a
# prefix of these programs (F11).
BROAD_SHELLS = ("sh", "bash", "zsh", "python", "python3", "node", "env", "perl", "ruby", "osascript", "uv run", "npx", "deno", "bun")


def is_broad_bash(rule):
    """Whether an allow rule lets a run execute arbitrary code: bare Bash, a wildcard, or a shell/interpreter prefix."""
    r = rule.strip()
    if r == "Bash":
        return True
    if not (r.startswith("Bash(") and r.endswith(")")):
        return False
    body = r[5:-1].strip().strip("\"'")
    if body in ("", "*", ":*", "**"):
        return True
    prog = re.split(r"[:*]", body)[0].strip()
    first = prog.split(" ")[0].rsplit("/", 1)[-1]   # /usr/bin/python3 counts as python3
    rest = (first + " " + " ".join(prog.split(" ")[1:])).strip()
    return not prog or any(rest == b or rest.startswith(b + " ") for b in BROAD_SHELLS)


def strip_broad_bash(rules, trusted):
    """The allow rules a non-owner run keeps: a trusted membership keeps all, anyone else loses every broad Bash rule."""
    return list(rules) if trusted else [r for r in rules if not is_broad_bash(r)]


def current(mode):
    """The mode a run starts in for a mode from any source: a retired one as the mode that replaced it."""
    return RETIRED.get(mode, mode) if isinstance(mode, str) else mode


def retired():
    """Where this Mac's file names a retired mode: [(key, mode)], e.g. ('agents.coder.permission_mode', 'dontAsk')."""
    try:
        s = json.loads(FILE.read_text())
    except (OSError, ValueError):
        return []
    if not isinstance(s, dict):
        return []
    agents = s.get('agents') if isinstance(s.get('agents'), dict) else {}
    places = [('permission_mode', s)] + [(f'agents.{n}.permission_mode', a) for n, a in agents.items() if isinstance(a, dict)]
    return [(k, d['permission_mode']) for k, d in places if current(d.get('permission_mode')) != d.get('permission_mode')]


def read():
    """(what the file sets, why it can't be used or None). A file that can't be used sets nothing."""
    try:
        s = json.loads(FILE.read_text())
    except FileNotFoundError:
        return {}, None
    except (OSError, ValueError) as e:
        return {}, f'{FILE} is unreadable ({e})'
    if not isinstance(s, dict):
        return {}, f'{FILE} is not a JSON object'
    agents = s.get('agents', {})
    # Old files still read back: a retired mode as the mode that replaced it (kith doctor says where one is).
    for d in [s] + (list(agents.values()) if isinstance(agents, dict) else []):
        if isinstance(d, dict) and 'permission_mode' in d:
            d['permission_mode'] = current(d['permission_mode'])
    if not isinstance(agents, dict) or not all(isinstance(a, dict) for a in agents.values()):
        return {}, f'{FILE}: agents is not {{"<agent>": {{...}}}}'
    plugins = s.get('plugins', [])
    if not isinstance(plugins, list) or not all(isinstance(p, str) for p in plugins):
        return {}, f'{FILE}: plugins is not a list of plugin ids'
    for where, d in [('', s)] + [(f'agents.{n}.', a) for n, a in agents.items()]:
        b = d.get('backups', [])
        if not isinstance(b, list) or not all(isinstance(i, str) for i in b):
            return {}, f'{FILE}: {where}backups is not a list of entry ids'
        if 'account' in d and not (isinstance(d['account'], str) and d['account']):
            return {}, f'{FILE}: {where}account is not an entry id'
        for k, v in d.items():
            if k in CHOICES and v not in CHOICES[k]:
                return {}, f'{FILE}: {where}{k} {v!r} is not one of {", ".join(CHOICES[k])}'
        if 'limit' in d and not valid_limit(d['limit']):
            return {}, f'{FILE}: {where}limit {d["limit"]!r} is not a number of dollars above 0'
    delegation = s.get('delegation', {})
    if not isinstance(delegation, dict) or not valid_chain_cap(delegation.get('chain_cost_cap_usd', CHAIN_CAP_USD)):
        return {}, f'{FILE}: delegation.chain_cost_cap_usd is not a number of dollars above 0 (or "none")'
    return s, None


def valid_chain_cap(v):
    """A chain's cost cap: dollars above 0, or "none" for no cap."""
    return v == 'none' or valid_limit(v)


def chain_cost_cap():
    """The dollars one delegation chain of kith asks may cost before it starts no new ask, None for no cap. A settings file
    that can't be read leaves the default, so a broken file never lifts the cap."""
    v = read()[0].get('delegation', {}).get('chain_cost_cap_usd', CHAIN_CAP_USD)
    return None if v == 'none' else v


def set_chain_cost_cap(value):
    """Record the chain cap: a number of dollars, 'none' for no cap, 'default' (or '') to forget the choice."""
    v = (value or '').strip().lstrip('$')
    if v.lower() in ('', 'default'):
        amount = None
    elif v.lower() in ('none', 'off'):
        amount = 'none'
    else:
        try:
            amount = float(v)
        except ValueError:
            raise ValueError(f'cap {value!r} is not a number of dollars (or none)') from None
        if not valid_limit(amount):
            raise ValueError(f'cap {value!r} is not a number of dollars above 0')
        amount = int(amount) if amount == int(amount) else round(amount, 4)
    with locked():
        s, why = read()
        if why:
            raise ValueError(why)
        d = s.setdefault('delegation', {})
        if amount is None:
            d.pop('chain_cost_cap_usd', None)
        else:
            d['chain_cost_cap_usd'] = amount
        if not d:
            s.pop('delegation')
        write(s)


def valid_limit(v):
    """A monthly limit: dollars above 0 (a bool is not a number here)."""
    return isinstance(v, (int, float)) and not isinstance(v, bool) and 0 < v < 1e7


def limit(agent):
    """(dollars or None, mode): this Mac's monthly limit for the agent and what happens at it (default ask).
    A file that can't be read sets no limit: a broken settings file never stops a run."""
    a = read()[0].get('agents', {}).get(agent, {})
    return (a.get('limit'), a.get('at_limit') or 'ask')


def set_limit(agent, value):
    """Record the agent's monthly limit; 'none', '' or 0 forgets it. Keeps the rest of the file."""
    v = (value or '').strip().lstrip('$')
    if v.lower() in ('', 'none', 'off', '0'):
        amount = None
    else:
        try:
            amount = float(v)
        except ValueError:
            raise ValueError(f'limit {value!r} is not a number of dollars (or none)') from None
        if not valid_limit(amount):
            raise ValueError(f'limit {value!r} is not a number of dollars above 0')
        amount = int(amount) if amount == int(amount) else round(amount, 2)
    with locked():
        s, why = read()
        if why:
            raise ValueError(why)
        a = s.setdefault('agents', {}).setdefault(agent, {})
        if amount is None:
            a.pop('limit', None)
        else:
            a['limit'] = amount
        if not a:
            s['agents'].pop(agent, None)
        write(s)


def get(key, agent=None):
    """kith's setting, or with an agent this Mac's choice for it ('' when there is none)."""
    if agent:
        return read()[0].get('agents', {}).get(agent, {}).get(key, '')
    return read()[0].get(key, DEFAULTS[key])


def valid_call_model(value):
    """A model Claude Code takes: an alias (optionally with its 1M window), a full claude-* id, or kith's own words."""
    return bool(re.fullmatch(r'(default|own|(sonnet|haiku|opus)(\[1m\])?|claude-[a-z0-9.\-]+(\[1m\])?)', value))


def global_call_model():
    """The model every agent's calls use unless it chose its own ('own' = the agent's own model)."""
    v = read()[0].get('call_model', DEFAULTS['call_model'])
    return v if isinstance(v, str) and v else DEFAULTS['call_model']


def call_model(agent=None):
    """The model a voice-call turn runs on for the agent ('' when it keeps its own model): the agent's choice, else the
    global one, else the default. 'own' is the explicit way to say "the agent's own model"."""
    s = read()[0]
    chosen = s.get('agents', {}).get(agent, {}).get('call_model') if agent else None
    if not chosen:
        chosen = s.get('call_model', DEFAULTS['call_model'])
    chosen = chosen if isinstance(chosen, str) else ''
    return '' if chosen in ('own', 'default') else chosen.strip()


def choose_global(key, value):
    """Record this Mac's choice for every agent; 'default' or '' forgets it. Keeps the rest of the file."""
    with locked():
        s, why = read()
        if why:
            raise ValueError(why)
        if key == 'call_model' and value and not valid_call_model(value):
            raise ValueError(f'call model {value!r} is not sonnet, haiku, opus, a claude-* model id, own or default')
        if value in ('', 'default'):
            s.pop(key, None)
        else:
            s[key] = value
        write(s)


def choose(agent, key, value):
    """Record this Mac's choice for one agent, keeping the rest of the file."""
    with locked():
        s, why = read()
        if why:
            raise ValueError(why)
        if key in CHOICES and value not in CHOICES[key] + ('',):
            raise ValueError(f'{key} {value!r} is not one of {", ".join(CHOICES[key])}')
        agents = s.setdefault('agents', {})
        if key == 'call_model' and value == 'default':
            value = ''
        if key == 'call_model' and value and not valid_call_model(value):
            raise ValueError(f'call model {value!r} is not sonnet, haiku, opus, a claude-* model id, own or default')
        if value == '':   # forgetting the choice: the agent's own suggestion and kith's default apply again
            agents.get(agent, {}).pop(key, None)
        else:
            agents.setdefault(agent, {})[key] = value
        write(s)


_held = False


@contextlib.contextmanager
def locked():
    """Exclusive lock on a sibling file around a read-modify-write, so two writers (the host, a CLI) never lose each
    other's change. Reentrant in this process; flock goes with the process, so a crash leaves nothing behind."""
    global _held
    if _held:
        yield
        return
    FILE.parent.mkdir(parents=True, exist_ok=True)
    with open(FILE.with_name(FILE.name + '.lock'), 'a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        _held = True
        try:
            yield
        finally:
            _held = False


def write(s):
    """The whole file, atomically (a reader never sees half of it)."""
    with locked():
        fd, tmp = tempfile.mkstemp(prefix=FILE.name + '.', suffix='.tmp', dir=FILE.parent)
        try:
            with os.fdopen(fd, 'w') as f:
                f.write(json.dumps(s, indent=2) + '\n')
                f.flush()
                os.fsync(f.fileno())
            umask = os.umask(0)
            os.umask(umask)
            os.chmod(tmp, 0o666 & ~umask)   # as a plain write_text made it, not mkstemp's 0600
            os.replace(tmp, FILE)
        except BaseException:
            try:
                os.unlink(tmp)
            except OSError:
                pass
            raise
        dfd = os.open(FILE.parent, os.O_RDONLY)
        try:
            os.fsync(dfd)
        finally:
            os.close(dfd)


def backups(agent=None):
    """(ids, where): the chain that applies to the agent, and whether it is the agent's own ('agent', even when []) or 'global'."""
    s = read()[0]
    own = s.get('agents', {}).get(agent, {}) if agent else {}
    if 'backups' in own:
        return list(own['backups']), 'agent'
    return list(s.get('backups', [])), 'global'


def set_backups(agent, ids):
    """Record the chain: one agent's override, or with agent None the global one. Keeps the rest of the file."""
    with locked():
        s, why = read()
        if why:
            raise ValueError(why)
        if agent:
            s.setdefault('agents', {}).setdefault(agent, {})['backups'] = list(ids)
        else:
            s['backups'] = list(ids)
        write(s)


def clear_backups(agent):
    """Drop one agent's own chain, so it follows the global one again. Keeps the rest of the file."""
    with locked():
        s, why = read()
        if why:
            raise ValueError(why)
        own = s.get('agents', {}).get(agent)
        if own is not None and 'backups' in own:
            del own['backups']
            write(s)


def account(agent=None):
    """(id or '', where): the main account set for the agent ('agent'), else the global one ('global'); '' = default login.
    The caller checks the id against the agent's harness (lib/kith_accounts.py main_id)."""
    s = read()[0]
    own = s.get('agents', {}).get(agent, {}) if agent else {}
    if own.get('account'):
        return own['account'], 'agent'
    return s.get('account') or '', 'global'


def set_account(agent, entry):
    """Record the main account: one agent's, or with agent None the global one. Keeps the rest of the file."""
    with locked():
        s, why = read()
        if why:
            raise ValueError(why)
        if agent:
            s.setdefault('agents', {}).setdefault(agent, {})['account'] = entry
        else:
            s['account'] = entry
        write(s)


def clear_account(agent):
    """Forget one agent's main account (it follows the global one again), or with agent None the global one."""
    with locked():
        s, why = read()
        if why:
            raise ValueError(why)
        d = s.get('agents', {}).get(agent) if agent else s
        if d is not None and 'account' in d:
            del d['account']
            if agent and not d:
                s['agents'].pop(agent, None)
            write(s)


if __name__ == '__main__':
    if len(sys.argv) == 5 and sys.argv[1] == 'set' and sys.argv[2] == 'limit':   # set limit AGENT N|none
        try:
            set_limit(sys.argv[3], sys.argv[4])
        except ValueError as e:
            sys.exit(f'kith: {e}')
        sys.exit(0)
    if len(sys.argv) == 4 and sys.argv[1:3] == ['set', 'delegation.chain_cost_cap_usd']:
        try:
            set_chain_cost_cap(sys.argv[3])
        except ValueError as e:
            sys.exit(f'kith: {e}')
        sys.exit(0)
    if len(sys.argv) == 2 and sys.argv[1] == 'delegation.chain_cost_cap_usd':
        cap = chain_cost_cap()
        print('none' if cap is None else cap)
        sys.exit(0)
    if len(sys.argv) == 3 and sys.argv[1] == 'limit':
        amount, at = limit(sys.argv[2])
        print(json.dumps({'limit': amount, 'at': at}))
        sys.exit(0)
    if len(sys.argv) in (4, 5) and sys.argv[1] == 'set':   # set KEY [AGENT] VALUE
        if sys.argv[2] not in ('permission_mode', 'call_model', 'at_limit') or (len(sys.argv) == 4 and sys.argv[2] != 'call_model'):
            sys.exit(f'kith: {sys.argv[2]} is not a setting kith can set per agent')
        try:
            if len(sys.argv) == 4:
                choose_global(sys.argv[2], sys.argv[3])
            else:
                choose(sys.argv[3], sys.argv[2], sys.argv[4])
        except ValueError as e:
            sys.exit(f'kith: {e}')
        sys.exit(0)
    if len(sys.argv) not in (2, 3) or sys.argv[1] not in DEFAULTS:
        sys.exit(__doc__.strip())
    if sys.argv[1] == 'call_model':   # the model that applies, '' for the agent's own
        print(call_model(*sys.argv[2:]))
        sys.exit(0)
    if sys.argv[1] == 'backups':
        print('\n'.join(backups(*sys.argv[2:])[0]))
        sys.exit(0)
    if sys.argv[1] == 'account':
        print(account(*sys.argv[2:])[0])
        sys.exit(0)
    v = get(*sys.argv[1:])
    print('\n'.join(v) if isinstance(v, list) else v)
