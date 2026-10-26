"""The bridge core, the same on every platform: people, access, rooms, threads, sessions and the queue.

People: who sent a message (`kith-auth who`) and whether they may use an agent (`kith-auth resolve`). Rooms: where
people talk (a DM, #home, a group), keyed `<connection>:<chat>`, each with an optional default agent. Threads: one
task each, keyed by the adapter, belonging to the person who started it (its sessions hold what the agents saw for
them), with a session per agent that has answered there ({agent: session}).

Who answers a message: `@agent` (or `/agent`) in it; else, in a thread, the agent that answered last; else the room's
default (an agent's own Discord channel defaults to that agent); else, when the bot was addressed, the bot asks "who
should take this?" and never guesses. The first agent in a thread owns the task; one mentioned later answers in its
own session there.

The queue: messages sent while an agent works wait in the thread's inbox and go to it together as one prompt when
the run ends. With a Jev key (granted to `bridge`) each is routed by Jev first: "stop" kills the run, a correction
stops it and starts it again with the correction merged in, a different subject becomes its own turn, anything else
is merged; without Jev, or when Jev fails, it is merged.

An adapter (discord_chat, telegram_chat, slack_chat) receives messages, turns each into an Incoming and calls
Core.handle; the core calls the adapter back (the Adapter methods below) to post, show the working line, ask and
react. Agents' own posts (kith-send) reach the thread through the run's outbox folder, KITH_BRIDGE_OUTBOX, or on
Discord through the channel's webhook.
Live agents (`session: live` in agent.md, e.g. coder) run as one session of their own: the owner's messages to one
are typed into that session (kith-live send, reply=bridge:<box>) instead of starting a run, and its `kith-send --to
bridge:<box>` posts land in the thread through outbox/live-<box>/. When no session runs, the message runs once as
usual. Anyone else's messages to a live agent are refused.
"""
from __future__ import annotations

import asyncio
import hashlib
import json
import os
import re
import signal
import subprocess
import sys
import time
import urllib.request
from dataclasses import dataclass, field
from pathlib import Path

KERNEL = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(KERNEL / 'lib'))
from kith_paths import paths  # noqa: E402  the one place that knows where kith's folders are

P = paths()
STATE_DIR = P['KITH_MACHINE'] / 'bridge'
STATE = STATE_DIR / 'state.json'
AUTH_BIN, KITH_BIN = str(KERNEL / 'bin' / 'kith-auth'), str(KERNEL / 'bin' / 'kith')
LIVE_BIN = str(KERNEL / 'bin' / 'kith-live')
KEEP = 5000   # threads and reply links kept in the state: the newest few thousand are enough to come back to
LATE = 120    # seconds: a message older than this when it arrives was sent while the bridge was away
REMOVED = 'Your access to kith on this computer was removed.'
# While kith serve is out of sync with the Kith cloud (`who` says held: asleep, offline, or a change not yet applied),
# anyone but the owner waits: the message is handled once the sync is back, or answered "try again" after this long.
HOLD = float(os.environ.get('KITH_BRIDGE_HOLD', '120'))


def log(*a):
    print(time.strftime('%Y-%m-%d %H:%M:%S'), 'kith-bridge:', *a, flush=True)


def late_note(sent):
    """What the agent is told about a message that waited for the bridge."""
    return (f'(Sent at {time.strftime("%Y-%m-%d %H:%M", time.localtime(sent))} while kith\'s chat bridge was away; it '
            f'reached you at {time.strftime("%H:%M")}. Say briefly that you picked it up late.)\n\n')


_dirs = {'at': 0.0, 'dirs': {}}


def agent_dirs():
    """{agent: package folder}, looked up again at most every 30 s (new agents show up without a restart)."""
    if time.time() - _dirs['at'] > 30:
        out = {}
        for root in (P['KITH_AGENTS'], KERNEL / 'agents'):
            for d in sorted(root.iterdir()) if root.is_dir() else []:
                pkg = d / 'role' if (d / 'role' / 'agent.md').is_file() else d   # role/ next to the agent's life
                if not d.name.startswith('.') and (pkg / 'agent.md').is_file():
                    out.setdefault(d.name, pkg)
        _dirs.update(at=time.time(), dirs=out)
    return _dirs['dirs']


def agents():
    return sorted(agent_dirs())


def card(agent, key):
    pkg = agent_dirs().get(agent)
    m = re.search(rf'^{key}:\s*(.+)$', (pkg / 'agent.md').read_text(), re.M) if pkg else None
    return m.group(1).strip() if m else None


def description(agent):
    return (card(agent, 'description') or f'kith {agent}')[:1024]


def display_name(agent):
    """The name an agent posts under (display_name in agent.md); anything that isn't an agent stays as it is."""
    return card(agent, 'display_name') or agent


def cmd_name(agent):   # Telegram commands: lowercase letters, digits and _
    return agent.replace('-', '_')


def find_agent(word):
    w = (word or '').lower()
    return next((a for a in agents() if w in (a, cmd_name(a))), None)


