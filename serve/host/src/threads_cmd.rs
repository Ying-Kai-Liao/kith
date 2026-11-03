//! `kith kernel threads <verb>`: the writer claim and the session registry (`kernel/bin/kith-sessions`) from the
//! `kith-kernel` crate. This is the one place that asks a running host (`resolveThread` over loopback, the way
//! `kith-sessions host_call` does), because the crate never calls the host itself. Not routed from `kith` yet, and the
//! host's own claims (`terminal/claim.rs`) do not go through the crate yet either: the host never switches ahead of the CLI.

use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

use kith_kernel::paths::{process_env, process_home};
use kith_kernel::threads::{Ctx, Host, cli};
use serde_json::{Value, json};

use crate::kernel;

/// The host on this Mac, if one answers on loopback with the owner token.
struct Loopback {
    kernel: PathBuf,
    home: PathBuf,
}

impl Loopback {
    /// `(data dir, port)` of this install's host: the env wins, else `kith-serve endpoint`, else the defaults.
    fn endpoint(&self) -> (PathBuf, String) {
        let (data, port) = (process_env("KITH_SERVE_DIR"), process_env("KITH_SERVE_PORT"));
        if let (Some(d), Some(p)) = (&data, &port) {
            return (PathBuf::from(d), p.clone());
        }
        let said = std::process::Command::new(self.kernel.join("bin").join("kith-serve"))
            .arg("endpoint")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
        let mut words = said.as_deref().unwrap_or("").split_whitespace();
        let (d, p) = match (words.next(), words.next()) {
            (Some(d), Some(p)) => (d.to_owned(), p.to_owned()),
            _ => (self.home.join(".kith").join("serve").to_string_lossy().into_owned(), "19224".to_owned()),
        };
        (PathBuf::from(data.unwrap_or(d)), port.unwrap_or(p))
    }
}

impl Host for Loopback {
    fn resolve_thread(&self, thread: &str) -> Option<Value> {
        let (data, port) = self.endpoint();
        let token = std::fs::read_to_string(data.join("token")).ok()?.trim().to_owned();
        let (status, body) = post(&port, "resolveThread", &token, &json!({ "thread": thread }).to_string())?;
        // A refusal with a body is an answer ("unknown thread"); a 404 is an older host.
        if status == 404 {
            return Some(json!({"error": "unknown method", "unknownMethod": true}));
        }
        serde_json::from_slice(&body).ok()
    }
}

/// One POST to `/api/<method>` on 127.0.0.1, with the owner token; `(status, body)`. Four seconds for all of it.
fn post(port: &str, method: &str, token: &str, body: &str) -> Option<(u16, Vec<u8>)> {
    let limit = Duration::from_secs(4);
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().ok()?;
    let mut s = TcpStream::connect_timeout(&addr, limit).ok()?;
    s.set_read_timeout(Some(limit)).ok()?;
    s.set_write_timeout(Some(limit)).ok()?;
    let req = format!(
        "POST /api/{method} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).ok()?;
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).ok()?;
    parse_response(&raw)
}

fn parse_response(raw: &[u8]) -> Option<(u16, Vec<u8>)> {
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(&raw[..split]).to_ascii_lowercase();
    let status = head.lines().next()?.split_whitespace().nth(1)?.parse().ok()?;
    let rest = &raw[split + 4..];
    if !head.lines().any(|l| l.starts_with("transfer-encoding:") && l.contains("chunked")) {
        return Some((status, rest.to_vec()));
    }
    let (mut body, mut at) = (Vec::new(), 0);
    loop {
        let line_end = rest[at..].windows(2).position(|w| w == b"\r\n")? + at;
        let size =
            usize::from_str_radix(String::from_utf8_lossy(&rest[at..line_end]).split(';').next()?.trim(), 16).ok()?;
        if size == 0 {
            return Some((status, body));
        }
        let chunk = rest.get(line_end + 2..line_end + 2 + size)?;
        body.extend_from_slice(chunk);
        at = line_end + 2 + size + 2;
    }
}

/// `kith kernel threads <verb> ...`; returns the exit code.
pub fn run(kernel_dir: Option<PathBuf>, args: &[String]) -> i32 {
    let home = process_home();
    let kernel_dir = kernel_dir.or_else(kernel::current_kernel).unwrap_or_else(|| home.join("kith/kernel"));
    let machine = kith_kernel::paths::paths(&home, &kernel_dir, &process_env).machine;
    let ctx = Ctx {
        state: process_env("KITH_STATE").map(PathBuf::from),
        machine: Some(machine),
        home: home.clone(),
        env: &process_env,
    };
    let host = Loopback { kernel: kernel_dir, home };
    let stdin = || {
        let mut text = String::new();
        let _ = std::io::stdin().read_to_string(&mut text);
        text
    };
    let out = cli::run(&ctx, &host, args, &stdin);
    print!("{}", out.stdout);
    eprint!("{}", out.stderr);
    let _ = std::io::stdout().flush();
    out.code
}

#[cfg(test)]
mod tests {
    use super::parse_response;

    #[test]
    fn a_plain_and_a_chunked_response_both_read() {
        let plain = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}";
        assert_eq!(parse_response(plain), Some((200, b"{}".to_vec())));
        let chunked =
            b"HTTP/1.1 404 Not Found\r\nTransfer-Encoding: chunked\r\n\r\n3\r\n{\"a\r\n4;x=1\r\n\":1}\r\n0\r\n\r\n";
        assert_eq!(parse_response(chunked), Some((404, b"{\"a\":1}".to_vec())));
        assert_eq!(parse_response(b"garbage"), None);
    }
}
