"""kith_host — the running host's local API for the commands that ask it (kith group/bot, kith invite): the owner token
over loopback. The bin scripts that call it: kith-group, kith-people. (kith-auth keeps its own copy of the two functions.)"""
import json
import os
import subprocess
from pathlib import Path


def host_endpoint():
    """(data dir, port) of this install's kith serve. kith-serve owns the rule (~/.kith/serve on 19224); the
    env overrides win."""
    data, port = os.environ.get('KITH_SERVE_DIR'), os.environ.get('KITH_SERVE_PORT')
    if not (data and port):
        try:
            d, p = subprocess.run([str(Path(__file__).resolve().parent.parent / 'bin' / 'kith-serve'), 'endpoint'],
                                  capture_output=True, text=True, timeout=4, check=True).stdout.split()
        except Exception:  # noqa: BLE001 — fall back to the defaults
            d, p = str(Path.home() / '.kith' / 'serve'), '19224'
        data, port = data or d, port or p
    return Path(data), port


def host_call(method, body):
    """(answer, None), or (None, why it couldn't be reached)."""
    import urllib.error
    import urllib.request
    try:
        data, port = host_endpoint()
        token = (data / 'token').read_text().strip()
        req = urllib.request.Request(f'http://127.0.0.1:{port}/api/{method}', json.dumps(body).encode(),
                                     {'Authorization': f'Bearer {token}', 'Content-Type': 'application/json'})
        with urllib.request.urlopen(req, timeout=30) as r:
            return json.load(r), None
    except urllib.error.HTTPError as e:
        try:
            why = json.load(e).get('error')
            return None, why if isinstance(why, str) and why else f'kith serve answered {e.code}'
        except (ValueError, AttributeError):
            return None, f'kith serve answered {e.code}'
    except Exception as e:  # noqa: BLE001
        return None, f'kith serve is not reachable ({e.__class__.__name__})'
