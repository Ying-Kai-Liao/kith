#!/usr/bin/env python3
"""The Python side of the `lifecycle` difftest cases: kernel/bin/kith-lifecycle (kith-lifecycle has no Rust side yet).

The verbs it has (`sweep`, `status`, `intent`) run unchanged. `compact SID WHAT` calls compact() of kith-lifecycle for a
session whose decision is already on record (the sweep cannot reach compact()'s own refusals from outside: they are what
a change between the decision and the claim makes of it) and prints what compact() returned. WHAT:

  none      nothing changed: compact runs
  claim     another process holds the writer claim (pid 1)
  resumed   a resume cleared the decision after it was made
  newturn   the session has a new turn since
  self      Claude compacted the session itself since
"""
import importlib.machinery, importlib.util, json, os, sys

KERNEL = os.path.join(os.path.dirname(os.path.realpath(__file__)), '..', '..')
sys.path.insert(0, os.path.join(KERNEL, 'lib'))
import kith_lifecycle as lc  # noqa: E402

loader = importlib.machinery.SourceFileLoader('kl', os.path.join(KERNEL, 'bin', 'kith-lifecycle'))
spec = importlib.util.spec_from_loader('kl', loader)
kl = importlib.util.module_from_spec(spec)
loader.exec_module(kl)


def compact(sid, what):
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
    print(kl.compact(ks, e, since, None))
    return 0


if sys.argv[1:2] == ['compact'] and len(sys.argv) == 4:
    sys.exit(compact(sys.argv[2], sys.argv[3]))
sys.exit(kl.main(sys.argv[1:]))
