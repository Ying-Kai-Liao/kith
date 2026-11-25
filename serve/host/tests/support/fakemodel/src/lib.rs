//! The fake model's brain: which turn a request gets, and the bytes of the reply.

use std::path::PathBuf;
use std::sync::Mutex;

use serde_json::{Value, json};

const FALLBACK: &str = "Done.";

struct Turn {
    matches: Option<String>,
    text: Option<String>,
    tools: Vec<(String, Value)>,
    stop: String,
}

pub struct Server {
    turns: Vec<Turn>,
    used: Vec<bool>,
    fallback: String,
    log: Option<PathBuf>,
    next_id: u64,
}

impl Server {
    pub fn new(script: Option<&str>, log: Option<PathBuf>) -> Result<Self, String> {
        let v: Value = match script {
            Some(s) => serde_json::from_str(s).map_err(|e| format!("script: {e}"))?,
            None => json!({"turns": []}),
        };
        let mut turns = Vec::new();
        for t in v["turns"].as_array().ok_or("script: `turns` must be an array")? {
            let tools: Vec<(String, Value)> = t["tool_use"]
                .as_array()
                .map(|a| {
                    a.iter().map(|u| (u["name"].as_str().unwrap_or("Bash").to_string(), u["input"].clone())).collect()
                })
                .unwrap_or_default();
            let default_stop = if tools.is_empty() { "end_turn" } else { "tool_use" };
            turns.push(Turn {
                matches: t["match"].as_str().map(String::from),
                text: t["text"].as_str().map(String::from),
                stop: t["stop_reason"].as_str().unwrap_or(default_stop).to_string(),
                tools,
            });
        }
        let fallback = v["fallback"].as_str().unwrap_or(FALLBACK).to_string();
        Ok(Self { used: vec![false; turns.len()], turns, fallback, log, next_id: 0 })
    }

    /// The first unused turn whose `match` (if any) is a substring of the last user message.
    fn take(&mut self, last: &str) -> Option<usize> {
        let i = (0..self.turns.len())
            .find(|&i| !self.used[i] && self.turns[i].matches.as_deref().is_none_or(|m| last.contains(m)))?;
        self.used[i] = true;
        Some(i)
    }

    fn log(&self, method: &str, path: &str, body: &[u8]) {
        let Some(p) = &self.log else { return };
        let body = serde_json::from_slice::<Value>(body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(body).into()));
        let line = json!({"method": method, "path": path, "body": body}).to_string();
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
            use std::io::Write;
            let _ = writeln!(f, "{line}");
        }
    }
}

/// Text of the last user message: its text blocks and its tool_result contents, flattened.
fn last_user_text(req: &Value) -> String {
    let Some(m) = req["messages"].as_array().and_then(|a| a.iter().rev().find(|m| m["role"] == "user")) else {
        return String::new();
    };
    fn flat(v: &Value, out: &mut String) {
        match v {
            Value::String(s) => {
                out.push_str(s);
                out.push('\n');
            }
            Value::Array(a) => a.iter().for_each(|x| flat(x, out)),
            Value::Object(o) => ["text", "content"].iter().filter_map(|k| o.get(*k)).for_each(|x| flat(x, out)),
            _ => {}
        }
    }
    let mut out = String::new();
    flat(&m["content"], &mut out);
    out
}

fn http(status: &str, ctype: &str, body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 {status}\r\ncontent-type: {ctype}\r\ncontent-length: {}\r\nconnection: keep-alive\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

fn json_reply(v: &Value) -> Vec<u8> {
    http("200 OK", "application/json", &v.to_string())
}

fn sse(events: &[(&str, Value)]) -> Vec<u8> {
    let body: String = events.iter().map(|(n, d)| format!("event: {n}\ndata: {d}\n\n")).collect();
    // No content-length and a closed connection: the body ends where the stream does.
    format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncache-control: no-cache\r\nconnection: close\r\n\r\n{body}").into_bytes()
}

/// Answers one request. Returns the reply bytes and whether the connection may be reused.
pub fn handle(server: &Mutex<Server>, method: &str, path: &str, body: &[u8]) -> (Vec<u8>, bool) {
    let mut s = server.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    s.log(method, path, body);
    let route = path.split('?').next().unwrap_or(path);
    let req: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    match (method, route) {
        ("HEAD" | "GET", "/" | "/health") => (http("200 OK", "text/plain", "ok"), true),
        ("GET", "/v1/models") => {
            (json_reply(&json!({"data": [], "has_more": false, "first_id": null, "last_id": null})), true)
        }
        ("POST", "/v1/messages/count_tokens") => (json_reply(&json!({"input_tokens": 10})), true),
        ("POST", "/v1/messages") => {
            if req.is_null() {
                let e =
                    json!({"type": "error", "error": {"type": "invalid_request_error", "message": "body is not JSON"}});
                return (http("400 Bad Request", "application/json", &e.to_string()), true);
            }
            let model = req["model"].as_str().unwrap_or("fake").to_string();
            // Claude Code's small side calls (titles, summaries) run on the haiku model and must not eat script turns.
            let side = model.contains("haiku");
            let turn = if side { None } else { s.take(&last_user_text(&req)) };
            let (text, tools, stop) = match turn {
                Some(i) => {
                    let t = &s.turns[i];
                    (t.text.clone(), t.tools.clone(), t.stop.clone())
                }
                None if side => (Some("{}".to_string()), vec![], "end_turn".to_string()),
                None => (Some(s.fallback.clone()), vec![], "end_turn".to_string()),
            };
            s.next_id += 1;
            let id = s.next_id;
            let mut content = Vec::new();
            if let Some(t) = text {
                content.push(json!({"type": "text", "text": t}));
            }
            for (n, (name, input)) in tools.into_iter().enumerate() {
                content.push(
                    json!({"type": "tool_use", "id": format!("toolu_fake{id}_{n}"), "name": name, "input": input}),
                );
            }
            let msg = |content: &Value, stop: Value| {
                json!({"id": format!("msg_fake{id}"), "type": "message", "role": "assistant", "model": model,
                    "content": content, "stop_reason": stop, "stop_sequence": null,
                    "usage": {"input_tokens": 10, "output_tokens": 5}})
            };
            if req["stream"] != true {
                return (json_reply(&msg(&Value::Array(content), json!(stop))), true);
            }
            let mut ev =
                vec![("message_start", json!({"type": "message_start", "message": msg(&json!([]), Value::Null)}))];
            for (i, b) in content.iter().enumerate() {
                let (start, delta) = if b["type"] == "text" {
                    (json!({"type": "text", "text": ""}), json!({"type": "text_delta", "text": b["text"]}))
                } else {
                    let mut st = b.clone();
                    st["input"] = json!({});
                    (st, json!({"type": "input_json_delta", "partial_json": b["input"].to_string()}))
                };
                ev.push((
                    "content_block_start",
                    json!({"type": "content_block_start", "index": i, "content_block": start}),
                ));
                ev.push(("content_block_delta", json!({"type": "content_block_delta", "index": i, "delta": delta})));
                ev.push(("content_block_stop", json!({"type": "content_block_stop", "index": i})));
            }
            ev.push(("message_delta", json!({"type": "message_delta", "delta": {"stop_reason": stop, "stop_sequence": null}, "usage": {"output_tokens": 5}})));
            ev.push(("message_stop", json!({"type": "message_stop"})));
            (sse(&ev), false)
        }
        _ => {
            let e = json!({"type": "error", "error": {"type": "not_found_error", "message": format!("no route {method} {route}")}});
            (http("404 Not Found", "application/json", &e.to_string()), true)
        }
    }
}
