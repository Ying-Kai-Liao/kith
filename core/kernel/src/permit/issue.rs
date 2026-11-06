//! `kith-permit issue AGENT SESSION PID`: ask the host for an owner ask token bound to PID and print export lines for sh.

use super::host::{HOST_START_S, host_call, host_up, restart_host};
use super::pyval::{self, repr_str, str_of, truthy};
use super::{Env, Out};
use crate::cost::pyfmt::shell_quote;
use serde_json::{Map, Value, json};
use std::path::Path;
use std::time::Duration;

/// The agent whose `kith ask` mid is (its registration), '' for a terminal.
fn asked_by(env: &Env, kernel: &Path, mid: &str) -> String {
    let reg = env
        .paths(kernel)
        .machine
        .join("live/ask")
        .join(format!("{mid}.reg"));
    let Ok(text) = std::fs::read_to_string(reg) else {
        return String::new();
    };
    let Ok(Value::Object(reg)) = serde_json::from_str::<Value>(&text) else {
        return String::new();
    };
    match reg.get("agent") {
        Some(Value::String(who)) if is_agent_name(who) => who.clone(),
        _ => String::new(),
    }
}

/// `[a-z0-9][a-z0-9._-]{0,63}`, the whole string.
fn is_agent_name(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=64).contains(&b.len())
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b[1..].iter().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'_' | b'-')
        })
}

/// `[A-Za-z0-9._-]{1,64}`, the whole string.
pub fn is_ask_id(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

/// export lines for sh, or exit 1 with one stderr line. Never raises: an owner run goes on without a prompt tool.
pub fn issue(env: &Env, kernel: &Path, agent: &str, session: &str, pid: &str) -> Out {
    let Some(pid) = pyval::int(pid) else {
        // Python dies on `int(pid)` with a traceback; this is its last line
        return Out {
            stderr: format!(
                "ValueError: invalid literal for int() with base 10: {}\n",
                repr_str(pid)
            ),
            code: 1,
            ..Out::default()
        };
    };
    let mut body = Map::new();
    body.insert("agent".to_owned(), json!(agent));
    body.insert("session".to_owned(), json!(session));
    body.insert("pid".to_owned(), json!(pid));
    // A run `kith ask` started: its cards remember that ask, and who asked (its registration), so a later answer goes back
    // to it (kith-ask).
    let mid = env.get("KITH_ASK_ID").unwrap_or("");
    if is_ask_id(mid) {
        body.insert("replyTo".to_owned(), json!(mid));
        body.insert("askedBy".to_owned(), json!(asked_by(env, kernel, mid)));
    }
    let body = Value::Object(body);
    let ask = || host_call(env, kernel, "issueAskToken", &body, Duration::from_secs(8));
    let mut r = ask().filter(|v| !v.is_null());
    if r.is_none() && !host_up(env, kernel) {
        // The host is down: its job is restarted once (rule 5) and asked again, else nothing can show a card. A terminal
        // start (KITH_PERMIT_NO_RESTART) never waits for that: the run starts, the hook prints nothing.
        let restarted = env.get("KITH_PERMIT_NO_RESTART").is_none()
            && restart_host(
                env,
                kernel,
                cfg!(target_os = "macos"),
                Duration::from_secs(HOST_START_S),
            );
        if !restarted {
            return Out {
                stderr: "kith: no approval cards for this run: the Kith host isn't running; asks are refused\n".to_owned(),
                code: 1,
                ..Out::default()
            };
        }
        r = ask().filter(|v| !v.is_null());
    }
    let vars = r
        .as_ref()
        .and_then(|v| v.get("env"))
        .and_then(Value::as_object);
    let given = |k: &str| vars.and_then(|e| e.get(k)).filter(|v| truthy(v));
    let (Some(token), Some(socket)) = (given("KITH_ASK_TOKEN"), given("KITH_ASK_SOCKET")) else {
        return Out {
            stderr: "kith: no approval cards for this run (the Kith host gave no ask token); asks are refused\n".to_owned(),
            code: 1,
            ..Out::default()
        };
    };
    Out {
        stdout: format!(
            "export KITH_ASK_TOKEN={}\nexport KITH_ASK_SOCKET={}\n",
            shell_quote(&str_of(token)),
            shell_quote(&str_of(socket))
        ),
        ..Out::default()
    }
}
