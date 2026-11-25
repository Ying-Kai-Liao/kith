//! How an owner tool on this computer calls the host: over a channel that proves who serves it, never over loopback TCP.
//!
//! The host's TCP port is open to any account on the machine, and a process of another account can bind it while the host is
//! down. A tool that sent the owner's bearer token there would hand it over. So the host also listens on a local endpoint that
//! only this account can make, and the owner tools go there:
//! - Unix: `<serve data folder>/owner/host.sock`, a socket (0600) in a folder (0700). The client checks that both are ours and
//!   that no other account can open them before it writes anything, so a squatter cannot be at that path.
//! - Windows: a named pipe with an owner-only access list, opened with `kith_winsys::pipe::connect`, which checks that the
//!   server runs as this account and that the pipe is owned by it.
//!
//! The endpoint belongs to one serve data folder (`KITH_SERVE_DIR`), because a second host (a worktree's sandbox host) has a
//! folder of its own beside the main one. The request is the host's own HTTP, `POST /api/<method>` with the bearer token and
//! `Connection: close`, so the host's checks are the same on both transports; only the proof of the server is new.
//!
//! A host that is down is an error ([`LocalErr::Down`]), and so is one that is not ours ([`LocalErr::Untrusted`]): there is no
//! fall back to TCP. [`status`] tells a host that is down from an older one that has no endpoint yet.

use std::fmt;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The folder of the socket, inside the serve data folder.
const DIR: &str = "owner";
const SOCKET: &str = "host.sock";
/// Longest path `sockaddr_un` takes on macOS (104 with the NUL); Linux has 108.
const MAX_SOCKET_PATH: usize = 103;
/// A head or a body larger than this is no answer of the host's.
const MAX_HEAD: usize = 64 * 1024;
const MAX_BODY: usize = 32 * 1024 * 1024;

/// Why the host could not be called. Nothing was sent unless it is `TimedOut` or `Io`.
#[derive(Debug)]
pub enum LocalErr {
    /// Nobody listens: the host is not running (or, on Windows, has no pipe yet).
    Down,
    /// Something listens, but this client cannot vouch for it: another account's file or pipe, wrong permissions.
    Untrusted(String),
    TimedOut,
    Io(io::Error),
}

impl fmt::Display for LocalErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Down => f.write_str("the Kith host is not running"),
            Self::Untrusted(why) => write!(
                f,
                "the host's local endpoint is not this account's ({why}); nothing was sent"
            ),
            Self::TimedOut => f.write_str("the Kith host did not answer in time"),
            Self::Io(e) => write!(f, "the Kith host could not be reached ({e})"),
        }
    }
}

impl LocalErr {
    fn io(e: &io::Error) -> Self {
        match e.kind() {
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => Self::TimedOut,
            _ => Self::Io(io::Error::new(e.kind(), e.to_string())),
        }
    }
}

/// An HTTP answer of the host.
#[derive(Debug)]
pub struct Reply {
    pub status: u16,
    pub body: Vec<u8>,
}

/// Where the host of this serve data folder listens for local owner tools.
#[cfg(unix)]
pub fn endpoint(data: &Path) -> PathBuf {
    data.join(DIR).join(SOCKET)
}

