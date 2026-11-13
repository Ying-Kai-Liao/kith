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
  accounts          the account set: ordered ids ("claude", "claude:work", "claude-api:main", "codex", "codex:x"; see
                    lib/kith_accounts.py), all equal, any number per provider. Absent = no choice yet: every agent runs
                    on its harness's default login. An agent's runs take the accounts of its own harness family first, in
                    this order (or by account_policy), then those of another harness (a Claude agent falling over to
                    Codex); `kith account add|remove|order|move` edit it
  account_policy    how a run picks among the set: "fill" (default: the set's order, each account until it is out of
                    usage) or "spread" (the ready account with the most room left, ties by least recently started)
  account_cross_harness  true: the set's accounts of another harness also serve as fallbacks (a Claude agent falls over to a
                    Codex account in the set, and the other way round). Default false: the global set serves each agent with
                    the accounts of its own harness family only. agents.<a>.account_cross_harness overrides it for one agent
                    (kith account cross-harness on|off [--agent A]). An agent's own list (agents.<a>.accounts) is allowed to name
                    other harnesses whatever this says
  agents            per agent, what this Mac chose over the agent's own settings: {"<agent>": {"permission_mode": MODE,
                    "accounts": [...], "pin": {...}}}. An agent's defaultMode is its author's suggestion; the mode chosen
                    here (kith install asks) wins over it, and only the caller's wins over this
  agents.<a>.accounts  a subset/order for one agent, replacing the global set for it (kith account set IDS --agent A).
                    An explicit [] means no account is listed: the harness's default login, nothing to fall back to
  agents.<a>.pin    {"id": ID, "only": BOOL}: the account the agent's runs start on (kith account pin A ID [--only]).
                    Preferred (only: false) falls back to the set when the pinned account is out; only: true never leaves it
  Old files (account, backups, agents.<a>.account, agents.<a>.backups) are read as the keys above and rewritten in the new
  shape by the next write: account + backups -> accounts (main first); an agent's account -> its pin and the head of its
  accounts; an agent's backups -> its accounts (an explicit [] stays "no fallback"). account_cross_harness is turned on only
  when the old global backups listed an account of another harness than the old global main's (the default login of
  Claude when there was none): that chain said so; an old global account of another family never does
  call_model        the model a turn spoken in a voice call runs on (same session, only the model differs): a model name
                    Claude Code takes ("sonnet", "haiku", "opus"), or "own" for the agent's own model. Default "sonnet":
                    a call wants a quick reply. agents.<agent>.call_model replaces it for that agent. Schedules, channels
                    and typed messages never use it

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
  reads_others      capabilities.yaml `reads_others: false` marks an agent that works on the owner's own files only; its owner
                    runs may use bypassPermissions. Without it (or true) the agent reads other people's text and its
                    owner runs are held to auto (kith_settings.py cap MODE CAPS_FILE prints the mode that applies)

  kith_settings.py accounts [AGENT]  the set that applies (the agent's own, else the global one), one id per line
  kith_settings.py account_policy    fill or spread
  kith_settings.py pin AGENT         the agent's pin as JSON, or nothing
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
DEFAULTS = {'permission_mode': 'auto', 'plugins': [], 'call_model': 'sonnet', 'account_policy': 'fill', 'accounts': []}
CHOICES = {'permission_mode': ('default', 'acceptEdits', 'plan', 'auto', 'bypassPermissions'),
           'at_limit': ('ask', 'tell', 'hold'), 'account_policy': ('fill', 'spread')}
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


def reads_others(caps_file):
    """Does the agent read text other people wrote (pages, papers, posts, logs)? True unless its capabilities.yaml says
    `reads_others: false`, so an agent nobody marked (a private role, a new one) takes the conservative side."""
    try:
        text = open(caps_file).read()
    except OSError:
        return True
    return not re.search(r'^reads_others: *false\b', text, re.M)


def cap_mode(mode, caps_file):
    """The mode an owner's run of the agent may start in: bypass only for an agent that works on the owner's own files
    (`reads_others: false`); a reader agent is held to auto, where the checker can stop an instruction planted in what it
    reads. Applies whoever asked for bypass (caller, this Mac, the role)."""
    return 'auto' if mode == 'bypassPermissions' and reads_others(caps_file) else mode


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
        for key in ('backups', 'accounts'):   # backups: the old shape, migrated below
            b = d.get(key, [])
            if not isinstance(b, list) or not all(isinstance(i, str) for i in b):
                return {}, f'{FILE}: {where}{key} is not a list of account ids'
        if 'account' in d and not (isinstance(d['account'], str) and d['account']):
            return {}, f'{FILE}: {where}account is not an account id'
        if 'account_cross_harness' in d and not isinstance(d['account_cross_harness'], bool):
            return {}, f'{FILE}: {where}account_cross_harness is not true or false'
        pin = d.get('pin')
        if 'pin' in d and not (where and isinstance(pin, dict) and isinstance(pin.get('id'), str) and pin['id']
                               and isinstance(pin.get('only', False), bool)):
            return {}, f'{FILE}: {where}pin is not {{"id": ID, "only": true|false}}'
        for k, v in d.items():
            if k in CHOICES and v not in CHOICES[k]:
                return {}, f'{FILE}: {where}{k} {v!r} is not one of {", ".join(CHOICES[k])}'
        if 'limit' in d and not valid_limit(d['limit']):
            return {}, f'{FILE}: {where}limit {d["limit"]!r} is not a number of dollars above 0'
    migrate_accounts(s)
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


# --- the account set (lib/kith_accounts.py picks from it) -------------------------------------------------------------------

def _family_of(entry, own):
    """Whether account id `entry` is of harness `own`'s family (claude-api is a Claude one)."""
    import kith_accounts
    return kith_accounts.split(entry)[0].split(':')[0] in kith_accounts.family(own)


def _harness_of(agent):
    import kith_accounts
    return kith_accounts.agent_harness(kith_accounts.agent_md_of(agent))


def _dedup(ids):
    return list(dict.fromkeys(i for i in ids if isinstance(i, str) and i))


def migrate_accounts(s):
    """In place, on what read() returns: the old main account + backups chain (account, backups, agents.<a>.account,
    agents.<a>.backups) as the account set, a pin and per-agent subsets. Idempotent; a file with the new keys keeps them
    (old keys beside them are dropped). Nothing is written here: the next write persists the new shape. Whether the
    file still had the old shape."""
    old_main = s.get('account') or ''
    old_backups = s.get('backups') if isinstance(s.get('backups'), list) else []
    agents = s.get('agents', {})
    old = 'account' in s or 'backups' in s or any('account' in a or 'backups' in a for a in agents.values())
    if not old:
        return False
    main_family = (old_main.split(':')[0] if old_main else 'claude').replace('claude-api', 'claude')
    mixed = any(i.split(':')[0].replace('claude-api', 'claude') != main_family for i in old_backups)
    if mixed and 'account_cross_harness' not in s:
        s['account_cross_harness'] = True   # the old chain named accounts of another harness: those fallbacks stay
    if 'accounts' not in s and (old_main or old_backups):
        head = [old_main] if old_main else ['claude']
        if old_main.startswith('codex'):   # claude agents ran on their default login then: keep it in the set
            head.append('claude')
        s['accounts'] = _dedup(head + old_backups)
    for name, a in agents.items():
        own_main = a.get('account')
        own_backups = a.get('backups') if isinstance(a.get('backups'), list) else None
        if own_main or own_backups is not None:
            h = _harness_of(name)
            main = next((m for m in (own_main, old_main) if m and _family_of(m, h)), h)
            chain = own_backups if own_backups is not None else [i for i in old_backups if mixed or _family_of(i, h)]
            a.setdefault('accounts', _dedup([main] + chain))
        if own_main:
            a.setdefault('pin', {'id': own_main, 'only': False})
        a.pop('account', None)
        a.pop('backups', None)
    s.pop('account', None)
    s.pop('backups', None)
    return True


def migrate_file():
    """Write the new shape now when the file is still in the old one (kith account does this; everything else reads it
    migrated and leaves the file alone until its next write). Whether it wrote."""
    with locked():
        try:
            raw = json.loads(FILE.read_text())
        except (OSError, ValueError):
            return False
        s, why = read()
        if why or not isinstance(raw, dict) or raw == s:
            return False
        write(s)
        return True


def edit(change):
    with locked():
        s, why = read()
        if why:
            raise ValueError(why)
        change(s)
        write(s)


def _agent_entry(s, agent):
    return s.setdefault('agents', {}).setdefault(agent, {})


def _tidy(s, agent):
    if not s.get('agents', {}).get(agent, {'x': 1}):
        s['agents'].pop(agent, None)


def accounts(agent=None):
    """(ids or None, where): the set that applies to the agent: its own ('agent', even when []), else the global one
    ('global'); None when the file has no set at all (every agent on its harness's default login)."""
    s = read()[0]
    own = s.get('agents', {}).get(agent, {}) if agent else {}
    if 'accounts' in own:
        return list(own['accounts']), 'agent'
    return (list(s['accounts']) if 'accounts' in s else None), 'global'


def set_accounts(agent, ids):
    """Record the set: one agent's own (a subset/order), or with agent None the global one. ids None forgets it (an agent
    then follows the global set). Keeps the rest of the file."""
    def change(s):
        if agent:
            if ids is None:
                s.get('agents', {}).get(agent, {}).pop('accounts', None)
                _tidy(s, agent)
            else:
                _agent_entry(s, agent)['accounts'] = _dedup(ids)
        elif ids is None:
            s.pop('accounts', None)
        else:
            s['accounts'] = _dedup(ids)
    edit(change)


def cross_harness(agent=None):
    """Whether accounts of another harness serve as fallbacks for the agent: its own switch, else the global one."""
    s = read()[0]
    own = s.get('agents', {}).get(agent, {}) if agent else {}
    return bool(own['account_cross_harness'] if 'account_cross_harness' in own else s.get('account_cross_harness', False))


def set_cross_harness(agent, value):
    """On/off for one agent (None forgets it: the global switch applies) or, with agent None, for all."""
    def change(s):
        if agent:
            if value is None:
                s.get('agents', {}).get(agent, {}).pop('account_cross_harness', None)
                _tidy(s, agent)
            else:
                _agent_entry(s, agent)['account_cross_harness'] = bool(value)
        else:
            s['account_cross_harness'] = bool(value)
    edit(change)


def account_policy():
    return read()[0].get('account_policy', DEFAULTS['account_policy'])


def set_account_policy(policy):
    if policy not in CHOICES['account_policy']:
        raise ValueError(f'policy is one of {", ".join(CHOICES["account_policy"])}')
    edit(lambda s: s.__setitem__('account_policy', policy))


def pin(agent):
    """The agent's pin {"id", "only"}, or None."""
    p = read()[0].get('agents', {}).get(agent, {}).get('pin')
    return {'id': p['id'], 'only': bool(p.get('only', False))} if p else None


def set_pin(agent, entry, only=False):
    edit(lambda s: _agent_entry(s, agent).__setitem__('pin', {'id': entry, 'only': bool(only)}))


def clear_pin(agent):
    def change(s):
        s.get('agents', {}).get(agent, {}).pop('pin', None)
        _tidy(s, agent)
    edit(change)


def stored(agent, key):
    """What this Mac has recorded for one agent's setting, None when nothing (the default applies)."""
    v = read()[0].get('agents', {}).get(agent, {}).get(key)
    return v if v not in ('', None) else None


def set_with_json(key, agent, value, setter):
    """`kith A set ... --json`: run the setter, print {agent, setting, value, previous, changed}; an error is
    {"ok": false, "error": ...} on stdout, exit 1 as without --json, nothing on stderr."""
    name = {'permission_mode': 'permission', 'call_model': 'call-model', 'at_limit': 'at-limit', 'limit': 'limit'}[key]
    try:
        before = stored(agent, key)
        setter()
        after = stored(agent, key)
    except ValueError as e:
        print(json.dumps({'ok': False, 'error': f'kith: {e}'}))
        sys.exit(1)
    print(json.dumps({'agent': agent, 'setting': name, 'value': after, 'previous': before, 'changed': before != after}))
    sys.exit(0)


if __name__ == '__main__':
    as_json = '--json' in sys.argv
    sys.argv = [a for a in sys.argv if a != '--json']
    if len(sys.argv) == 5 and sys.argv[1] == 'set' and sys.argv[2] == 'limit':   # set limit AGENT N|none
        if as_json:
            set_with_json('limit', sys.argv[3], sys.argv[4], lambda: set_limit(sys.argv[3], sys.argv[4]))
        try:
            set_limit(sys.argv[3], sys.argv[4])
        except ValueError as e:
            sys.exit(f'kith: {e}')
        sys.exit(0)
    if len(sys.argv) == 4 and sys.argv[1:3] == ['set', 'delegation.chain_cost_cap_usd']:
        if any(os.environ.get(k) for k in ('KITH_ASK_ID', 'KITH_AGENT', 'CLAUDE_CODE_SESSION_ID')):
            sys.exit('kith: the delegation cap is the owner\'s to change, not an agent\'s run')
        try:
            set_chain_cost_cap(sys.argv[3])
        except ValueError as e:
            sys.exit(f'kith: {e}')
        sys.exit(0)
    if len(sys.argv) == 2 and sys.argv[1] == 'delegation.chain_cost_cap_usd':
        cap = chain_cost_cap()
        print('none' if cap is None else cap)
        sys.exit(0)
    if len(sys.argv) == 4 and sys.argv[1] == 'cap':   # cap MODE CAPS_FILE
        print(cap_mode(sys.argv[2], sys.argv[3]))
        sys.exit(0)
    if len(sys.argv) == 3 and sys.argv[1] == 'reads-others':
        print(1 if reads_others(sys.argv[2]) else 0)
        sys.exit(0)
    if len(sys.argv) == 3 and sys.argv[1] == 'limit':
        amount, at = limit(sys.argv[2])
        print(json.dumps({'limit': amount, 'at': at}))
        sys.exit(0)
    if len(sys.argv) in (4, 5) and sys.argv[1] == 'set':   # set KEY [AGENT] VALUE
        if sys.argv[2] not in ('permission_mode', 'call_model', 'at_limit') or (len(sys.argv) == 4 and sys.argv[2] != 'call_model'):
            sys.exit(f'kith: {sys.argv[2]} is not a setting kith can set per agent')
        if as_json and len(sys.argv) == 5:
            set_with_json(sys.argv[2], sys.argv[3], sys.argv[4], lambda: choose(sys.argv[3], sys.argv[2], sys.argv[4]))
        try:
            if len(sys.argv) == 4:
                choose_global(sys.argv[2], sys.argv[3])
            else:
                choose(sys.argv[3], sys.argv[2], sys.argv[4])
        except ValueError as e:
            sys.exit(f'kith: {e}')
        sys.exit(0)
    if len(sys.argv) not in (2, 3) or sys.argv[1] not in (*DEFAULTS, 'pin'):
        sys.exit(__doc__.strip())
    if sys.argv[1] == 'call_model':   # the model that applies, '' for the agent's own
        print(call_model(*sys.argv[2:]))
        sys.exit(0)
    if sys.argv[1] == 'accounts':
        print('\n'.join(accounts(*sys.argv[2:])[0] or []))
        sys.exit(0)
    if sys.argv[1] == 'pin':
        p = pin(*sys.argv[2:]) if len(sys.argv) == 3 else None
        print(json.dumps(p) if p else '')
        sys.exit(0)
    v = get(*sys.argv[1:])
    print('\n'.join(v) if isinstance(v, list) else v)
