#!/usr/bin/env python3
"""harness.py — run a kith agent on a harness other than Claude Code (bin/kith calls it; not for people).

  harness.py <harness> [--output json] [--resume SESSION] [--permission-mode MODE] [-- task...]

bin/kith builds everything a run needs, whatever the harness: the prompt (agent.md + PROTOCOL + memory + project), the
directories, the MCP servers the agent is granted, the model and effort. This turns that into one CLI's flags:

  codex     OpenAI Codex CLI          codex exec --json · developer_instructions · sandbox writable roots · MCP
  pi        pi coding agent           pi -p --mode json · --append-system-prompt · MCP through a run extension
  agy       Antigravity CLI           agy -p --output-format json · instructions go in front of the first task
  hermes    Hermes Agent              hermes chat -q --format stream-json · HERMES_EPHEMERAL_SYSTEM_PROMPT
  qwen      Qwen Code                 qwen -p -o json · --append-system-prompt · --include-directories
  dsh       DeepSeek Harness          dsh --profile headless --json · instructions in front of the first task
  opencode  opencode                  opencode run --format json · instructions and MCP through OPENCODE_CONFIG_CONTENT

Inputs from bin/kith's environment: KITH_RUN_PROMPT (the prompt file), KITH_RUN_DIRS (directories, one per line),
KITH_RUN_MCP (MCP config files, one per line, each {"mcpServers": {...}}), KITH_RUN_GLOBAL_SKILLS (1: the harness's own
skills and plugins stay on), KITH_MODEL, KITH_EFFORT, plus KITH_AGENT, KITH_PROJECT, KITH_VIA for the cost ledger.

With a task the answer goes to stdout: the text alone, or with --output json one object in the shape `claude -p
--output-format json` has (result, session_id, is_error, total_cost_usd, num_turns), so kith ask and the bridge read
every harness the same way. Session ids come back as "<harness>:<id>"; a --resume naming another harness (or a bare
Claude id) starts a new session instead. Without a task: the harness's own interactive session.

What only Claude Code runs have: the agent's settings.json (permissions, hooks), kith-cost's per-request ledger (here
one row per run, with the harness's own cost when it reports one), and for agy and hermes, per-run MCP servers.
"""
import datetime as dt, json, os, shutil, subprocess, sys, uuid
from pathlib import Path

KITH = Path(__file__).resolve().parent.parent
HARNESSES = ('codex', 'pi', 'agy', 'hermes', 'qwen', 'dsh', 'opencode')


def note(msg):
    print('kith: %s' % msg, file=sys.stderr)


def mcp_servers():
    """The granted servers and the agent's own (bin/kith passes both files), merged; later files win."""
    servers = {}
    for f in filter(None, os.environ.get('KITH_RUN_MCP', '').split('\n')):
        try:
            servers.update(json.loads(Path(f).read_text()).get('mcpServers') or {})
        except (OSError, ValueError) as e:
            note('MCP config %s unreadable: %s' % (f, e))
    return servers


def toml(v):
    """A TOML value for codex's -c key=value (JSON strings, lists and numbers are valid TOML)."""
    if isinstance(v, dict):
        return '{%s}' % ', '.join('%s = %s' % (json.dumps(k), toml(x)) for k, x in v.items())
    if isinstance(v, (list, tuple)):
        return '[%s]' % ', '.join(toml(x) for x in v)
    return json.dumps(v)


def instructions_first(prompt, task):
    """For harnesses with no system-prompt option: the agent's instructions travel with the first task."""
    return ('<hq-agent-instructions>\n%s\n</hq-agent-instructions>\n\nThe instructions above are who you are and how '
            'you work for this whole session. The task:\n\n%s' % (prompt, task))