/// Where the host of this serve data folder listens for local owner tools: pipe names are one namespace for the machine, so the
/// folder is hashed into the name (as the core does for its own pipes).
#[cfg(not(unix))]
pub fn endpoint(data: &Path) -> PathBuf {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for b in data.to_string_lossy().to_lowercase().bytes() {
        hash = (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
    }
    PathBuf::from(format!(r"\\.\pipe\kith-serve-owner-{hash:016x}"))
}

/// The folder of the socket, which the host makes (0700) before it binds.
pub fn folder(data: &Path) -> PathBuf {
    data.join(DIR)
}

/// The host cannot bind a path `sockaddr_un` cannot hold; it says so by this.
pub fn path_fits(path: &Path) -> bool {
    cfg!(not(unix)) || path.as_os_str().len() <= MAX_SOCKET_PATH
}

#[cfg(unix)]
type Conn = std::os::unix::net::UnixStream;
#[cfg(not(unix))]
type Conn = kith_winsys::pipe::Stream;

#[cfg(unix)]
fn connect(data: &Path, timeout: Duration) -> Result<Conn, LocalErr> {
    use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
    let me = crate::threads::os::uid();
    let refuse = |what: &str| LocalErr::Untrusted(what.to_owned());
    let check = |path: &Path, want_dir: bool| -> Result<(), LocalErr> {
        // `symlink_metadata`: a link is never followed to a place someone else chose.
        let m = std::fs::symlink_metadata(path).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => LocalErr::Down,
            _ => LocalErr::io(&e),
        })?;
        let kind = if want_dir {
            m.is_dir()
        } else {
            m.file_type().is_socket()
        };
        if !kind {
            return Err(refuse(if want_dir {
                "the folder is not a folder"
            } else {
                "the file is not a socket"
            }));
        }
        if m.uid() != me {
            return Err(refuse("it belongs to another account"));
        }
        if m.mode() & 0o077 != 0 {
            return Err(refuse("other accounts can open it"));
        }
        Ok(())
    };
    check(&folder(data), true)?;
    let sock = endpoint(data);
    check(&sock, false)?;
    let s = Conn::connect(&sock).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => LocalErr::Down,
        _ => LocalErr::io(&e),
    })?;
    s.set_read_timeout(Some(timeout))
        .map_err(|e| LocalErr::io(&e))?;
    s.set_write_timeout(Some(timeout))
        .map_err(|e| LocalErr::io(&e))?;
    Ok(s)
}

#[cfg(not(unix))]
fn connect(data: &Path, timeout: Duration) -> Result<Conn, LocalErr> {
    let mut s = kith_winsys::pipe::connect(&endpoint(data).to_string_lossy(), timeout).map_err(
        |e| match e.kind() {
            io::ErrorKind::NotFound => LocalErr::Down,
            io::ErrorKind::PermissionDenied => LocalErr::Untrusted(e.to_string()),
            _ => LocalErr::io(&e),
        },
    )?;
    s.set_read_timeout(Some(timeout));
    s.set_write_timeout(Some(timeout));
    Ok(s)
}

/// One request to the host and its answer: `path` is `/api/<method>` (or `/ingest/statusline`), `token` the serve token,
/// `body` the JSON. `timeout` bounds each read and write.
pub fn post(
    data: &Path,
    path: &str,
    token: &str,
    body: &str,
    timeout: Duration,
) -> Result<Reply, LocalErr> {
    // A header value with a line break would split the request.
    if token.contains(['\r', '\n']) || path.contains(['\r', '\n', ' ']) {
        return Err(LocalErr::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "bad request line or token",
        )));
    }
    let mut s = connect(data, timeout)?;
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: localhost\r\nAccept-Encoding: identity\r\nAuthorization: Bearer {token}\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).map_err(|e| LocalErr::io(&e))?;
    read_reply(&mut s).ok_or(LocalErr::Io(io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "no answer",
    )))
}

/// What a local tool can say of the host.
#[derive(Debug, PartialEq, Eq)]
pub enum Status {
    /// The local endpoint takes a connection (a host that answers no to a request is up).
    Up,
    /// No endpoint, but the port takes a connection: a host older than this tool, which must be restarted to be reached.
    /// Nothing is sent to it.
    Older,
    Down,
}

/// Whether the host of `data` is reachable by [`post`]. The port is only knocked on (no byte is sent), to tell a host that is
/// down from one that has no local endpoint yet.
pub fn status(data: &Path, port: u16) -> Status {
    match connect(data, Duration::from_secs(2)) {
        Ok(_) => return Status::Up,
        // A refusal to vouch is not a host that is down: restarting it would not help, and the caller says why.
        Err(LocalErr::Untrusted(_)) => return Status::Down,
        Err(_) => {}
    }
    let knock = std::net::TcpStream::connect_timeout(
        &([127, 0, 0, 1], port).into(),
        Duration::from_secs(2),
    );
    if knock.is_ok() {
        Status::Older
    } else {
        Status::Down
    }
}

