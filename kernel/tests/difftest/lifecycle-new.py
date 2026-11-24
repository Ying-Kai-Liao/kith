#!/usr/bin/env python3
"""The Rust side of the `lifecycle` difftest cases: `kith kernel lifecycle` (argv: BIN KERNEL then the case's line).

The verbs `sweep`, `status` and `intent` go to the program unchanged. `compact SID WHAT` first does the setup the case names (the
decision on record, then the change between the decision and the claim that the case is about), here in Python because the Rust
verb is only `compact SID` (it has no way to take a claim for pid 1), then runs `kith kernel lifecycle compact SID` and prints
what it printed. The setup writes the same files the Python lifecycle did when the golden was recorded (the Python is gone).
"""
import datetime as dt, importlib.machinery, importlib.util, json, os, subprocess, sys

BIN, KERNEL, args = sys.argv[1], sys.argv[2], sys.argv[3:]
base = [BIN, 'kernel', 'lifecycle', '--kernel', KERNEL]
if not (args[:1] == ['compact'] and len(args) == 3):
    sys.exit(subprocess.run(base + args).returncode)

sid, what = args[1], args[2]
state = os.environ['KITH_STATE']
entry = json.load(open(f'{state}/live/terminal/{sid}.json'))
transcript = entry['transcriptPath']


def is_turn(e):
    if e.get('type') not in ('user', 'assistant') or e.get('isSidechain') or e.get('isMeta') or e.get('isCompactSummary'):
        return False
    if e.get('type') == 'user':
        c = (e.get('message') or {}).get('content')
        t = c if isinstance(c, str) else '\n'.join(b.get('text') or '' for b in c or [] if isinstance(b, dict) and b.get('type') == 'text')
        return not t.lstrip().startswith(('<command-name>', '<local-command', '<system-reminder>'))
    return True


def last_turn_at():
    for line in reversed(open(transcript).read().splitlines()):
        try:
            e = json.loads(line)
        except ValueError:
            continue
        if isinstance(e, dict) and is_turn(e):
            try:
                return dt.datetime.fromisoformat(str(e.get('timestamp')).replace('Z', '+00:00')).timestamp()
            except ValueError:
                pass
    return None


decision = f'{state}/sessions/decisions/{sid}.json'
os.makedirs(os.path.dirname(decision), mode=0o700, exist_ok=True)
json.dump({'decision': 'compact', 'at': 0, 'idleSince': last_turn_at(), 'compacted': False}, open(decision, 'w'))
if what == 'claim':   # launchd: alive, and the same pid in every run
    loader = importlib.machinery.SourceFileLoader('kith_sessions', os.path.join(KERNEL, 'bin', 'kith-sessions'))
    spec = importlib.util.spec_from_loader('kith_sessions', loader)
    ks = importlib.util.module_from_spec(spec)
    loader.exec_module(ks)
    ks.take_claim(sid, 1, 'terminal')
elif what == 'resumed':
    os.unlink(decision)
elif what == 'newturn':
    open(transcript, 'a').write(json.dumps({'type': 'user', 'timestamp': '2099-01-01T00:00:00.000Z', 'message': {'content': 'back'}}) + '\n')
elif what == 'self':
    open(transcript, 'a').write(json.dumps({'type': 'system', 'subtype': 'compact_boundary', 'timestamp': '2099-01-01T00:00:00.000Z'}) + '\n')
sys.exit(subprocess.run(base + ['compact', sid], env={**os.environ, 'KITH_LIFECYCLE_TEST_VERB': '1'}).returncode)
