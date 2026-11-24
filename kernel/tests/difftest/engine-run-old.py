#!/usr/bin/env python3
"""The old side of the `engine-run-*` difftest cases: what kernel/adapters/harness.py did for a one-shot over ACP before
`kith kernel engine-run` took it over (acp_one_shot, finish and record, as they were at origin/main 346f67ba9), frozen here.

  engine-run-old.py ENGINE [--output json] [--permission-mode M] -- task

Same inputs as harness.py (KITH_RUN_PROMPT, KITH_RUN_DIRS, KITH_RUN_MCP, KITH_MODEL, KITH_EFFORT, KITH_AGENT, ...). The Rust
program is $KITH_BIN (the case sets bin_both). Resume went native then, so it has no case here.
"""
import datetime as dt, json, os, subprocess, sys
from pathlib import Path

KITH = Path(__file__).resolve().parent.parent.parent
BIN = os.environ['KITH_BIN']


def note(msg):
    print('kith: %s' % msg, file=sys.stderr)


def mcp_servers():
    servers = {}
    for f in filter(None, os.environ.get('KITH_RUN_MCP', '').split('\n')):
        try:
            servers.update(json.loads(Path(f).read_text()).get('mcpServers') or {})
        except (OSError, ValueError) as e:
            note('MCP config %s unreadable: %s' % (f, e))
    return servers


def instructions_first(prompt, task):
    return ('<hq-agent-instructions>\n%s\n</hq-agent-instructions>\n\nThe instructions above are who you are and how '
            'you work for this whole session. The task:\n\n%s' % (prompt, task))


def jsonl(text):
    for line in (text or '').splitlines():
        line = line.strip()
        if line.startswith('{'):
            try:
                yield json.loads(line)
            except ValueError:
                pass


def record(harness, session, model, task, usage, cost):
    if not session:
        return
    row = {'id': '%s@%s' % (session, dt.datetime.now(dt.timezone.utc).strftime('%Y%m%dT%H%M%S.%f')),
           'session': session, 'model': '%s/%s' % (harness, model) if model != harness else harness,
           'turn': ' '.join((task or '').split())[:100], 'cost': cost, **usage}
    subprocess.run([BIN, 'kernel', 'cost', '--kernel', str(KITH), 'record'], input=json.dumps(row), text=True,
                   timeout=15, capture_output=True)


class Run:
    def __init__(self, harness, task, output, permission_mode):
        self.harness, self.task, self.output, self.mode = harness, task, output, permission_mode or ''
        self.prompt_file = os.environ['KITH_RUN_PROMPT']
        self.prompt = Path(self.prompt_file).read_text()
        self.dirs = list(dict.fromkeys(os.path.realpath(d) for d in os.environ.get('KITH_RUN_DIRS', '').split('\n') if d))
        self.model, self.effort = os.environ.get('KITH_MODEL', ''), os.environ.get('KITH_EFFORT', '')
        self.mcp = mcp_servers()
        self.bypass = self.mode == 'bypassPermissions'
        self.env = dict(os.environ)
        for k in [k for k in self.env if k.startswith('KITH_RUN_')]:
            self.env.pop(k)

    def go(self):
        if self.effort:
            note('%s over ACP takes no effort per run; this run is without effort %s' % (self.harness, self.effort))
        cmd = [BIN, 'kernel', 'run-once', self.harness, '--json', '--cwd', os.getcwd(), '--timeout', '86400']
        for d in self.dirs[1:]:
            cmd += ['--add-dir', d]
        if self.model:
            cmd += ['--model', self.model]
        if self.bypass:
            cmd.append('--approve')
        if self.mcp:
            cmd += ['--mcp-config', json.dumps({'mcpServers': self.mcp})]
        p = subprocess.run(cmd + ['--', instructions_first(self.prompt, self.task)], env=self.env, text=True,
                           stdout=subprocess.PIPE, stderr=subprocess.PIPE, stdin=subprocess.DEVNULL)
        sys.stderr.write(p.stderr)
        d = next(reversed(list(jsonl(p.stdout))), None)
        r = {'session': None, 'text': '', 'error': None, 'usage': {}, 'turns': 1}
        if d is None:
            r['error'] = ' '.join(p.stderr.split())[-500:] or 'exited %d' % p.returncode
        elif d.get('error'):
            r['error'] = d['error']
            r['cost'] = d.get('cost_usd')
            if d.get('kind') == 'depleted':
                r['usage_limit'] = {'resets_at': d.get('resets_at')}
        else:
            r.update(session=d.get('session_id'), text=d.get('text') or '', cost=d.get('cost_usd'),
                     usage={k: v for k, v in (d.get('usage') or {}).items() if v or k in ('input', 'output')})
        return self.finish(r)

    def finish(self, r):
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


def main(argv):
    harness, rest, opts = argv[0], argv[1:], {'--output': None, '--permission-mode': None}
    while rest and rest[0] in opts:
        opts[rest[0]] = rest[1]
        rest = rest[2:]
    if rest[:1] == ['--']:
        rest = rest[1:]
    return Run(harness, ' '.join(rest), opts['--output'], opts['--permission-mode']).go()


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
