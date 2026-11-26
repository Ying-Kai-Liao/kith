#!/usr/bin/env python3
"""The origin of a run, asked of the host: the call key and (with --outbox) the run outbox of the agent's app chat
with the person the run is for. `kith-sched` asks for each scheduled run, `bin/kith` for an owner's terminal run.

    kith_runenv.py AGENT [--person ID] [--outbox]     prints the host's JSON answer, {"chat": ..., "env": {...}}
                                                       exit 1, nothing printed: the host can't be reached

`chat` is null when the person has no app chat with the agent: there is no origin to stamp. The host makes the key
and the outbox for exactly the agent and person named here, so the caller names the person its run is for and nothing
the run itself sets can change it."""
import json, sys


def fetch(agent, person=None, outbox=False):
    """The host's `runEnv` answer, or None when it can't be reached or doesn't answer (an older host, a refusal)."""
    import kith_hostlocal  # the host's local endpoint, never loopback TCP
    out, _ = kith_hostlocal.call('runEnv', {'agent': agent, 'person': person, 'outbox': outbox}, 4)
    return out if isinstance(out, dict) and isinstance(out.get('env'), dict) else None


if __name__ == '__main__':
    args = sys.argv[1:]
    person = args[args.index('--person') + 1] if '--person' in args else None
    got = fetch(args[0], person or None, '--outbox' in args)
    if got is None:
        sys.exit(1)
    print(json.dumps(got))
