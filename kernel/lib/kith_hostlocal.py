"""kith_hostlocal — how a command of this install calls the running host: over its local endpoint, never loopback TCP.

The host's TCP port is open to every account on the machine, and another account can bind it while the host is down, so a
command that sent the owner token there would hand it over. The host also listens on `<serve data folder>/owner/host.sock`, a
socket (0600) in a folder (0700); this checks that both are ours before it writes anything. Rust twin:
core/kernel/src/hostlocal.rs (same checks, same wire: `POST /api/<method>` with the bearer token). Nothing falls back to TCP: a
host that is down, or not ours, is an error that says so, and one with a port but no endpoint is an older host to restart.

    request(path, body, timeout)   (status, bytes), or raises HostUnreachable(why)
    call(method, body, timeout)    (answer, None), or (None, why): the 2xx JSON, or what the host said, or why not reached
"""
import http.client
import json
import os
import socket
import stat
import subprocess
from pathlib import Path


OLDER = 'the Kith host is older than this command and has no local endpoint: restart it'


class HostUnreachable(Exception):
    """Nothing was sent, or no answer came. `down` is true when nobody listens (the host is not running). `name` is what
    follows "kith serve is not reachable": Python's own name for a host that is off (URLError), so a script's wording and the
    Rust kernel's agree, else the reason."""

    def __init__(self, why, down=False, name='URLError'):
        super().__init__(why)
        self.down = down
        self.name = name


def host_endpoint():
    """(data dir, port) of this install's kith serve. kith-serve owns the rule (~/.kith/serve on 19224); the env wins."""
    data, port = os.environ.get('KITH_SERVE_DIR'), os.environ.get('KITH_SERVE_PORT')
    if not (data and port):
        try:
            d, p = subprocess.run([str(Path(__file__).resolve().parent.parent / 'bin' / 'kith-serve'), 'endpoint'],
                                  capture_output=True, text=True, timeout=4, check=True).stdout.split()
        except Exception:  # noqa: BLE001 — fall back to the defaults
            d, p = str(Path.home() / '.kith' / 'serve'), '19224'
        data, port = data or d, port or p
    return Path(data), port


def _connect(data, timeout):
    if os.name != 'posix':
        raise HostUnreachable('this command does not reach the host on Windows yet', name='not on Windows yet')
    me = os.geteuid()
    folder, sock = Path(data) / 'owner', Path(data) / 'owner' / 'host.sock'
    for path, is_folder in ((folder, True), (sock, False)):
        try:
            st = os.lstat(path)  # lstat: a link is never followed to a place someone else chose
        except FileNotFoundError:
            raise HostUnreachable('the Kith host is not running', down=True) from None
        kind = stat.S_ISDIR(st.st_mode) if is_folder else stat.S_ISSOCK(st.st_mode)
        if not kind or st.st_uid != me or st.st_mode & 0o077:
            raise HostUnreachable(f"the host's local endpoint ({path}) is not this account's; nothing was sent",
                                  name="the host's local endpoint is not this account's")
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(timeout)
    try:
        s.connect(str(sock))
    except (FileNotFoundError, ConnectionRefusedError):
        s.close()
        raise HostUnreachable('the Kith host is not running', down=True) from None
    except OSError as e:
        s.close()
        raise HostUnreachable(f'the Kith host could not be reached ({e.__class__.__name__})') from None
    return s


class _Conn(http.client.HTTPConnection):
    def __init__(self, data, timeout):
        super().__init__('localhost', timeout=timeout)
        self._data = data

    def connect(self):
        self.sock = _connect(self._data, self.timeout)


def older_host(port):
    """The port takes a connection but there is no local endpoint: a host older than this command. Nothing is sent."""
    try:
        with socket.create_connection(('127.0.0.1', int(port)), timeout=2):
            return True
    except (OSError, ValueError):
        return False


def request(path, body, timeout=30):
    """One POST to the host with the serve token. (status, response bytes); raises HostUnreachable."""
    data, port = host_endpoint()
    try:
        token = (data / 'token').read_text().strip()
    except OSError as e:
        raise HostUnreachable(f'the Kith host is not installed here ({e.__class__.__name__})', down=True,
                              name=e.__class__.__name__) from None
    if '\r' in token or '\n' in token:
        raise HostUnreachable('the serve token is not a token')
    conn = _Conn(data, timeout)
    try:
        conn.request('POST', path, json.dumps(body).encode(),
                     {'Authorization': f'Bearer {token}', 'Content-Type': 'application/json'})
        r = conn.getresponse()
        return r.status, r.read()
    except HostUnreachable as e:
        if e.down and older_host(port):
            raise HostUnreachable(OLDER, name=OLDER) from None
        raise
    except (OSError, http.client.HTTPException) as e:
        raise HostUnreachable(f'the Kith host did not answer ({e.__class__.__name__})') from None
    finally:
        conn.close()


def stream(path, timeout=30):
    """GET `path` (an event stream, /events?since=0): the open response, whose readline() ends when the host closes it or
    nothing comes within `timeout`. The caller closes it. Raises HostUnreachable."""
    data, port = host_endpoint()
    try:
        token = (data / 'token').read_text().strip()
    except OSError as e:
        raise HostUnreachable(f'the Kith host is not installed here ({e.__class__.__name__})', down=True,
                              name=e.__class__.__name__) from None
    if '\r' in token or '\n' in token:
        raise HostUnreachable('the serve token is not a token')
    conn = _Conn(data, timeout)
    try:
        conn.request('GET', path, headers={'Authorization': f'Bearer {token}'})
        r = conn.getresponse()
    except HostUnreachable as e:
        conn.close()
        if e.down and older_host(port):
            raise HostUnreachable(OLDER, name=OLDER) from None
        raise
    except (OSError, http.client.HTTPException) as e:
        conn.close()
        raise HostUnreachable(f'the Kith host did not answer ({e.__class__.__name__})') from None
    if r.status != 200:
        conn.close()
        raise HostUnreachable(f'the Kith host answered {r.status}', name=f'the host answered {r.status}')
    return r


def call(method, body, timeout=30):
    """(answer, None), or (None, why it couldn't be had): the host's own `error` when it refused."""
    try:
        status, payload = request(f'/api/{method}', body, timeout)
    except HostUnreachable as e:
        return None, f'kith serve is not reachable ({e.name})'
    if 200 <= status < 300:
        try:
            return json.loads(payload), None
        except ValueError:
            return None, 'the Kith host answered something that is not JSON'
    try:
        why = json.loads(payload).get('error')
    except (ValueError, AttributeError):
        why = None
    return None, why if isinstance(why, str) and why else f'kith serve answered {status}'


def up():
    """The local endpoint takes a connection (a host that answers no to a request is up)."""
    try:
        _connect(host_endpoint()[0], 2).close()
        return True
    except HostUnreachable:
        return False
