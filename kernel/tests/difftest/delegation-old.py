#!/usr/bin/env python3
"""The Python side of the `delegation` difftest cases: `kith kernel delegation ...` asked of kernel/bin/kith-ask.

kith-ask has no verb that only decides, so this runs its own functions (the same ones `kith ask` runs) and prints what
the Rust verb prints:

  limits          its three constants and the default cap
  cap             `kith ask --cap` with no argument
  record ID       read_reg(ID) as JSON; exit 1 when there is none
  parked ID       true or false
  check [--id ID] [--crossing]   open_record(ID, ...) as `kith ask` calls it, which refuses (exit 4) or writes the
                  record; the record is printed and then removed, so the case's files stay as the Rust side leaves them
"""
import importlib.util, json, os, sys
from importlib.machinery import SourceFileLoader

here = os.path.dirname(os.path.realpath(__file__))
path = os.path.join(here, '..', '..', 'bin', 'kith-ask')
spec = importlib.util.spec_from_loader('kith_ask', SourceFileLoader('kith_ask', path))
ask = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ask)


def main(argv):
    verb, rest = (argv[0], argv[1:]) if argv else ('', [])
    if verb == 'limits':
        print(json.dumps({'max_local_hops': ask.MAX_LOCAL_HOPS, 'max_cross_hops': ask.MAX_CROSS_HOPS,
                          'refused_exit': ask.REFUSED, 'default_cap_usd': float(ask.kith_settings.CHAIN_CAP_USD)}))
    elif verb == 'cap':
        return ask.cap_cmd([])
    elif verb == 'record':
        reg = ask.read_reg(rest[0]) if ask.ID.fullmatch(rest[0]) else None   # a bare read takes no lock
        if reg is None:
            return 1
        print(json.dumps(reg))
    elif verb == 'parked':
        print('true' if ask.ID.fullmatch(rest[0]) and ask.box(rest[0], 'parked').exists() else 'false')
    elif verb == 'check':
        mid = 'ask-check'
        if '--id' in rest:
            mid = rest[rest.index('--id') + 1]
        reg = ask.open_record(mid, 'probe', crossing='--crossing' in rest)   # exits 4 with the refusal on stderr
        with ask.box_locked():
            ask.box(mid, 'reg').unlink()
        print(json.dumps({'chain': reg['chain'], 'hop': reg['hop'], 'cross_hops': reg['cross_hops'],
                          'crossed': reg['crossed'], 'ask': reg['reply_to']['ask'], 'cost_base': float(reg['cost_base'])}))
    else:
        sys.exit('usage: delegation-old.py limits|cap|record ID|parked ID|check [--id ID] [--crossing]')
    return 0


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
