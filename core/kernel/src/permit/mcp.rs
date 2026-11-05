//! The stdio MCP server (`kith-permit` with no verb): one JSON-RPC message per line in, one reply per line out. It is the
//! permission prompt tool of every `claude -p` run kith starts, and offers `approve`, `request_access`, `report_bug` and, to a run
//! with a host to ask, `request_secret`; to a guest's run, `ask_helper` and `run_door`.

use super::access::{request_access, request_secret};
use super::bug::report_bug;
use super::decide::{decide, deny};
use super::door::run_door;
use super::helper::ask_helper;
use super::host::credentials;
use super::pyval::{truthy, type_name};
use super::Env;
use crate::secrets::pyjson::dumps;
use serde_json::{Map, Value, json};
use std::io::{BufRead, Write};
use std::path::Path;

const SERVER: &str = "kithpermit";
const APPROVE: &str = "approve";

/// Where Python would have died with a traceback (a `.get` on something that is no dict).
pub struct Crash(pub String);

fn no_get(v: &Value) -> Crash {
    Crash(format!(
        "AttributeError: '{}' object has no attribute 'get'",
        type_name(v)
    ))
}

/// `x or {}`, then the dict it must be for `.get` to work.
fn dict_or_empty(v: Option<&Value>) -> Result<Map<String, Value>, Crash> {
    match v {
        None => Ok(Map::new()),
        Some(x) if !truthy(x) => Ok(Map::new()),
        Some(Value::Object(m)) => Ok(m.clone()),
        Some(x) => Err(no_get(x)),
    }
}

#[allow(clippy::needless_pass_by_value)]
fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({"name": name, "description": description,
           "inputSchema": {"type": "object", "properties": properties, "required": required}})
}

fn approve_def() -> Value {
    tool(
        APPROVE,
        "Ask for approval of a tool call that needs a permission prompt.",
        json!({"tool_name": {"type": "string"}, "input": {"type": "object"}, "tool_use_id": {"type": "string"}}),
        &["tool_name", "input"],
    )
}

fn access_def() -> Value {
    tool(
        "request_access",
        "Ask the owner for a folder, file or website your sandbox blocks (a command failed with \"Operation not permitted\", or the \
         network was refused). Give the absolute path, or the host name, and why. The answer comes at your next turn: after this call, \
         end your turn and say what you are waiting for.",
        json!({"path": {"type": "string", "description": "absolute path of the file or folder"},
               "domain": {"type": "string", "description": "host name, e.g. pypi.org"},
               "reason": {"type": "string"},
               "write": {"type": "boolean", "description": "also write there (default: read only)"}}),
        &["reason"],
    )
}

fn secret_def() -> Value {
    tool(
        "request_secret",
        "Ask the owner for a key or token of a service you need (one the service declares) and are not allowed to use yet. The owner \
         picks an existing account or types the key into a card; you never see it. The answer comes at your next turn: after this \
         call, end your turn and say what you are waiting for.",
        json!({"service": {"type": "string", "description": "service name, e.g. github"},
               "purpose": {"type": "string", "description": "what you need it for"}}),
        &["service", "purpose"],
    )
}

fn bug_def() -> Value {
    tool(
        "report_bug",
        "Report a bug in Kith itself (not in the person's own work). Give a short title and what happened. It is scrubbed of secrets \
         and personal details, shown to the owner on a card, and filed on GitHub only if they say yes. Tell the person what you \
         reported and that it waits on the owner.",
        json!({"title": {"type": "string"},
               "details": {"type": "string", "description": "what happened, what you expected"}}),
        &["title"],
    )
}

fn helper_def() -> Value {
    tool(
        "ask_helper",
        "Ask a helper agent your membership grants and get its answer, as `kith ask AGENT \"task\"` does for an owner. A shell in \
         this run cannot start another agent, so use this tool. Give the helper agent and the whole task; it sees nothing of this \
         conversation. A helper you are not granted asks the owner first, and runs only if they say yes.",
        json!({"agent": {"type": "string", "description": "the helper agent, e.g. personal"},
               "task": {"type": "string", "description": "what to ask it, self-contained"}}),
        &["agent", "task"],
    )
}

fn door_def() -> Value {
    tool(
        "run_door",
        "Run a door your agent declares (a fixed command, such as a video render, that your shell cannot run) on one folder of your \
         person's files, and wait for it to end. It has no internet: download assets into the project first. A door the person's \
         jobs don't turn on asks the owner first.",
        json!({"door": {"type": "string", "description": "the door, e.g. render"},
               "path": {"type": "string", "description": "the project folder, absolute"}}),
        &["door", "path"],
    )
}