class Run:
    def __init__(self, harness, task, output, resume, permission_mode):
        self.harness, self.task, self.output, self.mode = harness, task, output, permission_mode or ''
        self.prompt_file = os.environ['KITH_RUN_PROMPT']
        self.prompt = Path(self.prompt_file).read_text()
        self.dirs = [d for d in os.environ.get('KITH_RUN_DIRS', '').split('\n') if d]
        self.model, self.effort = os.environ.get('KITH_MODEL', ''), os.environ.get('KITH_EFFORT', '')
        self.global_skills = os.environ.get('KITH_RUN_GLOBAL_SKILLS') == '1'
        self.mcp = mcp_servers()
        self.bypass = self.mode == 'bypassPermissions'
        self.env = dict(os.environ)
        for k in [k for k in self.env if k.startswith('KITH_RUN_')]:
            self.env.pop(k)
        self.resume = None
        if resume:
            h, _, sid = resume.partition(':')
            if h == harness and sid:
                self.resume = sid
            else:
                note('session %s is not a %s session; starting a new one' % (resume, harness))
        self.tmp = Path(self.prompt_file).parent

    def unsupported_mcp(self):
        if self.mcp:
            note('%s takes no MCP servers per run; this run is without %s (Claude Code runs have them)'
                 % (self.harness, ', '.join(self.mcp)))

    # --- one function per harness: argv for a one-shot run (task) or an interactive one (no task) ------------------

    def codex(self):
        effort = {'max': 'xhigh'}.get(self.effort, self.effort)
        cfg = ['developer_instructions=' + toml(self.prompt)]
        if effort:
            cfg.append('model_reasoning_effort=' + toml(effort))
        if not self.bypass:
            cfg += ['sandbox_mode="workspace-write"', 'sandbox_workspace_write.writable_roots=' + toml(self.dirs)]
        for name, s in self.mcp.items():
            entry = {'url': s['url']} if s.get('url') else {'command': s['command'], 'args': s.get('args', [])}
            if s.get('env'):
                entry['env'] = s['env']
            cfg.append('mcp_servers.%s=%s' % (json.dumps(name), toml(entry)))
        flags = [x for c in cfg for x in ('-c', c)]
        if self.model:
            flags += ['-m', self.model]
        if self.bypass:
            flags.append('--dangerously-bypass-approvals-and-sandbox')
        if not self.global_skills:
            flags += ['--disable', 'plugins', '--disable', 'apps']
        if self.task is None:
            return ['codex', *flags, *[x for d in self.dirs for x in ('--add-dir', d)]]
        head = ['codex', 'exec', 'resume', self.resume] if self.resume else ['codex', 'exec']
        return head + ['--json', '--skip-git-repo-check', *flags, '--', self.task]

    def codex_result(self, out):
        r = {'session': None, 'text': '', 'error': None, 'usage': {}, 'turns': 0}
        for d in jsonl(out):
            t = d.get('type')
            if t == 'thread.started':
                r['session'] = d.get('thread_id')
            elif t == 'item.completed' and (d.get('item') or {}).get('type') == 'agent_message':
                r['text'] = d['item'].get('text') or ''
            elif t == 'turn.completed':
                r['turns'] += 1
                u = d.get('usage') or {}
                r['usage'] = {'input': u.get('input_tokens', 0), 'output': u.get('output_tokens', 0),
                              'cache_read': u.get('cached_input_tokens', 0)}
            elif t in ('turn.failed', 'error'):
                r['error'] = (d.get('error') or {}).get('message') or d.get('message') or t
                limit = usage_limit(r['error'])
                if limit is not None:
                    r['usage_limit'] = limit
        return r

    def pi(self):
        flags = ['--append-system-prompt', self.prompt_file]   # a path: pi appends the file's contents
        if self.model:
            flags += ['--model', self.model]
        if self.effort:
            flags += ['--thinking', self.effort]
        if not self.global_skills:
            flags.append('--no-skills')
        if self.mcp:   # pi has no per-run MCP flag; an extension registers the servers for this session only
            ext = self.tmp / ('%d.pi-mcp.js' % os.getpid())
            ext.write_text('export default function (pi) {\n%s}\n' % ''.join(
                '  pi.registerMcpServer(%s, %s);\n' % (json.dumps(n), json.dumps(s)) for n, s in self.mcp.items()))
            flags += ['-e', str(ext)]
        if self.task is None:
            return ['pi', *flags]
        self.session = self.resume or str(uuid.uuid4())   # pi takes the id we choose and creates the session
        return ['pi', '-p', '--mode', 'json', '--session-id', self.session, *flags, '--', self.task]

    def pi_result(self, out):
        r = {'session': None, 'text': '', 'error': None, 'usage': {}, 'turns': 0, 'cost': 0.0}
        for d in jsonl(out):
            t = d.get('type')
            if t == 'session':
                r['session'] = d.get('id')
            elif t == 'turn_end':
                r['turns'] += 1
            elif t == 'message_end' and (d.get('message') or {}).get('role') == 'assistant':
                m = d['message']
                text = ''.join(b.get('text', '') for b in m.get('content') or [] if b.get('type') == 'text')
                if text:
                    r['text'] = text
                if m.get('stopReason') == 'error':
                    r['error'] = m.get('errorMessage') or 'error'
                u = m.get('usage') or {}
                for k, f in (('input', 'input'), ('output', 'output'), ('cache_read', 'cacheRead'),
                             ('cache_write', 'cacheWrite')):
                    r['usage'][k] = r['usage'].get(k, 0) + (u.get(f) or 0)
                r['cost'] += (u.get('cost') or {}).get('total') or 0
        r['session'] = r['session'] or getattr(self, 'session', None)
        return r

    def agy(self):
        self.unsupported_mcp()
        flags = [x for d in self.dirs for x in ('--add-dir', d)]
        if self.model:
            flags += ['--model', self.model]
        if self.effort:
            flags += ['--effort', self.effort]
        if self.bypass:
            flags.append('--dangerously-skip-permissions')
        if not self.global_skills:
            flags.append('--disable-slash-commands')
        if self.task is None:
            return ['agy', *flags, '-i', instructions_first(self.prompt, 'Wait for the user\'s first message.')]
        if self.resume:
            return ['agy', *flags, '--conversation', self.resume, '--output-format', 'json', '-p', self.task]
        return ['agy', *flags, '--output-format', 'json', '-p', instructions_first(self.prompt, self.task)]

    def agy_result(self, out):
        d = next(reversed(list(jsonl(out))), {})
        u = d.get('usage') or {}
        return {'session': d.get('conversation_id'), 'text': d.get('response') or '', 'turns': d.get('num_turns', 0),
                'error': None if d.get('status') == 'SUCCESS' else (d.get('error') or d.get('status') or 'no answer'),
                'usage': {'input': u.get('input_tokens', 0), 'output': u.get('output_tokens', 0),
                          'cache_read': u.get('cache_read_tokens', 0)}}

    def hermes(self):
        self.unsupported_mcp()
        self.env['HERMES_EPHEMERAL_SYSTEM_PROMPT'] = self.prompt   # never written to the session's history
        flags = []
        if self.model:
            flags += ['-m', self.model]
        if self.effort:
            flags += ['--reasoning', {'max': 'xhigh'}.get(self.effort, self.effort)]
        if self.bypass:
            flags.append('--yolo')
        if self.task is None:
            return ['hermes', 'chat', *flags]
        if self.resume:
            flags += ['--resume', self.resume]
        return ['hermes', 'chat', *flags, '-Q', '--format', 'stream-json', '-q', self.task]

    def hermes_result(self, out):
        r = {'session': None, 'text': '', 'error': None, 'usage': {}, 'turns': 1}
        for d in jsonl(out):
            r['session'] = d.get('session_id') or r['session']
            if d.get('type') == 'result':
                r['text'] = d.get('text') or ''
                if d.get('exit_code'):
                    r['error'] = d.get('error') or 'exit code %s' % d['exit_code']
                t = d.get('tokens') or {}
                r['usage'] = {'input': t.get('input', 0), 'output': t.get('output', 0),
                              'cache_read': t.get('cache_read', 0), 'cache_write': t.get('cache_write', 0)}
            elif d.get('type') == 'error':
                r['error'] = d.get('message') or d.get('error') or 'error'
        return r

    def qwen(self):
        self.unsupported_mcp()
        self.env['QWEN_CODE_SUPPRESS_YOLO_WARNING'] = '1'
        flags = ['--append-system-prompt', self.prompt,
                 '--approval-mode', 'yolo' if self.bypass else 'auto-edit']
        if self.dirs:
            flags += ['--include-directories', ','.join(self.dirs)]
        if self.model:
            flags += ['-m', self.model]
        if self.task is None:
            return ['qwen', *flags]
        if self.resume:
            flags += ['--resume', self.resume]
        return ['qwen', *flags, '-o', 'json', '-p', self.task]

    def qwen_result(self, out):
        try:
            events = json.loads(out)
        except ValueError:
            events = list(jsonl(out))
        d = next((e for e in reversed(events) if isinstance(e, dict) and e.get('type') == 'result'), {})
        u = d.get('usage') or {}
        return {'session': d.get('session_id'), 'text': d.get('result') or '', 'turns': d.get('num_turns', 0),
                'error': (d.get('result') or d.get('subtype') or 'error') if d.get('is_error') or not d else None,
                'usage': {'input': u.get('input_tokens', 0), 'output': u.get('output_tokens', 0),
                          'cache_read': u.get('cache_read_input_tokens', 0)}}

    def dsh(self):
        self.unsupported_mcp()
        if self.model:
            note('dsh picks its model in its profile (dsh web → Models), not per run; ignoring %s' % self.model)
        if 'DEEPSEEK_API_KEY' not in self.env:   # kith's own key when the agent is granted the deepseek service
            k = subprocess.run([str(KITH / 'bin/kith-auth'), 'get', 'deepseek', 'DEEPSEEK_API_KEY'],
                               capture_output=True, text=True, env=self.env)
            if k.returncode == 0 and k.stdout.strip():
                self.env['DEEPSEEK_API_KEY'] = k.stdout.strip()
        dsh = ['dsh'] if shutil.which('dsh') else ['npx', '-y', '@deepseek-ai/dsh']
        if self.task is None:
            note('dsh has no way to give its terminal app the agent\'s instructions; give a task, or use another harness')
            sys.exit(1)
        argv = dsh + ['--profile', 'headless', '--json']
        if self.resume:
            argv += ['--session-id', self.resume]
        self.stdin = self.task if self.resume else instructions_first(self.prompt, self.task)
        return argv + ['-']   # the task on stdin: no quoting or length limits

    def dsh_result(self, out):
        r = {'session': None, 'text': '', 'error': None, 'usage': {}, 'turns': 0}
        for d in jsonl(out):
            t = d.get('type')
            if t == 'session':
                r['session'] = d.get('sessionId')
            elif t == 'final':
                r['text'] = d.get('text') or ''
            elif t == 'status' and d.get('phase') == 'turn_end':
                r['turns'] += 1
                reason = d.get('reason') or {}
                if reason.get('kind') == 'error':
                    r['error'] = (reason.get('error') or {}).get('message') or 'error'
            elif t == 'usage':
                for k in ('input', 'output'):
                    r['usage'][k] = r['usage'].get(k, 0) + (d.get(k) or d.get(k + '_tokens') or 0)
        return r

    def opencode(self):
        cfg = {'instructions': [self.prompt_file]}
        if self.mcp:
            cfg['mcp'] = {n: ({'type': 'remote', 'url': s['url'], 'headers': s.get('headers', {})} if s.get('url') else
                              {'type': 'local', 'command': [s['command'], *s.get('args', [])],
                               'environment': s.get('env', {})}) for n, s in self.mcp.items()}
        if self.bypass:
            cfg['permission'] = {'edit': 'allow', 'bash': 'allow', 'webfetch': 'allow'}
        self.env['OPENCODE_CONFIG_CONTENT'] = json.dumps(cfg)
        flags = ['-m', self.model] if self.model else []
        oc = ['opencode'] if shutil.which('opencode') else ['npx', '-y', 'opencode-ai']
        if self.task is None:
            return [*oc, *flags]
        if self.resume:
            flags += ['--session', self.resume]
        if self.effort:
            flags += ['--variant', self.effort]
        return [*oc, 'run', '--format', 'json', *flags, self.task]

    def opencode_result(self, out):
        r = {'session': None, 'text': '', 'error': None, 'usage': {}, 'turns': 0, 'cost': 0.0}
        step = []
        for d in jsonl(out):
            r['session'] = d.get('sessionID') or r['session']
            part = d.get('part') or {}
            if d.get('type') == 'step_start':
                step = []
            elif d.get('type') == 'text' and part.get('text'):
                step.append(part['text'])   # the answer is the last step that said something
                r['text'] = ''.join(step).split('</think>')[-1].strip()
            elif d.get('type') == 'step_finish':
                r['turns'] += 1
                r['cost'] += part.get('cost') or 0
                t = part.get('tokens') or {}
                for k, f in (('input', 'input'), ('output', 'output')):
                    r['usage'][k] = r['usage'].get(k, 0) + (t.get(f) or 0)
                c = t.get('cache') or {}
                r['usage']['cache_read'] = r['usage'].get('cache_read', 0) + (c.get('read') or 0)
            elif d.get('type') == 'error':
                e = d.get('error') or {}
                r['error'] = (e.get('data') or {}).get('message') or e.get('name') or 'error'
        return r

    # --- running ---------------------------------------------------------------------------------------------------

    def go(self):
        self.stdin = None
        if self.task is not None and not self.resume:
            r = self.acp_one_shot()
            if r is not None:
                return self.finish(r)
        argv = getattr(self, self.harness)()
        if not shutil.which(argv[0]):
            sys.exit('kith: %s is not installed (%s); see kernel/adapters/harness.py' % (argv[0], self.harness))
        if self.task is None:
            os.execvpe(argv[0], argv, self.env)
        p = subprocess.run(argv, env=self.env, input=self.stdin, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                           text=True, stdin=None if self.stdin is not None else subprocess.DEVNULL)
        sys.stderr.write(p.stderr)
        r = getattr(self, self.harness + '_result')(p.stdout)
        if not r['text'] and not r['error'] and p.returncode:
            # The harness's own last words (a missing login, a bad flag), not JSON: the reason the run failed.
            said = [l.strip() for l in (p.stderr + '\n' + p.stdout).splitlines()
                    if l.strip() and not l.lstrip().startswith(('{', '[')) and 'Reading additional input' not in l]
            r['error'] = ' '.join(said[-3:])[-500:] or 'exited %d' % p.returncode
        return self.finish(r)

    def acp_one_shot(self):
        """The run's result (the shape of <harness>_result) from `kith kernel run-once`, when the core does one-shots
        over ACP (lib/core-switched.txt `run-once`, or KITH_CORE=rust) and the engine's table row says `oneshot: acp`
        with a native ACP mode. None: this run goes the harness's own way. Once run-once is switched, the per-engine
        argv and *_result code above is dead for the engines whose row says `oneshot: acp` and that have a native ACP
        mode: pi (no local mode) and opencode/qwen/agy stay here only for resumed runs and until the verb is switched
        (docs/plans/one-bot-model.md A2b)."""
        if not shutil.which(self.harness):
            return None   # the native path says what is missing, as it always has
        kith = run_once_bin()
        if not kith:
            return None
        if self.effort:
            note('%s over ACP takes no effort per run; this run is without effort %s' % (self.harness, self.effort))
        if self.dirs[1:]:
            note('%s over ACP works in one directory; this run is without %s' % (self.harness, ', '.join(self.dirs[1:])))
        cmd = [kith, 'kernel', 'run-once', self.harness, '--json', '--cwd', os.getcwd(), '--timeout', '86400']
        if self.model:
            cmd += ['--model', self.model]
        if self.bypass:
            cmd.append('--approve')
        if self.mcp:
            cmd += ['--mcp-config', json.dumps({'mcpServers': self.mcp})]
        p = subprocess.run(cmd + ['--', instructions_first(self.prompt, self.task)], env=self.env, text=True,
                           stdout=subprocess.PIPE, stderr=subprocess.PIPE, stdin=subprocess.DEVNULL)
        if p.returncode == 78:
            return None   # no ACP one-shot for this engine here; nothing was started
        sys.stderr.write(p.stderr)
        d = next(reversed(list(jsonl(p.stdout))), None)
        r = {'session': None, 'text': '', 'error': None, 'usage': {}, 'turns': 1}
        if d is None:
            r['error'] = ' '.join(p.stderr.split())[-500:] or 'exited %d' % p.returncode
        elif d.get('error'):
            r['error'] = d['error']
            if d.get('kind') == 'depleted':
                r['usage_limit'] = {'resets_at': d.get('resets_at')}
        else:
            r.update(session=d.get('session_id'), text=d.get('text') or '', cost=d.get('cost_usd'),
                     usage={k: v for k, v in (d.get('usage') or {}).items() if v or k in ('input', 'output')})
        return r

    def finish(self, r):
        """Records the run in the cost ledger and prints its result."""
        sid = '%s:%s' % (self.harness, r['session']) if r.get('session') else None
        cost = r.get('cost') or None
        record(self.harness, sid, self.model or self.harness, self.task, r.get('usage') or {}, cost)
        if self.output == 'json':
            print(json.dumps({'type': 'result', 'subtype': 'error' if r['error'] else 'success', 'harness': self.harness,
                              'is_error': bool(r['error']), 'result': r['text'] or r['error'] or '',
                              'session_id': sid, 'num_turns': r.get('turns', 0), 'total_cost_usd': cost,
                              'usage': r.get('usage') or {},
                              **({'usage_limit': r['usage_limit']} if 'usage_limit' in r else {})}, ensure_ascii=False))
        else:
            if r['text']:
                print(r['text'])
            if r['error']:
                note('%s: %s' % (self.harness, r['error']))
        return 1 if r['error'] else 0


