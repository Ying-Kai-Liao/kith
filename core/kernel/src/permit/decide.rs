//! `decide`: the permission decision for one ask, the dict Claude Code reads (`behavior` allow|deny). One line to the host over
//! the run socket, one line back; anything that is not a clear allow is a deny.

use super::Env;
use super::hooks::WAIT_S;
use super::host::{credentials, exchange};
use super::issue::is_ask_id;
use serde_json::{Map, Value, json};
use std::path::Path;
use std::time::Duration;

/// `deny(message)`.
pub fn deny(message: &str) -> Value {
    json!({"behavior": "deny", "message": message})
}

/// The `message` of a decision (an allow has none).
pub fn message_of(d: &Value) -> String {
    d.get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// Whether the decision is a clear allow.
pub fn allowed(d: &Value) -> bool {
    d.get("behavior").and_then(Value::as_str) == Some("allow")
}

/// The reply's `message` when it is a non-empty string.
fn text(reply: &Map<String, Value>, key: &str) -> Option<String> {
    match reply.get(key) {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

/// The permission decision for one ask.
pub fn decide(env: &Env, kernel: &Path, tool: &str, tool_input: &Value) -> Value {
    decide_within(env, kernel, tool, tool_input, Duration::from_secs(WAIT_S))
}

/// `decide` with the socket timeout given (the host waits its live window; tests wait less).
pub fn decide_within(
    env: &Env,
    kernel: &Path,
    tool: &str,
    tool_input: &Value,
    timeout: Duration,
) -> Value {
    let Some((token, path)) = credentials(env) else {
        return deny(
            "No approval channel for this run: the action was refused. Say so and carry on without it.",
        );
    };
    let mut msg = Map::new();
    msg.insert("token".to_owned(), Value::String(token));
    msg.insert(
        "method".to_owned(),
        Value::String("requestPermission".to_owned()),
    );
    msg.insert("tool".to_owned(), Value::String(tool.to_owned()));
    msg.insert("input".to_owned(), tool_input.clone());
    let line = crate::secrets::pyjson::dumps(&Value::Object(msg));
    let Some(reply) = exchange(&path, &line, timeout) else {
        return deny(
            "Could not reach the approval service, so the action was refused. End your turn and say what you needed.",
        );
    };
    let Value::Object(reply) = reply else {
        return deny("The approval service gave no clear answer; the action was refused.");
    };
    let message = text(&reply, "message");
    if reply.get("waiting") == Some(&Value::Bool(true)) {
        park(env, kernel);
    }
    let ok = reply.get("ok") == Some(&Value::Bool(true));
    let behavior = reply.get("behavior").and_then(Value::as_str);
    if ok && behavior == Some("allow") {
        return json!({"behavior": "allow", "updatedInput": tool_input});
    }
    if ok && behavior == Some("deny") {
        return deny(message.as_deref().unwrap_or("Not approved."));
    }
    let said = message
        .or_else(|| text(&reply, "error"))
        .unwrap_or_else(|| "The approval service refused the request.".to_owned());
    deny(&said)
}

/// The card outlived its live window and the run is ending its turn on it: the `kith ask` that started the run
/// (`KITH_ASK_ID`) gets "waiting" and the card's later answer instead of this turn's words (kith-ask). The marker is written
/// under the ask box's lock (kith-ask's `box_locked`), so it cannot land between the steps of an end, and not at all once the
/// ask has been ended: nothing is left to resume.
pub(super) fn park(env: &Env, kernel: &Path) {
    use fs2::FileExt as _;
    let mid = env.get("KITH_ASK_ID").unwrap_or_default();
    if !is_ask_id(mid) {
        return;
    }
    let boxed = env.paths(kernel).machine.join("live/ask");
    let Ok(()) = std::fs::create_dir_all(&boxed) else { return };
    let Ok(lock) = std::fs::File::create(boxed.join(".lock")) else { return };
    if lock.lock_exclusive().is_err() {
        return;
    }
    if crate::delegation::read_reg(&boxed, mid).is_some_and(|r| crate::delegation::truthy(r.get("ended_at"))) {
        return;
    }
    let _ = std::fs::write(boxed.join(format!("{mid}.parked")), "");
}
