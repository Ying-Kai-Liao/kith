#!/usr/bin/env python3
"""kith engines [--json]: the engines this kernel can run an agent on, and whether each is installed.

This is the Python side of the core's `engines` module (core/kernel/src/engines.rs): it knows only what
adapters/harness.py knows (its HARNESSES, and claude). The Rust core lists the host's engines too and says more per
engine; the differential harness lists that difference as intended (kernel/tests/difftest/intended.json).
`install` and `login` print a command from the core's table, so they need a kith program with the engines module.
"""
import json, os, shutil, sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'adapters'))
import harness  # noqa: E402

IDS = ('claude',) + harness.HARNESSES


def main(argv):
    if argv[:1] in (['install'], ['login']):
        print('kith: `kith engines %s` needs the kith program (kith serve install); this one has no engines table' % argv[0],
              file=sys.stderr)
        return 1
    rows = [{'id': i, 'installed': shutil.which(i) is not None} for i in IDS]
    if '--json' in argv:
        print(json.dumps({'engines': rows}))
    else:
        for r in rows:
            print('%-12s %s' % (r['id'], 'installed' if r['installed'] else 'not installed'))
    return 0


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
