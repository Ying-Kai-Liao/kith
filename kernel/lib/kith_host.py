"""kith_host — the running host's local API for the commands that ask it (kith group/bot, kith invite): the owner token
over the host's local endpoint. The bin scripts that call it: kith-group, kith-people. (kith-auth keeps its own copy of the two functions.)"""
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
    """(answer, None), or (None, why it couldn't be reached): over the host's local endpoint (kith_hostlocal), never TCP."""
    import kith_hostlocal
    return kith_hostlocal.call(method, body, 30)
