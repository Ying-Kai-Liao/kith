//! The origin of an owner's terminal run, asked of the host (`kernel/lib/kith_runenv.py AGENT --outbox`): the call key and the
//! outbox of the agent's app chat with the owner. The host answers `runEnv` over HTTP on 127.0.0.1 with its bearer token, the
//! same on every OS; a host that is down, older or says no is no origin, and the run goes on without one.

use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use super::assemble::Env;

/// The names a terminal run takes from the host's answer.
const NAMES: [&str; 3] = ["KITH_CALL_CHAT", "KITH_CALL_KEY", "KITH_RUN_OUTBOX"];

/// Python's `urlopen(timeout=4)`.
const TIMEOUT: Duration = Duration::from_secs(4);

/// (data dir, port) of this Mac's host: the env overrides, else what `kith-serve endpoint` says, else the dev defaults.
fn host_endpoint(kernel: &Path, env: &Env) -> (PathBuf, String) {
    let var = |k: &str| env.get(k).filter(|v| !v.is_empty()).cloned();
    let (mut data, mut port) = (var("KITH_SERVE_DIR"), var("KITH_SERVE_PORT"));
    if data.is_none() || port.is_none() {
        // `kith-serve` is a bash script: only where there is bash. Windows has the defaults until its host says otherwise.
        let asked = ask_endpoint(kernel);
        let home = env.get("HOME").cloned().unwrap_or_default();
        let (d, p) = asked.unwrap_or_else(|| {
            (
                Path::new(&home)
                    .join(".kith")
                    .join("serve")
                    .to_string_lossy()
                    .into_owned(),
                "19224".to_owned(),
            )
        });
        data = data.or(Some(d));
        port = port.or(Some(p));
    }
    (
        PathBuf::from(data.unwrap_or_default()),
        port.unwrap_or_default(),
    )
}

#[cfg(unix)]
fn ask_endpoint(kernel: &Path) -> Option<(String, String)> {
    let out = crate::secrets::run_command_with_timeout(
        {
            let mut c = std::process::Command::new(kernel.join("bin/kith-serve"));
            c.arg("endpoint");
            c
        },
        Duration::from_secs(4),
    )?;
    let mut w = out.split_whitespace();
    Some((w.next()?.to_owned(), w.next()?.to_owned()))
}

#[cfg(not(unix))]
fn ask_endpoint(_kernel: &Path) -> Option<(String, String)> {
    None
}

/// One `POST /api/runEnv` with a JSON body; the response's body when the status is 200.
fn post(port: &str, token: &str, body: &str) -> Option<String> {
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().ok()?;
    let mut s = TcpStream::connect_timeout(&addr, TIMEOUT).ok()?;
    s.set_read_timeout(Some(TIMEOUT)).ok()?;
    s.set_write_timeout(Some(TIMEOUT)).ok()?;
    let req = format!(
        "POST /api/runEnv HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).ok()?;
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).ok()?;
    let text = String::from_utf8(raw).ok()?;
    let (head, rest) = text.split_once("\r\n\r\n")?;
    if head.lines().next()?.split_whitespace().nth(1)? != "200" {
        return None;
    }
    // The host answers with a Content-Length, never chunked; a chunked answer is no answer.
    if head.to_ascii_lowercase().contains("transfer-encoding") {
        return None;
    }
    Some(rest.to_owned())
}

/// The host's `runEnv` answer for the owner's run of `agent` with the run outbox, or `None` when it can't be reached or doesn't
/// answer.
pub fn fetch(agent: &str, kernel: &Path, env: &Env) -> Option<Value> {
    let (data, port) = host_endpoint(kernel, env);
    let token = std::fs::read_to_string(data.join("token")).ok()?;
    let body = json!({"agent": agent, "person": Value::Null, "outbox": true}).to_string();
    let out: Value = serde_json::from_str(&post(&port, token.trim(), &body)?).ok()?;
    out.get("env")?.is_object().then_some(out)
}

/// The variables an owner's terminal run takes from the host's answer, in the host's order; none when there is no answer.
pub fn outbox_env(agent: &str, kernel: &Path, env: &Env) -> Vec<(String, String)> {
    let Some(Value::Object(e)) = fetch(agent, kernel, env).and_then(|a| a.get("env").cloned())
    else {
        return vec![];
    };
    e.into_iter()
        .filter(|(k, _)| NAMES.contains(&k.as_str()))
        .filter_map(|(k, v)| v.as_str().map(|v| (k, v.to_owned())))
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn answer(listener: TcpListener, status: &str, body: &'static str) -> std::thread::JoinHandle<String> {
        let status = status.to_owned();
        std::thread::spawn(move || {
            let (mut c, _) = listener.accept().unwrap();
            let mut got = vec![0u8; 4096];
            let n = c.read(&mut got).unwrap();
            let resp = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            c.write_all(resp.as_bytes()).unwrap();
            String::from_utf8_lossy(&got[..n]).into_owned()
        })
    }

    fn rig(tag: &str, port: u16) -> Env {
        let dir = std::env::temp_dir().join(format!("kith-runenv-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("token"), "tok\n").unwrap();
        [
            ("KITH_SERVE_DIR", dir.to_string_lossy().into_owned()),
            ("KITH_SERVE_PORT", port.to_string()),
        ]
        .map(|(k, v)| (k.to_owned(), v))
        .into()
    }

    #[test]
    fn the_host_answers_with_the_outbox_names_only() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let env = rig("ok", l.local_addr().unwrap().port());
        let h = answer(
            l,
            "200 OK",
            r#"{"chat":"c","env":{"KITH_CALL_CHAT":"c","KITH_CALL_KEY":"k","KITH_RUN_OUTBOX":"/o","KITH_X":"no"}}"#,
        );
        let got = outbox_env("sam", Path::new("/nowhere"), &env);
        let sent = h.join().unwrap();
        assert!(sent.starts_with("POST /api/runEnv HTTP/1.1"));
        assert!(sent.contains("Authorization: Bearer tok"));
        assert!(sent.ends_with(r#"{"agent":"sam","person":null,"outbox":true}"#));
        assert_eq!(
            got,
            [("KITH_CALL_CHAT", "c"), ("KITH_CALL_KEY", "k"), ("KITH_RUN_OUTBOX", "/o")]
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
        );
    }

    #[test]
    fn a_refusal_or_a_host_that_is_down_is_no_origin() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let env = rig("no", l.local_addr().unwrap().port());
        let h = answer(l, "403 Forbidden", r#"{"error":"no"}"#);
        assert!(outbox_env("sam", Path::new("/nowhere"), &env).is_empty(), "no origin");
        h.join().unwrap();
        // Nothing listens here any more.
        assert!(outbox_env("sam", Path::new("/nowhere"), &env).is_empty(), "no origin");
    }
}
