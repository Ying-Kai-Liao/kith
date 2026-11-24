"""A guest run's way to the kernel tools its sandbox keeps it from running itself: ask the core over its run socket.

An invited person's (or a bot guest's) run holds KITH_RUN_TOKEN and KITH_CORE_RUN_SOCKET (`<machine>/core/run.sock`). The core
decides who is asking from the token alone (core/runtime/src/runs.rs), runs the tool as that identity and sends back its output.
KITH_RUN_SOCKET is the host's, for the cards; the tools here never go there. A guest is the token alone: with no core
socket it gets a message and a non-zero code, never the owner's path. Nothing here reads a Kith folder. Standard library
only, so it imports before anything that looks at the disk.
"""
import json
import os
import socket
import sys


def guest_run():
    """The (token, core socket path or '') of a guest run, else None (an owner's run or shell has no token)."""
    token = os.environ.get('KITH_RUN_TOKEN')
    return (token, os.environ.get('KITH_CORE_RUN_SOCKET') or '') if token else None


def ask(tool, method, args):
    """Send one request, print the core's stdout and stderr, return its exit code. A core that is missing, restarted,
    too old to know the method or says no is a message and a non-zero code, not a traceback."""
    token, path = guest_run()
    if not path:
        print('%s: the Kith core could not be reached from this run' % tool, file=sys.stderr)
        return 1
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
            s.settimeout(40)
            s.connect(path)
            s.sendall(json.dumps({'token': token, 'method': method, 'args': args}).encode() + b'\n')
            s.shutdown(socket.SHUT_WR)
            data = b''
            while True:
                chunk = s.recv(65536)
                if not chunk:
                    break
                data += chunk
                if len(data) > 1 << 20:
                    raise ValueError('reply too large')
        reply = json.loads(data.decode())
    except (OSError, ValueError):
        print('%s: the Kith core could not be reached from this run' % tool, file=sys.stderr)
        return 1
    if not reply.get('ok'):
        print('%s: %s' % (tool, reply.get('error') or 'the core refused'), file=sys.stderr)
        return 2
    sys.stdout.write(reply.get('stdout', ''))
    sys.stderr.write(reply.get('stderr', ''))
    return int(reply.get('code', 0))