fn text(t: &str) -> Value {
    json!({"content": [{"type": "text", "text": t}]})
}

/// One JSON-RPC message in, the reply out (`None` for a notification).
pub fn handle(msg: &Map<String, Value>, env: &Env, kernel: &Path) -> Result<Option<Value>, Crash> {
    let mid = match msg.get("id") {
        None | Some(Value::Null) => return Ok(None),
        Some(m) => m.clone(),
    };
    let method = msg.get("method").and_then(Value::as_str);
    let result = match method {
        Some("initialize") => {
            let params = dict_or_empty(msg.get("params"))?;
            let ver = match params.get("protocolVersion") {
                Some(v) if truthy(v) => v.clone(),
                _ => Value::String("2024-11-05".to_owned()),
            };
            json!({"protocolVersion": ver, "capabilities": {"tools": {}},
                   "serverInfo": {"name": SERVER, "version": "1"}})
        }
        Some("tools/list") => {
            let mut tools = vec![approve_def(), access_def(), bug_def()];
            // a key card needs a host to ask, so request_secret is only offered to a run that has a token
            if credentials(env).is_some() {
                tools.push(secret_def());
            }
            if env.get("KITH_PERSON").is_some() {
                tools.extend([helper_def(), door_def()]);
            }
            json!({"tools": tools})
        }
        Some("ping") => json!({}),
        Some("tools/call") => call(msg, env, kernel)?,
        _ => {
            return Ok(Some(
                json!({"jsonrpc": "2.0", "id": mid, "error": {"code": -32601, "message": "method not found"}}),
            ));
        }
    };
    Ok(Some(json!({"jsonrpc": "2.0", "id": mid, "result": result})))
}

fn call(msg: &Map<String, Value>, env: &Env, kernel: &Path) -> Result<Value, Crash> {
    let p = dict_or_empty(msg.get("params"))?;
    let a = match p.get("arguments") {
        Some(x) if truthy(x) => x.clone(),
        _ => Value::Object(Map::new()),
    };
    let name = p.get("name").and_then(Value::as_str);
    // every tool but a malformed approve reads its arguments with `.get`
    let args = |f: fn(&Map<String, Value>, &Env, &Path) -> String| -> Result<Value, Crash> {
        match &a {
            Value::Object(m) => Ok(text(&f(m, env, kernel))),
            other => Err(no_get(other)),
        }
    };
    match name {
        Some("request_access") => args(|m, e, _| request_access(m, e)),
        Some("request_secret") => args(|m, e, _| request_secret(m, e)),
        Some("ask_helper") => args(ask_helper),
        Some("run_door") => args(run_door),
        Some("report_bug") => args(report_bug),
        _ => {
            if name != Some(APPROVE) {
                return Ok(text(&dumps(&deny("Malformed permission request."))));
            }
            let Value::Object(m) = &a else {
                return Err(no_get(&a));
            };
            let d = match (m.get("tool_name"), m.get("input")) {
                (Some(Value::String(t)), Some(input @ Value::Object(_))) => {
                    decide(env, kernel, t, input)
                }
                _ => deny("Malformed permission request."),
            };
            Ok(text(&dumps(&d)))
        }
    }
}

/// Reads lines until the end of `stdin`, answering each on `out` (flushed). The exit code: 0, or 1 where Python would have died.
pub fn serve(env: &Env, kernel: &Path, stdin: &mut dyn BufRead, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let mut raw = Vec::new();
    loop {
        raw.clear();
        match stdin.read_until(b'\n', &mut raw) {
            Ok(0) | Err(_) => return 0,
            Ok(_) => {}
        }
        let line = String::from_utf8_lossy(&raw);
        let line = super::pyval::strip(&line);
        if line.is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(line) {
            Ok(Value::Object(m)) => match handle(&m, env, kernel) {
                Ok(r) => r,
                Err(Crash(why)) => {
                    let _ = writeln!(err, "{why}");
                    return 1;
                }
            },
            Ok(_) => None,
            Err(_) => Some(
                json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "parse error"}}),
            ),
        };
        if let Some(r) = reply {
            let _ = writeln!(out, "{}", dumps(&r));
            let _ = out.flush();
        }
    }
}
