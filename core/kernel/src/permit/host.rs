//! Reaching the host: the run socket (a JSON line each way, `call_host`) and its local API over loopback (`host_call`,
//! with the serve token), the host's port (`endpoint`, `host_up`) and its launchd job (`restart_host`).

use super::Env;
use serde_json::{Map, Value};
use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long a restarted host gets to open its port before the ask token is asked for again.
pub const HOST_START_S: u64 = 15;

/// `(token, socket path)` of this run, the guest's pair first.
pub fn credentials(env: &Env) -> Option<(String, String)> {
    [
        ("KITH_RUN_TOKEN", "KITH_RUN_SOCKET"),
        ("KITH_ASK_TOKEN", "KITH_ASK_SOCKET"),
    ]
    .into_iter()
    .find_map(|(t, s)| Some((env.get(t)?.to_owned(), env.get(s)?.to_owned())))
}

/// One request to the host over the run socket: the reply when it is a JSON object, `None` when there is no token, no
/// socket or no answer.
pub fn call_host(
    env: &Env,
    method: &str,
    body: &Map<String, Value>,
    timeout: Duration,
) -> Option<Map<String, Value>> {
    let (token, path) = credentials(env)?;
    let mut msg = body.clone();
    msg.insert("token".to_owned(), Value::String(token));
    msg.insert("method".to_owned(), Value::String(method.to_owned()));
    let mut line = crate::secrets::pyjson::dumps(&Value::Object(msg)).into_bytes();
    line.push(b'\n');
    let mut s = UnixStream::connect(&path).ok()?;
    s.set_read_timeout(Some(timeout)).ok()?;
    s.set_write_timeout(Some(timeout)).ok()?;
    s.write_all(&line).ok()?;
    let mut buf = Vec::new();
    let mut chunk = vec![0u8; 65536];
    while !buf.ends_with(b"\n") {
        let n = s.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    match serde_json::from_slice::<Value>(&buf).ok()? {
        Value::Object(m) => Some(m),
        _ => None,
    }
}

/// `(data dir, port)` of this Mac's host: `KITH_SERVE_DIR` and `KITH_SERVE_PORT`, else what `kith-serve endpoint` says.
pub fn endpoint(env: &Env, kernel: &Path) -> Option<(String, String)> {
    let (data, port) = (env.get("KITH_SERVE_DIR"), env.get("KITH_SERVE_PORT"));
    if let (Some(d), Some(p)) = (data, port) {
        return Some((d.to_owned(), p.to_owned()));
    }
    let mut cmd = Command::new(kernel.join("bin/kith-serve"));
    cmd.arg("endpoint");
    let out = crate::secrets::run_command_with_timeout(cmd, Duration::from_secs(4))?;
    let mut words = out.split_whitespace();
    let (d, p) = (words.next()?, words.next()?);
    if words.next().is_some() {
        return None;
    }
    Some((data.unwrap_or(d).to_owned(), port.unwrap_or(p).to_owned()))
}

/// The host's local API over loopback with the serve token. `None` when it can't be reached or doesn't say 2xx with JSON.
pub fn host_call(
    env: &Env,
    kernel: &Path,
    method: &str,
    body: &Value,
    timeout: Duration,
) -> Option<Value> {
    let (data, port) = endpoint(env, kernel)?;
    let token = std::fs::read_to_string(Path::new(&data).join("token")).ok()?;
    let token = super::pyval::strip(&token);
    let port: u16 = port.trim().parse().ok()?;
    let body = crate::secrets::pyjson::dumps(body);
    post(port, &format!("/api/{method}"), token, &body, timeout)
}

/// `POST` over a fresh connection; the JSON of a 2xx answer.
fn post(port: u16, path: &str, token: &str, body: &str, timeout: Duration) -> Option<Value> {
    let addr = ("127.0.0.1", port).to_socket_addrs().ok()?.next()?;
    let mut s = TcpStream::connect_timeout(&addr, timeout).ok()?;
    s.set_read_timeout(Some(timeout)).ok()?;
    s.set_write_timeout(Some(timeout)).ok()?;
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept-Encoding: identity\r\nAuthorization: Bearer {token}\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).ok()?;
    let (status, payload) = read_response(&mut s)?;
    let _ = s.shutdown(Shutdown::Both);
    if !(200..300).contains(&status) {
        return None;
    }
    serde_json::from_slice(&payload).ok()
}

/// `(status, body)` of an HTTP/1.x answer: by Content-Length, chunked, or to the end of the stream.
fn read_response(s: &mut TcpStream) -> Option<(u16, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
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
    if chunked {
        loop {
            if let Some(out) = dechunk(&body) {
                return Some((status, out));
            }
            let n = s.read(&mut chunk).ok()?;
            if n == 0 {
                return None;
            }
            body.extend_from_slice(&chunk[..n]);
        }
    }
    loop {
        if length.is_some_and(|l| body.len() >= l) {
            body.truncate(length.unwrap_or(0));
            return Some((status, body));
        }
        let n = s.read(&mut chunk).ok()?;
        if n == 0 {
            // a short body is an error for Python's client; no length means "to the end"
            return length.is_none().then_some((status, body));
        }
        body.extend_from_slice(&chunk[..n]);
    }
}

/// The payload of a complete chunked body, `None` while chunks are still missing.
fn dechunk(raw: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut at = 0;
    loop {
        let eol = raw[at..].windows(2).position(|w| w == b"\r\n")? + at;
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

/// The host's port takes a connection (a host that answers no to a request is up).
pub fn host_up(env: &Env, kernel: &Path) -> bool {
    let Some((_, port)) = endpoint(env, kernel) else {
        return false;
    };
    let Ok(port) = port.trim().parse::<u16>() else {
        return false;
    };
    let Some(addr) = ("127.0.0.1", port)
        .to_socket_addrs()
        .ok()
        .and_then(|mut a| a.next())
    else {
        return false;
    };
    TcpStream::connect_timeout(&addr, Duration::from_secs(2)).is_ok()
}

/// Restarts the host's launchd job as the Mac app does (apps/desktop host-service.ts): a job that was stopped is loaded
/// from its plist first, then kickstarted. Then waits for its port. False when that didn't bring it up, and on any system
/// but macOS (`mac`).
pub fn restart_host(env: &Env, kernel: &Path, mac: bool, wait: Duration) -> bool {
    if !mac {
        return false;
    }
    let label = "dev.kith.serve";
    let domain = format!("gui/{}", crate::threads::os::uid());
    let launchctl = env.get("KITH_LAUNCHCTL").unwrap_or("launchctl").to_owned();
    let run = |args: &[&str]| -> Option<i32> {
        let mut child = Command::new(&launchctl)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let start = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(st)) => return Some(st.code().unwrap_or(-1)),
                Ok(None) if start.elapsed() < Duration::from_secs(10) => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
            }
        }
    };
    let job = format!("{domain}/{label}");
    match run(&["print", &job]) {
        None => return false,
        Some(0) => {}
        Some(_) => {
            let plist: PathBuf = env
                .home
                .join("Library/LaunchAgents")
                .join(format!("{label}.plist"));
            if run(&["bootstrap", &domain, &plist.to_string_lossy()]) != Some(0) {
                return false;
            }
        }
    }
    if run(&["kickstart", "-k", &job]) != Some(0) {
        return false;
    }
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        if host_up(env, kernel) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    false
}