def chunks(text, n=1900):
    """Pieces of at most n characters, cut at line ends, with a code fence cut in two closed and reopened."""
    text = text.strip() or '(no reply)'
    fence = ''
    while text:
        cut = text.rfind('\n', 0, n - 8) if len(text) > n - 8 else len(text)
        cut = cut if cut > n // 2 else min(n - 8, len(text))
        part, text = fence + text[:cut], text[cut:].lstrip('\n')
        opened = re.findall(r'^```(\S*)', part, re.M)
        fence = ''
        if len(opened) % 2 and text:
            part += '\n```'
            fence = f'```{opened[-1]}\n'
        yield part


# ---- state ----

def read_state():
    try:
        st = json.loads(STATE.read_text())
    except (OSError, ValueError):
        st = {}
    return migrate(st)


def migrate(st):
    """State v2 from the Discord-only bridge's (v1): its webhooks, threads, inbox and Telegram rooms and replies."""
    tables = ('conns', 'rooms', 'threads', 'replies', 'inbox', 'pending', 'live')
    if st.get('version') == 2:
        for t in tables:
            st.setdefault(t, {})
        return st
    new = {'version': 2, **{t: {} for t in tables}}
    dc, tc = 'discord/hq', 'telegram/hq'   # v1 had one of each: the env token's Discord bot, telegram/hq
    channel = {a: str(v['channel_id']) for a, v in st.get('webhooks', {}).items()}
    if st.get('webhooks'):
        new['conns'][dc] = {'hooks': {str(v['channel_id']): v['url'] for v in st['webhooks'].values()}}
    for tid, t in st.get('threads', {}).items():
        a = t.get('agent')
        new['threads'][f'{dc}:{tid}'] = {
            'person': t.get('person', ''), 'owner': a, 'last': a, 'room': f'{dc}:{channel.get(a, "")}',
            'sessions': {a: t['session_id']} if a and t.get('session_id') else {},
            'conv': {'key': f'{dc}:{tid}', 'thread': tid, 'channel': channel.get(a, '')}}
    for tid, v in st.get('inbox', {}).items():
        if v.get('batches') and v.get('agent'):
            new['inbox'][f'{dc}:{tid}#{v["agent"]}'] = v['batches']
    tg = st.get('telegram', {})
    for cid, room in tg.get('rooms', {}).items():
        new['rooms'][f'{tc}:{cid}'] = room
    by_session = {}
    for ref, row in tg.get('replies', {}).items():
        cid, _, mid = ref.partition(':')
        sid = row.get('session_id') or ref
        key = by_session.setdefault(sid, f'{tc}:{cid}:{mid}')
        th = new['threads'].setdefault(key, {'person': row.get('person', ''), 'owner': row['agent'],
                                             'last': row['agent'], 'room': f'{tc}:{cid}', 'sessions': {},
                                             'conv': {'key': key, 'chat': int(cid), 'reply_to': int(mid)}})
        if row.get('session_id'):
            th['sessions'][row['agent']] = row['session_id']
        new['replies'][f'{tc}:{ref}'] = key
    if 'telegram_offset' in st:
        new['conns'][tc] = {'offset': st['telegram_offset']}
    return new


def write_state(st):
    STATE_DIR.mkdir(parents=True, exist_ok=True)
    tmp = STATE.with_suffix('.tmp')
    tmp.write_text(json.dumps(st, indent=2))
    tmp.replace(STATE)


# ---- the router (Jev) ----

def jev_config():
    """JEV_* from the environment, the key from kith-auth (kith-auth grant bridge jev-browser <profile>); kept out of
    os.environ so agent runs never see the key."""
    cfg = {k: v for k, v in os.environ.items() if k.startswith('JEV_')}
    if 'JEV_API_KEY' not in cfg:
        r = subprocess.run([AUTH_BIN, 'get', '--for', 'bridge', 'jev-browser', 'TYPESAFE_API_KEY'],
                           capture_output=True, text=True)
        if r.returncode == 0 and r.stdout.strip():
            cfg['JEV_API_KEY'] = r.stdout.strip()
    return cfg


# Narrow yes/no questions (Jev ignores negation inside a question); thresholds from probes on 2026-10-16.
ROUTE_QUESTIONS = {
    'stop': 'Does `latest` ask the assistant to stop, cancel or abort the work in `task`?',
    'change': 'Does `latest` replace or correct an instruction given in `task`?',
    'add': 'Does `latest` add a detail or an extra request to the work in `task`?',
    'other': 'Is `latest` a request about a different subject from `task`?',
    'partial': 'Does `latest` end mid-thought, as if the user will send the rest in a next message?',
}


