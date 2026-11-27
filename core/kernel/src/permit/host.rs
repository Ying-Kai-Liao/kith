//! Reaching the host: the run socket (a JSON line each way, `call_host`; on Windows a named pipe only this account can open,
//! `kith_winsys::pipe`) and its local API over loopback (`host_call`, with the serve token), the host's port (`endpoint`,
//! `host_up`) and its launchd job (`restart_host`; on Windows it is refused by name until the service exists, W11).

use super::Env;
use crate::hostlocal;
use serde_json::{Map, Value};
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use std::process::{Command, Stdio};
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

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

/// `(token, socket path)` of the owner's run only: `KITH_ASK_TOKEN` on `KITH_ASK_SOCKET`, never the guest's pair.
pub fn ask_credentials(env: &Env) -> Option<(String, String)> {
    Some((
        env.get("KITH_ASK_TOKEN")?.to_owned(),
        env.get("KITH_ASK_SOCKET")?.to_owned(),
    ))
}

/// One request to the host over the run socket: the reply when it is a JSON object, `None` when there is no token, no
/// socket or no answer.
pub fn call_host(
    env: &Env,
    method: &str,
    body: &Map<String, Value>,
    timeout: Duration,
) -> Option<Map<String, Value>> {
    call(credentials(env)?, method, body, timeout)
}

/// The same on the owner's socket alone (`feedPost`): a run that holds only a guest's token gets no answer, so the guest
/// socket never sees the method.
pub fn call_host_ask(
    env: &Env,
    method: &str,
    body: &Map<String, Value>,
    timeout: Duration,
) -> Option<Map<String, Value>> {
    call(ask_credentials(env)?, method, body, timeout)
}

fn call(
    (token, path): (String, String),
    method: &str,
    body: &Map<String, Value>,
    timeout: Duration,
) -> Option<Map<String, Value>> {
    let mut msg = body.clone();
    msg.insert("token".to_owned(), Value::String(token));
    msg.insert("method".to_owned(), Value::String(method.to_owned()));
    match exchange(
        &path,
        &crate::secrets::pyjson::dumps(&Value::Object(msg)),
        timeout,
    )? {
        Value::Object(m) => Some(m),
        _ => None,
    }
}

/// One request line over a Unix socket and its one-line answer as JSON of any shape. `None` when the socket can't be reached,
/// the answer can't be read in time or is no JSON (Python's `OSError`, `ValueError`, `UnicodeDecodeError`).
#[cfg(unix)]
pub fn exchange(path: &str, line: &str, timeout: Duration) -> Option<Value> {
    let mut s = UnixStream::connect(path).ok()?;
    s.set_read_timeout(Some(timeout)).ok()?;
    s.set_write_timeout(Some(timeout)).ok()?;
    ask(&mut s, line)
}

/// The same over the host's named pipe (`path` is its `\\.\pipe\...` name). The pipe is owner-only, made so by the host when
/// it creates it; the client opens with identification-level impersonation, so a host that is not ours cannot act as us.
#[cfg(windows)]
pub fn exchange(path: &str, line: &str, timeout: Duration) -> Option<Value> {
    let mut s = kith_winsys::pipe::connect(path, timeout).ok()?;
    s.set_read_timeout(Some(timeout));
    s.set_write_timeout(Some(timeout));
    ask(&mut s, line)
}

/// One request line out and the one-line answer back, on a connection whose timeouts are set.
fn ask<S: Read + Write>(s: &mut S, line: &str) -> Option<Value> {
    let mut line = line.as_bytes().to_vec();
    line.push(b'\n');
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
    serde_json::from_slice::<Value>(&buf).ok()
}

/// `(data dir, port)` of this Mac's host: `KITH_SERVE_DIR` and `KITH_SERVE_PORT`, else what `kith-serve endpoint` says.
pub fn endpoint(env: &Env, kernel: &Path) -> Option<(String, String)> {
    let (data, port) = (env.get("KITH_SERVE_DIR"), env.get("KITH_SERVE_PORT"));
    if let (Some(d), Some(p)) = (data, port) {
        return Some((d.to_owned(), p.to_owned()));
    }
    asked(kernel, data, port)
}

/// What `kith-serve endpoint` says, for the half the run's variables left out.
#[cfg(unix)]
fn asked(kernel: &Path, data: Option<&str>, port: Option<&str>) -> Option<(String, String)> {
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

/// `kith-serve endpoint` is a script (windows-owner WI2): on Windows only the run's own `KITH_SERVE_DIR` and `KITH_SERVE_PORT`
/// name the host, and a run without both has none to ask. [`ENDPOINT_UNKNOWN`] says so.
#[cfg(windows)]
fn asked(_: &Path, _: Option<&str>, _: Option<&str>) -> Option<(String, String)> {
    None
}

/// Why a Windows run with no `KITH_SERVE_DIR` and `KITH_SERVE_PORT` cannot reach the host (named, never silent).
#[cfg(windows)]
pub const ENDPOINT_UNKNOWN: &str = "not on Windows yet: looking up the host's endpoint (this run has no KITH_SERVE_DIR and KITH_SERVE_PORT)";

/// The host's local API over its local endpoint with the serve token (`kith_kernel::hostlocal`: never loopback TCP). `None` when
/// it can't be reached or doesn't say 2xx with JSON.
pub fn host_call(
    env: &Env,
    kernel: &Path,
    method: &str,
    body: &Value,
    timeout: Duration,
) -> Option<Value> {
    let (data, _) = endpoint(env, kernel)?;
    let token = std::fs::read_to_string(Path::new(&data).join("token")).ok()?;
    let token = super::pyval::strip(&token);
    let body = crate::secrets::pyjson::dumps(body);
    let reply = hostlocal::post(
        Path::new(&data),
        &format!("/api/{method}"),
        token,
        &body,
        timeout,
    )
    .ok()?;
    if !(200..300).contains(&reply.status) {
        return None;
    }
    serde_json::from_slice(&reply.body).ok()
}

/// What can be said of the host: its local endpoint takes a connection ([`hostlocal::Status::Up`], a host that answers no to a
/// request is up), it is a host older than this command that has none, or it is down.
pub fn host_status(env: &Env, kernel: &Path) -> hostlocal::Status {
    let Some((data, port)) = endpoint(env, kernel) else {
        return hostlocal::Status::Down;
    };
    let Ok(port) = port.trim().parse::<u16>() else {
        return hostlocal::Status::Down;
    };
    hostlocal::status(Path::new(&data), port)
}

/// The host's local endpoint takes a connection.
#[cfg(unix)]
pub fn host_up(env: &Env, kernel: &Path) -> bool {
    host_status(env, kernel) == hostlocal::Status::Up
}

/// Why the host cannot be restarted: on Windows it is the service's job (W11), which is not built yet. Never a fall back to
/// launchctl and never a quiet skip: the caller says this to the person.
#[cfg(windows)]
pub const RESTART_REFUSED: &str = "not on Windows yet: host restart (W11)";

/// The host is not restarted from here on Windows: [`RESTART_REFUSED`] is the answer, and this says no.
#[cfg(windows)]
pub fn restart_host(_: &Env, _: &Path, _: bool, _: Duration) -> bool {
    false
}

/// Restarts the host's launchd job as the Mac app does (apps/desktop host-service.ts): a job that was stopped is loaded
/// from its plist first, then kickstarted. Then waits for its port. False when that didn't bring it up, and on any system
/// but macOS (`mac`).
#[cfg(unix)]
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
