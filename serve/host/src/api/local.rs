//! The host's local endpoint: where the owner tools of this computer call it (`kith_kernel::hostlocal`), instead of loopback TCP.
//!
//! The TCP port is open to every account on the machine, and another account can bind it while the host is down, so a tool that
//! sent the owner token there would hand it over. This endpoint is a socket (0600) in a folder (0700) of the serve data folder
//! on Unix and a named pipe with an owner-only access list on Windows; the client checks that it is ours before it writes.
//! What the host adds is that it serves the very same router: a request here goes through the same handlers as one over TCP,
//! with the same bearer token check (`http::authorized`), because the channel proves the server, not the caller (any process of
//! this account can open the socket; the token file is what the agents' guard keeps from them).
//!
//! Only `POST /api/<method>`, `POST /ingest/statusline` and the event stream `GET /events` are answered, one request to a
//! connection. The terminal stream stays on TCP with the apps' own clients (the TUI), and so do the browser-facing routes
//! (`/channel`, `/oauth/callback`).

use std::io::{Read, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{Context as _, Result};
use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use kith_kernel::hostlocal;
use tower::ServiceExt as _;

/// Connections served at once; one more is told the host is busy.
const MAX_CONNS: usize = 64;
const MAX_HEAD: usize = 64 * 1024;
const MAX_BODY: usize = 16 * 1024 * 1024;
/// How long a client gets to send its request. The answer is not limited: some commands take minutes.
const SEND_WAIT: Duration = Duration::from_secs(10);

#[cfg(unix)]
type Conn = std::os::unix::net::UnixStream;
#[cfg(not(unix))]
type Conn = kith_winsys::pipe::Stream;

/// The made endpoint. A crashed host leaves its socket behind, and the next [`bind`] removes it.
pub struct Endpoint {
    #[cfg(unix)]
    listener: std::os::unix::net::UnixListener,
    #[cfg(not(unix))]
    listener: kith_winsys::pipe::Listener,
    #[cfg_attr(not(unix), allow(dead_code))]
    path: PathBuf,
}

impl Endpoint {
    #[cfg_attr(not(unix), allow(clippy::unused_self))]
    pub fn close(&self) {
    pub fn close(&self) {
        #[cfg(unix)]
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Makes the endpoint of the serve data folder `data`. Done before the TCP port opens, so a client that finds the port finds
/// the endpoint too (a port without one is an older host).
#[cfg(unix)]
pub fn bind(data: &Path) -> Result<Endpoint> {
    use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _};
    let (dir, path) = (hostlocal::folder(data), hostlocal::endpoint(data));
    anyhow::ensure!(
        hostlocal::path_fits(&path),
        "{} is too long for a socket path; set KITH_SERVE_HOME to a shorter folder",
        path.display()
    );
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    // Fails when the folder is another account's: a folder that is not ours is never used.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("securing {}", dir.display()))?;
    // What a host that did not shut down cleanly left. The host lock means no other host of this data folder is running; a
    // file that is not a socket, or is not ours, is not touched.
    if let Ok(old) = std::fs::symlink_metadata(&path) {
        let ours = old.uid() == std::fs::metadata(&dir)?.uid();
        anyhow::ensure!(old.file_type().is_socket() && ours, "{} is in the way and is not this host's", path.display());
        std::fs::remove_file(&path)?;
    }
    let listener =
        std::os::unix::net::UnixListener::bind(&path).with_context(|| format!("binding {}", path.display()))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    Ok(Endpoint { listener, path })
}

/// The named pipe of the serve data folder: owner-only, and the first instance of its name, so nothing else already holds it.
#[cfg(not(unix))]
pub fn bind(data: &Path) -> Result<Endpoint> {
    let path = hostlocal::endpoint(data);
    let listener = kith_winsys::pipe::listen(&path.to_string_lossy())
        .with_context(|| format!("making the pipe {}", path.display()))?;
    Ok(Endpoint { listener, path })
}

impl Endpoint {
    fn accept(&self) -> std::io::Result<Conn> {
        #[cfg(unix)]
        {
            self.listener.accept().map(|(s, _)| s)
        }
        #[cfg(not(unix))]
        {
            self.listener.accept()
        }
    }
}

/// Answers requests on `endpoint` in the background with `router` (the one the TCP listener serves).
pub fn serve(endpoint: Arc<Endpoint>, router: Router) {
    let rt = tokio::runtime::Handle::current();
    let live = Arc::new(AtomicUsize::new(0));
    let spawned = std::thread::Builder::new().name("local-endpoint".into()).spawn(move || {
        loop {
            let mut conn = match endpoint.accept() {
                Ok(c) => c,
                Err(error) => {
                    tracing::warn!(%error, "local endpoint: accept failed");
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
            };
            if live.fetch_add(1, Ordering::SeqCst) >= MAX_CONNS {
                live.fetch_sub(1, Ordering::SeqCst);
                respond(&mut conn, StatusCode::SERVICE_UNAVAILABLE, br#"{"error":"the host is busy"}"#);
                continue;
            }
            let (router, rt, counted) = (router.clone(), rt.clone(), live.clone());
            let ran = std::thread::Builder::new().name("local-call".into()).spawn(move || {
                handle(&mut conn, &router, &rt);
                counted.fetch_sub(1, Ordering::SeqCst);
            });
            if ran.is_err() {
                // The connection went with the closure; the count is ours to give back.
                live.fetch_sub(1, Ordering::SeqCst);
            }
        }
    });
    if let Err(error) = spawned {
        tracing::warn!(%error, "local endpoint not started: owner tools cannot reach the host");
    }
}

fn limit_waits(c: &mut Conn) {
    #[cfg(unix)]
    {
        let _ = c.set_read_timeout(Some(SEND_WAIT));
        let _ = c.set_write_timeout(Some(SEND_WAIT));
    }
    #[cfg(not(unix))]
    {
        c.set_read_timeout(Some(SEND_WAIT));
        c.set_write_timeout(Some(SEND_WAIT));
    }
}

/// A call from a command of this install to the running host (`kith kernel`'s `hostlocal` on a blocking thread): `(2xx?, the
/// answer's JSON or null)`. `Err` is why the host could not be called: down, not this account's, or older than this command
/// (named, so it is restarted by the person and not mistaken for a host that is off). `data` is the serve data folder, `port`
/// is only knocked on to tell an older host from none.
pub async fn call(
    data: PathBuf,
    port: u16,
    token: String,
    method: &str,
    body: &serde_json::Value,
    timeout: Duration,
) -> Result<(bool, serde_json::Value), String> {
    let (path, body) = (format!("/api/{method}"), body.to_string());
    post_to(data, port, path, token, body, timeout).await
}

/// [`call`] for any path the endpoint serves.
pub async fn post_to(
    data: PathBuf,
    port: u16,
    path: String,
    token: String,
    body: String,
    timeout: Duration,
) -> Result<(bool, serde_json::Value), String> {
    tokio::task::spawn_blocking(move || {
        hostlocal::post(&data, &path, &token, &body, timeout)
            .map(|r| ((200..300).contains(&r.status), serde_json::from_slice(&r.body).unwrap_or_default()))
            .map_err(|e| match e {
                hostlocal::LocalErr::Down if hostlocal::status(&data, port) == hostlocal::Status::Older => {
                    format!("can't reach the Kith host: {}", hostlocal::OLDER_HOST)
                }
                e => format!("can't reach the Kith host: {e}"),
            })
    })
    .await
    .map_err(|e| format!("can't reach the Kith host: {e}"))?
}

/// The routes this endpoint answers: `POST /api/<method>`, the statusline's POST, and the event stream `GET /events` (it stays
/// open until the client leaves, which is how `kith group` reads the rooms).
fn served(method: &str, path: &str) -> bool {
    let route = path.split('?').next().unwrap_or_default();
    match method {
        "POST" => {
            route == "/ingest/statusline"
                || (route == path
                    && route.strip_prefix("/api/").is_some_and(|m| !m.is_empty() && !m.contains(['/', '#'])))
        }
        "GET" => route == "/events",
        _ => false,
    }
}

fn handle(conn: &mut Conn, router: &Router, rt: &tokio::runtime::Handle) {
    limit_waits(conn);
    let request = match read_request(conn) {
        Ok(r) => r,
        Err(status) => return respond(conn, status, br#"{"error":"bad request"}"#),
    };
    if !served(&request.method, &request.path) {
        return respond(conn, StatusCode::NOT_FOUND, br#"{"error":"not served here"}"#);
    }
    let mut builder = Request::builder()
        .method(request.method.as_str())
        .uri(&request.path)
        // The handlers check the peer is loopback: this endpoint is more local than that.
        .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))));
    for (name, value) in &request.headers {
        builder = builder.header(name.as_str(), value.as_str());
    }
    let Ok(req) = builder.body(Body::from(request.body)) else {
        return respond(conn, StatusCode::BAD_REQUEST, br#"{"error":"bad request"}"#);
    };
    if request.method == "GET" {
        return rt.block_on(stream(conn, router.clone(), req));
    }
    let answered = rt.block_on(async {
        let response = router.clone().oneshot(req).await.ok()?;
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), MAX_BODY).await.ok()?;
        Some((status, body))
    });
    match answered {
        Some((status, body)) => respond(conn, status, &body),
        None => respond(conn, StatusCode::INTERNAL_SERVER_ERROR, br#"{"error":"no answer"}"#),
    }
}

/// An event stream: the head, then each event as the router makes it, until the client goes (a write fails) or the stream ends.
async fn stream<W: Write>(conn: &mut W, router: Router, req: Request<Body>) {
    use futures::StreamExt as _;
    let Ok(response) = router.oneshot(req).await;
    let status = response.status();
    if !status.is_success() {
        let body = axum::body::to_bytes(response.into_body(), MAX_BODY).await.unwrap_or_default();
        return respond(conn, status, &body);
    }
    let head =
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n";
    if conn.write_all(head.as_bytes()).and_then(|()| conn.flush()).is_err() {
        return;
    }
    let mut events = response.into_body().into_data_stream();
    while let Some(Ok(chunk)) = events.next().await {
        if conn.write_all(&chunk).and_then(|()| conn.flush()).is_err() {
            return;
        }
    }
}

struct Parsed {
    method: String,
    path: String,
    /// Only the headers the handlers read.
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

fn read_request<S: Read>(s: &mut S) -> Result<Parsed, StatusCode> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        if buf.len() > MAX_HEAD {
            return Err(StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE);
        }
        match s.read(&mut chunk) {
            Ok(0) | Err(_) => return Err(StatusCode::BAD_REQUEST),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    };
    let head = std::str::from_utf8(&buf[..head_end]).map_err(|_| StatusCode::BAD_REQUEST)?;
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or_default().split_whitespace();
    let (method, path) = (first.next().unwrap_or_default().to_owned(), first.next().unwrap_or_default().to_owned());
    let mut headers = Vec::new();
    let mut length = 0usize;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else { return Err(StatusCode::BAD_REQUEST) };
        let (name, value) = (name.trim().to_ascii_lowercase(), value.trim().to_owned());
        match name.as_str() {
            "content-length" => length = value.parse().map_err(|_| StatusCode::BAD_REQUEST)?,
            "transfer-encoding" => return Err(StatusCode::NOT_IMPLEMENTED),
            "authorization" | "content-type" => headers.push((name, value)),
            _ => {}
        }
    }
    if length > MAX_BODY {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < length {
        match s.read(&mut chunk) {
            Ok(0) | Err(_) => return Err(StatusCode::BAD_REQUEST),
            Ok(n) => body.extend_from_slice(&chunk[..n]),
        }
    }
    body.truncate(length);
    Ok(Parsed { method, path, headers, body })
}

fn respond<S: Write>(s: &mut S, status: StatusCode, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        status.as_u16(),
        status.canonical_reason().unwrap_or(""),
        body.len()
    );
    let _ = s.write_all(head.as_bytes()).and_then(|()| s.write_all(body)).and_then(|()| s.flush());
}

#[cfg(all(test, unix))]
mod tests;