def route(cfg, task, recent, latest):
    """-> (stop | interrupt | separate | merge | merge-partial, path) for a message that arrived while `task` runs.
    Any failure merges."""
    if not cfg.get('JEV_API_KEY') or os.environ.get('KITH_BRIDGE_ROUTER', 'jev').lower() == 'off':
        return 'merge', 'no router'
    base = cfg.get('JEV_BASE_URL', 'https://api.typesafe.ai').rstrip('/')
    url = base if base.endswith('/systemone') else base + ('/systemone' if base.endswith('/v1') else '/v1/systemone')
    body = json.dumps({'model': cfg.get('JEV_MODEL', 'jev-latest'),
                       'state': {'task': task[-4000:], 'recent': recent[-6:], 'latest': latest[-2000:]},
                       'questions': {k: {'type': 'noul', 'instructions': q} for k, q in ROUTE_QUESTIONS.items()}},
                      ensure_ascii=False).encode()
    req = urllib.request.Request(url, data=body, method='POST', headers={
        'Authorization': 'Bearer ' + cfg['JEV_API_KEY'], 'Content-Type': 'application/json', 'User-Agent': 'kith-bridge'})
    try:
        with urllib.request.urlopen(req, timeout=5) as r:
            ans = json.loads(r.read().decode())['answers']
        p = {k: max(0.0, min(1.0, float(ans[k]['noul']))) for k in ROUTE_QUESTIONS}
    except Exception as e:  # noqa: BLE001  the router is advice; the inbox works without it
        return 'merge', f'jev failed ({type(e).__name__})'
    path = ' '.join(f'{k}:{v:.2f}' for k, v in p.items())
    if p['stop'] >= 0.9 and p['stop'] > p['change']:   # "wait, do August instead" scores stop 0.80, change 0.96
        return 'stop', path
    if p['change'] >= 0.9 and os.environ.get('KITH_BRIDGE_INTERRUPT', 'on').lower() != 'off':
        return 'interrupt', path   # "actually make it sushi", "do August instead": the running work is off course
    if p['other'] >= 0.9 and p['add'] < 0.7 and p['change'] < 0.5:   # "make it sushi": other 0.93 but change 0.95
        return 'separate', path
    return ('merge-partial' if p['partial'] >= 0.8 else 'merge'), path


INTERRUPTED = ('(Your previous attempt at this was stopped part-way because the user corrected it; files or other '
               'things it touched may be half-changed, so check before you go on.)\n\n')


def merged(items):
    if len(items) == 1:
        return items[0]
    return ('The user sent several messages in a row; answer them together, in one reply:\n\n'
            + '\n\n'.join(f'[{i}] {t}' for i, t in enumerate(items, 1)))


def context_pct(res):
    """How full the session's context is after this turn, in percent; None when the result doesn't say."""
    its = (res.get('usage') or {}).get('iterations') or []
    models = [m for m in (res.get('modelUsage') or {}).values() if m.get('contextWindow')]
    if not its or not models:
        return None
    last = its[-1]   # the turn's final API call: what the next turn starts from
    used = sum(last.get(k, 0) for k in ('input_tokens', 'cache_read_input_tokens', 'cache_creation_input_tokens',
                                        'output_tokens'))
    main = max(models, key=lambda m: m.get('inputTokens', 0) + m.get('cacheReadInputTokens', 0)
               + m.get('cacheCreationInputTokens', 0))   # the agent's own model, not a helper's
    return 100 * used / main['contextWindow']


def kill(proc):
    try:
        os.killpg(proc.pid, signal.SIGTERM)  # kith → claude → its tools: the whole group
    except (ProcessLookupError, PermissionError):
        pass


# ---- what the core and an adapter pass each other ----

@dataclass
class Incoming:
    """One message, as the core needs it."""
    uid: str                    # the sender's platform user id (numeric, or Slack's U0123ABC)
    text: str                   # with the bot's own mention taken out and agent mentions as @agent
    room: str                   # '<connection>:<chat>'
    origin: dict                # where the message is, for the adapter (open_thread, ask); JSON-safe
    thread: str | None = None   # the thread key when it came inside a thread (ours or not)
    private: bool = False       # a direct message with the bot
    addressed: bool = False     # the bot or an agent was mentioned, or the bot's message replied to
    room_agent: str | None = None   # the agent whose own room this is (Discord's agent channels)
    admin: bool = False         # the server's owner (Discord), taken as kith's owner until kith knows their account
    quoted: str = ''            # the text of a message replied to that isn't kith's
    files: list = field(default_factory=list)   # local paths of the files sent with it
    ref: object = None          # the adapter's own handle on the message (reactions)
    sent: float | None = None   # when it was sent (unix time), so one that waited for the bridge is marked late


class Adapter:
    """One connection (a bot on a platform). Subclasses fill these in; the core never touches the platform."""
    platform = ''
    footer = ''   # added to the task: where the answer goes and how to send files
    quiet = None  # seconds without a sign of life (self.alive) after which kith-bridge restarts the connection

    def __init__(self, conn, secrets, core):
        self.conn, self.secrets, self.core = conn, secrets, core
        self.cs = core.st['conns'].setdefault(conn, {})   # this connection's own state (offsets, webhooks)
        self.alive = time.monotonic()                     # start() sets it whenever it hears from the platform

    async def start(self):                          # connect and receive; returns or raises when the connection drops
        raise NotImplementedError

    async def reply(self, m, text):                 # a short note from kith itself, answering m
        raise NotImplementedError

    async def open_thread(self, m, agent, title):   # -> (thread key, conv): where a new task's messages go
        raise NotImplementedError

    def follow(self, conv, m):                      # a new message in a thread: where the answer to it goes
        return conv

    async def post(self, conv, agent, text, ctx=None):   # the agent's answer (Markdown); ctx: context used, in %
        raise NotImplementedError

    async def send_files(self, conv, agent, files, caption=''):
        raise NotImplementedError

    async def status(self, conv, agent, wid):       # the small "working…" line with a Stop button; -> handle
        return None

    async def status_text(self, handle, text):      # change the working line (queued count, restarting)
        pass

    async def status_end(self, conv, agent, handle, ended):   # done: remove it; stopped: say so
        pass

    async def react(self, m, what):                 # what: 'stop' or 'redo'
        pass

    async def ask(self, m, pid, options):           # "Who should take this?" with a button per agent
        raise NotImplementedError

    async def typing(self, conv):                   # show that the agent is working, until cancelled
        await asyncio.Event().wait()

    def env(self, conv, agent):                     # extra environment for the run (Discord: the thread's webhook)
        return {}


