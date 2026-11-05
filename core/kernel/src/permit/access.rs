//! `request_access` and `request_secret`: the way out of the guest sandbox (an absolute path or a host name) and the ask for a
//! service's key. The host does the judging (it refuses secrets and rule W); these relay one line and its answer.

use super::Env;
use super::host::call_host;
use super::pyval::{strip, truthy};
use serde_json::{Map, Value};
use std::time::Duration;

/// How long the host gets to answer a request that makes a card but does not wait for it.
const CALL_S: u64 = 30;

/// `msg = reply.get('message') or reply.get('error')`, then the text if it is a non-empty string, else `fallback`.
fn said(reply: &Map<String, Value>, fallback: &str) -> String {
    let msg = match reply.get("message") {
        Some(m) if truthy(m) => Some(m),
        _ => reply.get("error"),
    };
    match msg {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => fallback.to_owned(),
    }
}

/// The text the agent is told for one `request_access` call.
pub fn request_access(a: &Map<String, Value>, env: &Env) -> String {
    let null = Value::Null;
    let (path, domain) = (a.get("path").unwrap_or(&null), a.get("domain").unwrap_or(&null));
    let chosen = if truthy(path) { path } else { domain };
    let reason = a.get("reason").unwrap_or(&null);
    if truthy(path) == truthy(domain) || !chosen.is_string() || !reason.is_string() {
        return "Give either an absolute path or a domain, and a reason.".to_owned();
    }
    let kind = if truthy(path) { "path" } else { "domain" };
    let mut body = Map::new();
    body.insert("kind".to_owned(), Value::String(kind.to_owned()));
    body.insert("target".to_owned(), chosen.clone());
    body.insert("reason".to_owned(), reason.clone());
    body.insert(
        "write".to_owned(),
        Value::Bool(a.get("write") == Some(&Value::Bool(true))),
    );
    match call_host(env, "requestAccess", &body, Duration::from_secs(CALL_S)) {
        None => "No approval channel for this run: access was not requested. Say what you needed and carry on without it.".to_owned(),
        Some(reply) => said(
            &reply,
            "The approval service gave no clear answer; nothing was requested.",
        ),
    }
}

/// The text the agent is told for one `request_secret` call.
pub fn request_secret(a: &Map<String, Value>, env: &Env) -> String {
    let given = |k: &str| match a.get(k) {
        Some(Value::String(s)) if !strip(s).is_empty() => Some(s.clone()),
        _ => None,
    };
    let (Some(service), Some(purpose)) = (given("service"), given("purpose")) else {
        return "Give the service and what you need it for.".to_owned();
    };
    let mut body = Map::new();
    body.insert(
        "service".to_owned(),
        Value::String(strip(&service).to_owned()),
    );
    body.insert("purpose".to_owned(), Value::String(purpose));
    match call_host(env, "requestSecret", &body, Duration::from_secs(CALL_S)) {
        None => "No approval channel for this run: no key was requested. Say what you needed and carry on without it.".to_owned(),
        Some(reply) => said(
            &reply,
            "The approval service gave no clear answer; nothing was requested.",
        ),
    }
}