/// What a caller says when [`status`] is [`Status::Older`].
pub const OLDER_HOST: &str =
    "the Kith host is older than this command and has no local endpoint: restart it";

/// `(status, body)` of an HTTP/1.x answer: by Content-Length, chunked, or to the end of the stream.
fn read_reply<R: Read>(s: &mut R) -> Option<Reply> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        if buf.len() > MAX_HEAD {
            return None;
        }
        let n = s.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let status: u16 = lines.next()?.split_whitespace().nth(1)?.parse().ok()?;
    let header = |name: &str| {
        lines.clone().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim()
                .eq_ignore_ascii_case(name)
                .then(|| v.trim().to_owned())
        })
    };
    let length: Option<usize> = header("content-length").and_then(|v| v.parse().ok());
    let chunked = header("transfer-encoding").is_some_and(|v| v.to_lowercase().contains("chunked"));
    let mut body = buf[head_end + 4..].to_vec();
    loop {
        if chunked {
            if let Some(out) = dechunk(&body) {
                return Some(Reply { status, body: out });
            }
        } else if length.is_some_and(|l| body.len() >= l) {
            body.truncate(length.unwrap_or(0));
            return Some(Reply { status, body });
        }
        if body.len() > MAX_BODY {
            return None;
        }
        let n = s.read(&mut chunk).ok()?;
        if n == 0 {
            // A short body is no answer; no length and not chunked means "to the end".
            return (!chunked && length.is_none()).then_some(Reply { status, body });
        }
        body.extend_from_slice(&chunk[..n]);
    }
}

/// The payload of a complete chunked body, `None` while chunks are still missing.
fn dechunk(raw: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut at = 0;
    loop {
        let eol = raw.get(at..)?.windows(2).position(|w| w == b"\r\n")? + at;
        let size = std::str::from_utf8(&raw[at..eol]).ok()?;
        let size = usize::from_str_radix(size.split(';').next()?.trim(), 16).ok()?;
        at = eol + 2;
        if size == 0 {
            return Some(out);
        }
        out.extend_from_slice(raw.get(at..at + size)?);
        at += size + 2;
        if at > raw.len() {
            return None;
        }
    }
}

#[cfg(all(test, unix))]
mod tests;
#[cfg(all(test, windows))]
mod win_tests;

/// What a test needs to stand in for the host: its folder and socket as the host makes them.
#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used)]
pub(crate) mod testkit {
    use std::io::{Read as _, Write as _};
    use std::os::unix::fs::PermissionsExt as _;
    use std::os::unix::net::UnixListener;
    use std::path::{Path, PathBuf};

    /// A fresh serve data folder with a token (`tok`), short because a socket path is.
    pub fn data_dir(tag: &str) -> PathBuf {
        let dir = PathBuf::from(format!("/tmp/kh-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("token"), "tok\n").unwrap();
        dir
    }

    /// The host's socket as it makes it: the folder 0700, the socket 0600.
    pub fn listen(data: &Path) -> UnixListener {
        let dir = super::folder(data);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let l = UnixListener::bind(super::endpoint(data)).unwrap();
        std::fs::set_permissions(
            super::endpoint(data),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        l
    }

    /// Answers one request with `status` and `body`; the request as it came.
    pub fn serve_once(
        data: &Path,
        status: &str,
        body: &'static str,
    ) -> std::thread::JoinHandle<String> {
        let l = listen(data);
        let status = status.to_owned();
        std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            let mut got = vec![0u8; 4096];
            let n = c.read(&mut got).unwrap();
            let resp = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            c.write_all(resp.as_bytes()).unwrap();
            // The socket goes with the host.
            let _ = std::fs::remove_file(l.local_addr().unwrap().as_pathname().unwrap());
            String::from_utf8_lossy(&got[..n]).into_owned()
        })
    }
}
