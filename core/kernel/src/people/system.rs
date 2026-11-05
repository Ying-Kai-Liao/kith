//! The real backend: `kith-auth people --json` for who is invited, the host's loopback API for invites.

use super::Backend;
use crate::secrets::pyjson;
use serde_json::Value;
use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

/// What the host call reads besides the command line.
#[derive(Clone, Debug, Default)]
pub struct Env {
    /// `KITH_SERVE_DIR`: the host's data folder (`token` lives there).
    pub serve_dir: Option<String>,
    /// `KITH_SERVE_PORT`.
    pub serve_port: Option<String>,
}

impl Env {
    pub fn from_process() -> Env {
        Env {
            serve_dir: crate::paths::process_env("KITH_SERVE_DIR"),
            serve_port: crate::paths::process_env("KITH_SERVE_PORT"),
        }
    }
}

pub struct System {
    pub kernel: PathBuf,
    pub home: PathBuf,
    pub env: Env,
}

/// How long the host gets to answer (`urlopen(timeout=30)`).
const HOST_WAIT: Duration = Duration::from_secs(30);

impl System {
    /// (data dir, port) of this install's `kith serve`: the env wins, else `kith-serve endpoint`, else the defaults.
    fn endpoint(&self) -> (PathBuf, String) {
        let given = |v: &Option<String>| v.clone().filter(|s| !s.is_empty());
        let (dir, port) = (given(&self.env.serve_dir), given(&self.env.serve_port));
        if let (Some(d), Some(p)) = (&dir, &port) {
            return (PathBuf::from(d), p.clone());
        }
        let (d, p) = self.ask_endpoint().unwrap_or_else(|| {
            (
                self.home
                    .join(".kith")
                    .join("serve")
                    .to_string_lossy()
                    .into_owned(),
                "19224".to_owned(),
            )
        });
        (PathBuf::from(dir.unwrap_or(d)), port.unwrap_or(p))
    }

    /// `kith-serve endpoint`: two words, within 4 seconds.
    fn ask_endpoint(&self) -> Option<(String, String)> {
        let mut cmd = Command::new(self.kernel.join("bin").join("kith-serve"));
        cmd.arg("endpoint");
        let out = crate::secrets::run_command_with_timeout(cmd, Duration::from_secs(4))?;
        let mut words = out.split_whitespace();
        let (d, p) = (words.next()?, words.next()?);
        words.next().is_none().then(|| (d.to_owned(), p.to_owned()))
    }
}

impl Backend for System {
    fn people(&self) -> Result<Value, String> {
        let r = Command::new(self.kernel.join("bin").join("kith-auth"))
            .args(["people", "--json"])
            .stdin(Stdio::null())
            .output()
            .map_err(|_| "kith-auth could not list people".to_owned())?;
        if !r.status.success() {
            let err = String::from_utf8_lossy(&r.stderr);
            let err = err.trim();
            return Err(if err.is_empty() {
                "kith-auth could not list people".to_owned()
            } else {
                err.strip_prefix("kith-auth: ").unwrap_or(err).to_owned()
            });
        }
        serde_json::from_slice(&r.stdout)
            .map_err(|_| "kith-auth people gave something that is not JSON".to_owned())
    }

    fn host_call(&self, method: &str, body: &Value) -> Result<Value, String> {
        let (dir, port) = self.endpoint();
        let unreachable = |why: &str| format!("kith serve is not reachable ({why})");
        let token =
            std::fs::read(dir.join("token")).map_err(|e| unreachable(&os_error_name(&e)))?;
        let token = String::from_utf8(token)
            .map_err(|_| unreachable("UnicodeDecodeError"))?
            .trim()
            .to_owned();
        let reply = post(
            &port,
            &format!("/api/{method}"),
            &token,
            &pyjson::dumps(body),
        )
        .map_err(|why| unreachable(&why))?;
        if reply.status >= 400 {
            // `{"error": "..."}` says why; anything else is only the status.
            let why = serde_json::from_slice::<Value>(&reply.body)
                .ok()
                .and_then(|v| {
                    v.get("error")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                });
            return Err(why.unwrap_or_else(|| format!("kith serve answered {}", reply.status)));
        }
        std::str::from_utf8(&reply.body)
            .map_err(|_| unreachable("UnicodeDecodeError"))
            .and_then(|t| serde_json::from_str(t).map_err(|_| unreachable("JSONDecodeError")))
    }
}

/// The name of the Python exception a failed file read raises.
fn os_error_name(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::NotFound => "FileNotFoundError",
        std::io::ErrorKind::PermissionDenied => "PermissionError",
        _ if e.raw_os_error() == Some(21) => "IsADirectoryError",
        _ => "OSError",
    }
    .to_owned()
}

struct Reply {
    status: u16,
    body: Vec<u8>,
}

/// One `POST` over loopback HTTP/1.1, `Connection: close`. The `Err` is the Python exception's name.
fn post(port: &str, path: &str, token: &str, body: &str) -> Result<Reply, String> {
    let port: u16 = port.parse().map_err(|_| "ValueError".to_owned())?;
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut s = TcpStream::connect_timeout(&addr, HOST_WAIT).map_err(|_| "URLError".to_owned())?;
    let timed_out = |e: &std::io::Error| {
        if matches!(
            e.kind(),
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
        ) {
            "TimeoutError"
        } else {
            "URLError"
        }
        .to_owned()
    };
    s.set_read_timeout(Some(HOST_WAIT))
        .map_err(|e| timed_out(&e))?;
    s.set_write_timeout(Some(HOST_WAIT))
        .map_err(|e| timed_out(&e))?;
    let head = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    s.write_all(head.as_bytes())
        .and_then(|()| s.write_all(body.as_bytes()))
        .map_err(|e| timed_out(&e))?;
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).map_err(|e| timed_out(&e))?;
    parse_reply(&raw).ok_or_else(|| "RemoteDisconnected".to_owned())
}

fn parse_reply(raw: &[u8]) -> Option<Reply> {
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = std::str::from_utf8(&raw[..split]).ok()?;
    let mut lines = head.split("\r\n");
    let status: u16 = lines.next()?.split_whitespace().nth(1)?.parse().ok()?;
    let mut chunked = false;
    let mut length = None;
    for l in lines {
        let (k, v) = l.split_once(':')?;
        match k.trim().to_ascii_lowercase().as_str() {
            "transfer-encoding" => chunked = v.to_ascii_lowercase().contains("chunked"),
            "content-length" => length = v.trim().parse::<usize>().ok(),
            _ => {}
        }
    }
    let rest = &raw[split + 4..];
    let body = if chunked {
        dechunk(rest)?
    } else {
        rest[..length.map_or(rest.len(), |n| n.min(rest.len()))].to_vec()
    };
    Some(Reply { status, body })
}

fn dechunk(mut rest: &[u8]) -> Option<Vec<u8>> {
    let mut body = Vec::new();
    loop {
        let eol = rest.windows(2).position(|w| w == b"\r\n")?;
        let size = usize::from_str_radix(
            std::str::from_utf8(&rest[..eol])
                .ok()?
                .split(';')
                .next()?
                .trim(),
            16,
        )
        .ok()?;
        rest = &rest[eol + 2..];
        if size == 0 {
            return Some(body);
        }
        body.extend_from_slice(rest.get(..size)?);
        rest = rest.get(size + 2..)?;
    }
}
