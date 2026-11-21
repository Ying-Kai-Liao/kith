//! The guest branch of `project-id`: a run for someone other than the owner (`KITH_RUN_TOKEN` set and not empty: the token
//! alone) cannot read the registry, so it asks the core over its run socket (`KITH_CORE_RUN_SOCKET`), which answers about the
//! project the run was launched in only (the bare form, or `--info` of that project). This is `kernel/lib/kith_runsocket.py`
//! `ask` for the one tool: the same request line, the same words and exit codes. A token with no core socket is still a guest:
//! the answer is a message and exit 1, never the owner's data. Nothing here reads a Kith folder, and `run`
//! calls it before it opens the registry, so a guest can never fall through to `projects.json`.

use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use serde_json::{Map, Value, json};

use super::Out;
use crate::secrets::pyjson;

/// The token and the core's run socket of a guest run (the socket is empty when the run has none).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Guest {
    pub token: OsString,
    pub socket: OsString,
}

impl Guest {
    /// The guest the environment makes, if any: the token set and not empty, with or without a socket. Takes the raw values
    /// (`var_os`): a token that is not UTF-8 still counts, as it does for the Python.
    pub fn from_env(get: impl Fn(&str) -> Option<OsString>) -> Option<Self> {
        let value = |n: &str| get(n).filter(|v| !v.is_empty());
        Some(Self {
            token: value("KITH_RUN_TOKEN")?,
            socket: value("KITH_CORE_RUN_SOCKET").unwrap_or_default(),
        })
    }
}

/// How long one read or write may wait (`settimeout(40)` of the Python client).
const WAIT: Duration = Duration::from_secs(40);
/// A reply past this is no core's (`1 << 20` of the Python client).
const MAX_REPLY: usize = 1 << 20;
const UNREACHABLE: &str = "project-id: the Kith core could not be reached from this run";

/// Python's truthiness of a JSON value, for `not reply.get('ok')` and `reply.get('error') or ...`.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

fn exchange(guest: &Guest, args: &[String]) -> Option<Map<String, Value>> {
    if guest.socket.is_empty() {
        return None;
    }
    let mut stream = UnixStream::connect(Path::new(&guest.socket)).ok()?;
    stream.set_read_timeout(Some(WAIT)).ok()?;
    stream.set_write_timeout(Some(WAIT)).ok()?;
    let token: &OsStr = &guest.token;
    let request = json!({"token": token.to_string_lossy(), "method": "projectId", "args": args});
    let mut line = pyjson::dumps(&request).into_bytes();
    line.push(b'\n');
    stream.write_all(&line).ok()?;
    stream.shutdown(std::net::Shutdown::Write).ok()?;
    let mut data = Vec::new();
    // One byte past the limit is enough to know the reply is too large.
    stream
        .take(MAX_REPLY as u64 + 1)
        .read_to_end(&mut data)
        .ok()?;
    if data.len() > MAX_REPLY {
        return None;
    }
    match serde_json::from_str::<Value>(std::str::from_utf8(&data).ok()?).ok()? {
        Value::Object(m) => Some(m),
        _ => None,
    }
}

/// Sends one request and returns the core's stdout, stderr and exit code. A core that is missing, restarted, too old to know the
/// method or says no is a message and a non-zero code. No socket is the same message.
pub fn ask(guest: &Guest, args: &[String]) -> Out {
    let mut out = Out::default();
    let Some(reply) = exchange(guest, args) else {
        out.err(UNREACHABLE);
        out.code = 1;
        return out;
    };
    if !truthy(reply.get("ok")) {
        let why = match reply.get("error") {
            Some(Value::String(s)) if !s.is_empty() => s.clone(),
            Some(v) if truthy(Some(v)) => v.to_string(),
            _ => "the core refused".to_owned(),
        };
        out.err(format!("project-id: {why}"));
        out.code = 2;
        return out;
    }
    // A reply the Python client would die on (a null or non-integer code, a stdout that is not text) is the core not being one:
    // unreachable, exit 1, no traceback. A missing field is empty (code 0), as there.
    let unreachable = || {
        let mut out = Out::default();
        out.err(UNREACHABLE);
        out.code = 1;
        out
    };
    let text = |k: &str| match reply.get(k) {
        None => Some(String::new()),
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => None,
    };
    let code = match reply.get("code") {
        None => Some(0),
        Some(Value::Number(n)) => n.as_i64().and_then(|c| i32::try_from(c).ok()),
        Some(_) => None,
    };
    let (Some(stdout), Some(stderr), Some(code)) = (text("stdout"), text("stderr"), code) else {
        return unreachable();
    };
    out.stdout = stdout;
    out.stderr = stderr;
    out.code = code;
    out
}
