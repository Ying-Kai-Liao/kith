#!/usr/bin/env python3
"""The origin of a run, asked of the host: the call key and (with --outbox) the run outbox of the agent's app chat
with the person the run is for. `kith-sched` asks for each scheduled run, `bin/kith` for an owner's terminal run.

    kith_runenv.py AGENT [--person ID] [--outbox]     prints the host's JSON answer, {"chat": ..., "env": {...}}
                                                       exit 1, nothing printed: the host can't be reached

`chat` is null when the person has no app chat with the agent: there is no origin to stamp. The host makes the key
and the outbox for exactly the agent and person named here, so the caller names the person its run is for and nothing
the run itself sets can change it."""
import json, os, subprocess, sys, urllib.request
from pathlib import Path


def host_endpoint():
    """(data dir, port) of this Mac's host: the env overrides, else what `kith-serve endpoint` says."""
    data, port = os.environ.get('KITH_SERVE_DIR'), os.environ.get('KITH_SERVE_PORT')
    if not (data and port):
        try:
            d, p = subprocess.run([str(Path(__file__).resolve().parent.parent / 'bin' / 'kith-serve'), 'endpoint'],
                                  capture_output=True, text=True, timeout=4, check=True).stdout.split()
        except Exception:  # noqa: BLE001 — fall back to the dev defaults
            d, p = str(Path.home() / '.kith' / 'serve'), '19224'
        data, port = data or d, port or p
    return Path(data), port


def fetch(agent, person=None, outbox=False):
    """The host's `runEnv` answer, or None when it can't be reached or doesn't answer (an older host, a refusal)."""
    try:
        data, port = host_endpoint()
        token = (data / 'token').read_text().strip()
        body = {'agent': agent, 'person': person, 'outbox': outbox}
        req = urllib.request.Request(f'http://127.0.0.1:{port}/api/runEnv', json.dumps(body).encode(),
                                     {'Authorization': f'Bearer {token}', 'Content-Type': 'application/json'})
        with urllib.request.urlopen(req, timeout=4) as r:
            out = json.load(r)
    except Exception:  # noqa: BLE001 — host down: no key, and the caller goes on as it did
        return None
    return out if isinstance(out, dict) and isinstance(out.get('env'), dict) else None


if __name__ == '__main__':
    args = sys.argv[1:]
    person = args[args.index('--person') + 1] if '--person' in args else None
    got = fetch(args[0], person or None, '--outbox' in args)
    if got is None:
        sys.exit(1)
    print(json.dumps(got))