def run_once_bin():
    """The kith program when the core answers `run-once` (the rule of lib/kith-core.sh): not KITH_CORE=python, the verb
    in lib/core-switched.txt (or KITH_CORE=rust), and the program lists the module in `kith kernel version`."""
    mode = os.environ.get('KITH_CORE', '')
    if mode == 'python':
        return None
    module = None
    try:
        for line in (KITH / 'lib/core-switched.txt').read_text().splitlines():
            parts = line.split('\t')
            if len(parts) == 2 and parts[0] == 'run-once':
                module = parts[1]
    except OSError:
        pass
    if not module:
        if mode != 'rust':
            return None
        module = 'acp'
    try:
        kith = subprocess.run([sys.executable, str(KITH / 'lib/kith_paths.py'), '--bin'], capture_output=True,
                              text=True, timeout=15).stdout.strip()
        version = subprocess.run([kith, 'kernel', 'version'], capture_output=True, text=True, timeout=15).stdout
    except (OSError, subprocess.SubprocessError):
        return None
    return kith if kith and '"%s"' % module in version else None


def usage_limit(message):
    """codex reports a spent plan as an error whose message is itself JSON with error.type usage_limit_reached (and
    resets_at, epoch seconds, or resets_in_seconds). {'resets_at': epoch or None} for that, else None. Plain text is
    matched only on codex's own opening words, so a task that talks about limits is not taken for one."""
    try:
        e = (json.loads(message) or {}).get('error') or {}
    except (TypeError, ValueError, AttributeError):
        e = {}
    if e.get('type') == 'usage_limit_reached':
        at = e.get('resets_at')
        if not at and e.get('resets_in_seconds'):
            at = int(dt.datetime.now().timestamp() + e['resets_in_seconds'])
        return {'resets_at': at}
    if isinstance(message, str) and message.lower().startswith("you've hit your usage limit"):
        return {'resets_at': None}
    return None


