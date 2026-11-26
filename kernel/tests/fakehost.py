"""fakehost — a stand-in for the Kith host's local endpoint, for the kernel checks.

The commands under test call the host over `<serve data folder>/owner/host.sock` (kernel/lib/kith_hostlocal.py), never loopback
TCP. A check's fake host is a normal http.server handler class served on that socket:

    import fakehost
    s = fakehost.UnixHTTPServer(fakehost.sock_path(data_dir), H)      # or ThreadingUnixHTTPServer
    s.serve_forever()

The folder is made 0700 and the socket 0600, as the host makes them (the client refuses anything else). Put this folder on
PYTHONPATH to import it.
"""
import http.server
import os
import socket
import socketserver


def sock_path(data):
    return os.path.join(data, 'owner', 'host.sock')


class UnixHTTPServer(http.server.HTTPServer):
    address_family = socket.AF_UNIX

    def server_bind(self):
        folder = os.path.dirname(self.server_address)
        os.makedirs(folder, exist_ok=True)
        os.chmod(folder, 0o700)
        if os.path.exists(self.server_address):
            os.unlink(self.server_address)
        self.socket.bind(self.server_address)
        os.chmod(self.server_address, 0o600)
        self.server_name, self.server_port = 'fakehost', 0

    def get_request(self):
        conn, _ = self.socket.accept()
        return conn, ('local', 0)


class ThreadingUnixHTTPServer(socketserver.ThreadingMixIn, UnixHTTPServer):
    daemon_threads = True
