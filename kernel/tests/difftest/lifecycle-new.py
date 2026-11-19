#!/usr/bin/env python3
"""The Rust side of the `lifecycle` difftest cases: `kith kernel lifecycle` (argv: BIN KERNEL then the case's line).

The verbs `sweep`, `status` and `intent` go to the program unchanged. `compact SID WHAT` does what `lifecycle-old.py` does for
WHAT (the decision on record, then the change between the decision and the claim that the case is about), here in Python
because the Rust verb is only `compact SID` (it has no way to take a claim for pid 1), then runs `kith kernel lifecycle compact SID`
and prints what it printed. The setup is the Python side's own, so both sides start from the same files.
"""
import importlib.machinery, importlib.util, json, os, subprocess, sys

BIN, KERNEL, args = sys.argv[1], sys.argv[2], sys.argv[3:]
base = [BIN, 'kernel', 'lifecycle', '--kernel', KERNEL]
if not (args[:1] == ['compact'] and len(args) == 3):
    sys.exit(subprocess.run(base + args).returncode)

sys.path.insert(0, os.path.join(KERNEL, 'lib'))
import kith_lifecycle as lc  # noqa: E402

loader = importlib.machinery.SourceFileLoader('kl', os.path.join(KERNEL, 'bin', 'kith-lifecycle'))
spec = importlib.util.spec_from_loader('kl', loader)
kl = importlib.util.module_from_spec(spec)
loader.exec_module(kl)

sid, what = args[1], args[2]
ks = kl.sessions_mod()
e = next(x for x in kl.entries(ks) if x['sessionId'] == sid)
since = lc.last_turn_at(e['transcriptPath'])
lc.write_decision(sid, {'decision': 'compact', 'at': 0, 'idleSince': since, 'compacted': False})
if what == 'claim':
    ks.take_claim(sid, 1, 'terminal')   # launchd: alive, and the same pid in every run
elif what == 'resumed':
    lc.clear_decision(sid)
elif what == 'newturn':
    open(e['transcriptPath'], 'a').write(json.dumps({'type': 'user', 'timestamp': '2099-01-01T00:00:00.000Z', 'message': {'content': 'back'}}) + '\n')
elif what == 'self':
    open(e['transcriptPath'], 'a').write(json.dumps({'type': 'system', 'subtype': 'compact_boundary', 'timestamp': '2099-01-01T00:00:00.000Z'}) + '\n')
sys.exit(subprocess.run(base + ['compact', sid]).returncode)