def jsonl(text):
    for line in (text or '').splitlines():
        line = line.strip()
        if line.startswith('{'):
            try:
                yield json.loads(line)
            except ValueError:
                pass


def record(harness, session, model, task, usage, cost):
    """One ledger row per run (kith-cost record); cost tracking never fails a run."""
    if not session:
        return
    row = {'id': '%s@%s' % (session, dt.datetime.now(dt.timezone.utc).strftime('%Y%m%dT%H%M%S.%f')),
           'session': session, 'model': '%s/%s' % (harness, model) if model != harness else harness,
           'turn': ' '.join((task or '').split())[:100], 'cost': cost, **usage}
    try:
        subprocess.run([str(KITH / 'bin/kith-cost'), 'record'], input=json.dumps(row), text=True, timeout=15,
                       capture_output=True)
    except (OSError, subprocess.SubprocessError):
        pass


def main(argv):
    if not argv or argv[0] not in HARNESSES:
        sys.exit('kith: unknown harness %r; one of: claude, %s' % (argv[0] if argv else '', ', '.join(HARNESSES)))
    harness, rest, opts = argv[0], argv[1:], {'--output': None, '--resume': None, '--permission-mode': None}
    while rest and rest[0] in opts:
        opts[rest[0]] = rest[1]
        rest = rest[2:]
    if rest[:1] == ['--']:
        rest = rest[1:]
    task = ' '.join(rest) if rest else None
    return Run(harness, task, opts['--output'], opts['--resume'], opts['--permission-mode']).go()


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
