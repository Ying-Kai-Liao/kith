#!/usr/bin/env python3
"""ask-driver: one `kith ask` command line of an ask-* difftest case, the same on both sides.

    ask-driver.py old|new [env:NAME=VALUE]... ARGS...      run `kith ask ARGS` (OLD: the Python kith-ask of the case's kernel copy,
                                                           NEW: `kith kernel ask`, the thin client of the core's ask rows)
    ask-driver.py old|new seed-bg ID AGENT SESSION PERSON  make a --bg job ID that holds SESSION, for `--to` (OLD: the files of
                                                           live/ask and ask-bg, NEW: an `ask.open` row), and print nothing

An argument `@ID` is the newest ask of `delegations --json` (a foreground ask prints no id, and a `--bg` one is made with a random
one). `env:NAME=VALUE` words at the start set the caller's environment for this line alone (an agent's run asking: KITH_AGENT,
CLAUDE_CODE_SESSION_ID). The case's environment names the kernel copy (ASK_K: a stub bin/kith, the real lib) and, for NEW, the
program (KITH_BIN, which the harness sets).
"""
import json
import os
import subprocess
import sys


def program(side):
    k = os.environ['ASK_K']
    if side == 'old':
        return ['python3', os.path.join(k, 'bin', 'kith-ask')]
    # `--` first: what follows is the command line of kith ask, whatever it starts with
    return [os.environ['KITH_BIN'], 'kernel', 'ask', '--kernel', k, '--']


def machine():
    sys.path.insert(0, os.path.join(os.environ['ASK_K'], 'lib'))
    from kith_paths import paths
    return paths()['KITH_MACHINE']


def seed_bg(side, ident, agent, session, person):
    caller = {'KITH_AGENT': 'coder', 'KITH_PERSON': person, 'CLAUDE_CODE_SESSION_ID': 's-caller'}
    if side == 'old':
        box, bg = machine() / 'live' / 'ask', machine() / 'ask-bg'
        box.mkdir(parents=True, exist_ok=True)
        bg.mkdir(parents=True, exist_ok=True)
        (box / (ident + '.reg')).write_text(json.dumps({
            'asked': agent, 'agent': caller['KITH_AGENT'], 'person': person, 'via': 'terminal',
            'reply_to': {'session': caller['CLAUDE_CODE_SESSION_ID'], 'ask': None}, 'chain': ident, 'hop': 0, 'cross_hops': 0,
            'crossed': False, 'cost': 0.0, 'cost_base': 0.0, 'state': 'open', 'at': 1700000000.0, 'pid': 1, 'pid_at': 1700000000.0}))
        (bg / (ident + '.cmd')).write_text('x')
        (bg / (ident + '.json')).write_text(json.dumps({'agent': agent, 'key': 'k', 'dir': '/', 'person': person, 'at': 1.0}))
        (bg / (ident + '.sid')).write_text(session)
        return 0
    params = {'id': ident, 'asked': agent, 'mode': 'bg', 'task': 'seed', 'conversation': session,
              'home': os.environ['HOME'], 'kernel': os.environ['ASK_K'], 'env': dict(os.environ, **caller)}
    got = subprocess.run([os.environ['KITH_BIN'], 'kernel', 'core-call', 'ask.open', '--params-stdin'], input=json.dumps(params),
                         capture_output=True, text=True)
    if got.returncode != 0:
        sys.stderr.write('seed-bg: %s' % (got.stderr or got.stdout))
    return got.returncode


def main(argv):
    side, args, env = argv[0], argv[1:], dict(os.environ)
    while args and args[0].startswith('env:'):
        name, _, value = args.pop(0)[4:].partition('=')
        env[name] = value
    if args and args[0] == 'seed-bg':
        return seed_bg(side, *args[1:5])
    prog = program(side)
    if '@ID' in args:
        got = subprocess.run(prog + ['delegations', '--json'], capture_output=True, text=True, env=dict(os.environ))
        rows = json.loads(got.stdout)['delegations']
        args = [rows[0]['id'] if a == '@ID' else a for a in args]
    return subprocess.run(prog + args, env=env).returncode


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
