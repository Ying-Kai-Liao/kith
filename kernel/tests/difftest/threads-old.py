#!/usr/bin/env python3
"""The Python side of the `threads` difftest cases: `kith kernel threads ...` asked of kernel/bin/kith-sessions.

The verbs kith-sessions has (record, resolve, claim, terminal, proc, sweep) run through it unchanged. The two the Rust verb
adds for tests and the two-actor race call kith-sessions' own functions:

  release PID     release_claims(PID): gives back every claim PID holds
  holder SID      the live claim on SID under either name, as its file says; exit 1 when there is none
"""
import importlib.util, json, os, sys
from importlib.machinery import SourceFileLoader

here = os.path.dirname(os.path.realpath(__file__))
path = os.path.join(here, '..', '..', 'bin', 'kith-sessions')
spec = importlib.util.spec_from_loader('kith_sessions', SourceFileLoader('kith_sessions', path))
ks = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ks)


def main(argv):
    if argv[:1] == ['release'] and len(argv) == 2:
        ks.release_claims(int(argv[1]))
    elif argv[:1] == ['holder'] and len(argv) == 2:
        for takeover in (False, True):
            p = ks.claim_path(argv[1], takeover)
            c = ks.read_claim(p) if p else None
            if c and ks.claim_live(c):
                print(json.dumps(c))
                return 0
        return 1
    else:
        sys.argv = [path] + argv
        ks_main = compile(open(path).read(), path, 'exec')
        exec(ks_main, {'__name__': '__main__', '__file__': path})
    return 0


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