def short(text):
    return hashlib.sha1(text.encode()).hexdigest()[:10]


class Core:
    def __init__(self):
        self.st = read_state()
        self.inbox = self.st['inbox']   # '<thread>#<agent>' → batches of messages, each batch one turn
        self.busy: dict[str, dict] = {}       # work key → the running turn: agent, task, proc, ended, hold, ...
        self.history: dict[str, list] = {}    # work key → the last user messages, for the router
        self.works: dict[str, str] = {}       # short id → work key, for Stop buttons
        self.jev = jev_config()
        self.pumps: set[str] = set()          # connections whose live-agent outboxes are being posted
        self.hinted = set()
        try:   # show a reply's context use from this %; off (or anything not a number) never shows it
            self.ctx_show = float(os.environ.get('KITH_BRIDGE_CONTEXT_SHOW', '30').strip().rstrip('%'))
        except ValueError:
            self.ctx_show = None
        for old in ('KITH_DISCORD_REPLY_TO', 'KITH_DISCORD_ALLOWED_IDS', 'KITH_DISCORD_OWNER_ID'):
            if os.environ.get(old):
                log(f'{old} is ignored now: people are in kith-auth (kith auth handle <id> discord <user-id>, '
                    f'kith auth invite <id>)')

    def save(self):
        for table in ('threads', 'replies'):
            t = self.st[table]
            for k in list(t)[:-KEEP]:
                if not any(w.startswith(k + '#') for w in self.inbox):
                    del t[k]
        write_state(self.st)

    # ---- people ----

    @staticmethod
    def who(platform, uid):
        r = subprocess.run([AUTH_BIN, 'who', platform, str(uid)], capture_output=True, text=True)
        try:
            return json.loads(r.stdout)
        except ValueError:
            return {'person': None, 'owner': False, 'was': []}

    @staticmethod
    def resolve(person, agent):
        r = subprocess.run([AUTH_BIN, 'resolve', agent, '--person', person], capture_output=True, text=True)
        try:
            return json.loads(r.stdout)
        except ValueError:
            return {'allowed': False, 'why': f"can't check whether {person} may use {agent}"}

    def usable(self, person):
        return [a for a in agents() if self.resolve(person, a).get('allowed')]

    @staticmethod
    def owns(th, w):
        """Whether the sender is the person the thread belongs to (also under an id they had before a rename)."""
        p = th.get('person', '')
        return (p == w.get('person') or p in w.get('was', [])) if p else bool(w.get('owner'))

    async def person(self, ad, m):
        """-> (person id, who) of the sender, or (None, who) when kith doesn't know them."""
        w = await asyncio.to_thread(self.who, ad.platform, m.uid)
        if not w.get('person') and m.admin and w.get('owner_id') and not w.get('owner_linked'):
            if ad.conn not in self.hinted:   # the server's owner, until kith knows their account on this platform
                self.hinted.add(ad.conn)
                log(f'{ad.conn}: taking the server owner {m.uid} as {w["owner_id"]} '
                    f'(kith auth handle {w["owner_id"]} {ad.platform} {m.uid} to say so)')
            w = {**w, 'person': w['owner_id'], 'owner': True}
        return w.get('person'), w

    # ---- a message ----

    async def handle(self, ad, m):
        text = m.text or ''
        cmd = re.fullmatch(r'\s*[/!](\w+)\s*(.*?)\s*', text, re.S)
        name, arg = (cmd.group(1).lower(), cmd.group(2)) if cmd else ('', '')
        if name == 'link' and re.fullmatch(r'[A-Za-z0-9]{4,12}', arg):   # a one-time code from kith-auth invite
            r = await asyncio.to_thread(subprocess.run, [AUTH_BIN, 'link', ad.platform, m.uid, arg],
                                        capture_output=True, text=True)
            log(f'{ad.conn} link {m.uid}: {"ok " + r.stdout.strip() if r.returncode == 0 else "refused"}')
            await ad.reply(m, f'Linked: you are {r.stdout.strip()} in kith.' if r.returncode == 0 else
                           (r.stderr.strip().removeprefix('kith-auth: ') or 'That code did not work.'))
            return
        person, w = await self.person(ad, m)
        key = m.thread if m.thread in self.st['threads'] else None
        th = self.st['threads'].get(key) if key else None
        if not person:
            if m.private or m.addressed or th:
                await ad.reply(m, 'This account is not linked to kith yet. Ask the owner for a code, then send '
                                  '/link CODE here.')
            return
        if w.get('revoked'):   # their handle is kept so they hear why (revoke-person)
            if m.private or m.addressed or th:
                await ad.reply(m, REMOVED)
            return
        if w.get('held') and not w.get('owner'):   # a revoke on another Mac may not have reached this one yet (R8)
            asyncio.create_task(self.held(ad, m))
            return
        if th and not self.owns(th, w):   # its sessions hold what the agents saw for them
            await ad.reply(m, f'This thread is {th.get("person") or "the owner"}\'s. Start your own outside it.')
            return
        room = self.st['rooms'].setdefault(m.room, {})
        if m.private and 'default' not in room:   # a new DM: the owner's starts with KITH_BRIDGE_DM_DEFAULT
            room['default'] = os.environ.get('KITH_BRIDGE_DM_DEFAULT', 'personal') if w.get('owner') else ''
            self.save()
        default = room['default'] if 'default' in room else (m.room_agent or '')
        if name in ('start', 'whoami', 'help'):
            await ad.reply(m, f'You are {person} in kith.' + (f' Messages without @agent here go to {default}.'
                                                             if default else '')
                           + ' Write @agent to pick one; /agents lists yours, /default sets this room\'s.')
            return
        if name == 'agents':
            mine = await asyncio.to_thread(self.usable, person)
            await ad.reply(m, 'Yours: ' + (', '.join(mine) or 'none yet') + '\nWrite @agent, or answer in a thread to '
                           'go on with it.')
            return
        if name == 'default':
            agent = find_agent(arg) if arg else None
            if not arg:
                await ad.reply(m, f'Messages without @agent here go to {default}.' if default else
                               'No default here: I ask who should take each message. /default <agent> sets one.')
                return
            if arg in ('none', '-'):
                room['default'] = ''
            elif not agent or not (await asyncio.to_thread(self.resolve, person, agent)).get('allowed'):
                await ad.reply(m, f'No agent of yours called {arg!r}. /agents lists them.')
                return
            else:
                room['default'] = agent
            self.save()
            await ad.reply(m, f'Messages without @agent here now go to {room["default"]}.' if room['default'] else
                           'No default: I\'ll ask who should take each message.')
            return
        if name == 'stop':
            wk = f'{key}#{th.get("last")}' if th else None
            if wk and (wk in self.busy or self.inbox.get(wk)):
                await self.stop(ad, wk, m)
            else:
                await ad.reply(m, 'Nothing is running here.')
            return
        agent, task = None, text
        lead = re.match(r'\s*[@/]([\w-]+)[,:]?\s*(.*)', text, re.S)
        if lead and find_agent(lead.group(1)):
            agent, task = find_agent(lead.group(1)), lead.group(2).strip() or text
        else:
            agent = next((find_agent(x) for x in re.findall(r'(?<![\w@/<])@([\w-]+)', text) if find_agent(x)), None)
        if m.quoted:
            task = f'(Replying to: "{m.quoted[:1500]}")\n\n{task}'
        if m.files:
            task += '\n\nAttached files: ' + ', '.join(m.files)
        if m.sent and time.time() - m.sent > LATE:
            task = late_note(m.sent) + task
        agent = agent or (th or {}).get('last') or default or None
        if not agent:
            if not (m.addressed or m.private):
                return   # talk in a room with no default that isn't for kith
            mine = await asyncio.to_thread(self.usable, person)
            if not mine:
                await ad.reply(m, 'You have no agents yet. Ask the owner to add you.')
                return
            pid = short(f'{m.room}:{m.origin}:{time.time()}')
            self.st['pending'][pid] = {'task': task, 'text': text, 'person': person, 'room': m.room,
                                       'origin': m.origin, 'thread': key, 'private': m.private, 'at': time.time()}
            for k in [k for k, v in self.st['pending'].items() if v.get('at', 0) < time.time() - 86400]:
                del self.st['pending'][k]
            self.save()
            await ad.ask(m, pid, mine)
            return
        await self.begin(ad, m, person, agent, task, key)

    async def held(self, ad, m):
        """Handles m once this Mac is in sync with the Kith cloud again; tells them to retry when it stays out."""
        end = time.time() + HOLD
        while time.time() < end:
            await asyncio.sleep(2)
            w = await asyncio.to_thread(self.who, ad.platform, m.uid)
            if not w.get('held'):
                await self.handle(ad, m)
                return
        log(f'{ad.conn}: held a message from {m.uid} for {HOLD:.0f}s: still out of sync with the Kith cloud')
        if m.private or m.addressed or m.thread in self.st['threads']:
            await ad.reply(m, 'Kith on this computer is catching up with your other computers. Send it again in a '
                              'minute.')

    async def picked(self, ad, pid, agent, uid):
        """A button under "Who should take this?" -> False when it isn't this person's question (or it's gone)."""
        pend = self.st['pending'].get(pid)
        w = await asyncio.to_thread(self.who, ad.platform, uid)
        if not pend or not (pend['person'] == w.get('person') or pend['person'] in w.get('was', [])):
            return False
        del self.st['pending'][pid]
        self.save()
        m = Incoming(uid=str(uid), text=pend['text'], room=pend['room'], origin=pend['origin'],
                     thread=pend.get('thread'), private=pend.get('private', False), addressed=True)
        await self.begin(ad, m, w['person'], agent, pend['task'], pend.get('thread'))
        return True

    async def begin(self, ad, m, person, agent, task, key):
        r = await asyncio.to_thread(self.resolve, person, agent)
        if not r.get('allowed'):
            await ad.reply(m, f'{r.get("why")}. Ask the owner to add you (kith auth member {agent} {person}).')
            return
        if key is None or key not in self.st['threads']:
            title = re.sub(r'\s+', ' ', re.sub(r'^\s*[@/][\w-]+[,:]?', '', m.text)).strip()[:90] or 'task'
            key, conv = await ad.open_thread(m, agent, title)
            th = self.st['threads'].setdefault(key, {'person': person, 'owner': agent, 'room': m.room,
                                                     'sessions': {}})
            th['conv'] = {**conv, 'key': key}   # conv['key']: adapters link their messages to the thread
        else:
            th = self.st['threads'][key]
            th['conv'] = ad.follow(th['conv'], m)
        th['last'], th['at'] = agent, time.time()
        self.st['threads'][key] = self.st['threads'].pop(key)   # newest last, for pruning
        self.save()
        if card(agent, 'session') == 'live':
            if not r.get('owner'):
                await ad.reply(m, f'{agent} takes work from the owner only.')
                return
            if await self.live(ad, key, agent, m, task):
                return
        await self.deliver(ad, key, agent, m, task)

    # ---- live agents ----

    async def live(self, ad, key, agent, m, task):
        """Types the owner's message into the agent's live session -> False when none runs (it then runs once)."""
        box = short(key)
        self.st['live'][box] = key
        self.save()
        (STATE_DIR / 'outbox' / f'live-{box}').mkdir(parents=True, exist_ok=True)
        mid = short(f'{key}:{m.origin}:{time.time()}')
        r = await asyncio.to_thread(subprocess.run, [LIVE_BIN, 'send', agent, '--reply', f'bridge:{box}', '--id', mid,
                                                     '--', task], capture_output=True, text=True, timeout=60)
        if r.returncode != 0:
            log(f'live {agent} {key}: not delivered ({r.stderr.strip()[-200:]}); running it once')
            return False
        log(f'live {agent} {key}: delivered {mid}')
        th = self.st['threads'][key]
        await ad.post(th['conv'], 'kith', f'Sent to {agent}\'s live session. Its answers come here.')
        asyncio.create_task(self.live_pump(ad))
        return True

    async def live_pump(self, ad):
        """Posts what live sessions left for this connection's threads (kith-send --to bridge:<box>), every second."""
        if ad.conn in self.pumps:
            return
        self.pumps.add(ad.conn)
        try:
            while True:
                await asyncio.sleep(1)
                for box, key in list(self.st['live'].items()):
                    if not key.startswith(ad.conn + ':'):
                        continue
                    th = self.st['threads'].get(key)
                    folder = STATE_DIR / 'outbox' / f'live-{box}'
                    if th is None:   # the thread was pruned
                        self.st['live'].pop(box, None)
                        continue
                    if folder.is_dir():
                        try:
                            await self.flush(ad, th['conv'], folder)
                        except Exception as e:  # noqa: BLE001  one bad post must not stop the others
                            log(f'live outbox {box}: {type(e).__name__}: {e}')
        finally:
            self.pumps.discard(ad.conn)

    # ---- the queue ----

    async def deliver(self, ad, key, agent, m, task):
        wk = f'{key}#{agent}'
        recent = self.history.setdefault(wk, [])
        q = self.inbox.setdefault(wk, [])
        busy = self.busy.get(wk, {})
        running = busy.get('proc')
        running = running if running is not None and running.returncode is None else None
        if running is None:
            # Nothing running: the message joins the prompt about to go (or starts one) right away, so the run can't
            # leave without it while Jev answers; Jev is then only asked whether it says "stop".
            waiting = wk in self.busy and bool(q)
            if q:
                q[-1].append(task)
            else:
                q.append([task])
            self.save()
            if wk not in self.busy:
                asyncio.create_task(self.worker(ad, key, agent))
                waiting = False
            if waiting:
                decision, path = await asyncio.to_thread(route, self.jev, merged(q[-1][:-1] or [task]), recent, m.text)
                log(f'route {wk} {path} -> {decision} (waiting)')
                if decision == 'merge-partial':
                    self.busy.get(wk, {})['hold'] = True
                elif decision == 'stop':
                    await self.stop(ad, wk, m)
            self.remember(recent, m)
            return
        decision, path = await asyncio.to_thread(route, self.jev, busy['task'], recent, m.text)
        log(f'route {wk} {path} -> {decision}')
        self.remember(recent, m)
        now = self.busy.get(wk, {})
        if decision == 'stop':
            await self.stop(ad, wk, m)
            return
        if decision == 'interrupt' and now.get('proc') is running and running.returncode is None:
            now['ended'], now['redo'] = 'interrupted', True
            more = q.pop(0) if q and now.get('cont') else []   # extras for this same work that were waiting
            q.insert(0, now['batch'] + more + [task])          # the same work again, corrected, before anything else
            now['cont'] = False
            kill(running)
            self.save()
            await ad.react(m, 'redo')
            await ad.status_text(now.get('status'), 'restarting with your correction…')
            return
        if decision == 'separate' or not q:
            q.append([task])
            if wk in self.busy:
                now['cont'] = decision != 'separate' and len(q) == 1   # the next batch continues the running work
        else:
            q[-1].append(task)
        self.save()
        if wk in self.busy:
            now['hold'] = now.get('hold') or decision == 'merge-partial'
            n = sum(len(b) for b in self.inbox.get(wk, []))
            await ad.status_text(now.get('status'), 'working…' + (f' · {n} queued' if n else ''))
        else:
            asyncio.create_task(self.worker(ad, key, agent))   # the run ended while Jev answered

    @staticmethod
    def remember(recent, m):
        recent.append({'role': 'user', 'text': (m.text or '')[-1000:]})
        del recent[:-6]

    async def worker(self, ad, key, agent):
        """Drains the thread's inbox for one agent: each batch of merged messages is one turn."""
        wk = f'{key}#{agent}'
        wid = short(wk)
        self.works[wid] = wk
        self.busy[wk] = {'agent': agent, 'task': '', 'ad': ad}
        conv = self.st['threads'][key]['conv']
        typing = asyncio.create_task(ad.typing(conv))
        try:
            while self.inbox.get(wk):
                await asyncio.sleep(float(os.environ.get('KITH_BRIDGE_DEBOUNCE', '2')))
                while self.busy[wk].pop('hold', False):   # the last message looked unfinished: give the rest time
                    await asyncio.sleep(8)
                if not self.inbox.get(wk):                 # a "stop" came in during the wait
                    break
                batch = self.inbox[wk].pop(0)
                self.busy[wk]['cont'] = False    # whatever is queued now came in before this turn started
                self.save()
                task = (INTERRUPTED if self.busy[wk].pop('redo', False) else '') + merged(batch)
                self.busy[wk].update(task=task, batch=batch, ended=None, proc=None)
                conv = self.st['threads'][key]['conv']
                try:
                    status = await ad.status(conv, agent, wid)
                except Exception as e:  # noqa: BLE001  no button is fine: typing "stop" still works
                    log(f'status message {wk}: {e}')
                    status = None
                self.busy[wk]['status'] = status
                ended = await self.run(ad, key, agent, task)
                self.busy[wk]['status'] = None
                try:
                    await ad.status_end(conv, agent, status, ended)
                except Exception as e:  # noqa: BLE001
                    log(f'status message {wk}: {e}')
        except Exception as e:  # noqa: BLE001  keep the inbox on disk; the next message or restart picks it up
            log(f'worker {wk} failed: {type(e).__name__}: {e}')
        finally:
            typing.cancel()
            self.busy.pop(wk, None)
            if not self.inbox.get(wk):
                self.inbox.pop(wk, None)
                self.save()

    async def run(self, ad, key, agent, task):
        """One agent turn in this thread; -> 'stopped', 'interrupted' or 'done'."""
        wk = f'{key}#{agent}'
        th = self.st['threads'][key]
        conv, person = th['conv'], th.get('person', '')
        session = th.setdefault('sessions', {}).get(agent)
        r = await self.access(person, agent)
        if not r.get('allowed'):   # access went while this waited in the inbox: nothing is spawned (R5)
            log(f'{wk}: {person} may no longer use {agent}; not starting the turn')
            return 'done'
        env = dict(os.environ, KITH_VIA=ad.platform, KITH_OUTPUT='json')
        if os.environ.get('KITH_BRIDGE_PERMISSION_MODE'):   # unset: the agent's mode, else kith's default (bin/kith)
            env['KITH_PERMISSION_MODE'] = os.environ['KITH_BRIDGE_PERMISSION_MODE']
        for k in [k for k in env if k.startswith(('JEV_', 'TELEGRAM_', 'SLACK_', 'DISCORD_', 'KITH_DISCORD_', 'HQ_DISCORD_'))
                  or k in ('KITH_AGENT', 'KITH_PERSON', 'KITH_RESUME', 'KITH_BRIDGE_OUTBOX',
                     'HQ_AGENT', 'HQ_PERSON', 'HQ_RESUME', 'HQ_BRIDGE_OUTBOX')]:
            env.pop(k)   # no bot tokens or keys, and this is no agent's helper
        if not r.get('owner'):   # bin/kith checks the membership again and sets their mode
            env['KITH_PERSON'] = person
        if session:   # any harness: bin/kith starts a new session when this one is another harness's
            env['KITH_RESUME'] = session
        outbox = STATE_DIR / 'outbox' / short(wk + str(time.time()))
        outbox.mkdir(parents=True, exist_ok=True)
        env.update(KITH_BRIDGE_OUTBOX=str(outbox), **ad.env(conv, agent))
        proc = await asyncio.create_subprocess_exec(
            KITH_BIN, agent, task + ad.footer, cwd=str(Path.home()), env=env, start_new_session=True,
            stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE)
        busy = self.busy[wk]
        busy['proc'] = proc
        if busy.get('ended'):            # Stop was pressed before the process existed
            kill(proc)
        pump = asyncio.create_task(self.pump(ad, conv, outbox, person, agent))
        try:
            out, err = await asyncio.wait_for(proc.communicate(), timeout=3600)
        except asyncio.TimeoutError:
            kill(proc)
            out, err = b'', b'timed out after 60 min'
        pump.cancel()
        await self.flush(ad, conv, outbox, person, agent)
        try:
            outbox.rmdir()
        except OSError:
            pass
        if busy.get('ended'):
            return busy['ended']
        if not await self.still(person, agent):   # revoked while it ran: the late answer is not delivered (R5)
            log(f'{wk}: {person} lost {agent} during the turn; its answer is dropped')
            return 'done'
        pct = None
        try:
            res = json.loads(out.decode())
            text, sid = res.get('result') or '', res.get('session_id')
            if res.get('is_error'):
                text = f'⚠️ {text or res.get("subtype", "error")}'
            pct = context_pct(res)
        except ValueError:
            text, sid = f'⚠️ {agent} failed:\n```\n{(err or out).decode(errors="replace")[-1500:]}\n```', None
        if sid:
            th['sessions'][agent] = sid
            self.save()
        show = pct if pct is not None and self.ctx_show is not None and pct >= self.ctx_show else None
        await ad.post(conv, agent, text, show)
        return 'done'

    async def pump(self, ad, conv, outbox, person='', agent=''):
        while True:
            await asyncio.sleep(1)
            await self.flush(ad, conv, outbox, person, agent)

    async def access(self, person, agent):
        """kith-auth's resolve for person and agent, asked while this Mac is in sync with the Kith cloud (R5, R8): while
        it says held, this waits (up to HOLD); still held then, it is refused, as a revoke on another Mac may not have
        reached this one (I2). The owner's own thread (person '') always may."""
        if not person:
            return {'owner': True, 'allowed': True}
        end = time.time() + HOLD
        while True:
            r = await asyncio.to_thread(self.resolve, person, agent)
            if not r.get('held'):
                return r
            if time.time() >= end:
                return {**r, 'allowed': False, 'why': 'out of sync with the Kith cloud'}
            await asyncio.sleep(2)

    async def still(self, person, agent):
        """Whether person may still use agent, in sync with the cloud (the owner's own thread, person '', always may)."""
        return bool((await self.access(person, agent)).get('allowed'))

    async def flush(self, ad, conv, outbox, person='', agent=''):
        """Posts what the run's kith-send calls left in its outbox, oldest first; nothing once person lost agent."""
        files = sorted(outbox.glob('*.json'))
        if files and agent and not await self.still(person, agent):
            for f in files:
                f.unlink(missing_ok=True)
            return
        for f in files:
            try:
                item = json.loads(f.read_text())
                f.unlink()
            except (OSError, ValueError):
                continue
            who = item.get('as') or 'kith'
            try:
                if item.get('files'):
                    await ad.send_files(conv, who, item['files'], item.get('caption', ''))
                else:
                    await ad.post(conv, who, item.get('text', ''))
            except Exception as e:  # noqa: BLE001  one failed post must not end the run's other posts
                log(f'{ad.conn}: kith-send post failed: {type(e).__name__}: {e}')

    async def stop(self, ad, wk, m=None):
        """Empty the inbox and end the running turn, or the one about to start."""
        self.inbox[wk] = []
        self.save()
        now = self.busy.get(wk, {})
        proc = now.get('proc')
        starting = proc is None and now.get('task')   # batch taken, process not started yet: run() kills it at once
        if starting or (proc is not None and proc.returncode is None):
            now['ended'] = 'stopped'
            if proc is not None:
                kill(proc)                   # the worker says "stopped" when the run comes back
        else:
            key, agent = wk.rsplit('#', 1)
            th = self.st['threads'].get(key)
            if th:
                await ad.status_end(th['conv'], agent, None, 'stopped')
        if m is not None:
            await ad.react(m, 'stop')   # only messages the bridge acted on get a reaction; queued ones are counted

    async def stop_pressed(self, ad, wid, uid):
        """The Stop button -> what to show on it: 'stopping', 'finished' or 'not yours'."""
        wk = self.works.get(wid)
        key = wk.rsplit('#', 1)[0] if wk else None
        th = self.st['threads'].get(key) if key else None
        w = await asyncio.to_thread(self.who, ad.platform, uid)
        if th is None or not self.owns(th, w):
            return 'not yours'
        now = self.busy.get(wk)
        if now is None or (now.get('proc') is not None and now['proc'].returncode is not None):
            return 'finished'
        log(f'stop {wk} pressed by {uid}')
        self.inbox[wk] = []
        self.save()
        now['ended'] = 'stopped'
        if now.get('proc') is not None:
            kill(now['proc'])
        return 'stopping'

    def resume(self, ad):
        """Messages that waited for this connection when the bridge last stopped, and live sessions' answers."""
        if any(k.startswith(ad.conn + ':') for k in self.st['live'].values()):
            asyncio.create_task(self.live_pump(ad))
        for wk, batches in list(self.inbox.items()):
            key, _, agent = wk.rpartition('#')
            if batches and key.startswith(ad.conn + ':') and wk not in self.busy and key in self.st['threads']:
                asyncio.create_task(self.worker(ad, key, agent))

    def link_reply(self, ad, chat, msg_id, key):
        """A message of a thread (Telegram has no threads: a reply to one of these continues it)."""
        self.st['replies'][f'{ad.conn}:{chat}:{msg_id}'] = key

    def replied_thread(self, ad, chat, msg_id):
        return self.st['replies'].get(f'{ad.conn}:{chat}:{msg_id}')
